use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use futures_util::StreamExt;
use tower::ServiceExt;
use unionc::{
    config::{LocalConfig, Settings},
    http,
    infra::database,
    state::{
        AppState, LocalSession, MAX_PENDING_SSE_TICKETS, MAX_PENDING_SSE_TICKETS_PER_SESSION,
        SSE_TICKET_TTL, SseTicket,
    },
};

async fn test_state() -> AppState {
    test_state_with_settings(Settings::default()).await
}

async fn test_state_with_settings(mut settings: Settings) -> AppState {
    settings.database.url = ":memory:".to_string();
    let pool = database::connect(&settings)
        .await
        .expect("connect in-memory SQLite");
    database::initialize_schema(&pool)
        .await
        .expect("initialize in-memory SQLite schema");
    let test_password_hash = bcrypt::hash("test-password", 4).expect("test bcrypt hash");
    AppState::new(
        settings,
        pool,
        test_password_hash.clone(),
        LocalConfig {
            application_version: env!("CARGO_PKG_VERSION").to_string(),
            admin_username: "admin".to_string(),
            admin_password_hash: test_password_hash,
        },
        unionc::system::ResourceMonitor::frozen(Default::default()),
    )
    .expect("capture in-memory database identity")
}

/// 会话绑定的 CSRF 令牌。改为双提交模式后，固定值 "1" 不再被接受。
const TEST_CSRF_TOKEN: &str = "test-csrf-token-value";
const TEST_PROXY_SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

async fn insert_expired_session(state: &AppState, token: &str) {
    state.auth.sessions.write().await.insert(
        token.to_string(),
        LocalSession {
            username: "admin".to_string(),
            expires_at: chrono::Utc::now() - chrono::Duration::minutes(5),
            csrf_token: TEST_CSRF_TOKEN.to_string(),
        },
    );
}

async fn insert_session(state: &AppState, token: &str) {
    state.auth.sessions.write().await.insert(
        token.to_string(),
        LocalSession {
            username: "admin".to_string(),
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(5),
            csrf_token: TEST_CSRF_TOKEN.to_string(),
        },
    );
}

#[tokio::test]
async fn health_is_public_but_current_user_requires_authentication() {
    let mut state = test_state().await;
    state.started_at = std::time::Instant::now() - std::time::Duration::from_secs(2);
    let app = http::router(state);
    let health = app
        .clone()
        .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let payload: serde_json::Value =
        serde_json::from_slice(&to_bytes(health.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert!(
        payload["uptime_seconds"]
            .as_i64()
            .is_some_and(|value| value >= 2)
    );

    let current_user = app
        .oneshot(Request::get("/api/auth/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(current_user.status(), StatusCode::UNAUTHORIZED);
    let payload: serde_json::Value =
        serde_json::from_slice(&to_bytes(current_user.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(payload["code"], "unauthorized");
    let mut keys = payload
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(keys, ["code", "message"]);
}

#[tokio::test]
async fn ready_reuses_a_fresh_database_health_snapshot() {
    let state = test_state().await;
    *state.database_health.lock().await = Some(unionc::state::DatabaseHealthSnapshot {
        checked_at: std::time::Instant::now(),
        available: false,
    });

    let response = http::router(state)
        .oneshot(Request::get("/api/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let payload: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(payload["database"], false);
}

#[tokio::test]
async fn ready_invalidates_a_fresh_success_after_database_replacement() {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("unionc.db");
    let replacement = directory.path().join("replacement.db");
    let displaced = directory.path().join("displaced.db");

    let mut settings = Settings::default();
    settings.database.url = path.display().to_string();
    let pool = database::connect(&settings)
        .await
        .expect("connect runtime database");
    database::initialize_schema(&pool)
        .await
        .expect("initialize runtime database");

    let mut replacement_settings = Settings::default();
    replacement_settings.database.url = replacement.display().to_string();
    let replacement_pool = database::connect(&replacement_settings)
        .await
        .expect("connect replacement database");
    database::initialize_schema(&replacement_pool)
        .await
        .expect("initialize replacement database");
    replacement_pool.close().await;

    let test_password_hash = bcrypt::hash("test-password", 4).expect("test bcrypt hash");
    let state = AppState::new(
        settings,
        pool,
        test_password_hash.clone(),
        LocalConfig {
            application_version: env!("CARGO_PKG_VERSION").to_string(),
            admin_username: "admin".to_string(),
            admin_password_hash: test_password_hash,
        },
        unionc::system::ResourceMonitor::frozen(Default::default()),
    )
    .expect("capture runtime database identity");
    let app = http::router(state.clone());

    let healthy = app
        .clone()
        .oneshot(Request::get("/api/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let healthy_payload: serde_json::Value =
        serde_json::from_slice(&to_bytes(healthy.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(healthy_payload["database"], true);

    std::fs::rename(&path, displaced).expect("displace runtime database");
    std::fs::rename(replacement, path).expect("replace runtime database");
    let replaced = app
        .oneshot(Request::get("/api/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(replaced.status(), StatusCode::SERVICE_UNAVAILABLE);
    let replaced_payload: serde_json::Value =
        serde_json::from_slice(&to_bytes(replaced.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(replaced_payload["database"], false);
    state.db().close().await;
}

#[tokio::test]
async fn login_response_with_session_cookies_is_not_cacheable() {
    let response = http::router(test_state().await)
        .oneshot(
            Request::post("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"username":"admin","password":"test-password"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
}

#[tokio::test]
async fn cookie_authenticated_mutation_requires_csrf_header() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    let response = http::router(state)
        .oneshot(
            Request::post("/api/auth/logout")
                .header("cookie", "session=test-session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let payload: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(payload["code"], "forbidden");
}

#[tokio::test]
async fn cookie_authenticated_mutation_allows_csrf_header() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    let response = http::router(state)
        .oneshot(
            Request::post("/api/auth/logout")
                .header("cookie", "session=test-session")
                .header("x-csrf-token", TEST_CSRF_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn change_password_has_a_small_route_specific_body_limit() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    let oversized = serde_json::json!({
        "current_password": "x".repeat(5 * 1024),
        "new_password": "replacement-password",
    });
    let response = http::router(state)
        .oneshot(
            Request::post("/api/auth/change-password")
                .header("cookie", "session=test-session")
                .header("x-csrf-token", TEST_CSRF_TOKEN)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&oversized).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn expired_session_is_rejected_and_pruned() {
    let state = test_state().await;
    insert_expired_session(&state, "expired-session").await;
    let response = http::router(state.clone())
        .oneshot(
            Request::get("/api/auth/me")
                .header("cookie", "session=expired-session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(state.auth.sessions.read().await.is_empty());
}

#[tokio::test]
async fn production_login_requires_https_reverse_proxy_header() {
    let settings = Settings {
        production: true,
        ..Settings::default()
    };
    let response = http::router(test_state_with_settings(settings).await)
        .oneshot(
            Request::post("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"username":"admin","password":"irrelevant"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // 421 而非 403：这是"请求没走对链路"，不是"凭据不对"。二者混用会让 Agent
    // 把反代配置失误误判为凭据吊销并触发重新注册。
    assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
    let payload: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(payload["code"], "misdirected_request");
}

/// 生产环境下 XFF 缺失必须**硬失败**。
///
/// 做成软降级的话，拿不到客户端 IP 时 `login_quota_exhausted()` 会直接放行，
/// 按 IP 与按 (IP, 用户名) 的两层配额同时失效，只剩全局兜底，而这一切不产生
/// 任何信号——一份只配了 XFP 的反代能完全正常地跑下去。
#[tokio::test]
async fn production_login_requires_forwarded_for_not_just_forwarded_proto() {
    let settings = Settings {
        production: true,
        server: unionc::config::ServerSettings {
            proxy_secret: TEST_PROXY_SECRET.to_string(),
            ..unionc::config::ServerSettings::default()
        },
        ..Settings::default()
    };
    let response = http::router(test_state_with_settings(settings).await)
        .oneshot(
            Request::post("/api/auth/login")
                .header("content-type", "application/json")
                // XFP 齐了，但 XFF 缺失——软降级的实现会放行到密码校验。
                .header("x-forwarded-proto", "https")
                .body(Body::from(
                    r#"{"username":"admin","password":"irrelevant"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
    let payload: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(payload["code"], "misdirected_request");
    assert!(
        payload["message"]
            .as_str()
            .is_some_and(|message| message.contains("X-Forwarded-For")),
        "错误信息必须点名缺失的请求头，否则运维无从判断该改哪里：{payload}"
    );
}

#[tokio::test]
async fn production_rejects_an_unparseable_rightmost_forwarded_for_entry() {
    let settings = Settings {
        production: true,
        server: unionc::config::ServerSettings {
            proxy_secret: TEST_PROXY_SECRET.to_string(),
            ..unionc::config::ServerSettings::default()
        },
        ..Settings::default()
    };
    let app = http::router(test_state_with_settings(settings).await);

    for forwarded_for in [
        "198.51.100.1, not-an-ip",
        "198.51.100.1,",
        "198.51.100.1, 203.0.113.9:443",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/auth/login")
                    .header("content-type", "application/json")
                    .header("x-forwarded-proto", "https")
                    .header("x-forwarded-for", forwarded_for)
                    .header("x-unionc-proxy-secret", TEST_PROXY_SECRET)
                    .body(Body::from(
                        r#"{"username":"admin","password":"irrelevant"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::MISDIRECTED_REQUEST,
            "invalid trusted XFF suffix reached authentication: {forwarded_for:?}"
        );
        let payload: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(payload["code"], "misdirected_request");
    }
}

#[tokio::test]
async fn production_rejects_spoofable_forwarding_headers_without_proxy_proof() {
    let settings = Settings {
        production: true,
        server: unionc::config::ServerSettings {
            proxy_secret: TEST_PROXY_SECRET.to_string(),
            ..unionc::config::ServerSettings::default()
        },
        ..Settings::default()
    };
    let response = http::router(test_state_with_settings(settings).await)
        .oneshot(
            Request::post("/api/auth/login")
                .header("content-type", "application/json")
                .header("x-forwarded-proto", "https")
                .header("x-forwarded-for", "203.0.113.9")
                .body(Body::from(
                    r#"{"username":"admin","password":"irrelevant"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
    let payload: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert!(
        payload["message"]
            .as_str()
            .is_some_and(|message| message.contains("X-UnionC-Proxy-Secret"))
    );
}

#[tokio::test]
async fn production_accepts_forwarding_headers_from_the_configured_proxy() {
    let proxy_secret = TEST_PROXY_SECRET;
    let settings = Settings {
        production: true,
        server: unionc::config::ServerSettings {
            proxy_secret: proxy_secret.to_string(),
            ..unionc::config::ServerSettings::default()
        },
        ..Settings::default()
    };
    let response = http::router(test_state_with_settings(settings).await)
        .oneshot(
            Request::post("/api/auth/login")
                .header("content-type", "application/json")
                .header("x-forwarded-proto", "https")
                .header("x-forwarded-for", "203.0.113.9")
                .header("x-unionc-proxy-secret", proxy_secret)
                .body(Body::from(
                    r#"{"username":"admin","password":"irrelevant"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn compiled_console_module_route_never_falls_back_to_legacy_in_process_storage() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    let response = http::router(state)
        .oneshot(
            Request::get("/api/services/sunshine/hosts")
                .header("cookie", "session=test-session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let payload: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(payload["code"], "module_gateway_unavailable");
}

#[tokio::test]
async fn audit_export_is_authenticated_and_uses_a_stable_cursor() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    for index in 1..=3 {
        database::insert_audit(
            state.db().as_ref(),
            &format!("test.audit.{index}"),
            "test-target",
            None,
        )
        .await
        .expect("insert audit fixture");
    }
    let app = http::router(state);

    let unauthenticated = app
        .clone()
        .oneshot(
            Request::get("/api/audit-logs?limit=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let first = app
        .clone()
        .oneshot(
            Request::get("/api/audit-logs?limit=2")
                .header("cookie", "session=test-session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers().get("cache-control").unwrap(), "no-store");
    let first: serde_json::Value =
        serde_json::from_slice(&to_bytes(first.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(first["entries"][0]["action"], "test.audit.3");
    assert_eq!(first["entries"][1]["action"], "test.audit.2");
    let cursor = first["next_before_id"].as_i64().expect("next cursor");

    let second = app
        .oneshot(
            Request::get(format!("/api/audit-logs?limit=2&before_id={cursor}"))
                .header("cookie", "session=test-session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second: serde_json::Value =
        serde_json::from_slice(&to_bytes(second.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(second["entries"].as_array().unwrap().len(), 1);
    assert_eq!(second["entries"][0]["action"], "test.audit.1");
    assert!(second["next_before_id"].is_null());
}

#[tokio::test]
async fn audit_export_reports_database_outages_as_service_unavailable() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    *state.database_health.lock().await = Some(unionc::state::DatabaseHealthSnapshot {
        checked_at: std::time::Instant::now(),
        available: false,
    });

    let response = http::router(state)
        .oneshot(
            Request::get("/api/audit-logs")
                .header("cookie", "session=test-session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn module_gateway_uses_a_server_generated_request_id_without_core_database_writes() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    *state.database_health.lock().await = Some(unionc::state::DatabaseHealthSnapshot {
        checked_at: std::time::Instant::now(),
        available: false,
    });
    let response = http::router(state.clone())
        .oneshot(
            Request::post("/api/monitoring/agent-instances")
                .header("cookie", "session=test-session")
                .header("x-csrf-token", TEST_CSRF_TOKEN)
                .header("x-request-id", "client-controlled-audit-correlation")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    // The request reaches the module gateway even while the control-plane SQLite database is
    // unavailable. No worker is running in this unit test, so the gateway fails closed here.
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let response_request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .expect("response request id")
        .to_string();
    uuid::Uuid::parse_str(&response_request_id).expect("server request id must be a UUID");
    assert_ne!(response_request_id, "client-controlled-audit-correlation");

    let page = database::list_audit_logs(state.db().as_ref(), None, 10)
        .await
        .expect("read audit rows");
    assert!(
        page.entries.is_empty(),
        "module business requests must not write the Union control-plane database"
    );
}

#[tokio::test]
async fn removed_database_settings_route_returns_not_found() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    let response = http::router(state)
        .oneshot(
            Request::get("/api/settings/database")
                .header("cookie", "session=test-session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn host_session_cookie_is_preferred_in_full_router() {
    let state = test_state().await;
    insert_session(&state, "secure-session").await;
    let response = http::router(state)
        .oneshot(
            Request::get("/api/auth/me")
                .header(
                    "cookie",
                    "session=stale-session; __Host-session=secure-session",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let payload: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(payload["username"], "admin");
}

#[tokio::test]
async fn sse_ticket_is_available_once_embedded_database_is_initialized() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    let response = http::router(state)
        .oneshot(
            Request::post("/api/events/ticket")
                .header("cookie", "session=test-session")
                .header("x-csrf-token", TEST_CSRF_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let payload: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert!(
        payload["ticket"]
            .as_str()
            .is_some_and(|ticket| !ticket.is_empty())
    );
}

#[tokio::test]
async fn unconsumed_sse_tickets_have_per_session_and_global_bounds() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    let app = http::router(state.clone());
    let request_ticket = || {
        Request::post("/api/events/ticket")
            .header("cookie", "session=test-session")
            .header("x-csrf-token", TEST_CSRF_TOKEN)
            .body(Body::empty())
            .unwrap()
    };

    for _ in 0..MAX_PENDING_SSE_TICKETS_PER_SESSION {
        let response = app.clone().oneshot(request_ticket()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let response = app.clone().oneshot(request_ticket()).await.unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    {
        let mut tickets = state.auth.sse_tickets.lock().await;
        tickets.clear();
        for index in 0..MAX_PENDING_SSE_TICKETS {
            tickets.insert(
                format!("preloaded-{index}"),
                SseTicket {
                    session_token: format!("other-session-{index}"),
                    issued_at: std::time::Instant::now(),
                },
            );
        }
    }
    let response = app.clone().oneshot(request_ticket()).await.unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    // Expired tickets are removed before applying either bound, so capacity
    // recovers without a restart.
    state
        .auth
        .sse_tickets
        .lock()
        .await
        .get_mut("preloaded-0")
        .unwrap()
        .issued_at = std::time::Instant::now() - SSE_TICKET_TTL;
    let response = app.oneshot(request_ticket()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        state.auth.sse_tickets.lock().await.len(),
        MAX_PENDING_SSE_TICKETS
    );
}

#[tokio::test]
async fn logout_closes_an_already_established_sse_stream() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    let app = http::router(state);

    let ticket_response = app
        .clone()
        .oneshot(
            Request::post("/api/events/ticket")
                .header("cookie", "session=test-session")
                .header("x-csrf-token", TEST_CSRF_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(
        &to_bytes(ticket_response.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    let ticket = payload["ticket"].as_str().unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/api/events?ticket={ticket}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    let initial = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
        .await
        .expect("initial SSE event timed out");
    assert!(initial.is_some(), "SSE should emit its initial snapshot");

    let logout = app
        .oneshot(
            Request::post("/api/auth/logout")
                .header("cookie", "session=test-session")
                .header("x-csrf-token", TEST_CSRF_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);

    let end = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
        .await
        .expect("revoked SSE stream did not close promptly");
    assert!(end.is_none(), "revoked SSE stream emitted another frame");
}

#[tokio::test]
async fn application_shutdown_closes_an_already_established_sse_stream() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    let response = http::router(state.clone())
        .oneshot(
            Request::get("/api/events")
                .header("cookie", "session=test-session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    let initial = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
        .await
        .expect("initial SSE event timed out");
    assert!(initial.is_some(), "SSE should emit its initial snapshot");

    state.request_shutdown();

    let end = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
        .await
        .expect("SSE stream did not close promptly during application shutdown");
    assert!(
        end.is_none(),
        "shutting-down SSE stream emitted another frame"
    );
}

#[tokio::test]
async fn graceful_server_shutdown_is_not_blocked_by_an_active_sse_response() {
    let state = test_state().await;
    insert_session(&state, "test-session").await;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind loopback test listener");
    let address = listener.local_addr().expect("read test listener address");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let shutdown_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, http::router(state))
            .with_graceful_shutdown(async move {
                shutdown_rx.await.expect("receive test shutdown signal");
                shutdown_state.request_shutdown();
            })
            .await
            .expect("serve test application");
    });

    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("build loopback test client");
    let mut response = client
        .get(format!("http://{address}/api/events"))
        .header(reqwest::header::COOKIE, "session=test-session")
        .send()
        .await
        .expect("establish real SSE response");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let initial = tokio::time::timeout(std::time::Duration::from_secs(1), response.chunk())
        .await
        .expect("real SSE initial event timed out")
        .expect("read real SSE initial event");
    assert!(initial.is_some(), "real SSE response ended before shutdown");

    shutdown_tx.send(()).expect("signal test server shutdown");
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .expect("active SSE response blocked graceful server shutdown")
        .expect("join test server");
    let end = tokio::time::timeout(std::time::Duration::from_secs(1), response.chunk())
        .await
        .expect("real SSE body did not reach EOF after server shutdown")
        .expect("read real SSE EOF");
    assert!(end.is_none());
}

#[tokio::test]
async fn monitoring_routes_remain_console_authenticated() {
    let response = http::router(test_state().await)
        .oneshot(
            Request::get("/api/monitoring/hosts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
