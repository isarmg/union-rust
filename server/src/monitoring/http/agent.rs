use axum::{Router, extract::DefaultBodyLimit, routing::post};

use crate::state::AppState;

use super::{
    activate_pairing_request, create_pairing_request, pairing_request_status,
    public_pairing_request, report_metrics,
};

pub(super) fn router() -> Router<AppState> {
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
            axum::routing::get(public_pairing_request),
        )
        .route(
            "/api/agent/v2/pairing-requests/{request_id}/status",
            post(pairing_request_status),
        )
        .route("/api/agent/v2/activate", post(activate_pairing_request))
        .layer(DefaultBodyLimit::max(16 * 1024));
    reporting.merge(pairing)
}
