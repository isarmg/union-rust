//! API 访问控制中间件。
//!
//! 这里集中处理会话认证、SSE 短效票据、数据库可用性检查和 Cookie CSRF 防护。
//! 路由模块只负责请求解析和业务调用，不重复实现安全策略。

use std::time::{Duration, Instant};

use axum::{
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::Response,
};

use crate::{
    error::AppError,
    infra::database,
    state::{AppState, LocalSession},
};

use crate::auth::http as auth;

const DATABASE_HEALTH_TTL: Duration = Duration::from_secs(1);

pub(super) async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let path = request.uri().path().to_string();

    if is_public_path(&path) {
        return Ok(next.run(request).await);
    }

    // EventSource 不支持自定义请求头，因此 SSE 使用一次性短效票据。
    if path == "/api/events"
        && let Some(ticket) = sse_ticket(request.uri().query()).map(str::to_owned)
    {
        return authenticate_sse(&state, &ticket, request, next).await;
    }

    let token = auth::session_cookie(request.headers()).ok_or(AppError::Unauthorized)?;
    let session = auth::local_session(&state, &token).await?;
    let username = session.username.clone();

    if path == "/api/events" {
        let cancellation = auth::sse_session_cancellation(&state, &token).await?;
        request.extensions_mut().insert(cancellation);
    }

    ensure_database_available(&state, &path).await?;
    ensure_csrf_protected(&request, &session)?;

    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    Ok(database::with_audit_context(
        database::AuditContext {
            actor: username,
            request_id,
        },
        next.run(request),
    )
    .await)
}

fn is_public_path(path: &str) -> bool {
    matches!(path, "/api/health" | "/api/ready" | "/api/auth/login")
}

fn sse_ticket(query: Option<&str>) -> Option<&str> {
    query?
        .split('&')
        .find_map(|parameter| parameter.strip_prefix("ticket="))
}

/// 用一张短效票据换取 SSE 连接。
///
/// 票据是一次性的（`remove` 而非 `get`），且必须同时满足两个条件：
///
/// 1. 签发后未超过 60 秒；
/// 2. **签发它的会话此刻仍然有效**。
///
/// 第二条不可省。只校验时效的话，票据就成了一张与账号状态脱钩的通行证：管理员
/// 注销或改密（后者会踢掉该账号的全部会话）之后，此前签发的票据在剩余窗口内
/// 依然能建立连接——"我已经登出了"与"连接确实断了"之间因此存在一个静默的窗口。
async fn authenticate_sse(
    state: &AppState,
    ticket: &str,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let issued = state.auth.sse_tickets.lock().await.remove(ticket);
    let Some(issued) = issued.filter(|entry| entry.issued_at.elapsed() < Duration::from_secs(60))
    else {
        return Err(AppError::Unauthorized);
    };
    // 会话已注销或已因改密失效时，这里返回 Unauthorized；成功时把该会话的
    // 撤销通知注入 handler，使已经建立的长连接也会随注销/改密立即结束。
    let cancellation = auth::sse_session_cancellation(state, &issued.session_token).await?;
    request.extensions_mut().insert(cancellation);

    ensure_database_available(state, "/api/events").await?;
    Ok(next.run(request).await)
}

async fn ensure_database_available(state: &AppState, path: &str) -> Result<(), AppError> {
    if requires_database(path) && !database_available(state).await {
        return Err(AppError::DatabaseUnavailable(
            "本地数据库暂不可用，请检查数据目录、磁盘空间和文件权限".to_string(),
        ));
    }
    Ok(())
}

async fn database_available(state: &AppState) -> bool {
    let now = Instant::now();
    {
        let cached = state.database_health.lock().await;
        if let Some(snapshot) = cached.as_ref()
            && now.duration_since(snapshot.checked_at) < DATABASE_HEALTH_TTL
        {
            return snapshot.available;
        }
    }

    let available = database::ping(state.db().as_ref()).await.is_ok();
    *state.database_health.lock().await = Some(crate::state::DatabaseHealthSnapshot {
        checked_at: Instant::now(),
        available,
    });
    available
}

/// 双提交（double-submit）CSRF 校验。
///
/// 前端从非 HttpOnly 的 CSRF cookie 中读取令牌，回填到 `x-csrf-token` 请求头；
/// 服务端把请求头与**该会话**存储的令牌做恒定时间比较。
///
/// 令牌是每会话随机的，而不是固定值（如 `x-csrf-token: 1`）。固定值依赖"浏览器禁止
/// 跨源发送自定义头"这一外部前提；随机令牌即便将来误配 CORS 允许了任意请求头，
/// 攻击者仍需要猜出令牌值，而跨站脚本读不到本站的 cookie。
fn ensure_csrf_protected(request: &Request, session: &LocalSession) -> Result<(), AppError> {
    if !is_state_changing(request.method()) {
        return Ok(());
    }
    let presented = request
        .headers()
        .get(auth::CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !session.csrf_token_matches(presented) {
        return Err(AppError::Forbidden(
            "missing or invalid CSRF protection header".to_string(),
        ));
    }
    Ok(())
}

fn is_state_changing(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn requires_database(path: &str) -> bool {
    path == "/api/audit-logs"
        || path == "/api/events"
        || path == "/api/events/ticket"
        || path.starts_with("/api/services")
        || path.starts_with("/api/monitoring")
}
