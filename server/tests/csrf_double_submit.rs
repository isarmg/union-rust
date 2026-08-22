//! 双提交 CSRF 令牌。
//!
//! 固定值（如 `x-csrf-token: 1`）的安全性完全建立在"浏览器禁止跨源发送
//! 自定义头"这一外部前提上——一旦将来引入 CORS 中间件且配置为 `Allow-Headers: *`，
//! 防线会瞬间失效，且不会有任何测试失败。
//!
//! 现在改为每会话随机令牌：即便攻击者能跨源发送任意请求头，也猜不出令牌值。

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use tower::ServiceExt;
use unionc::{
    config::{LocalConfig, Settings},
    http,
    infra::database,
    state::{AppState, LocalSession},
};

const SESSION: &str = "session-token-under-test";
const CSRF: &str = "9f2c1ae4b7d84c02a1e6f35d8b7c0192";
/// `ADMIN_PASSWORD` 的 bcrypt 哈希（cost 4，仅测试用）。
const ADMIN_HASH: &str = "$2b$04$IHGexj5MjQyIveMqHnWkyej6tgcrDQL/ku/UBIHvU.cPVbjA86UvO";
const ADMIN_PASSWORD: &str = "correct-horse-battery-staple";

fn state_with_session() -> AppState {
    let state = AppState::new(
        Settings::default(),
        database::in_memory_pool().expect("in-memory test pool"),
        "unused".into(),
        LocalConfig {
            application_version: env!("CARGO_PKG_VERSION").to_string(),
            admin_username: "admin".into(),
            admin_password_hash: "unused".into(),
        },
        unionc::system::ResourceMonitor::frozen(Default::default()),
    );
    let sessions = state.auth.sessions.clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            sessions.write().await.insert(
                SESSION.to_string(),
                LocalSession {
                    username: "admin".into(),
                    expires_at: Utc::now() + Duration::minutes(5),
                    csrf_token: CSRF.to_string(),
                },
            );
        })
    });
    state
}

/// 发一个状态变更请求，可选携带 CSRF 头。
async fn mutate_with(csrf_header: Option<&str>) -> StatusCode {
    // 用 logout（POST，受 require_auth 保护且不访问数据库）作为状态变更样本，
    // 避免测试被无关的数据库连接超时拖慢。
    let app = http::router(state_with_session());
    let mut request =
        Request::post("/api/auth/logout").header("cookie", format!("session={SESSION}"));
    if let Some(value) = csrf_header {
        request = request.header("x-csrf-token", value);
    }
    app.oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test(flavor = "multi_thread")]
async fn mutation_without_csrf_header_is_forbidden() {
    assert_eq!(mutate_with(None).await, StatusCode::FORBIDDEN);
}

/// 关键回归：旧的固定值必须**不再**被接受。
///
/// 若这条断言失败，说明代码退回了"任何人都能构造的常量令牌"，双提交形同虚设。
#[tokio::test(flavor = "multi_thread")]
async fn the_old_constant_token_is_no_longer_accepted() {
    assert_eq!(
        mutate_with(Some("1")).await,
        StatusCode::FORBIDDEN,
        "固定值 \"1\" 仍被接受——CSRF 防护退回到了可被任意构造的常量"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guessed_token_of_the_same_length_is_rejected() {
    let wrong = "0".repeat(CSRF.len());
    assert_eq!(mutate_with(Some(&wrong)).await, StatusCode::FORBIDDEN);
}

/// 令牌前缀正确但长度不同，同样必须拒绝（防止截断攻击）。
#[tokio::test(flavor = "multi_thread")]
async fn a_truncated_token_is_rejected() {
    assert_eq!(
        mutate_with(Some(&CSRF[..CSRF.len() - 1])).await,
        StatusCode::FORBIDDEN
    );
}

/// 携带本会话真实令牌时应当放行。
#[tokio::test(flavor = "multi_thread")]
async fn the_session_token_passes_the_csrf_check() {
    assert_eq!(
        mutate_with(Some(CSRF)).await,
        StatusCode::NO_CONTENT,
        "持有本会话真实 CSRF 令牌的请求应当被放行"
    );
}

/// 只读请求不需要 CSRF 令牌——它们不改变状态。
#[tokio::test(flavor = "multi_thread")]
async fn read_only_requests_do_not_require_a_token() {
    let app = http::router(state_with_session());
    let status = app
        .oneshot(
            Request::get("/api/auth/me")
                .header("cookie", format!("session={SESSION}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);
}

/// 登录响应必须同时下发会话 cookie 与**可被 JS 读取**的 CSRF cookie。
///
/// 会话 cookie 保持 HttpOnly；CSRF cookie 若也设成 HttpOnly，前端就取不到令牌，
/// 所有写操作都会 403——这是改造中最容易搞错的一处，因此对真实登录响应做断言。
#[tokio::test(flavor = "multi_thread")]
async fn login_issues_a_readable_csrf_cookie_and_an_http_only_session_cookie() {
    let state = AppState::new(
        Settings::default(),
        database::in_memory_pool().expect("in-memory test pool"),
        ADMIN_HASH.into(),
        LocalConfig {
            application_version: env!("CARGO_PKG_VERSION").to_string(),
            admin_username: "admin".into(),
            admin_password_hash: ADMIN_HASH.into(),
        },
        unionc::system::ResourceMonitor::frozen(Default::default()),
    );
    let response = http::router(state)
        .oneshot(
            Request::post("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "username": "admin", "password": ADMIN_PASSWORD })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "测试口令应能登录成功");

    let cookies: Vec<String> = response
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().to_string())
        .collect();

    let session = cookies
        .iter()
        .find(|cookie| cookie.starts_with("session="))
        .expect("登录响应必须下发会话 cookie");
    let csrf = cookies
        .iter()
        .find(|cookie| cookie.starts_with("csrf="))
        .expect("登录响应必须下发 CSRF cookie");

    assert!(
        session.contains("HttpOnly"),
        "会话 cookie 必须是 HttpOnly：{session}"
    );
    assert!(
        !csrf.contains("HttpOnly"),
        "CSRF cookie 不能是 HttpOnly，否则前端读不到令牌，所有写操作都会 403：{csrf}"
    );
    assert!(
        csrf.contains("SameSite=Strict"),
        "CSRF cookie 应保留 SameSite=Strict：{csrf}"
    );

    // 令牌必须是随机值，不能是常量。
    let token = csrf.trim_start_matches("csrf=").split(';').next().unwrap();
    assert!(token.len() >= 16, "CSRF 令牌过短，熵不足：{token}");
    assert_ne!(token, "1", "CSRF 令牌退回成了固定值");
}
