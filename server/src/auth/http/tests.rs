#[cfg(test)]
mod tests {
    use super::*;

    fn password_state(password_hash: String) -> AppState {
        AppState::new(
            crate::config::Settings::default(),
            crate::infra::database::in_memory_pool().expect("in-memory database"),
            password_hash.clone(),
            crate::config::LocalConfig {
                application_version: env!("CARGO_PKG_VERSION").to_string(),
                admin_username: "admin".to_string(),
                admin_password_hash: password_hash,
            },
            crate::system::ResourceMonitor::frozen(Default::default()),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn persistence_failure_does_not_publish_the_new_password() {
        let old_hash = bcrypt::hash("old-password-value", 4).unwrap();
        let state = password_state(old_hash.clone());
        let result = replace_password_with(
            &state,
            "old-password-value".to_string(),
            "replacement-password-value".to_string(),
            "".to_string(),
            |_config| async { Err(anyhow::anyhow!("simulated fsync failure")) },
        )
        .await;

        assert!(result.is_err());
        assert_eq!(
            state.auth.local_config.read().await.admin_password_hash,
            old_hash,
            "memory must remain on the last successfully persisted snapshot"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_changes_using_the_same_old_password_cannot_both_commit() {
        let old_hash = bcrypt::hash("old-password-value", 4).unwrap();
        let state = password_state(old_hash);
        let commits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let spawn_change = |new_password: &'static str| {
            let state = state.clone();
            let commits = commits.clone();
            tokio::spawn(async move {
                replace_password_with(
                    &state,
                    "old-password-value".to_string(),
                    new_password.to_string(),
                    "".to_string(),
                    move |config| async move {
                        commits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        Ok(config)
                    },
                )
                .await
            })
        };

        let (first, second) = tokio::join!(
            spawn_change("first-replacement-password"),
            spawn_change("second-replacement-password")
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_ne!(first.is_ok(), second.is_ok());
        assert_eq!(commits.load(std::sync::atomic::Ordering::SeqCst), 1);

        let final_hash = state
            .auth
            .local_config
            .read()
            .await
            .admin_password_hash
            .clone();
        let first_won = bcrypt::verify("first-replacement-password", &final_hash).unwrap();
        let second_won = bcrypt::verify("second-replacement-password", &final_hash).unwrap();
        assert_ne!(first_won, second_won);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn login_verified_with_old_password_cannot_outlive_password_change() {
        let state = password_state(bcrypt::hash("old-password-value", 4).unwrap());
        let authenticated = authenticate(
            &state,
            "admin",
            "old-password-value".to_string(),
            None,
        )
        .await
        .expect("the old password should verify before the change");

        replace_password_with(
            &state,
            "old-password-value".to_string(),
            "replacement-password-value".to_string(),
            "".to_string(),
            |config| async move { Ok(config) },
        )
        .await
        .expect("password replacement should succeed");

        assert!(matches!(
            finalize_login(&state, authenticated).await,
            Err(AppError::Unauthorized)
        ));
        assert!(
            state.auth.sessions.read().await.is_empty(),
            "a login authenticated against the superseded hash must not create a session"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelling_the_request_cannot_split_persisted_and_live_password_state() {
        let old_hash = bcrypt::hash("old-password-value", 4).unwrap();
        let state = password_state(old_hash.clone());
        let read_guard = state.auth.local_config.read().await;
        let (persisted_tx, persisted_rx) = tokio::sync::oneshot::channel();

        let request_state = state.clone();
        let request = tokio::spawn(async move {
            replace_password_with(
                &request_state,
                "old-password-value".to_string(),
                "replacement-password-value".to_string(),
                "".to_string(),
                move |config| async move {
                    let persisted_snapshot = config.clone();
                    let _ = persisted_tx.send(persisted_snapshot);
                    Ok(config)
                },
            )
            .await
        });

        let persisted = persisted_rx
            .await
            .expect("the replacement snapshot should reach durable persistence");
        assert!(bcrypt::verify(
            "replacement-password-value",
            &persisted.admin_password_hash
        )
        .unwrap());
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert_eq!(
            read_guard.admin_password_hash, old_hash,
            "the held read lock should still expose the pre-transaction snapshot"
        );
        drop(read_guard);

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let current_hash = state
                    .auth
                    .local_config
                    .read()
                    .await
                    .admin_password_hash
                    .clone();
                if bcrypt::verify("replacement-password-value", &current_hash).unwrap() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the detached transaction must publish after the request is cancelled");
    }

    #[tokio::test]
    async fn revoking_a_session_notifies_its_established_sse_stream() {
        let state = password_state(bcrypt::hash("old-password-value", 4).unwrap());
        state.auth.sessions.write().await.insert(
            "revoked-session".to_string(),
            LocalSession {
                username: "admin".to_string(),
                expires_at: chrono::Utc::now() + chrono::Duration::minutes(5),
                csrf_token: "csrf".to_string(),
            },
        );
        let mut cancellation = sse_session_cancellation(&state, "revoked-session")
            .await
            .unwrap();

        revoke_session(&state, "revoked-session").await;
        tokio::time::timeout(std::time::Duration::from_secs(1), cancellation.cancelled())
            .await
            .expect("SSE cancellation must be signalled promptly");
        assert!(cancellation.is_cancelled());
    }

    #[tokio::test]
    async fn password_change_revokes_other_session_streams_but_keeps_the_caller() {
        let state = password_state(bcrypt::hash("old-password-value", 4).unwrap());
        let expires_at = chrono::Utc::now() + chrono::Duration::minutes(5);
        for token in ["current-session", "other-session"] {
            state.auth.sessions.write().await.insert(
                token.to_string(),
                LocalSession {
                    username: "admin".to_string(),
                    expires_at,
                    csrf_token: "csrf".to_string(),
                },
            );
        }
        let mut current = sse_session_cancellation(&state, "current-session")
            .await
            .unwrap();
        let mut other = sse_session_cancellation(&state, "other-session")
            .await
            .unwrap();

        revoke_user_sessions_except(&state, "admin", "current-session").await;

        tokio::time::timeout(std::time::Duration::from_secs(1), other.cancelled())
            .await
            .expect("revoked device SSE must close promptly");
        assert!(other.is_cancelled());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), current.cancelled())
                .await
                .is_err(),
            "the session performing the password change should remain connected"
        );
        assert!(!current.is_cancelled());
    }

    fn headers(forwarded_for: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in forwarded_for {
            headers.append("x-forwarded-for", HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    fn ip(value: &str) -> std::net::IpAddr {
        value.parse().unwrap()
    }

    /// 单行 XFF：取最右项，即离本服务最近的那一跳写入的地址。
    #[test]
    fn takes_the_rightmost_entry_of_a_single_header() {
        assert_eq!(
            client_ip(&headers(&["1.2.3.4, 5.6.7.8, 203.0.113.9"])),
            Some(ip("203.0.113.9"))
        );
    }

    /// 反代另起一行追加 XFF 时（Traefik、部分 ALB），攻击者自带的那一行是**第一个**。
    ///
    /// 回归：此前用 `HeaderMap::get()` 只读第一个头，于是这里会返回攻击者完全可控的
    /// `1.2.3.4`——每次请求换一个伪造 IP 即可绕过按 IP 与按 (IP,用户名) 的两层登录
    /// 限流，且不产生任何异常信号。
    #[test]
    fn a_spoofed_first_header_cannot_shadow_the_proxy_appended_one() {
        assert_eq!(
            client_ip(&headers(&["1.2.3.4", "203.0.113.9"])),
            Some(ip("203.0.113.9")),
            "必须采信最后一个 XFF 头，而不是攻击者自带的第一个"
        );
        // 攻击者在自己那一行里塞多少伪造项都不影响结果。
        assert_eq!(
            client_ip(&headers(&["9.9.9.9, 8.8.8.8", "203.0.113.9"])),
            Some(ip("203.0.113.9"))
        );
    }

    /// 伪造项无法通过"不可解析"把取值挤回上一项。
    #[test]
    fn unparseable_entries_are_skipped_within_the_trusted_header() {
        assert_eq!(
            client_ip(&headers(&["1.2.3.4", "203.0.113.9, not-an-ip"])),
            Some(ip("203.0.113.9"))
        );
    }

    #[test]
    fn missing_or_unparseable_forwarded_for_yields_none() {
        assert_eq!(client_ip(&HeaderMap::new()), None);
        assert_eq!(client_ip(&headers(&["not-an-ip"])), None);
        assert_eq!(client_ip(&headers(&[""])), None);
    }

    /// IPv6 与带端口的写法。带端口的项不可解析为 `IpAddr`，会被跳过——
    /// 这是刻意的：宁可取不到来源、退回全局桶，也不要把端口误当作地址的一部分。
    #[test]
    fn ipv6_entries_are_parsed() {
        assert_eq!(
            client_ip(&headers(&["1.2.3.4", "2001:db8::1"])),
            Some(ip("2001:db8::1"))
        );
    }
}
