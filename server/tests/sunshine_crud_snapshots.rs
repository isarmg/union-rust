//! Sunshine 配置 CRUD 与慢上游解耦的 HTTP 回归测试。
//!
//! 配置写入完成后响应必须明确返回 `pending/null`，而不是在请求内等待 Sunshine；列表
//! 随后读取同一份内存快照。更新会清掉旧绿灯，删除会同步移除快照。

use std::time::Duration;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use chrono::{Duration as ChronoDuration, Utc};
use tower::ServiceExt;
use unionc::{
    config::{LocalConfig, Settings},
    http,
    infra::database,
    state::{AppState, LocalSession, SunshineHostHealth},
    system::ResourceMonitor,
};

mod common;

const SESSION: &str = "sunshine-crud-session";
const CSRF: &str = "sunshine-crud-csrf";

fn dedicated_database_url(base: &str) -> String {
    let mut url = reqwest::Url::parse(base).expect("test database url");
    let name = url.path().trim_start_matches('/').to_string();
    url.set_path(&format!("{name}_sunshine_crud_snapshots"));
    url.to_string()
}

async fn response_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("JSON response")
}

fn console_request(method: &str, path: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("cookie", format!("session={SESSION}"))
        .header("x-csrf-token", CSRF)
        .header("content-type", "application/json")
        .body(body)
        .expect("request")
}

#[tokio::test]
async fn crud_returns_pending_snapshots_without_waiting_for_health_probe() {
    common::init_test_keyring();
    let base_url = common::test_database_url(
        "crud_returns_pending_snapshots_without_waiting_for_health_probe",
    );

    let mut settings = Settings::default();
    settings.database.url = dedicated_database_url(&base_url);
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");
    let state = AppState::new(
        settings,
        pool,
        "unused".into(),
        LocalConfig {
            application_version: env!("CARGO_PKG_VERSION").to_string(),
            admin_username: "admin".into(),
            admin_password_hash: "unused".into(),
        },
        ResourceMonitor::frozen(Default::default()),
    )
    .expect("capture test database identity");
    state.auth.sessions.write().await.insert(
        SESSION.into(),
        LocalSession {
            username: "admin".into(),
            expires_at: Utc::now() + ChronoDuration::minutes(5),
            csrf_token: CSRF.into(),
        },
    );
    // `http::router` 本身不会启动后台探测；这是刻意的，借此验证 CRUD/list 的契约
    // 完全不依赖一轮探测先跑完。
    let app = http::router(state.clone());
    let create_body = serde_json::json!({
        "name": "slow-upstream",
        "host": "192.0.2.1",
        "web_port": 47990,
        "username": "admin",
        "password": "test-password",
        "verify_tls": false
    });

    let create = tokio::time::timeout(
        Duration::from_secs(2),
        app.clone().oneshot(console_request(
            "POST",
            "/api/services/sunshine/hosts",
            Body::from(create_body.to_string()),
        )),
    )
    .await
    .expect("create must not wait for an upstream probe")
    .expect("create response");
    assert_eq!(create.status(), StatusCode::CREATED);
    let created = response_json(create).await;
    assert_eq!(created["probe_status"], "pending");
    assert!(created["reachable"].is_null());
    assert!(created["connected"].is_null());
    let host_id = created["id"].as_str().expect("host id").to_string();

    let list = app
        .clone()
        .oneshot(console_request(
            "GET",
            "/api/services/sunshine/hosts",
            Body::empty(),
        ))
        .await
        .expect("list response");
    assert_eq!(list.status(), StatusCode::OK);
    let listed = response_json(list).await;
    assert_eq!(listed[0]["id"], host_id);
    assert_eq!(listed[0]["probe_status"], "pending");
    assert!(listed[0]["reachable"].is_null());

    // 模拟当前配置曾经是绿色；任何配置修改都必须立即覆盖为 pending，而不是沿用。
    state.hosts.sunshine_health.write().await.insert(
        host_id.clone(),
        SunshineHostHealth::completed(true, &Ok(())),
    );
    let update_body = serde_json::json!({
        "name": "updated-upstream",
        "host": "192.0.2.2"
    });
    let update = tokio::time::timeout(
        Duration::from_secs(2),
        app.clone().oneshot(console_request(
            "PATCH",
            &format!("/api/services/sunshine/hosts/{host_id}"),
            Body::from(update_body.to_string()),
        )),
    )
    .await
    .expect("update must not wait for an upstream probe")
    .expect("update response");
    assert_eq!(update.status(), StatusCode::OK);
    let updated = response_json(update).await;
    assert_eq!(updated["probe_status"], "pending");
    assert!(updated["reachable"].is_null());
    assert!(updated["connected"].is_null());

    let delete = tokio::time::timeout(
        Duration::from_secs(2),
        app.oneshot(console_request(
            "DELETE",
            &format!("/api/services/sunshine/hosts/{host_id}"),
            Body::empty(),
        )),
    )
    .await
    .expect("delete must not wait for an upstream probe")
    .expect("delete response");
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);
    assert!(
        !state
            .hosts
            .sunshine_health
            .read()
            .await
            .contains_key(&host_id),
        "删除主机必须同步清掉健康快照"
    );
}
