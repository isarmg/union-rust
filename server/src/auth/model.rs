use serde::{Deserialize, Serialize};

/// Limits shared by every administrator credential entry point.
pub const MAX_USERNAME_BYTES: usize = 128;
pub const MAX_BCRYPT_INPUT_BYTES: usize = 72;

/// 账号密码登录请求。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// 登录成功响应。
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub username: String,
}

#[derive(Debug, Serialize)]
pub struct UserInfoResponse {
    pub username: String,
}

/// 有效期 60 秒、仅用于 SSE 连接认证的短效票据。
#[derive(Serialize)]
pub struct SseTicketResponse {
    pub ticket: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}
