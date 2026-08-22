//! Read-only multi-host monitoring endpoints.

use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::monitoring::{
    ActivateAgentRequest, ActivateAgentResponse, AgentInstanceSummary, AgentPairingRequest,
    AgentPairingResponse, AgentPairingStatusResponse, AgentReport, AgentReportExt,
    AgentReportResponse, CreateAgentInstanceRequest, CreatedAgentInstance, HistoryPoint,
    HistoryQuery, HistoryResponse, HostDetailResponse, HostListQuery, HostListResponse,
    HostSummary, MetricSummary,
};
use crate::{
    error::{AppError, AppResult},
    infra::database,
    state::AppState,
};

const REPORT_AUTH_WINDOW: Duration = Duration::from_secs(60);
const MAX_REPORT_AUTH_PER_IP: usize = 15_000;
const MAX_REPORT_AUTH_GLOBAL: usize = 18_000;
const PAIRING_WINDOW: Duration = Duration::from_secs(60);
const MAX_PAIRING_PER_IP: usize = 120;
const MAX_PAIRING_GLOBAL: usize = 6_000;
const PAIRING_TTL_SECONDS: i64 = 15 * 60;
const PAIRING_POLL_INTERVAL_SECONDS: u64 = 5;

pub(crate) fn console_router() -> Router<AppState> {
    Router::new()
        .route("/api/monitoring/hosts", get(list_hosts))
        .route("/api/monitoring/hosts/{host_id}", get(host_detail))
        .route("/api/monitoring/hosts/{host_id}/history", get(host_history))
        .route("/api/monitoring/hosts/{host_id}/revoke", post(revoke_host))
        .route(
            "/api/monitoring/agent-instances",
            get(list_agent_instances).post(create_agent_instance),
        )
        .route(
            "/api/monitoring/agent-instances/{request_id}",
            axum::routing::delete(cancel_agent_instance),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
}

pub(crate) fn agent_router() -> Router<AppState> {
    let reporting = Router::new()
        .route("/api/agent/v1/report", post(report_metrics))
        .layer(DefaultBodyLimit::max(512 * 1024));
    let pairing = Router::new()
        .route(
            "/api/agent/v2/pairing-requests",
            post(create_pairing_request),
        )
        .route(
            "/api/agent/v2/pairing-requests/{request_id}",
            get(public_pairing_request),
        )
        .route(
            "/api/agent/v2/pairing-requests/{request_id}/status",
            post(pairing_request_status),
        )
        .route("/api/agent/v2/activate", post(activate_pairing_request))
        .layer(DefaultBodyLimit::max(16 * 1024));
    reporting.merge(pairing)
}

async fn create_agent_instance(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> AppResult<Response> {
    require_json_content_type(&headers)?;
    let request: CreateAgentInstanceRequest = serde_json::from_slice(&body).map_err(|error| {
        AppError::BadRequest(format!("invalid agent instance request: {error}"))
    })?;
    let (display_name, expires_in_minutes, requested_instance_id) = request.validated()?;
    let existing_instance = requested_instance_id.is_some();
    let instance_id = requested_instance_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let invite_id = uuid::Uuid::new_v4().to_string();
    let activation_code = format!("uci_{}", uuid::Uuid::new_v4().simple());
    let expires_at = Utc::now() + chrono::Duration::minutes(expires_in_minutes);
    let summary = match crate::monitoring::store::create_agent_instance_invite(
        state.db().as_ref(),
        &invite_id,
        &instance_id,
        &token_hash(&activation_code),
        &display_name,
        expires_at,
        existing_instance,
    )
    .await
    .map_err(|error| agent_database_unavailable("create agent instance invite", error))?
    {
        crate::monitoring::store::CreateInviteResult::Created(summary) => summary,
        crate::monitoring::store::CreateInviteResult::InstanceNotFound => {
            return Err(AppError::NotFound(
                "monitored instance not found".to_string(),
            ));
        }
        crate::monitoring::store::CreateInviteResult::Conflict => {
            return Err(AppError::Conflict(
                "an unconsumed invite already exists for this instance".to_string(),
            ));
        }
    };
    let mut response = (
        StatusCode::CREATED,
        Json(CreatedAgentInstance {
            summary,
            activation_code,
        }),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn list_agent_instances(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<AgentInstanceSummary>>> {
    Ok(Json(
        crate::monitoring::store::list_agent_instance_invites(state.db().as_ref())
            .await
            .map_err(AppError::Anyhow)?,
    ))
}

async fn cancel_agent_instance(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
) -> AppResult<StatusCode> {
    let request_id = validate_uuid(&request_id, "agent instance request id")?;
    match crate::monitoring::store::revoke_agent_instance_invite(state.db().as_ref(), &request_id)
        .await
        .map_err(AppError::Anyhow)?
    {
        crate::monitoring::store::RevokeInviteResult::Revoked => Ok(StatusCode::NO_CONTENT),
        crate::monitoring::store::RevokeInviteResult::NotFound => Err(AppError::NotFound(
            "agent instance invite not found".to_string(),
        )),
        crate::monitoring::store::RevokeInviteResult::NotPending => Err(AppError::Conflict(
            "only a pending agent instance invite can be cancelled".to_string(),
        )),
    }
}

async fn create_pairing_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> AppResult<Response> {
    let client = require_agent_reverse_proxy(&state, &headers)?;
    // Anonymous quota and the 16 KiB route limit are both applied before JSON
    // deserialization.
    check_pairing_rate(&state, client).await?;
    require_json_content_type(&headers)?;
    let request: AgentPairingRequest = serde_json::from_slice(&body)
        .map_err(|error| AppError::BadRequest(format!("invalid pairing request: {error}")))?;
    request.validate()?;
    let proposed_request_id = uuid::Uuid::new_v4().to_string();
    let expires_at = Utc::now() + chrono::Duration::seconds(PAIRING_TTL_SECONDS);
    let stored = match crate::monitoring::store::create_agent_pairing_request(
        state.db().as_ref(),
        &proposed_request_id,
        &request,
        expires_at,
    )
    .await
    .map_err(|error| agent_database_unavailable("create pairing request", error))?
    {
        crate::monitoring::store::CreatePairingResult::Ready(stored) => stored,
        crate::monitoring::store::CreatePairingResult::Expired => {
            return Err(AppError::Gone(
                "the pairing request associated with this polling secret has expired".to_string(),
            ));
        }
        crate::monitoring::store::CreatePairingResult::Conflict => {
            return Err(AppError::Conflict(
                "polling secret or agent token is already associated with another pairing request"
                    .to_string(),
            ));
        }
    };
    let expires_in = (stored.expires_at - Utc::now()).num_seconds().max(1) as u64;
    let activation_url = format!("/agent/activate/{}", stored.request_id);
    let status = if stored.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let mut response = (
        status,
        Json(AgentPairingResponse {
            request_id: stored.request_id,
            activation_url,
            expires_in,
            poll_interval: PAIRING_POLL_INTERVAL_SECONDS,
        }),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn public_pairing_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> AppResult<Response> {
    let client = require_agent_reverse_proxy(&state, &headers)?;
    check_pairing_rate(&state, client).await?;
    let request_id = validate_uuid(&request_id, "pairing request id")?;
    let summary =
        crate::monitoring::store::public_agent_pairing_request(state.db().as_ref(), &request_id)
            .await
            .map_err(|error| agent_database_unavailable("read pairing request", error))?
            .ok_or_else(|| AppError::NotFound("pairing request not found".to_string()))?;
    let mut response = Json(summary).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn pairing_request_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> AppResult<Response> {
    let client = require_agent_reverse_proxy(&state, &headers)?;
    // Polling has a separate high-volume anonymous-auth allowance; the secret
    // is still checked only after the quota is consumed.
    check_report_auth_rate(&state, client).await?;
    let secret = pairing_secret(&headers).ok_or(AppError::Unauthorized)?;
    if !(32..=256).contains(&secret.len()) || secret.chars().any(char::is_whitespace) {
        return Err(AppError::Unauthorized);
    }
    let request_id = validate_uuid(&request_id, "pairing request id")?;
    let status = crate::monitoring::store::agent_pairing_status(
        state.db().as_ref(),
        &request_id,
        &token_hash(secret),
    )
    .await
    .map_err(|error| agent_database_unavailable("read pairing status", error))?
    .ok_or(AppError::Unauthorized)?;
    let mut response = Json(AgentPairingStatusResponse {
        status: status.status,
        instance_id: status.instance_id,
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn activate_pairing_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> AppResult<Response> {
    let client = require_agent_reverse_proxy(&state, &headers)?;
    check_pairing_rate(&state, client).await?;
    require_json_content_type(&headers)?;
    let request: ActivateAgentRequest = serde_json::from_slice(&body)
        .map_err(|error| AppError::BadRequest(format!("invalid activation request: {error}")))?;
    let request_id = validate_uuid(&request.request_id, "pairing request id")?;
    if request.activation_code.len() > 256
        || request.activation_code.chars().any(char::is_whitespace)
    {
        return Err(AppError::Unauthorized);
    }
    let instance_id = match crate::monitoring::store::activate_agent_pairing(
        state.db().as_ref(),
        &request_id,
        &token_hash(&request.activation_code),
    )
    .await
    .map_err(|error| agent_database_unavailable("activate pairing request", error))?
    {
        crate::monitoring::store::ActivatePairingResult::Active(instance_id) => instance_id,
        crate::monitoring::store::ActivatePairingResult::RequestNotFound => {
            return Err(AppError::NotFound("pairing request not found".to_string()));
        }
        crate::monitoring::store::ActivatePairingResult::InvalidCode => {
            return Err(AppError::Unauthorized);
        }
        crate::monitoring::store::ActivatePairingResult::Expired => {
            return Err(AppError::Gone(
                "pairing request or activation code has expired".to_string(),
            ));
        }
        crate::monitoring::store::ActivatePairingResult::Conflict => {
            return Err(AppError::Conflict(
                "activation code or pairing request has already been used".to_string(),
            ));
        }
    };
    let mut response = Json(ActivateAgentResponse {
        instance_id,
        status: "active".to_string(),
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// POST /api/agent/v1/report —— 接收一份主机指标报文。
///
/// # 为什么 body 是 `Bytes` 而不是 `Json<AgentReport>`
///
/// axum 的提取器在 **handler 体执行之前**运行。用 `Json<AgentReport>` 时，一个完全
/// 未认证的请求也能驱动一次完整的 512 KiB JSON 反序列化——认证检查在解析**之后**才
/// 轮到执行，等于把解析成本白送给任何匿名调用方。
///
/// 改为先取原始 `Bytes`（只是把已读入的 body 交出来，不做结构化解析），认证与限流
/// 通过后再 `serde_json::from_slice`。
///
/// 需要说清这**没有**省掉什么：`Bytes` 提取器仍然会把最多 512 KiB 的 body 完整读入
/// 内存才进入 handler，因此带宽与内存占用不变，`DefaultBodyLimit` 仍是那道唯一的
/// 闸门。省掉的只是 JSON 解析与 `AgentReport` 的结构化分配——对一份含上千个设备
/// 条目的报文而言，这仍是未认证路径上最大的一块 CPU 开销。
async fn report_metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> AppResult<Response> {
    let client = require_agent_reverse_proxy(&state, &headers)?;
    check_report_auth_rate(&state, client).await?;
    let credential = bearer_token(&headers).ok_or(AppError::Unauthorized)?;
    let authenticated_host = match crate::monitoring::store::monitoring_host_for_token(
        state.db().as_ref(),
        &token_hash(credential),
    )
    .await
    .map_err(|error| {
        tracing::warn!(%error, "agent token lookup failed");
        AppError::DatabaseUnavailable("database is unavailable".to_string())
    })? {
        crate::monitoring::store::MonitoringTokenAuthentication::Active(host_id) => host_id,
        crate::monitoring::store::MonitoringTokenAuthentication::Revoked => {
            return Err(AppError::AgentRevoked);
        }
        crate::monitoring::store::MonitoringTokenAuthentication::Unknown => {
            return Err(AppError::Unauthorized);
        }
    };
    check_report_rate(&state, &authenticated_host).await?;
    // 认证与限流都通过了，现在才检查媒体类型并为 body 花解析成本。
    require_json_content_type(&headers)?;
    let report: AgentReport = serde_json::from_slice(&body)
        .map_err(|error| AppError::BadRequest(format!("invalid agent report: {error}")))?;
    report.validate()?;
    let reported_host = uuid::Uuid::parse_str(&report.host.id)
        .expect("validated host UUID")
        .to_string();
    if reported_host != authenticated_host {
        return Err(AppError::Forbidden(
            "agent token does not belong to the reported host".to_string(),
        ));
    }
    let (accepted, received_at) =
        crate::monitoring::store::store_monitoring_report(state.db().as_ref(), &report)
            .await
            .map_err(|error| {
                // report_id 由 Agent 生成，撞上另一台主机的 id 属于客户端输入冲突，
                // 应当是 409 而不是 500——重试同一个 id 永远不会成功。
                match error.downcast_ref::<crate::monitoring::store::StoreReportError>() {
                    Some(
                        crate::monitoring::store::StoreReportError::ReportIdBelongsToAnotherHost,
                    ) => AppError::Conflict(error.to_string()),
                    Some(crate::monitoring::store::StoreReportError::HostNotActive) => {
                        AppError::AgentRevoked
                    }
                    None => AppError::Anyhow(error),
                }
            })?;
    Ok((
        StatusCode::ACCEPTED,
        Json(AgentReportResponse {
            host_id: authenticated_host,
            report_id: report.report_id,
            accepted,
            received_at,
        }),
    )
        .into_response())
}

/// 单页返回的主机数上限与缺省值。
///
/// 缺省 200 足够覆盖绝大多数部署一屏拉完；上限 1000 与历史曲线的 limit 同口径。
const DEFAULT_HOST_PAGE: i64 = 200;
const MAX_HOST_PAGE: i64 = 1000;

async fn list_hosts(
    State(state): State<AppState>,
    Query(query): Query<HostListQuery>,
) -> AppResult<Json<HostListResponse>> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_HOST_PAGE)
        .clamp(1, MAX_HOST_PAGE);
    let offset = query.offset.unwrap_or(0).max(0);
    let (stored, total) =
        crate::monitoring::store::list_monitored_hosts(state.db().as_ref(), limit, offset)
            .await
            .map_err(AppError::Anyhow)?;
    Ok(Json(HostListResponse {
        hosts: stored.into_iter().map(host_summary).collect(),
        total,
        limit,
        offset,
    }))
}

async fn host_detail(
    State(state): State<AppState>,
    Path(host_id): Path<String>,
) -> AppResult<Json<HostDetailResponse>> {
    validate_host_id(&host_id)?;
    let stored = crate::monitoring::store::get_monitored_host(state.db().as_ref(), &host_id)
        .await
        .map_err(AppError::Anyhow)?
        .ok_or_else(|| AppError::NotFound("monitored host not found".to_string()))?;
    let latest = stored.latest.clone();
    Ok(Json(HostDetailResponse {
        host: host_summary(stored),
        latest,
    }))
}

async fn host_history(
    State(state): State<AppState>,
    Path(host_id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> AppResult<Json<HistoryResponse>> {
    let host_id = validate_host_id(&host_id)?;
    if query.from.zip(query.to).is_some_and(|(from, to)| from > to) {
        return Err(AppError::BadRequest(
            "history from must not be after to".to_string(),
        ));
    }
    // 存在性判断由同一条查询完成：返回 None 即主机不存在。
    let points = crate::monitoring::store::monitoring_history(
        state.db().as_ref(),
        &host_id,
        query.from,
        query.to,
        query.limit.unwrap_or(300).clamp(1, 1000),
    )
    .await
    .map_err(AppError::Anyhow)?
    .ok_or_else(|| AppError::NotFound("monitored host not found".to_string()))?
    .into_iter()
    .map(history_point)
    .collect();
    Ok(Json(HistoryResponse { host_id, points }))
}

async fn revoke_host(
    State(state): State<AppState>,
    Path(host_id): Path<String>,
) -> AppResult<StatusCode> {
    let host_id = validate_host_id(&host_id)?;
    require_database(&state).await?;
    if !crate::monitoring::store::revoke_monitored_host(state.db().as_ref(), &host_id)
        .await
        .map_err(AppError::Anyhow)?
    {
        return Err(AppError::NotFound("monitored host not found".to_string()));
    }
    state.agents.forget_host(&host_id).await;
    Ok(StatusCode::NO_CONTENT)
}

fn host_summary(stored: crate::monitoring::store::StoredHost) -> HostSummary {
    // 指标直接来自数据库的摘要数值列；此处不再解析 latest_report JSON 文本。
    let metrics = stored.metrics;
    let status = if stored.lifecycle_status == "revoked" {
        "revoked".to_string()
    } else {
        host_status(stored.last_seen_at, stored.latest_interval_seconds)
    };
    HostSummary {
        id: stored.identity.id,
        name: stored.identity.name,
        os: stored.identity.os,
        os_version: stored.identity.os_version,
        kernel_version: stored.identity.kernel_version,
        arch: stored.identity.arch,
        agent_version: stored.identity.agent_version,
        lifecycle_status: stored.lifecycle_status,
        registered_at: stored.registered_at,
        last_seen_at: stored.last_seen_at,
        latest_collected_at: stored.latest_collected_at,
        status,
        capabilities: stored.capabilities,
        cpu_usage_percent: metrics.cpu_usage_percent,
        memory_usage_percent: metrics.memory_usage_percent,
        network_received_bytes_per_second: metrics.network_received_bytes_per_second,
        network_transmitted_bytes_per_second: metrics.network_transmitted_bytes_per_second,
        disk_read_bytes_per_second: metrics.disk_read_bytes_per_second,
        disk_written_bytes_per_second: metrics.disk_written_bytes_per_second,
        max_temperature_celsius: metrics.max_temperature_celsius,
        gpu_utilization_percent: metrics.gpu_utilization_percent,
        gpu_memory_usage_percent: metrics.gpu_memory_usage_percent,
    }
}

fn history_point(stored: crate::monitoring::store::StoredHistoryPoint) -> HistoryPoint {
    let MetricSummary {
        cpu_usage_percent,
        memory_usage_percent,
        network_received_bytes_per_second,
        network_transmitted_bytes_per_second,
        disk_read_bytes_per_second,
        disk_written_bytes_per_second,
        max_temperature_celsius,
        gpu_utilization_percent,
        gpu_memory_usage_percent,
    } = stored.metrics;
    HistoryPoint {
        report_id: stored.report_id,
        collected_at: stored.collected_at,
        received_at: stored.received_at,
        cpu_usage_percent,
        memory_usage_percent,
        network_received_bytes_per_second,
        network_transmitted_bytes_per_second,
        disk_read_bytes_per_second,
        disk_written_bytes_per_second,
        max_temperature_celsius,
        gpu_utilization_percent,
        gpu_memory_usage_percent,
    }
}

fn host_status(last_seen: chrono::DateTime<Utc>, interval: Option<f64>) -> String {
    let age = (Utc::now() - last_seen).num_seconds().max(0) as f64;
    let interval = interval.unwrap_or(10.0).clamp(1.0, 3600.0);
    if age <= (interval * 3.0).max(30.0) {
        "online"
    } else if age <= (interval * 12.0).max(300.0) {
        "stale"
    } else {
        "offline"
    }
    .to_string()
}

async fn require_database(state: &AppState) -> AppResult<()> {
    database::ping(state.db().as_ref())
        .await
        .map_err(|error| agent_database_unavailable("check database health", error))
}

fn agent_database_unavailable(operation: &str, error: impl std::fmt::Display) -> AppError {
    tracing::warn!(operation, %error, "Agent database operation failed");
    AppError::DatabaseUnavailable("database is unavailable".to_string())
}

/// Agent 接口的反向代理契约。与控制台接口共用同一份实现。
///
/// 返回 421 而非 403 是刻意的：Agent 把 403 当作实例被持久撤销并进入
/// `reauth_required`（见 `agent/src/transport.rs` 的 `ensure_success`）。若这里沿用 403，
/// 一次反代漏配请求头就会让每台 Agent 误以为自己已被退役。421 归入可重试类，反代修好后
/// 同一份报文原样重发即可。
fn require_agent_reverse_proxy(
    state: &AppState,
    headers: &HeaderMap,
) -> AppResult<Option<std::net::IpAddr>> {
    crate::auth::http::require_reverse_proxy_contract(state, headers, "Agent 接口")
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

fn pairing_secret(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, secret) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("pairing") && !secret.is_empty()).then_some(secret)
}

fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn require_json_content_type(headers: &HeaderMap) -> AppResult<()> {
    let mut values = headers.get_all(header::CONTENT_TYPE).iter();
    let Some(value) = values.next() else {
        return Err(AppError::UnsupportedMediaType(
            "Content-Type must be application/json".to_string(),
        ));
    };
    if values.next().is_some() {
        return Err(AppError::UnsupportedMediaType(
            "Content-Type must contain exactly one application/json value".to_string(),
        ));
    }
    let raw = value.to_str().ok();
    let parsed = raw.and_then(|value| value.parse::<mime::Mime>().ok());
    if parsed
        .as_ref()
        .is_none_or(|value| value.essence_str() != "application/json")
        || raw.is_some_and(|value| {
            value.trim_end().ends_with(';')
                || value
                    .split(';')
                    .skip(1)
                    .any(|parameter| parameter.trim().is_empty())
        })
    {
        return Err(AppError::UnsupportedMediaType(
            "Content-Type must be application/json".to_string(),
        ));
    }
    Ok(())
}

fn validate_host_id(value: &str) -> AppResult<String> {
    validate_uuid(value, "host id")
}

fn validate_uuid(value: &str, field: &str) -> AppResult<String> {
    let parsed = uuid::Uuid::parse_str(value).map_err(|_| {
        AppError::BadRequest(format!(
            "{field} must be a canonical lowercase, hyphenated UUID"
        ))
    })?;
    if parsed.to_string() != value {
        return Err(AppError::BadRequest(format!(
            "{field} must be a canonical lowercase, hyphenated UUID"
        )));
    }
    Ok(value.to_string())
}

async fn check_pairing_rate(state: &AppState, client: Option<std::net::IpAddr>) -> AppResult<()> {
    let now = Instant::now();
    let mut global = state.agents.pairing_attempts.lock().await;
    prune_pairing_attempts(&mut global, now);
    if global.len() >= MAX_PAIRING_GLOBAL {
        return Err(AppError::TooManyRequests(
            "global agent pairing rate limit exceeded".to_string(),
        ));
    }
    if let Some(address) = client {
        let mut by_ip = state.agents.pairing_attempts_by_ip.lock().await;
        if by_ip.len() >= MAX_PAIRING_GLOBAL * 2 {
            by_ip.retain(|_, attempts| {
                prune_pairing_attempts(attempts, now);
                !attempts.is_empty()
            });
        }
        let attempts = by_ip.entry(address).or_default();
        prune_pairing_attempts(attempts, now);
        if attempts.len() >= MAX_PAIRING_PER_IP {
            return Err(AppError::TooManyRequests(
                "agent pairing rate limit exceeded".to_string(),
            ));
        }
        attempts.push_back(now);
    }
    global.push_back(now);
    Ok(())
}

fn prune_pairing_attempts(attempts: &mut std::collections::VecDeque<Instant>, now: Instant) {
    while attempts
        .front()
        .is_some_and(|attempt| now.duration_since(*attempt) >= PAIRING_WINDOW)
    {
        attempts.pop_front();
    }
}

/// 未认证上报的入口配额。
///
/// 有效 token 的主机桶只能保护写入路径；随机 Bearer token 在查库前还没有 host_id，
/// 若不先按来源限流，匿名请求仍能无限触发 token 索引查询。阈值按最多 20 台 Agent
/// 共用一个 NAT、且都使用协议允许的 10 次/秒极限配置留足余量。
async fn check_report_auth_rate(
    state: &AppState,
    client: Option<std::net::IpAddr>,
) -> AppResult<()> {
    let now = Instant::now();
    let mut global = state.agents.report_auth_attempts.lock().await;
    prune_report_auth_attempts(&mut global, now);
    if global.len() >= MAX_REPORT_AUTH_GLOBAL {
        return Err(AppError::TooManyRequests(
            "global agent report authentication rate limit exceeded".to_string(),
        ));
    }
    if let Some(address) = client {
        let mut by_ip = state.agents.report_auth_attempts_by_ip.lock().await;
        // 分布式来源会留下空桶；超过全局窗口容量的两倍时做一次有界回收，避免每个
        // 热路径请求都 O(n) retain，也避免长期运行后 map 无上限增长。
        if by_ip.len() >= MAX_REPORT_AUTH_GLOBAL * 2 {
            by_ip.retain(|_, attempts| {
                prune_report_auth_attempts(attempts, now);
                !attempts.is_empty()
            });
        }
        let attempts = by_ip.entry(address).or_default();
        prune_report_auth_attempts(attempts, now);
        if attempts.len() >= MAX_REPORT_AUTH_PER_IP {
            return Err(AppError::TooManyRequests(
                "agent report authentication rate limit exceeded".to_string(),
            ));
        }
        attempts.push_back(now);
    }
    global.push_back(now);
    Ok(())
}

fn prune_report_auth_attempts(attempts: &mut std::collections::VecDeque<Instant>, now: Instant) {
    while attempts
        .front()
        .is_some_and(|attempt| now.duration_since(*attempt) >= REPORT_AUTH_WINDOW)
    {
        attempts.pop_front();
    }
}

/// 上报限流：按主机令牌桶。
///
/// 不限流的话，持有有效 token 的主机（或凭据泄露方）可以无节制地写入数据库。
/// 阈值取得高于任何合法配置所能产生的速率（见 `TokenBucket` 的常量说明），
/// 因此正常 Agent 与断线补传都不会被误伤。
async fn check_report_rate(state: &AppState, host_id: &str) -> AppResult<()> {
    if state.agents.allow_report(host_id, Instant::now()).await {
        return Ok(());
    }
    Err(AppError::TooManyRequests(
        "agent report rate limit exceeded for this host".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{LocalConfig, Settings},
        infra::database,
    };

    fn state() -> AppState {
        AppState::new(
            Settings::default(),
            database::in_memory_pool().expect("in-memory test pool"),
            "unused".to_string(),
            LocalConfig {
                application_version: env!("CARGO_PKG_VERSION").to_string(),
                admin_username: "admin".to_string(),
                admin_password_hash: "unused".to_string(),
            },
            crate::system::ResourceMonitor::frozen(Default::default()),
        )
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
