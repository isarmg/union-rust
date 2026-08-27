use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("request must pass through the Union gateway")]
    GatewayRequired,
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Upstream(String),
    #[error("database unavailable")]
    Database(#[source] sqlx::Error),
    #[error("cryptographic operation failed")]
    Crypto,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized | Self::GatewayRequired => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::Database(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Crypto | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::GatewayRequired => "union_gateway_required",
            Self::Forbidden(_) => "forbidden",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Upstream(_) => "upstream_error",
            Self::Database(_) => "database_unavailable",
            Self::Crypto => "crypto_error",
            Self::Internal(_) => "internal_error",
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(value: sqlx::Error) -> Self {
        Self::Database(value)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message = match &self {
            Self::BadRequest(message)
            | Self::Forbidden(message)
            | Self::NotFound(message)
            | Self::Conflict(message)
            | Self::Upstream(message) => message.clone(),
            Self::Unauthorized => "unauthorized".to_string(),
            Self::GatewayRequired => "request must pass through the Union gateway".to_string(),
            Self::Database(_) => "database unavailable".to_string(),
            Self::Crypto | Self::Internal(_) => "internal error".to_string(),
        };
        if status.is_server_error() {
            tracing::error!(error = %self, "Sunshine worker request failed");
        }
        (
            status,
            Json(ErrorBody {
                code: self.code(),
                message,
            }),
        )
            .into_response()
    }
}
