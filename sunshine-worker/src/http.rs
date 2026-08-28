use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::RwLock;

use crate::{
    InternalAuth, InternalIdentity,
    auth::{AUDIENCE_HEADER, PREFIX_HEADER, PRINCIPAL_HEADER, PROTOCOL_HEADER, TOKEN_HEADER},
    client::UpstreamClient,
    crypto::SecretBox,
    db,
    error::{AppError, AppResult},
    model::{
        ClientUpdateRequest, CoverUploadRequest, HealthSnapshot, Host, HostInfo, HostPatchRequest,
        HostSaveRequest, HostStatus, PinRequest, ProbeStatus, UnpairRequest, web_url,
    },
};

#[derive(Clone)]
pub struct WorkerState {
    pub pool: PgPool,
    pub secrets: SecretBox,
    pub auth: InternalAuth,
    pub production: bool,
    upstream: UpstreamClient,
    health: Arc<RwLock<HashMap<String, HealthSnapshot>>>,
}

impl WorkerState {
    pub fn new(
        pool: PgPool,
        secrets: SecretBox,
        auth: InternalAuth,
        production: bool,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            pool,
            secrets,
            auth,
            production,
            upstream: UpstreamClient::new()?,
            health: Arc::new(RwLock::new(HashMap::new())),
        })
    }
}

pub fn router(state: WorkerState) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route(
            "/api/services/sunshine/hosts",
            get(list_hosts).post(create_host),
        )
        .route(
            "/api/services/sunshine/hosts/{id}",
            patch(update_host).delete(delete_host),
        )
        .route("/api/services/sunshine/hosts/{id}/status", get(status))
        .route(
            "/api/services/sunshine/hosts/{id}/apps",
            get(apps_list).post(apps_save),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/apps/close",
            post(apps_close),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/apps/{index}",
            delete(apps_delete),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/clients",
            get(clients_list),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/clients/unpair",
            post(clients_unpair),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/clients/unpair-all",
            post(clients_unpair_all),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/clients/update",
            post(clients_update),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/config",
            get(config_get).post(config_save),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/config/locale",
            get(config_locale),
        )
        .route("/api/services/sunshine/hosts/{id}/api-logs", get(logs))
        .route("/api/services/sunshine/hosts/{id}/pin", post(pin))
        .route("/api/services/sunshine/hosts/{id}/restart", post(restart))
        .route(
            "/api/services/sunshine/hosts/{id}/reset-display",
            post(reset_display),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/covers/{index}",
            get(cover),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/covers/upload",
            post(cover_upload),
        )
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}

async fn authenticate(
    State(state): State<WorkerState>,
    mut request: Request,
    next: Next,
) -> AppResult<Response> {
    // Union must consume its browser session and strip it at the gateway. A
    // Cookie reaching this process indicates a broken trust boundary.
    if request.headers().contains_key(header::COOKIE) {
        return Err(AppError::Unauthorized);
    }
    if !state.auth.validates(request.headers()) {
        return Err(AppError::GatewayRequired);
    }
    if !request.uri().path().starts_with("/health/") {
        let subject = crate::auth::parse_principal(request.headers())
            .map_err(|_| AppError::Unauthorized)?
            .to_owned();
        request
            .extensions_mut()
            .insert(InternalIdentity { subject });
    }
    // The business handlers and upstream Sunshine client never need Union's
    // proof or browser-facing credentials after this trust transition.
    for name in [
        PROTOCOL_HEADER,
        AUDIENCE_HEADER,
        TOKEN_HEADER,
        PREFIX_HEADER,
        PRINCIPAL_HEADER,
    ] {
        request.headers_mut().remove(name);
    }
    request.headers_mut().remove(header::AUTHORIZATION);
    Ok(next.run(request).await)
}

async fn live(State(state): State<WorkerState>) -> Response {
    with_module_identity(
        &state,
        Json(serde_json::json!({ "status": "ok" })).into_response(),
    )
}

async fn ready(State(state): State<WorkerState>) -> Response {
    let ready = db::ready(&state.pool).await;
    let response = if ready {
        (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ready" })),
        )
            .into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "status": "not-ready" })),
        )
            .into_response()
    };
    with_module_identity(&state, response)
}

fn with_module_identity(state: &WorkerState, mut response: Response) -> Response {
    state.auth.apply_health_headers(response.headers_mut());
    response
}

async fn list_hosts(State(state): State<WorkerState>) -> AppResult<Json<Vec<HostInfo>>> {
    let hosts = db::list_hosts(&state.pool, &state.secrets).await?;
    let health = state.health.read().await;
    Ok(Json(
        hosts
            .iter()
            .map(|host| host_info(host, health.get(&host.id)))
            .collect(),
    ))
}

async fn create_host(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Json(request): Json<HostSaveRequest>,
) -> AppResult<(StatusCode, Json<HostInfo>)> {
    let worker = state.clone();
    let host = finish(async move {
        db::insert_host(
            &worker.pool,
            &worker.secrets,
            request,
            worker.production,
            &identity.subject,
        )
        .await
    })
    .await?;
    state
        .health
        .write()
        .await
        .insert(host.id.clone(), HealthSnapshot::default());
    Ok((StatusCode::CREATED, Json(host_info(&host, None))))
}

async fn update_host(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path(id): Path<String>,
    Json(request): Json<HostPatchRequest>,
) -> AppResult<Json<HostInfo>> {
    let worker = state.clone();
    let host = finish(async move {
        db::update_host(
            &worker.pool,
            &worker.secrets,
            &id,
            request,
            worker.production,
            &identity.subject,
        )
        .await
    })
    .await?;
    state
        .health
        .write()
        .await
        .insert(host.id.clone(), HealthSnapshot::default());
    Ok(Json(host_info(&host, None)))
}

async fn delete_host(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let worker = state.clone();
    let deleted_id = id.clone();
    finish(async move { db::delete_host(&worker.pool, &id, &identity.subject).await }).await?;
    state.health.write().await.remove(&deleted_id);
    Ok(StatusCode::NO_CONTENT)
}

async fn status(
    State(state): State<WorkerState>,
    Path(id): Path<String>,
) -> AppResult<Json<HostStatus>> {
    let host = load_host(&state, &id).await?;
    let health = state.health.read().await.get(&id).cloned();
    let reachable = health
        .as_ref()
        .and_then(|item| item.reachable)
        .unwrap_or(false);
    Ok(Json(HostStatus {
        host: host.host.clone(),
        web_port: host.web_port,
        web_url: web_url(&host),
        reachable,
        message: match health.and_then(|item| item.reachable) {
            Some(true) => "Sunshine Web UI port is reachable".into(),
            Some(false) => "Sunshine Web UI port is not reachable".into(),
            None => "Sunshine Web UI reachability check is pending".into(),
        },
    }))
}

async fn apps_list(
    State(state): State<WorkerState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(
        state
            .upstream
            .apps_list(&load_host(&state, &id).await?)
            .await?,
    ))
}

async fn apps_save(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> AppResult<Json<Value>> {
    validate_object(&body, 256 * 1024)?;
    let detail = body
        .get("name")
        .and_then(Value::as_str)
        .map(|name| format!("name={}", name.trim()));
    mutate(
        state,
        identity,
        id,
        "sunshine.app.save",
        detail,
        move |client, host| async move { client.apps_save(&host, &body).await },
    )
    .await
}

async fn apps_close(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    mutate(
        state,
        identity,
        id,
        "sunshine.app.close",
        None,
        |client, host| async move { client.apps_close(&host).await },
    )
    .await
}

async fn apps_delete(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path((id, index)): Path<(String, u32)>,
) -> AppResult<Json<Value>> {
    validate_index(index)?;
    mutate(
        state,
        identity,
        id,
        "sunshine.app.delete",
        Some(format!("index={index}")),
        move |client, host| async move { client.apps_delete(&host, index).await },
    )
    .await
}

async fn clients_list(
    State(state): State<WorkerState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(
        state
            .upstream
            .clients_list(&load_host(&state, &id).await?)
            .await?,
    ))
}

async fn clients_unpair(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path(id): Path<String>,
    Json(body): Json<UnpairRequest>,
) -> AppResult<Json<Value>> {
    let uuid = validate_opaque("client uuid", &body.uuid, 128)?.to_string();
    mutate(
        state,
        identity,
        id,
        "sunshine.client.unpair",
        Some(format!("client={uuid}")),
        move |client, host| async move { client.clients_unpair(&host, &uuid).await },
    )
    .await
}

async fn clients_unpair_all(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    mutate(
        state,
        identity,
        id,
        "sunshine.client.unpair_all",
        None,
        |client, host| async move { client.clients_unpair_all(&host).await },
    )
    .await
}

async fn clients_update(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path(id): Path<String>,
    Json(body): Json<ClientUpdateRequest>,
) -> AppResult<Json<Value>> {
    let uuid = validate_opaque("client uuid", &body.uuid, 128)?.to_string();
    let enabled = body.enabled;
    mutate(
        state,
        identity,
        id,
        "sunshine.client.update",
        Some(format!("client={uuid} enabled={enabled}")),
        move |client, host| async move { client.clients_update(&host, &uuid, enabled).await },
    )
    .await
}

async fn config_get(
    State(state): State<WorkerState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(
        state
            .upstream
            .config_get(&load_host(&state, &id).await?)
            .await?,
    ))
}

async fn config_save(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> AppResult<Json<Value>> {
    validate_object(&body, 1024 * 1024)?;
    mutate(
        state,
        identity,
        id,
        "sunshine.config.save",
        Some("config updated".into()),
        move |client, host| async move { client.config_save(&host, &body).await },
    )
    .await
}

async fn config_locale(
    State(state): State<WorkerState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(
        state
            .upstream
            .config_locale(&load_host(&state, &id).await?)
            .await?,
    ))
}

async fn logs(State(state): State<WorkerState>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    Ok(Json(
        state.upstream.logs(&load_host(&state, &id).await?).await?,
    ))
}

async fn pin(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path(id): Path<String>,
    Json(body): Json<PinRequest>,
) -> AppResult<Json<Value>> {
    let pin = body.pin.trim().to_string();
    if !(4..=8).contains(&pin.len()) || !pin.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AppError::BadRequest("PIN must contain 4-8 digits".into()));
    }
    let name = validate_opaque("client name", &body.name, 80)?.to_string();
    mutate(
        state,
        identity,
        id,
        "sunshine.client.pair",
        Some(format!("name={name}")),
        move |client, host| async move { client.pin(&host, &pin, &name).await },
    )
    .await
}

async fn restart(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    mutate(
        state,
        identity,
        id,
        "sunshine.system.restart",
        None,
        |client, host| async move { client.restart(&host).await },
    )
    .await
}

async fn reset_display(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    mutate(
        state,
        identity,
        id,
        "sunshine.display.reset",
        None,
        |client, host| async move { client.reset_display(&host).await },
    )
    .await
}

async fn cover(
    State(state): State<WorkerState>,
    Path((id, index)): Path<(String, u32)>,
) -> AppResult<Response> {
    validate_index(index)?;
    let (upstream_type, bytes) = state
        .upstream
        .cover(&load_host(&state, &id).await?, index)
        .await?;
    let mut response = bytes.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, safe_cover_type(&upstream_type));
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    Ok(response)
}

async fn cover_upload(
    State(state): State<WorkerState>,
    Extension(identity): Extension<InternalIdentity>,
    Path(id): Path<String>,
    Json(body): Json<CoverUploadRequest>,
) -> AppResult<Json<Value>> {
    let key = validate_opaque("cover key", &body.key, 512)?.to_string();
    let url = reqwest::Url::parse(body.url.trim())
        .map_err(|_| AppError::BadRequest("cover URL must be absolute".into()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(AppError::BadRequest(
            "cover URL must use HTTP or HTTPS".into(),
        ));
    }
    let url = url.to_string();
    mutate(
        state,
        identity,
        id,
        "sunshine.cover.upload",
        Some(format!("key={key}")),
        move |client, host| async move { client.cover_upload(&host, &key, &url).await },
    )
    .await
}

async fn mutate<F, Fut>(
    state: WorkerState,
    identity: InternalIdentity,
    id: String,
    action: &'static str,
    detail: Option<String>,
    operation: F,
) -> AppResult<Json<Value>>
where
    F: FnOnce(UpstreamClient, Host) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = AppResult<Value>> + Send + 'static,
{
    let host = load_host(&state, &id).await?;
    let pool = state.pool.clone();
    let actor = identity.subject;
    let client = state.upstream.clone();
    let target = id;
    let value = finish(async move {
        let value = operation(client, host).await?;
        db::audit_best_effort(&pool, action, &target, &actor, detail.as_deref()).await;
        Ok(value)
    })
    .await?;
    Ok(Json(value))
}

async fn finish<T: Send + 'static>(
    future: impl std::future::Future<Output = AppResult<T>> + Send + 'static,
) -> AppResult<T> {
    tokio::spawn(future)
        .await
        .map_err(|error| AppError::Internal(anyhow::anyhow!("detached mutation failed: {error}")))?
}

async fn load_host(state: &WorkerState, id: &str) -> AppResult<Host> {
    let host = db::get_host(&state.pool, &state.secrets, id).await?;
    if state.production && !host.verify_tls {
        return Err(AppError::BadRequest(
            "this host must enable TLS verification before use".into(),
        ));
    }
    Ok(host)
}

fn host_info(host: &Host, health: Option<&HealthSnapshot>) -> HostInfo {
    let health = health.cloned().unwrap_or_default();
    let complete = health.reachable.is_some() && health.connected.is_some();
    HostInfo {
        id: host.id.clone(),
        name: host.name.clone(),
        host: host.host.clone(),
        web_port: host.web_port,
        username: host.username.clone(),
        password_set: !host.password.is_empty(),
        verify_tls: host.verify_tls,
        web_url: web_url(host),
        probe_status: if complete {
            ProbeStatus::Complete
        } else {
            ProbeStatus::Pending
        },
        reachable: health.reachable,
        connected: health.connected,
        connection_error: health.connection_error,
    }
}

fn validate_object(value: &Value, limit: usize) -> AppResult<()> {
    if !value.is_object()
        || serde_json::to_vec(value)
            .map_err(|error| AppError::BadRequest(error.to_string()))?
            .len()
            > limit
    {
        return Err(AppError::BadRequest(
            "payload must be a JSON object within its size limit".into(),
        ));
    }
    Ok(())
}

fn validate_opaque<'a>(label: &str, value: &'a str, limit: usize) -> AppResult<&'a str> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > limit || value.chars().any(char::is_control) {
        return Err(AppError::BadRequest(format!("invalid {label}")));
    }
    Ok(value)
}

fn validate_index(index: u32) -> AppResult<()> {
    if index > 10_000 {
        Err(AppError::BadRequest(
            "Sunshine app index is out of range".into(),
        ))
    } else {
        Ok(())
    }
}

fn safe_cover_type(value: &str) -> HeaderValue {
    match value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/jpeg" => HeaderValue::from_static("image/jpeg"),
        "image/png" => HeaderValue::from_static("image/png"),
        "image/webp" => HeaderValue::from_static("image/webp"),
        "image/gif" => HeaderValue::from_static("image/gif"),
        "image/avif" => HeaderValue::from_static("image/avif"),
        _ => HeaderValue::from_static("application/octet-stream"),
    }
}

/// Run the process-owned health probe. A stale result is published only when
/// the complete host row is still current after the network round trip.
pub async fn probe_once(state: &WorkerState) -> AppResult<()> {
    let hosts = db::list_hosts(&state.pool, &state.secrets).await?;
    for host in hosts {
        let reachable = state.upstream.check_reachable(&host).await;
        let connection = if reachable {
            state
                .upstream
                .apps_list(&host)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        } else {
            Err("Sunshine Web port is not reachable".to_string())
        };
        let current = db::get_host(&state.pool, &state.secrets, &host.id).await;
        if current.as_ref().is_ok_and(|current| current == &host) {
            state.health.write().await.insert(
                host.id.clone(),
                HealthSnapshot {
                    reachable: Some(reachable),
                    connected: Some(reachable && connection.is_ok()),
                    connection_error: connection.err(),
                },
            );
        }
    }
    Ok(())
}

pub async fn probe_loop(state: WorkerState) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if let Err(error) = probe_once(&state).await {
            tracing::warn!(%error, "Sunshine health probe failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AUDIENCE, PREFIX, PROTOCOL};
    use axum::body::Body;
    use http_body_util::BodyExt;
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    fn test_router() -> Router {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgresql://localhost/unused")
            .unwrap();
        let state = WorkerState::new(
            pool,
            SecretBox::new("test", [2; 32]).unwrap(),
            InternalAuth::new(PROTOCOL, AUDIENCE, "a".repeat(64), PREFIX, AUDIENCE, PREFIX)
                .unwrap(),
            true,
        )
        .unwrap();
        router(state)
    }

    #[tokio::test]
    async fn all_routes_including_health_require_the_gateway_contract() {
        let live = test_router()
            .oneshot(Request::get("/health/live").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(live.status(), StatusCode::UNAUTHORIZED);
        let private = test_router()
            .oneshot(
                Request::get("/api/services/sunshine/hosts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(private.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn valid_gateway_proof_still_rejects_union_cookie() {
        let path = "/does-not-exist";
        let response = test_router()
            .oneshot(
                Request::get(path)
                    .header(PROTOCOL_HEADER, PROTOCOL)
                    .header(AUDIENCE_HEADER, AUDIENCE)
                    .header(TOKEN_HEADER, "a".repeat(64))
                    .header(PREFIX_HEADER, PREFIX)
                    .header(header::COOKIE, "union_session=browser")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("unauthorized"));
    }

    #[tokio::test]
    async fn authenticated_health_echoes_protocol_and_audience() {
        let response = test_router()
            .oneshot(
                Request::get("/health/live")
                    .header(PROTOCOL_HEADER, PROTOCOL)
                    .header(AUDIENCE_HEADER, AUDIENCE)
                    .header(TOKEN_HEADER, "a".repeat(64))
                    .header(PREFIX_HEADER, PREFIX)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[PROTOCOL_HEADER], PROTOCOL);
        assert_eq!(response.headers()[AUDIENCE_HEADER], AUDIENCE);
    }

    #[tokio::test]
    async fn platform_routes_require_the_canonical_operator_principal() {
        let without_principal = test_router()
            .oneshot(
                Request::get("/does-not-exist")
                    .header(PROTOCOL_HEADER, PROTOCOL)
                    .header(AUDIENCE_HEADER, AUDIENCE)
                    .header(TOKEN_HEADER, "a".repeat(64))
                    .header(PREFIX_HEADER, PREFIX)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(without_principal.status(), StatusCode::UNAUTHORIZED);

        let mut request = Request::get("/does-not-exist")
            .header(PROTOCOL_HEADER, PROTOCOL)
            .header(AUDIENCE_HEADER, AUDIENCE)
            .header(TOKEN_HEADER, "a".repeat(64))
            .header(PREFIX_HEADER, PREFIX)
            .body(Body::empty())
            .unwrap();
        sarmg_platform_gateway::insert_principal(request.headers_mut(), "管理员").unwrap();
        assert_eq!(
            test_router().oneshot(request).await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
    }
}
