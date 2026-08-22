//! HTTP 路由装配与全局中间件。
//!
//! 本模块**不含任何业务逻辑**——每个功能模块在自己的 `http.rs` 里声明 URL 与 handler
//! 的映射，这里只负责把它们组合起来，并决定哪些中间件套在哪一层。
//!
//! 中间件的分层是有讲究的：
//!
//! ```text
//! ┌─ 服务端请求 ID ───────────────────────────────┐  覆盖客户端值并回写响应
//! │  ┌─ 请求追踪 ───────────────────────────────┐ │
//! │  │  ┌─ 安全响应头 ────────────────────────┐ │ │  覆盖全部业务响应
//! │  │  │  ┌─ 控制台路由（鉴权 + CSRF）────┐ │ │ │
//! │  │  │  └───────────────────────────────┘ │ │ │
//! │  │  │  ┌─ Agent 路由（协议自身鉴权）──┐ │ │ │  不走会话鉴权
//! │  │  │  └───────────────────────────────┘ │ │ │
//! │  │  └─────────────────────────────────────┘ │ │
//! │  └──────────────────────────────────────────┘ │
//! └───────────────────────────────────────────────┘
//! ```

mod access_control;
mod security_headers;

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request},
    http::{HeaderValue, header::HeaderName},
    middleware::{self, Next},
    response::Response,
};
use tower_http::trace::TraceLayer;

use crate::state::AppState;

/// 需要管理员会话的路由。
fn console_routes() -> Router<AppState> {
    Router::new()
        .merge(crate::auth::http::router())
        .merge(crate::system::http::router())
        .merge(crate::monitoring::http::console_router())
        .merge(crate::sunshine::http::router())
}

/// 构造整个 HTTP API 路由树。
pub fn router(state: AppState) -> Router {
    let console = console_routes().layer(middleware::from_fn_with_state(
        state.clone(),
        access_control::require_auth,
    ));
    Router::new()
        .merge(console)
        // Agent 路由分别使用 Bearer、Pairing secret 或短时 capability，不套会话中间件。
        .merge(crate::monitoring::http::agent_router())
        // Defensive fallback for any route that does not declare a smaller
        // contract. The largest supported console payload is Sunshine config
        // at 1 MiB; auth and Agent routers override this with tighter limits.
        .layer(DefaultBodyLimit::max(1024 * 1024))
        // 安全头包住完整业务路由，因此鉴权失败和 handler 错误响应同样会携带这些头。
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_headers::apply,
        ))
        .layer(TraceLayer::new_for_http())
        // Always replace a caller-supplied correlation id. The trusted value is
        // persisted in audit rows, so accepting an arbitrary header would let a
        // client forge linkage and amplify the database with oversized ids.
        .layer(middleware::from_fn(assign_request_id))
        .with_state(state)
}

async fn assign_request_id(mut request: Request, next: Next) -> Response {
    static REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");
    let value = HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
        .expect("UUID text is always a valid HTTP header value");
    request
        .headers_mut()
        .insert(REQUEST_ID.clone(), value.clone());
    let mut response = next.run(request).await;
    response.headers_mut().insert(REQUEST_ID.clone(), value);
    response
}
