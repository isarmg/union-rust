//! 健康检查、系统资源和 SSE 事件流 handler。
//!
//! # SSE（Server-Sent Events）原理
//!
//! SSE 是一种从服务器向浏览器单向推送数据的技术：
//! - 客户端（浏览器）通过 `EventSource` API 建立一个长连接
//! - 服务器持续向这个连接写入数据，格式如：`data: {"status":"ok"}\n\n`
//! - 浏览器自动触发 `onmessage` 事件，无需客户端轮询
//!
//! SSE 比 WebSocket 简单，适合管理台单向接收服务状态更新。

use std::{convert::Infallible, time::Instant};

use async_stream::stream;
use axum::{
    Json, Router,
    extract::{Extension, Query, State},
    http::{HeaderValue, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use chrono::Utc;
use tokio::sync::broadcast;

use crate::auth::SseTicketResponse;
use crate::system::{EventPayload, HealthResponse, ReadinessResponse, ServiceStatus};
use crate::{error::AppResult, state::AppState};

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(root))
        .route("/api/health", get(health))
        .route("/api/ready", get(ready))
        .route("/api/services", get(services))
        .route("/api/system/resources", get(resources))
        .route("/api/audit-logs", get(audit_logs))
        .route("/api/events", get(events))
        .route("/api/events/ticket", post(issue_sse_ticket))
}

/// 为 SSE 连接签发一个短期（60 秒）有效的单次访问票据（ticket）。
///
/// # 为什么需要 ticket？
///
/// 浏览器的 `EventSource` API 不支持自定义请求头（如 `Authorization: Bearer <token>`），
/// 所以无法用通常的 Bearer Token 方式验证身份。
///
/// 解决方案：先用正常的认证请求获取一个临时 ticket（UUID），
/// 然后通过 URL 查询参数传给 SSE 端点：`GET /api/events?ticket=<uuid>`
///
/// # 安全设计
///
/// - ticket 有效期只有 60 秒（足够客户端立即使用）
/// - ticket 是随机 UUID，无法猜测
/// - 服务端验证成功后立即删除 ticket，使其只能使用一次
/// - 限制泄露风险：即使 URL 被日志记录，60 秒后 ticket 自动失效
/// - ticket 绑定签发它的会话：会话失效（注销、改密）后票据一并作废，
///   而不是继续有效到 60 秒窗口结束
pub(crate) async fn issue_sse_ticket(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> AppResult<Json<SseTicketResponse>> {
    // 走到这里说明已经过了 `require_auth`，因此 cookie 必定存在且会话有效；
    // 取它只是为了把票据和这个会话绑在一起。
    let session_token =
        crate::auth::http::extract_token(&headers).ok_or(crate::error::AppError::Unauthorized)?;
    let ticket = uuid::Uuid::new_v4().to_string(); // 生成随机 UUID 作为 ticket
    let mut tickets = state.auth.sse_tickets.lock().await;
    // 清理超过 60 秒未使用的过期 ticket。
    // `retain` 保留返回 true 的元素，这里保留"距今不超过 60 秒"的 ticket。
    // 这是一个懒清理策略：在写入时顺便清理，避免 HashMap 无限增长。
    tickets.retain(|_, entry: &mut crate::state::SseTicket| {
        entry.issued_at.elapsed() < crate::state::SSE_TICKET_TTL
    });
    if tickets.len() >= crate::state::MAX_PENDING_SSE_TICKETS
        || tickets
            .values()
            .filter(|entry| entry.session_token == session_token)
            .count()
            >= crate::state::MAX_PENDING_SSE_TICKETS_PER_SESSION
    {
        return Err(crate::error::AppError::TooManyRequests(
            "too many unconsumed event-stream tickets; consume an existing ticket or retry after it expires"
                .to_string(),
        ));
    }
    tickets.insert(
        ticket.clone(),
        crate::state::SseTicket {
            session_token,
            issued_at: Instant::now(),
        },
    );
    Ok(Json(SseTicketResponse { ticket }))
}

/// 根路由，返回 API 简介文字（主要用于快速验证服务是否运行）。
pub(crate) async fn root() -> &'static str {
    "UnionC API. Try GET /api/health."
}

/// 返回服务健康状态，包含版本号和运行时长（秒）。
///
/// 这个接口不需要认证，常用于监控系统的存活探针（liveness probe）。
pub(crate) async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(), // 编译时从 Cargo.toml 读取版本号
        uptime_seconds: i64::try_from(state.started_at.elapsed().as_secs()).unwrap_or(i64::MAX),
    })
}

/// 就绪探针同时验证数据库和运行数据目录。
pub(crate) async fn ready(
    State(state): State<AppState>,
) -> (axum::http::StatusCode, Json<ReadinessResponse>) {
    let database = crate::http::database_available(&state).await;
    let data_directory = crate::infra::paths::data_dir().is_dir();
    // SQLite 是启动必需的本地持久层；无法查询时进程仍存活，但不得接收业务流量。
    let ready = data_directory && database;
    (
        if ready {
            axum::http::StatusCode::OK
        } else {
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        },
        Json(ReadinessResponse {
            status: if ready { "ready" } else { "not-ready" }.to_string(),
            database,
            data_directory,
        }),
    )
}

/// 返回所有受管 Sunshine 主机的当前运行状态列表。
pub(crate) async fn services(State(state): State<AppState>) -> Json<Vec<ServiceStatus>> {
    Json(crate::sunshine::status::all_services(&state).await)
}

/// 返回最近一次系统资源快照（CPU、内存、磁盘、网络吞吐）。
///
/// **本 handler 不发起任何采样**，只读后台任务维护的快照。吞吐类指标是"读取即消费"的
/// 差值，若在 handler 里现场采样，两个并发观察者会互相吃掉对方的增量窗口——后到的那个
/// 直接读到 0。详见 `system` 模块顶部的说明。
pub(crate) async fn resources(
    State(state): State<AppState>,
) -> Json<crate::system::SystemResources> {
    Json(state.resources.snapshot())
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditLogQuery {
    pub limit: Option<i64>,
    pub before_id: Option<i64>,
}

/// Authenticated, cursor-paginated audit export.
pub(crate) async fn audit_logs(
    State(state): State<AppState>,
    Query(query): Query<AuditLogQuery>,
) -> AppResult<Response> {
    if query.before_id.is_some_and(|id| id <= 0) {
        return Err(crate::error::AppError::BadRequest(
            "before_id must be a positive audit id".to_string(),
        ));
    }
    let page = crate::infra::database::list_audit_logs(
        state.db().as_ref(),
        query.before_id,
        query.limit.unwrap_or(100).clamp(1, 500),
    )
    .await?;
    let mut response = Json(page).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// SSE 服务状态推送流：转发后台探测任务广播的状态更新（当前每 5 秒一次）。
///
/// # 为什么是订阅而不是自己轮询
///
/// 让每个连接各自跑一遍探测循环会有两个问题：对每台 Sunshine 主机的探测频率随浏览器
/// 标签数线性放大；且串行探测下 10 台离线主机 × 500ms 超时就要 5 秒，恰好等于推送
/// 周期，实际推送退化成约 10 秒一次。
///
/// 因此探测集中在 `startup::start_service_status_probe` 启动的后台 worker 里，本 handler
/// 只做订阅转发，连接数不影响对被监控主机的压力。
///
/// # axum SSE 的工作原理
///
/// `Sse::new(stream)` 接受一个实现了 `Stream<Item = Result<Event, Infallible>>` 的流。
/// `Event` 是一个 SSE 帧，包含：
/// - `.event("status")` — 事件名称（前端 `addEventListener("status", ...)` 监听）
/// - `.data(string)` — 事件数据（通常是 JSON 字符串）
///
/// `KeepAlive::default()` 会定期发送 SSE 心跳注释（`: ping`），
/// 防止代理服务器（如 nginx）因为连接长时间没有数据而关闭连接。
///
/// # 错误类型 `Infallible`
///
/// `Infallible` 是 Rust 标准库中表示"永不发生"的错误类型。
/// 这里 `yield Ok(...)` 保证不会产生错误，所以用 `Infallible` 作为错误类型。
pub(crate) async fn events(
    State(state): State<AppState>,
    Extension(mut session_cancellation): Extension<crate::state::SseSessionCancellation>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    // 本 handler **不发起任何探测**：探测由 startup 启动的后台 worker 负责，
    // 这里只订阅广播。因此连接数不再放大对被监控主机的探测频率。
    let mut updates = state.services.events.subscribe();
    let mut shutdown = state.subscribe_shutdown();

    // `stream!` 宏生成一个 `async_stream::AsyncStream`，可以被 `Sse` 消费
    let events = stream! {
        if *shutdown.borrow() || session_cancellation.is_cancelled() {
            return;
        }
        // 先立即推送当前快照，新连接无需等待下一个探测周期。
        let initial = crate::sunshine::status::all_services(&state).await;
        if *shutdown.borrow() || session_cancellation.is_cancelled() {
            return;
        }
        yield Ok(sse_event(initial));

        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow_and_update() {
                        break;
                    }
                }
                () = session_cancellation.cancelled() => break,
                update = updates.recv() => match update {
                    Ok(services) => yield Ok(sse_event(services)),
                    // 本连接消费过慢被落下：直接跳到最新状态即可，状态推送没有补发意义。
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::debug!("SSE 客户端落后 {skipped} 条状态更新，已跳到最新");
                        let latest = crate::sunshine::status::all_services(&state).await;
                        yield Ok(sse_event(latest));
                    }
                    // 所有发送端都已释放时结束本流。
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    };
    Sse::new(events).keep_alive(KeepAlive::default()) // 启用 SSE 心跳，防止连接超时断开
}

fn sse_event(services: Vec<ServiceStatus>) -> Event {
    let payload = EventPayload {
        kind: "service-status".to_string(),
        generated_at: Utc::now().to_rfc3339(), // RFC 3339 格式时间戳
        services,
    };
    // 序列化失败时退化为空 JSON 对象，避免中断整条流
    let data = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    Event::default().event("status").data(data)
}
