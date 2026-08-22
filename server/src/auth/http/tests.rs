#[cfg(test)]
mod tests {
    use super::*;

    async fn password_state(password_hash: String) -> AppState {
        password_state_with_settings(password_hash, crate::config::Settings::default()).await
    }

    async fn password_state_with_settings(
        password_hash: String,
        mut settings: crate::config::Settings,
    ) -> AppState {
        settings.database.url = ":memory:".to_string();
        let pool = crate::infra::database::in_memory_pool().expect("in-memory database");
        crate::infra::database::initialize_schema(&pool)
            .await
            .expect("initialize password test database");
        AppState::new(
            settings,
            pool,
            password_hash.clone(),
            crate::config::LocalConfig {
                application_version: env!("CARGO_PKG_VERSION").to_string(),
                admin_username: "admin".to_string(),
                admin_password_hash: password_hash,
            },
            crate::system::ResourceMonitor::frozen(Default::default()),
        )
        .expect("capture in-memory database identity")
    }

    fn observed_login_body(polled: std::sync::Arc<std::sync::atomic::AtomicBool>) -> axum::body::Body {
        axum::body::Body::from_stream(futures_util::stream::once(async move {
            polled.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
                br#"{"username":"admin","password":"irrelevant"}"#,
            ))
        }))
    }

    #[tokio::test]
    async fn login_admission_runs_before_body_polling() {
        use tower::ServiceExt;

        let production = crate::config::Settings {
            production: true,
            server: crate::config::ServerSettings {
                proxy_secret: "test-proxy-secret".to_string(),
                ..crate::config::ServerSettings::default()
            },
            ..crate::config::Settings::default()
        };
        let proxy_state = password_state_with_settings("unused".to_string(), production).await;
        let proxy_body_polled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let proxy_rejection = router()
            .with_state(proxy_state)
            .oneshot(
                axum::http::Request::post("/api/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(observed_login_body(proxy_body_polled.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(proxy_rejection.status(), StatusCode::MISDIRECTED_REQUEST);
        assert!(
            !proxy_body_polled.load(std::sync::atomic::Ordering::SeqCst),
            "an untrusted login request must be rejected without polling its body"
        );

        let quota_state = password_state("unused".to_string()).await;
        quota_state.auth.login_attempts.lock().await.global =
            std::iter::repeat_n(std::time::Instant::now(), MAX_GLOBAL_LOGIN_ATTEMPTS).collect();
        let quota_body_polled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let quota_rejection = router()
            .with_state(quota_state)
            .oneshot(
                axum::http::Request::post("/api/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(observed_login_body(quota_body_polled.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(quota_rejection.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            !quota_body_polled.load(std::sync::atomic::Ordering::SeqCst),
            "a rate-limited login request must be rejected without polling its body"
        );

        let media_type_state = password_state("unused".to_string()).await;
        let media_type_body_polled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let media_type_rejection = router()
            .with_state(media_type_state)
            .oneshot(
                axum::http::Request::post("/api/auth/login")
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(observed_login_body(media_type_body_polled.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            media_type_rejection.status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        assert!(
            !media_type_body_polled.load(std::sync::atomic::Ordering::SeqCst),
            "an unsupported login media type must be rejected without polling its body"
        );
    }

    #[tokio::test]
    async fn login_admission_counts_each_layer_exactly_once() {
        use tower::ServiceExt;

        let hash = bcrypt::hash("known-test-password", 4).unwrap();
        let state = password_state(hash).await;
        let client: std::net::IpAddr = "203.0.113.9".parse().unwrap();
        let response = router()
            .with_state(state.clone())
            .oneshot(
                axum::http::Request::post("/api/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-forwarded-for", client.to_string())
                    .body(axum::body::Body::from(
                        r#"{"username":"No-Such-User","password":"wrong-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let attempts = state.auth.login_attempts.lock().await;
        assert_eq!(attempts.global.len(), 1);
        assert_eq!(attempts.by_ip.get(&client).map(Vec::len), Some(1));
        assert_eq!(
            attempts
                .by_ip_username
                .get(&(client, "no-such-user".to_string()))
                .map(Vec::len),
            Some(1)
        );
    }

    #[tokio::test]
    async fn malformed_login_json_charges_only_the_source_layers() {
        use tower::ServiceExt;

        let state = password_state("unused".to_string()).await;
        let client: std::net::IpAddr = "198.51.100.20".parse().unwrap();
        let response = router()
            .with_state(state.clone())
            .oneshot(
                axum::http::Request::post("/api/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-forwarded-for", client.to_string())
                    .body(axum::body::Body::from(r#"{"username": "#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let attempts = state.auth.login_attempts.lock().await;
        assert_eq!(attempts.global.len(), 1);
        assert_eq!(attempts.by_ip.get(&client).map(Vec::len), Some(1));
        assert!(attempts.by_ip_username.is_empty());
    }

    #[tokio::test]
    async fn full_account_bucket_does_not_double_charge_source_layers() {
        use tower::ServiceExt;

        let state = password_state("unused".to_string()).await;
        let client: std::net::IpAddr = "192.0.2.25".parse().unwrap();
        state.auth.login_attempts.lock().await.by_ip_username.insert(
            (client, "admin".to_string()),
            std::iter::repeat_n(std::time::Instant::now(), MAX_LOGIN_ATTEMPTS).collect(),
        );
        let response = router()
            .with_state(state.clone())
            .oneshot(
                axum::http::Request::post("/api/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-forwarded-for", client.to_string())
                    .body(axum::body::Body::from(
                        r#"{"username":"admin","password":"wrong-password"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        let attempts = state.auth.login_attempts.lock().await;
        assert_eq!(attempts.global.len(), 1);
        assert_eq!(attempts.by_ip.get(&client).map(Vec::len), Some(1));
        assert_eq!(
            attempts
                .by_ip_username
                .get(&(client, "admin".to_string()))
                .map(Vec::len),
            Some(MAX_LOGIN_ATTEMPTS)
        );
    }

    async fn replace_test_password<P, Fut>(
        state: &AppState,
        current_password: String,
        new_password: String,
        request_id: &str,
        persist: P,
    ) -> AppResult<()>
    where
        P: FnOnce(crate::config::LocalConfig) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<crate::config::LocalConfig>>
            + Send
            + 'static,
    {
        database::with_audit_context(
            database::AuditContext {
                actor: "test-admin".to_string(),
                request_id: Some(request_id.to_string()),
            },
            replace_password_with(state, current_password, new_password, persist),
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn persistence_failure_does_not_publish_the_new_password() {
        let old_hash = bcrypt::hash("old-password-value", 4).unwrap();
        let state = password_state(old_hash.clone()).await;
        let result = replace_test_password(
            &state,
            "old-password-value".to_string(),
            "replacement-password-value".to_string(),
            "failed-persistence-request",
            |_config| async { Err(anyhow::anyhow!("simulated fsync failure")) },
        )
        .await;

        assert!(result.is_err());
        assert_eq!(
            state.auth.local_config.read().await.admin_password_hash,
            old_hash,
            "memory must remain on the last successfully persisted snapshot"
        );
        let audit = database::list_audit_logs(state.db().as_ref(), None, 10)
            .await
            .expect("load audit rows after failed password change");
        assert!(
            audit
                .entries
                .iter()
                .all(|entry| entry.action != "auth.password.change"),
            "a failed password change must not record a success audit event"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_changes_using_the_same_old_password_cannot_both_commit() {
        let old_hash = bcrypt::hash("old-password-value", 4).unwrap();
        let state = password_state(old_hash).await;
        let commits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let spawn_change = |new_password: &'static str| {
            let state = state.clone();
            let commits = commits.clone();
            tokio::spawn(async move {
                replace_test_password(
                    &state,
                    "old-password-value".to_string(),
                    new_password.to_string(),
                    new_password,
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
        let state = password_state(bcrypt::hash("old-password-value", 4).unwrap()).await;
        let authenticated = authenticate(
            &state,
            "admin",
            "old-password-value".to_string(),
            LoginAttemptReservation { client: None },
        )
        .await
        .expect("the old password should verify before the change");

        replace_test_password(
            &state,
            "old-password-value".to_string(),
            "replacement-password-value".to_string(),
            "login-race-request",
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
        let state = password_state(old_hash.clone()).await;
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
        let read_guard = state.auth.local_config.read().await;
        let (persisted_tx, persisted_rx) = tokio::sync::oneshot::channel();

        let request_state = state.clone();
        let request = tokio::spawn(async move {
            replace_test_password(
                &request_state,
                "old-password-value".to_string(),
                "replacement-password-value".to_string(),
                "cancelled-password-request",
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
        assert!(
            state.auth.sessions.read().await.is_empty(),
            "the detached transaction must revoke every user session"
        );

        let audit = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let page = database::list_audit_logs(state.db().as_ref(), None, 10)
                    .await
                    .expect("load password change audit rows");
                if page
                    .entries
                    .iter()
                    .any(|entry| entry.action == "auth.password.change")
                {
                    break page;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the detached transaction must finish its audit attempt");
        let password_audits = audit
            .entries
            .iter()
            .filter(|entry| entry.action == "auth.password.change")
            .collect::<Vec<_>>();
        assert_eq!(password_audits.len(), 1);
        let entry = password_audits[0];
        assert_eq!(entry.target, "admin");
        assert_eq!(entry.actor, "test-admin");
        assert_eq!(
            entry.request_id.as_deref(),
            Some("cancelled-password-request")
        );
    }

    #[tokio::test]
    async fn revoking_a_session_notifies_its_established_sse_stream() {
        let state = password_state(bcrypt::hash("old-password-value", 4).unwrap()).await;
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
    async fn password_change_revokes_every_session_stream_including_the_caller() {
        let state = password_state(bcrypt::hash("old-password-value", 4).unwrap()).await;
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

        revoke_user_sessions(&state, "admin").await;

        tokio::time::timeout(std::time::Duration::from_secs(1), other.cancelled())
            .await
            .expect("revoked device SSE must close promptly");
        assert!(other.is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(1), current.cancelled())
            .await
            .expect("the session performing the password change must also close");
        assert!(current.is_cancelled());
        assert!(state.auth.sessions.read().await.is_empty());
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

    /// 最右项由最近的可信代理负责，非法时必须拒绝整个头，不能向左回退到攻击者值。
    #[test]
    fn an_unparseable_rightmost_entry_rejects_the_entire_header() {
        for value in [
            "198.51.100.1, not-an-ip",
            "198.51.100.1,",
            "198.51.100.1, 203.0.113.9:443",
        ] {
            assert_eq!(client_ip(&headers(&[value])), None, "accepted {value:?}");
        }
        assert_eq!(
            client_ip(&headers(&["198.51.100.1", "not-an-ip"])),
            None,
            "an invalid final header must not expose the preceding client-controlled header"
        );
    }

    #[test]
    fn missing_or_unparseable_forwarded_for_yields_none() {
        assert_eq!(client_ip(&HeaderMap::new()), None);
        assert_eq!(client_ip(&headers(&["not-an-ip"])), None);
        assert_eq!(client_ip(&headers(&[""])), None);
    }

    /// 裸 IPv6 可解析；带端口的最右项由上一测试证明会拒绝整个 XFF。
    #[test]
    fn ipv6_entries_are_parsed() {
        assert_eq!(
            client_ip(&headers(&["1.2.3.4", "2001:db8::1"])),
            Some(ip("2001:db8::1"))
        );
    }
}
