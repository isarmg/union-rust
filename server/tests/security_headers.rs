//! 全局安全响应头。
//!
//! Union is both a JSON console and a fixed gateway for private workers. Global browser security
//! headers must therefore wrap local handlers, authentication failures and proxied responses.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;
use unionc::{
    config::{LocalConfig, Settings},
    http,
    infra::database,
    state::AppState,
};

fn app(production: bool) -> axum::Router {
    let mut settings = Settings {
        production,
        ..Settings::default()
    };
    settings.database.url = ":memory:".to_string();
    http::router(
        AppState::new(
            settings,
            database::in_memory_pool().expect("in-memory test pool"),
            "$2b$12$C6UzMDM.H6dfI/f/IKcEe.4n3W4O4L2hS2T/1B1Q6VYF2M9mV0X5K".into(),
            LocalConfig {
                application_version: env!("CARGO_PKG_VERSION").to_string(),
                admin_username: "admin".into(),
                admin_password_hash: "unused".into(),
            },
        )
        .expect("capture in-memory database identity"),
    )
}

async fn headers_of(app: axum::Router, uri: &str) -> axum::http::HeaderMap {
    app.oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .expect("response")
        .headers()
        .clone()
}

#[tokio::test]
async fn every_response_carries_the_baseline_security_headers() {
    let headers = headers_of(app(false), "/api/health").await;
    for (name, expected) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        ("cross-origin-resource-policy", "same-origin"),
    ] {
        assert_eq!(
            headers.get(name).and_then(|v| v.to_str().ok()),
            Some(expected),
            "缺少或错误的 {name}"
        );
    }
    let csp = headers
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        csp.contains("default-src 'none'") && csp.contains("frame-ancestors 'none'"),
        "CSP 应禁止一切资源加载与被嵌套，实际为：{csp}"
    );
}

/// 鉴权失败的响应同样要带上安全头——它们也是浏览器会渲染的响应。
#[tokio::test]
async fn unauthenticated_responses_are_also_protected() {
    let response = app(false)
        .oneshot(Request::get("/api/auth/me").body(Body::empty()).unwrap())
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff"),
        "401 响应也必须带 nosniff"
    );
}

/// HSTS 只在生产下发：开发环境走明文 HTTP，发了会把浏览器锁死在 https 上。
#[tokio::test]
async fn hsts_is_production_only() {
    assert!(
        headers_of(app(false), "/api/health")
            .await
            .get("strict-transport-security")
            .is_none(),
        "开发环境不应下发 HSTS"
    );

    let production = headers_of(app(true), "/api/health").await;
    let hsts = production
        .get("strict-transport-security")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        hsts.contains("max-age=31536000"),
        "生产环境应下发一年期 HSTS，实际为：{hsts}"
    );
}
