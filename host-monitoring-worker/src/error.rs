use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("{0}")]
    GatewayRequired(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    UnsupportedMediaType(String),
    #[error("{0}")]
    TooManyRequests(String),
    #[error("database is unavailable")]
    Database(#[source] anyhow::Error),
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    message: &'a str,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::GatewayRequired(_) => StatusCode::MISDIRECTED_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::UnsupportedMediaType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::Database(_) => StatusCode::SERVICE_UNAVAILABLE,
        };
        let message = self.to_string();
        (status, Json(ErrorBody { message: &message })).into_response()
    }
}

pub fn database(error: impl Into<anyhow::Error>) -> Error {
    let error = error.into();
    tracing::warn!(%error, "host-monitoring PostgreSQL operation failed");
    Error::Database(error)
}
