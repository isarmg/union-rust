#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;
    use crate::{
        config::{LocalConfig, Settings},
        infra::database,
    };
    use axum::{body::Body, http::Request};
    use futures_util::stream;
    use sqlx_core::query::query;
    use tower::ServiceExt;
    use unionc_protocol::AGENT_REPORT_MAX_BODY_BYTES;

    fn state_with_pool(pool: database::DbPool) -> AppState {
        AppState::new(
            Settings::default(),
            pool,
            "unused".to_string(),
            LocalConfig {
                application_version: env!("CARGO_PKG_VERSION").to_string(),
                admin_username: "admin".to_string(),
                admin_password_hash: "unused".to_string(),
            },
            crate::system::ResourceMonitor::frozen(Default::default()),
        )
    }

    fn state() -> AppState {
        state_with_pool(database::in_memory_pool().expect("in-memory test pool"))
    }

    async fn authenticated_report_state() -> (AppState, &'static str) {
        const TOKEN: &str = "unit-test-report-token";

        let pool = database::in_memory_pool().expect("in-memory test pool");
        database::initialize_schema(&pool)
            .await
            .expect("initialize report test schema");
        let host_id = uuid::Uuid::new_v4().to_string();
        let now = database::now_epoch_micros();
        let mut transaction = database::begin_write(&pool)
            .await
            .expect("begin report fixture transaction");
        query(
            r#"
            INSERT INTO monitored_hosts(
                host_id,name,os,arch,agent_version,registered_at,last_seen_at
            ) VALUES(?1,'test host','linux','x86_64','test',?2,?2)
            "#,
        )
        .bind(&host_id)
        .bind(now)
        .execute(transaction.connection())
        .await
        .expect("insert report host fixture");
        query(
            r#"
            INSERT INTO agent_credentials(credential_id,host_id,token_hash,issued_at)
            VALUES(?1,?2,?3,?4)
            "#,
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&host_id)
        .bind(token_hash(TOKEN))
        .bind(now)
        .execute(transaction.connection())
        .await
        .expect("insert report credential fixture");
        transaction.commit().await.expect("commit report fixture");
        (state_with_pool(pool), TOKEN)
    }

    fn observed_body(polled: Arc<AtomicBool>) -> Body {
        Body::from_stream(stream::once(async move {
            polled.store(true, Ordering::SeqCst);
            Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![
                b'x';
                AGENT_REPORT_MAX_BODY_BYTES
                    + 1
            ]))
        }))
    }

    #[test]
    fn route_uuids_must_use_canonical_text() {
        assert_eq!(
            validate_uuid("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "pairing request id").unwrap(),
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        );
        for value in [
            "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
            "aaaaaaaaaaaa4aaa8aaaaaaaaaaaaaaa",
        ] {
            assert!(validate_uuid(value, "pairing request id").is_err());
        }
    }

    #[test]
    fn raw_json_handlers_require_one_json_content_type() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("Application/JSON; Charset=\"UTF-8\""),
        );
        require_json_content_type(&headers).unwrap();

        for value in [
            "text/plain",
            "application/problem+json",
            "application/json;",
        ] {
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(value).unwrap());
            assert!(matches!(
                require_json_content_type(&headers),
                Err(AppError::UnsupportedMediaType(_))
            ));
        }

        headers.clear();
        assert!(matches!(
            require_json_content_type(&headers),
            Err(AppError::UnsupportedMediaType(_))
        ));
        headers.append(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.append(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        assert!(matches!(
            require_json_content_type(&headers),
            Err(AppError::UnsupportedMediaType(_))
        ));
    }

    #[tokio::test]
    async fn report_route_uses_the_shared_exact_body_limit() {
        let (state, token) = authenticated_report_state().await;
        let app = agent_router().with_state(state);
        let request = |size| {
            Request::post("/api/agent/v1/report")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(vec![b' '; size]))
                .unwrap()
        };

        let boundary = app
            .clone()
            .oneshot(request(AGENT_REPORT_MAX_BODY_BYTES))
            .await
            .unwrap();
        assert_eq!(
            boundary.status(),
            StatusCode::BAD_REQUEST,
            "the documented maximum must reach JSON parsing in the handler"
        );

        let oversized = app
            .oneshot(request(AGENT_REPORT_MAX_BODY_BYTES + 1))
            .await
            .unwrap();
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn report_authentication_and_media_type_run_before_body_polling() {
        let (state, token) = authenticated_report_state().await;
        let app = agent_router().with_state(state);

        let unknown_polled = Arc::new(AtomicBool::new(false));
        let unknown = app
            .clone()
            .oneshot(
                Request::post("/api/agent/v1/report")
                    .header(header::AUTHORIZATION, "Bearer unknown-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(observed_body(unknown_polled.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
        assert!(
            !unknown_polled.load(Ordering::SeqCst),
            "an unknown credential must be rejected without polling the body"
        );

        let media_type_polled = Arc::new(AtomicBool::new(false));
        let unsupported = app
            .oneshot(
                Request::post("/api/agent/v1/report")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(observed_body(media_type_polled.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unsupported.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(
            !media_type_polled.load(Ordering::SeqCst),
            "an unsupported media type must be rejected without polling the body"
        );
    }

    #[test]
    fn report_storage_failures_preserve_protocol_errors_and_retry_database_failures() {
        let conflict = anyhow::Error::new(
            crate::monitoring::store::StoreReportError::ReportIdBelongsToAnotherHost,
        );
        assert!(matches!(
            map_store_report_error(conflict),
            AppError::Conflict(_)
        ));

        let revoked = anyhow::Error::new(
            crate::monitoring::store::StoreReportError::HostNotActive,
        );
        assert!(matches!(
            map_store_report_error(revoked),
            AppError::AgentRevoked
        ));

        let superseded = anyhow::Error::new(
            crate::monitoring::store::StoreReportError::CredentialNotActive,
        );
        assert!(matches!(
            map_store_report_error(superseded),
            AppError::Unauthorized
        ));

        let database = map_store_report_error(anyhow::anyhow!("database is locked"));
        assert!(matches!(&database, AppError::DatabaseUnavailable(_)));
        assert_eq!(database.code(), "database_unavailable");
        assert_eq!(
            database.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn unauthenticated_report_flood_is_isolated_by_source() {
        let state = state();
        let attacker = "203.0.113.9".parse().unwrap();
        let other = "198.51.100.20".parse().unwrap();
        let now = Instant::now();
        state.agents.report_auth_attempts_by_ip.lock().await.insert(
            attacker,
            std::iter::repeat_n(now, MAX_REPORT_AUTH_PER_IP).collect(),
        );
        *state.agents.report_auth_attempts.lock().await =
            std::iter::repeat_n(now, MAX_REPORT_AUTH_PER_IP).collect();

        assert!(matches!(
            check_report_auth_rate(&state, Some(attacker)).await,
            Err(AppError::TooManyRequests(_))
        ));
        assert!(
            check_report_auth_rate(&state, Some(other)).await.is_ok(),
            "one source must not consume another source's allowance"
        );
    }

    #[tokio::test]
    async fn unauthenticated_report_global_flood_is_bounded() {
        let state = state();
        *state.agents.report_auth_attempts.lock().await =
            std::iter::repeat_n(Instant::now(), MAX_REPORT_AUTH_GLOBAL).collect();
        assert!(matches!(
            check_report_auth_rate(&state, None).await,
            Err(AppError::TooManyRequests(_))
        ));
    }

    #[tokio::test]
    async fn saturated_global_limits_do_not_create_source_buckets() {
        let state = state();
        let source = "2001:db8::1".parse().unwrap();
        let now = Instant::now();

        *state.agents.pairing_attempts.lock().await =
            std::iter::repeat_n(now, MAX_PAIRING_GLOBAL).collect();
        assert!(check_pairing_rate(&state, Some(source)).await.is_err());
        assert!(state.agents.pairing_attempts_by_ip.lock().await.is_empty());

        *state.agents.report_auth_attempts.lock().await =
            std::iter::repeat_n(now, MAX_REPORT_AUTH_GLOBAL).collect();
        assert!(check_report_auth_rate(&state, Some(source)).await.is_err());
        assert!(
            state
                .agents
                .report_auth_attempts_by_ip
                .lock()
                .await
                .is_empty()
        );
    }
}
