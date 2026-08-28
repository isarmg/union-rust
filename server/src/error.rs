//! 统一错误类型。
//!
//! Axum 的 handler 可以返回 `Result<T, AppError>`。当出现错误时，`IntoResponse`
//! 会把错误转换成统一 JSON 响应，前端就能稳定读取 `message` 字段。

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

use crate::config::LocalConfigError;
use crate::system::ErrorResponse;

/// 项目内 handler 常用的结果类型别名。
pub type AppResult<T> = Result<T, AppError>;

/// 应用错误分类。
#[derive(Debug, Error)]
pub enum AppError {
    /// 请求参数不合法，返回 400。
    #[error("{0}")]
    BadRequest(String),
    /// 请求体媒体类型不符合端点契约，返回 415。
    #[error("{0}")]
    UnsupportedMediaType(String),
    /// 本地管理员配置校验失败，返回可细分机器码。
    #[error(transparent)]
    LocalConfig(#[from] LocalConfigError),
    /// 认证失败，返回 401。
    #[error("unauthorized")]
    Unauthorized,
    /// 已认证但请求缺少必要的安全证明，返回 403。
    #[error("{0}")]
    Forbidden(String),
    /// 请求没有经过预期的反向代理链路（缺少 `X-Forwarded-Proto` / `X-Forwarded-For`），
    /// 返回 421 Misdirected Request。
    ///
    /// # 为什么不复用 403
    ///
    /// 这不是"凭据不对"，而是"请求走错了路"——凭据可能完全有效。一次反向代理漏透传
    /// 请求头属于可恢复的部署配置失误，不应伪装成身份认证错误。
    /// 421 的语义正是"这台服务器不该接收这个请求"，且它天然属于可重试类——
    /// 运维修好反代之后，同一份报文原样重发即可成功。
    #[error("{0}")]
    MisdirectedRequest(String),
    /// 请求的资源不存在，返回 404。
    #[error("{0}")]
    NotFound(String),
    /// 曾经存在但已经过期或不可再使用，返回 410。
    #[error("{0}")]
    Gone(String),
    /// 当前状态冲突，例如重复启动服务，返回 409。
    #[error("{0}")]
    Conflict(String),
    /// 请求过于频繁，返回 429。
    #[error("{0}")]
    TooManyRequests(String),
    /// 请求体未在服务器规定的总时限内传完，返回 408。
    #[error("{0}")]
    RequestTimeout(String),
    /// 请求体超过端点允许的最大值，返回 413。
    #[error("{0}")]
    PayloadTooLarge(String),
    /// 本地持久层暂不可用，业务接口暂不可用，返回 503。
    #[error("{0}")]
    ServiceUnavailable(String),
    /// 内嵌 SQLite 的固定文件身份或精确当前 schema 当前不可用。
    #[error("{0}")]
    DatabaseUnavailable(String),
    /// 文件系统错误。
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// 数据库错误。
    #[error(transparent)]
    Sqlx(#[from] sqlx_core::Error),
    /// 其他使用 anyhow 传递的错误。
    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::BadRequest(_) | AppError::LocalConfig(_) => StatusCode::BAD_REQUEST,
            AppError::UnsupportedMediaType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::MisdirectedRequest(_) => StatusCode::MISDIRECTED_REQUEST,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Gone(_) => StatusCode::GONE,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
            AppError::RequestTimeout(_) => StatusCode::REQUEST_TIMEOUT,
            AppError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            AppError::ServiceUnavailable(_) | AppError::DatabaseUnavailable(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            AppError::Io(_) | AppError::Sqlx(_) | AppError::Anyhow(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        // 内部错误记录完整信息用于调试，但对外只返回通用描述，不泄露路径或 SQL。
        let client_message = match &self {
            AppError::BadRequest(msg) => msg.clone(),
            AppError::UnsupportedMediaType(msg) => msg.clone(),
            AppError::LocalConfig(error) => error.to_string(),
            AppError::Unauthorized => "unauthorized".to_string(),
            AppError::Forbidden(msg) => msg.clone(),
            AppError::MisdirectedRequest(msg) => msg.clone(),
            AppError::NotFound(msg) => msg.clone(),
            AppError::Gone(msg) => msg.clone(),
            AppError::Conflict(msg) => msg.clone(),
            AppError::TooManyRequests(msg) => msg.clone(),
            AppError::RequestTimeout(msg) => msg.clone(),
            AppError::PayloadTooLarge(msg) => msg.clone(),
            AppError::ServiceUnavailable(msg) => msg.clone(),
            AppError::DatabaseUnavailable(msg) => msg.clone(),
            AppError::Io(_) => "storage error".to_string(),
            AppError::Sqlx(_) => "database error".to_string(),
            AppError::Anyhow(_) => "internal error".to_string(),
        };

        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!("internal error: {self}");
        }

        let body = Json(ErrorResponse {
            code: self.code().to_string(),
            message: client_message,
        });

        (status, body).into_response()
    }
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::UnsupportedMediaType(_) => "unsupported_media_type",
            Self::LocalConfig(error) => error.code(),
            Self::Unauthorized => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::MisdirectedRequest(_) => "misdirected_request",
            Self::NotFound(_) => "not_found",
            Self::Gone(_) => "gone",
            Self::Conflict(_) => "conflict",
            Self::TooManyRequests(_) => "too_many_requests",
            Self::RequestTimeout(_) => "request_timeout",
            Self::PayloadTooLarge(_) => "payload_too_large",
            Self::ServiceUnavailable(_) => "service_unavailable",
            Self::DatabaseUnavailable(_) => "database_unavailable",
            Self::Io(_) => "storage_error",
            Self::Sqlx(_) => "database_error",
            Self::Anyhow(_) => "internal_error",
        }
    }
}
