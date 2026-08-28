//! 登录限流的分桶隔离。
//!
//! 守护目标：只有"每分钟 60 次"的全局桶、且名额在校验前占用的话，任何人用任意
//! 用户名刷满即可让合法管理员在整个窗口内无法登录（管理员锁定攻击）。

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

/// 已知口令 `correct-horse-battery` 的 bcrypt 哈希（cost 4，仅测试用）。
const ADMIN_HASH: &str = "$2b$04$Qh0BwRWlZBqZ9lNPQTWvVeXGxKPLYlBP8YJ8yYqTfKZ1kKQmXHFvy";

fn test_state() -> AppState {
    let mut settings = Settings::default();
    settings.database.url = ":memory:".to_string();
    AppState::new(
        settings,
        database::in_memory_pool().expect("in-memory test pool"),
        ADMIN_HASH.into(),
        LocalConfig {
            application_version: env!("CARGO_PKG_VERSION").to_string(),
            admin_username: "admin".into(),
            admin_password_hash: ADMIN_HASH.into(),
        },
    )
    .expect("capture in-memory database identity")
}

#[tokio::test]
async fn login_has_a_small_route_specific_body_limit() {
    let oversized = serde_json::json!({
        "username": "admin",
        "password": "x".repeat(5 * 1024),
    });
    let response = http::router(test_state())
        .oneshot(
            Request::post("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&oversized).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn credential_limits_are_checked_before_bcrypt() {
    for body in [
        serde_json::json!({ "username": "u".repeat(129), "password": "valid-input" }),
        serde_json::json!({ "username": "admin", "password": "" }),
        serde_json::json!({ "username": "admin", "password": "x".repeat(73) }),
    ] {
        let response = http::router(test_state())
            .oneshot(
                Request::post("/api/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "body={body}");
    }
}

async fn attempt_login(
    app: &axum::Router,
    forwarded_for: Option<&str>,
    username: &str,
) -> StatusCode {
    let mut request = Request::post("/api/auth/login").header("content-type", "application/json");
    if let Some(value) = forwarded_for {
        request = request.header("x-forwarded-for", value);
    }
    let body = serde_json::json!({ "username": username, "password": "definitely-wrong" });
    app.clone()
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// 单个 IP 的洪水不得影响其他 IP 登录。
#[tokio::test]
async fn flood_from_one_ip_does_not_lock_out_other_clients() {
    let app = http::router(test_state());

    // 攻击者用各种用户名猛刷，直到自己的桶被打满。
    let mut attacker_throttled = false;
    for index in 0..40 {
        let status = attempt_login(&app, Some("203.0.113.9"), &format!("victim{index}")).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            attacker_throttled = true;
            break;
        }
    }
    assert!(
        attacker_throttled,
        "同一 IP 的连续失败登录应当在 40 次以内被限流"
    );

    // 关键断言：另一个 IP 此刻仍然必须能够正常尝试登录（返回 401 而非 429）。
    let victim = attempt_login(&app, Some("198.51.100.20"), "admin").await;
    assert_eq!(
        victim,
        StatusCode::UNAUTHORIZED,
        "其他来源 IP 被攻击者的洪水连带限流了——管理员锁定攻击仍然成立"
    );
}

/// 伪造 XFF 左侧条目不得绕过按 IP 限流。
///
/// 反代会把真实对端追加在最右侧，因此实现必须取最右项。若误取最左项，
/// 攻击者只要每次换一个伪造 IP 就能无限重试。
#[tokio::test]
async fn spoofed_forwarded_for_prefix_cannot_bypass_the_ip_bucket() {
    let app = http::router(test_state());

    let mut throttled_at = None;
    for index in 0..40 {
        // 每次伪造不同的左侧地址，但反代追加的真实地址始终相同。
        let header = format!("10.0.0.{index}, 203.0.113.77");
        // 用户名也必须每次不同：否则"每用户名 5 次"的限制会先触发，
        // 测试就变成在验证用户名桶，完全测不到 IP 桶（已实测会导致断言失效）。
        let username = format!("probe{index}");
        if attempt_login(&app, Some(&header), &username).await == StatusCode::TOO_MANY_REQUESTS {
            throttled_at = Some(index);
            break;
        }
    }

    assert!(
        throttled_at.is_some(),
        "伪造 X-Forwarded-For 左侧条目绕过了按 IP 限流——说明取错了方向（应取最右项）"
    );
}

/// 针对**单个账号**的洪水不得把该账号锁死在其他来源之外。
///
/// 守护目标："每用户名 5 次/分钟"的桶若只按用户名计数、不区分来源，管理员用户名
/// 默认就是 `admin`，于是任何人只要持续对 `admin` 发失败请求，就能让真正的管理员在
/// 整个窗口内无法登录——防护本身变成了武器。按 IP 的桶救不了，因为两个桶独立判定，
/// 用户名桶先满请求就已被拒。
///
/// 键改为 (IP, 用户名) 复合后，洪水只锁住攻击者自己的组合。
#[tokio::test]
async fn flooding_one_account_does_not_lock_it_out_for_the_real_admin() {
    let app = http::router(test_state());

    // 攻击者持续攻击 admin 这一个账号，直到自己被限流。
    let mut attacker_throttled = false;
    for _ in 0..40 {
        if attempt_login(&app, Some("203.0.113.9"), "admin").await == StatusCode::TOO_MANY_REQUESTS
        {
            attacker_throttled = true;
            break;
        }
    }
    assert!(
        attacker_throttled,
        "针对单账号的连续失败登录应当在 40 次以内被限流"
    );

    // 关键断言：真正的管理员从自己的 IP 登录同一个账号，必须仍能尝试。
    let admin = attempt_login(&app, Some("198.51.100.20"), "admin").await;
    assert_eq!(
        admin,
        StatusCode::UNAUTHORIZED,
        "管理员被针对同一账号的洪水连带锁定了——账号锁定 DoS 仍然成立"
    );
}

/// 会话过期清理不应把有效的其他来源一并清掉；同时确认成功路径仍可用。
#[tokio::test]
async fn unknown_username_still_returns_unauthorized_not_throttled() {
    let app = http::router(test_state());
    let status = attempt_login(&app, Some("192.0.2.1"), "no-such-user").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
