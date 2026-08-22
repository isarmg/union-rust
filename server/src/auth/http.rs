//! 认证与账号管理 handler。
//!
//! # 认证流程
//!
//! 管理台使用 JSON 登录，验证成功后通过 HttpOnly Cookie 建立会话。
//!
//! Token 是随机 UUID，仅保存在进程内存；有效期 7 天，重启或改密后失效。

use crate::auth::{
    ChangePasswordRequest, LoginRequest, LoginResponse, MAX_BCRYPT_INPUT_BYTES, MAX_USERNAME_BYTES,
    UserInfoResponse,
};
use crate::{
    config::save_local_config,
    error::{AppError, AppResult},
    infra::database,
    state::{AppState, LocalSession, LoginAttemptState, SseSessionCancellation},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};

const LOGIN_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_LOGIN_ATTEMPTS: usize = 5;
const MAX_LOGIN_ATTEMPTS_PER_IP: usize = 10;
/// 全局桶只作最后兜底（防 bcrypt 资源耗尽），阈值必须显著高于单 IP 上限，
/// 否则它本身就成了"任何人都能触发的管理员锁定"开关。真正的防刷靠按 IP 分桶。
const MAX_GLOBAL_LOGIN_ATTEMPTS: usize = 600;
const AUTH_JSON_BODY_LIMIT: usize = 4 * 1024;
const TRUSTED_PROXY_HEADER: &str = "x-unionc-proxy-secret";

// Authentication shares private session and rate-limit state. Keep one module
// boundary while separating the implementation into reviewable concerns.
include!("http/routes.rs");

include!("http/rate_limit.rs");
include!("http/password.rs");

include!("http/sessions.rs");

include!("http/tests.rs");
