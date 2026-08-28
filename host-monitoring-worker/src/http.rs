use std::{collections::HashMap, sync::Arc, time::Instant};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use tokio::sync::Mutex;
use unionc_protocol::{
    AGENT_REPORT_MAX_BODY_BYTES, ActivateAgentRequest, ActivateAgentResponse,
    ActivatePairingStatus, AgentPairingRequest, AgentPairingResponse, AgentPairingStatusResponse,
    AgentReport, AgentReportAck,
};

use crate::{
    auth::{self, Principal},
    error::{Error, Result, database},
    model::{
        CreateAgentInstanceRequest, CreatedAgentInstance, HistoryQuery, HistoryResponse,
        HostDetailResponse, HostListQuery, HostListResponse, UpdateMonitoringRemarkRequest,
        canonical_uuid, validate_pairing, validate_report,
    },
    store,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::PgPool,
    pub gateway: sarmg_platform_gateway::GatewayIdentity,
    report_buckets: Arc<Mutex<HashMap<String, TokenBucket>>>,
}

impl AppState {
    pub fn new(pool: sqlx::PgPool, gateway: sarmg_platform_gateway::GatewayIdentity) -> Self {
        Self {
            pool,
            gateway,
            report_buckets: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

struct TokenBucket {
    tokens: f64,
    updated: Instant,
}

impl TokenBucket {
    fn allow(&mut self) -> bool {
        let now = Instant::now();
        self.tokens =
            (self.tokens + now.duration_since(self.updated).as_secs_f64() * 16.0).min(64.0);
        self.updated = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

pub fn router(state: AppState) -> Router {
    let console = Router::new()
        .route("/api/monitoring/hosts", get(list_hosts))
        .route("/api/monitoring/hosts/{host_id}", get(host_detail))
        .route("/api/monitoring/hosts/{host_id}/history", get(host_history))
        .route(
            "/api/monitoring/agent-instances",
            get(list_instances).post(create_instance),
        )
        .route(
            "/api/monitoring/agent-instances/{request_id}",
            axum::routing::delete(cancel_instance),
        )
        .route(
            "/api/monitoring/managed-instances/{host_id}",
            axum::routing::patch(update_remark).delete(delete_host),
        )
        .route("/api/agent/v2/activate-admin", post(activate_admin))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            console_admission,
        ));
    let agent = Router::new()
        .route("/api/agent/v1/report", post(report))
        .route("/api/agent/v2/pairing-requests", post(create_pairing))
        .route(
            "/api/agent/v2/pairing-requests/{request_id}",
            get(pairing_public),
        )
        .route(
            "/api/agent/v2/pairing-requests/{request_id}/status",
            post(pairing_status),
        )
        .route("/api/agent/v2/activate", post(activate_capability))
        .layer(DefaultBodyLimit::max(AGENT_REPORT_MAX_BODY_BYTES));
    Router::new()
        .route("/health", get(live))
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .merge(console)
        .merge(agent)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            gateway_admission,
        ))
        .with_state(state)
}

async fn console_admission(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response> {
    let principal = auth::require_console(request.headers(), &state)?;
    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

async fn gateway_admission(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response> {
    auth::require(request.headers(), &state)?;
    Ok(next.run(request).await)
}

async fn live(State(state): State<AppState>) -> Response {
    with_module_identity(
        &state,
        Json(serde_json::json!({ "status": "ok" })).into_response(),
    )
}

async fn ready(State(state): State<AppState>) -> Response {
    let database = store::ready(&state.pool).await;
    let response = (
        if database {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(serde_json::json!({
            "status": if database { "ready" } else { "not-ready" },
            "database": database
        })),
    )
        .into_response();
    with_module_identity(&state, response)
}

fn with_module_identity(state: &AppState, mut response: Response) -> Response {
    state.gateway.apply_health_headers(response.headers_mut());
    response
}

async fn create_instance(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<CreateAgentInstanceRequest>,
) -> Result<Response> {
    let (name, expires) = request.validated()?;
    let (result, activation_code) =
        store::create_invite(&state.pool, &name, expires, &principal.subject)
            .await
            .map_err(database)?;
    match result {
        store::CreateInviteResult::Created(summary) => {
            let mut response = (
                StatusCode::CREATED,
                Json(CreatedAgentInstance {
                    summary,
                    activation_code: activation_code.expect("created invite has code"),
                }),
            )
                .into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            Ok(response)
        }
        store::CreateInviteResult::Conflict => {
            Err(Error::Conflict("a pending invite already exists".into()))
        }
    }
}

async fn list_instances(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::model::AgentInstanceSummary>>> {
    Ok(Json(
        store::list_invites(&state.pool).await.map_err(database)?,
    ))
}

async fn cancel_instance(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let id = canonical_uuid(&id, "agent instance request id")?;
    match store::cancel_invite(&state.pool, id, &principal.subject)
        .await
        .map_err(database)?
    {
        store::CancelInviteResult::Cancelled => Ok(StatusCode::NO_CONTENT),
        store::CancelInviteResult::NotFound => {
            Err(Error::NotFound("agent instance invite not found".into()))
        }
        store::CancelInviteResult::NotPending => Err(Error::Conflict(
            "only a pending invite can be cancelled".into(),
        )),
    }
}

async fn create_pairing(
    State(state): State<AppState>,
    Json(request): Json<AgentPairingRequest>,
) -> Result<Response> {
    validate_pairing(&request)?;
    match store::create_pairing(&state.pool, &request)
        .await
        .map_err(database)?
    {
        store::CreatePairingResult::Ready {
            request_id,
            expires_at,
            created,
        } => {
            let mut response = (
                if created {
                    StatusCode::CREATED
                } else {
                    StatusCode::OK
                },
                Json(AgentPairingResponse {
                    request_id: request_id.to_string(),
                    activation_url: activation_url(request_id),
                    expires_in: (expires_at - Utc::now()).num_seconds().max(1) as u64,
                    poll_interval: 5,
                }),
            )
                .into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            Ok(response)
        }
        store::CreatePairingResult::Expired => {
            Err(Error::BadRequest("pairing request expired".into()))
        }
        store::CreatePairingResult::Conflict => Err(Error::Conflict(
            "polling secret or agent token is already in use".into(),
        )),
        store::CreatePairingResult::AtCapacity => Err(Error::TooManyRequests(
            "too many pending pairing requests".into(),
        )),
    }
}

async fn pairing_public(State(state): State<AppState>, Path(id): Path<String>) -> Result<Response> {
    let id = canonical_uuid(&id, "pairing request id")?;
    let value = store::pairing_public(&state.pool, id)
        .await
        .map_err(database)?
        .ok_or_else(|| Error::NotFound("pairing request not found".into()))?;
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn pairing_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response> {
    let id = canonical_uuid(&id, "pairing request id")?;
    let secret = authorization(&headers, "pairing").ok_or(Error::Unauthorized)?;
    if !(32..=256).contains(&secret.len()) || secret.chars().any(char::is_whitespace) {
        return Err(Error::Unauthorized);
    }
    let (status, instance_id) = store::pairing_status(&state.pool, id, &crate::token_hash(secret))
        .await
        .map_err(database)?
        .ok_or(Error::Unauthorized)?;
    let mut response = Json(AgentPairingStatusResponse {
        status,
        instance_id,
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn activate_admin(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<ActivateAgentRequest>,
) -> Result<Response> {
    activate(&state, request, &principal.subject).await
}

async fn activate_capability(
    State(state): State<AppState>,
    Json(request): Json<ActivateAgentRequest>,
) -> Result<Response> {
    activate(&state, request, "agent-capability").await
}

async fn activate(
    state: &AppState,
    request: ActivateAgentRequest,
    actor: &str,
) -> Result<Response> {
    let id = canonical_uuid(&request.request_id, "pairing request id")?;
    if request.activation_code.len() > 256
        || request.activation_code.chars().any(char::is_whitespace)
    {
        return Err(Error::Unauthorized);
    }
    match store::activate(
        &state.pool,
        id,
        &crate::token_hash(&request.activation_code),
        actor,
    )
    .await
    .map_err(database)?
    {
        store::ActivateResult::Active(instance) => {
            let mut response = Json(ActivateAgentResponse {
                instance_id: instance.to_string(),
                status: ActivatePairingStatus::Active,
            })
            .into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            Ok(response)
        }
        store::ActivateResult::NotFound => Err(Error::NotFound("pairing request not found".into())),
        store::ActivateResult::InvalidCode => Err(Error::Unauthorized),
        store::ActivateResult::Expired => Err(Error::BadRequest(
            "pairing request or activation code expired".into(),
        )),
        store::ActivateResult::Conflict => Err(Error::Conflict(
            "activation code or pairing request already used".into(),
        )),
    }
}

fn activation_url(request_id: uuid::Uuid) -> String {
    format!("/modules/host-monitoring/activate/{request_id}")
}

async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(report): Json<AgentReport>,
) -> Result<Response> {
    let credential = authorization(&headers, "bearer").ok_or(Error::Unauthorized)?;
    let credential_hash = crate::token_hash(credential);
    let host = store::host_for_token(&state.pool, &credential_hash)
        .await
        .map_err(database)?
        .ok_or(Error::Unauthorized)?;
    if host.to_string() != report.host.id {
        return Err(Error::Unauthorized);
    }
    let metrics = validate_report(&report)?;
    let mut buckets = state.report_buckets.lock().await;
    let allowed = buckets
        .entry(report.host.id.clone())
        .or_insert(TokenBucket {
            tokens: 64.0,
            updated: Instant::now(),
        })
        .allow();
    drop(buckets);
    if !allowed {
        return Err(Error::TooManyRequests("agent report rate exceeded".into()));
    }
    let result = store::store_report(&state.pool, &report, &credential_hash, &metrics).await;
    let (accepted, received_at) = match result {
        Ok(value) => value,
        Err(error)
            if error
                .downcast_ref::<store::ReportStoreError>()
                .is_some_and(|e| matches!(e, store::ReportStoreError::Unauthorized)) =>
        {
            return Err(Error::Unauthorized);
        }
        Err(error)
            if error
                .downcast_ref::<store::ReportStoreError>()
                .is_some_and(|e| matches!(e, store::ReportStoreError::ReportIdConflict)) =>
        {
            return Err(Error::Conflict(error.to_string()));
        }
        Err(error) => return Err(database(error)),
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(AgentReportAck {
            host_id: report.host.id,
            report_id: report.report_id,
            accepted,
            received_at,
        }),
    )
        .into_response())
}

async fn list_hosts(
    State(state): State<AppState>,
    Query(query): Query<HostListQuery>,
) -> Result<Json<HostListResponse>> {
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let offset = query.offset.unwrap_or(0).max(0);
    let (hosts, total) = store::list_hosts(&state.pool, limit, offset)
        .await
        .map_err(database)?;
    Ok(Json(HostListResponse {
        hosts,
        total,
        limit,
        offset,
    }))
}

async fn host_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<HostDetailResponse>> {
    let id = canonical_uuid(&id, "host id")?;
    let (host, latest) = store::get_host(&state.pool, id)
        .await
        .map_err(database)?
        .ok_or_else(|| Error::NotFound("monitored host not found".into()))?;
    Ok(Json(HostDetailResponse { host, latest }))
}

async fn host_history(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryResponse>> {
    let id = canonical_uuid(&id, "host id")?;
    if query.from.zip(query.to).is_some_and(|(from, to)| from > to) {
        return Err(Error::BadRequest(
            "history from must not be after to".into(),
        ));
    }
    let points = store::history(
        &state.pool,
        id,
        query.from,
        query.to,
        query.limit.unwrap_or(300).clamp(1, 1000),
    )
    .await
    .map_err(database)?
    .ok_or_else(|| Error::NotFound("monitored host not found".into()))?;
    Ok(Json(HistoryResponse {
        host_id: id.to_string(),
        points,
    }))
}

async fn update_remark(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(request): Json<UpdateMonitoringRemarkRequest>,
) -> Result<StatusCode> {
    let id = canonical_uuid(&id, "host id")?;
    let remark = request.validated()?;
    if store::update_remark(&state.pool, id, &remark, &principal.subject)
        .await
        .map_err(database)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Error::NotFound("monitored host not found".into()))
    }
}

async fn delete_host(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let id = canonical_uuid(&id, "host id")?;
    if store::delete_host(&state.pool, id, &principal.subject)
        .await
        .map_err(database)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Error::NotFound("monitored host not found".into()))
    }
}

fn authorization<'a>(headers: &'a HeaderMap, expected_scheme: &str) -> Option<&'a str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, value) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case(expected_scheme) && !value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    fn app() -> Router {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://localhost/unused")
            .unwrap();
        let gateway = sarmg_platform_gateway::GatewayIdentity::new(
            auth::MODULE_PROTOCOL,
            auth::MODULE_AUDIENCE,
            "ab".repeat(32),
            auth::MODULE_PREFIX,
            auth::MODULE_AUDIENCE,
            auth::MODULE_PREFIX,
        )
        .unwrap();
        router(AppState::new(pool, gateway))
    }

    fn gateway(request: axum::http::request::Builder) -> axum::http::request::Builder {
        request
            .header(auth::MODULE_PROTOCOL_HEADER, auth::MODULE_PROTOCOL)
            .header(auth::MODULE_AUDIENCE_HEADER, auth::MODULE_AUDIENCE)
            .header(auth::MODULE_TOKEN_HEADER, "ab".repeat(32))
            .header(auth::FORWARDED_PREFIX_HEADER, auth::MODULE_PREFIX)
    }

    fn console_gateway(request: axum::http::request::Builder) -> axum::http::request::Builder {
        gateway(request).header(auth::PRINCIPAL_HEADER, "operator")
    }

    #[test]
    fn pairing_activation_url_targets_the_dynamic_host_module() {
        let request_id = uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        assert_eq!(
            activation_url(request_id),
            "/modules/host-monitoring/activate/00000000-0000-4000-8000-000000000001"
        );
    }

    #[tokio::test]
    async fn health_and_module_routes_require_the_same_gateway_contract() {
        assert_eq!(
            app()
                .oneshot(Request::get("/health/live").body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app()
                .oneshot(
                    Request::get("/api/monitoring/hosts")
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app()
                .oneshot(
                    Request::post("/api/agent/v2/pairing-requests")
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let response = app()
            .oneshot(
                gateway(Request::get("/health/live"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[auth::MODULE_PROTOCOL_HEADER],
            auth::MODULE_PROTOCOL
        );
        assert_eq!(
            response.headers()[auth::MODULE_AUDIENCE_HEADER],
            auth::MODULE_AUDIENCE
        );
    }

    #[tokio::test]
    async fn audience_confusion_and_cookie_forwarding_fail_before_database() {
        let request = Request::get("/api/monitoring/hosts")
            .header(auth::MODULE_PROTOCOL_HEADER, auth::MODULE_PROTOCOL)
            .header(auth::MODULE_AUDIENCE_HEADER, "photo-backup")
            .header(auth::MODULE_TOKEN_HEADER, "ab".repeat(32))
            .header(auth::FORWARDED_PREFIX_HEADER, auth::MODULE_PREFIX)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let request = gateway(Request::get("/api/monitoring/hosts"))
            .header(header::COOKIE, "union=secret")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app().oneshot(request).await.unwrap().status(),
            StatusCode::MISDIRECTED_REQUEST
        );
    }

    #[tokio::test]
    async fn admin_activation_requires_a_principal_but_capability_activation_does_not() {
        let request = console_gateway(Request::post("/api/agent/v2/activate-admin"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(
            app().oneshot(request).await.unwrap().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );

        let request = gateway(Request::post("/api/agent/v2/activate-admin"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(
            app().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );

        let request = gateway(Request::post("/api/agent/v2/activate"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(
            app().oneshot(request).await.unwrap().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
