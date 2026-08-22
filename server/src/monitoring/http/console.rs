use axum::{Router, extract::DefaultBodyLimit, routing::get};

use crate::state::AppState;

use super::{
    cancel_agent_instance, create_agent_instance, host_detail, host_history, list_agent_instances,
    list_hosts, revoke_host,
};

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/monitoring/hosts", get(list_hosts))
        .route("/api/monitoring/hosts/{host_id}", get(host_detail))
        .route("/api/monitoring/hosts/{host_id}/history", get(host_history))
        .route(
            "/api/monitoring/hosts/{host_id}/revoke",
            axum::routing::post(revoke_host),
        )
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
