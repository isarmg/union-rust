//! Read-only multi-host monitoring endpoints.

#[cfg(test)]
use std::time::Instant;

use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::monitoring::{
    ActivateAgentRequest, ActivateAgentResponse, ActivatePairingStatus, AgentInstanceSummary,
    AgentPairingRequest, AgentPairingRequestExt, AgentPairingResponse, AgentPairingStatusResponse,
    AgentReport, AgentReportAck, AgentReportExt, CreateAgentInstanceRequest, CreatedAgentInstance,
    HistoryPoint, HistoryQuery, HistoryResponse, HostDetailResponse, HostListQuery,
    HostListResponse, HostSummary, MetricSummary,
};
use crate::{
    error::{AppError, AppResult},
    infra::database,
    state::AppState,
};

const PAIRING_TTL_SECONDS: i64 = 15 * 60;
const PAIRING_POLL_INTERVAL_SECONDS: u64 = 5;

struct AuthenticatedReport {
    host_id: String,
    credential_hash: String,
}

/// Header-only admission for anonymous pairing writes. Keeping this separate from the final
/// `Bytes` extractor guarantees proxy validation, quota consumption and media-type validation all
/// complete before Axum starts polling the request body.
struct AnonymousPairingWrite;

impl FromRequestParts<AppState> for AnonymousPairingWrite {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let client = require_agent_reverse_proxy(state, &parts.headers)?;
        check_pairing_rate(state, client).await?;
        require_json_content_type(&parts.headers)?;
        Ok(Self)
    }
}

impl FromRequestParts<AppState> for AuthenticatedReport {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let client = require_agent_reverse_proxy(state, &parts.headers)?;
        check_report_auth_rate(state, client).await?;
        let credential = bearer_token(&parts.headers).ok_or(AppError::Unauthorized)?;
        let credential_hash = token_hash(credential);
        let host_id = match crate::monitoring::store::monitoring_host_for_token(
            state.db().as_ref(),
            &credential_hash,
        )
        .await
        .map_err(|error| agent_database_unavailable("authenticate monitoring report", error))?
        {
            crate::monitoring::store::MonitoringTokenAuthentication::Active(host_id) => host_id,
            crate::monitoring::store::MonitoringTokenAuthentication::Revoked => {
                return Err(AppError::AgentRevoked);
            }
            crate::monitoring::store::MonitoringTokenAuthentication::Unknown => {
                return Err(AppError::Unauthorized);
            }
        };
        check_report_rate(state, &host_id).await?;
        require_json_content_type(&parts.headers)?;
        Ok(Self {
            host_id,
            credential_hash,
        })
    }
}

mod agent;
mod console;
mod limits;

use limits::*;

pub(crate) fn console_router() -> Router<AppState> {
    console::router()
}

pub(crate) fn agent_router() -> Router<AppState> {
    agent::router()
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
            .map_err(|error| agent_database_unavailable("list agent instance invites", error))?,
    ))
}

async fn cancel_agent_instance(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
) -> AppResult<StatusCode> {
    let request_id = validate_uuid(&request_id, "agent instance request id")?;
    match crate::monitoring::store::revoke_agent_instance_invite(state.db().as_ref(), &request_id)
        .await
        .map_err(|error| agent_database_unavailable("cancel agent instance invite", error))?
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
    _admission: AnonymousPairingWrite,
    body: axum::body::Bytes,
) -> AppResult<Response> {
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
        crate::monitoring::store::CreatePairingResult::AtCapacity => {
            return Err(AppError::TooManyRequests(
                "too many pending agent pairing requests; retry after an earlier request expires"
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
    _admission: AnonymousPairingWrite,
    body: axum::body::Bytes,
) -> AppResult<Response> {
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
        status: ActivatePairingStatus::Active,
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
/// axum 按参数顺序运行提取器。`AuthenticatedReport` 是纯请求头提取器，先完成反代
/// 契约、匿名限流、凭据认证、主机限流与媒体类型检查；只有全部通过后，最后一个
/// `Bytes` 提取器才会读取最多 512 KiB 的请求体。handler 再执行 JSON 反序列化，避免
/// 匿名请求驱动应用层轮询、聚合请求体或消耗 JSON 解析 CPU。底层网络栈仍可能按自身
/// 缓冲策略预读少量数据，这不属于提取器能控制的边界。
async fn report_metrics(
    State(state): State<AppState>,
    authenticated: AuthenticatedReport,
    body: axum::body::Bytes,
) -> AppResult<Response> {
    let report: AgentReport = serde_json::from_slice(&body)
        .map_err(|error| AppError::BadRequest(format!("invalid agent report: {error}")))?;
    report.validate()?;
    let reported_host = uuid::Uuid::parse_str(&report.host.id)
        .expect("validated host UUID")
        .to_string();
    if reported_host != authenticated.host_id {
        return Err(AppError::AgentHostMismatch);
    }
    let (accepted, received_at) = crate::monitoring::store::store_authenticated_monitoring_report(
        state.db().as_ref(),
        &report,
        &authenticated.credential_hash,
    )
    .await
    .map_err(map_store_report_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(AgentReportAck {
            host_id: authenticated.host_id,
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
            .map_err(|error| agent_database_unavailable("list monitored hosts", error))?;
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
        .map_err(|error| agent_database_unavailable("read monitored host", error))?
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
    .map_err(|error| agent_database_unavailable("read monitoring history", error))?
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
        .map_err(|error| agent_database_unavailable("revoke monitored host", error))?
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

fn map_store_report_error(error: anyhow::Error) -> AppError {
    // report_id 由 Agent 生成，撞上另一台主机的 id 属于客户端输入冲突，
    // 应当是 409 而不是 503——重试同一个 id 永远不会成功。
    match error.downcast_ref::<crate::monitoring::store::StoreReportError>() {
        Some(crate::monitoring::store::StoreReportError::ReportIdBelongsToAnotherHost) => {
            AppError::Conflict(error.to_string())
        }
        Some(crate::monitoring::store::StoreReportError::HostNotActive) => AppError::AgentRevoked,
        Some(crate::monitoring::store::StoreReportError::CredentialNotActive) => {
            AppError::Unauthorized
        }
        None => agent_database_unavailable("store monitoring report", error),
    }
}

/// Agent 接口的反向代理契约。与控制台接口共用同一份实现。
///
/// 返回 421 而非 403 是刻意的：Agent 只对受控的 403 机器码采取不可逆动作，未知 403
/// 虽然会保留重试，但不能清楚表达“请求走错入口”。421 直接归入可重试部署故障，反代
/// 修好后同一份报文原样重发即可。
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

// Tests remain in this module scope so private handler and limiter contracts
// are exercised without exposing them from the HTTP boundary.
include!("tests.rs");
