//! UnionC 控制台配置模型。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalConfig {
    pub application_version: String,
    pub admin_username: String,
    pub admin_password_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LocalConfigError {
    #[error("local config was created by a different UnionC application version")]
    UnsupportedApplicationVersion,
    #[error("local admin username cannot be empty")]
    EmptyAdminUsername,
    #[error("local admin username contains invalid characters")]
    InvalidAdminUsername,
    #[error("local admin password hash cannot be empty")]
    EmptyAdminPasswordHash,
    #[error("local admin password hash must be a valid bcrypt hash")]
    InvalidAdminPasswordHash,
}

impl LocalConfigError {
    pub fn code(self) -> &'static str {
        match self {
            Self::UnsupportedApplicationVersion => "local_config_application_version_unsupported",
            Self::EmptyAdminUsername => "local_config_admin_username_empty",
            Self::InvalidAdminUsername => "local_config_admin_username_invalid",
            Self::EmptyAdminPasswordHash => "local_config_admin_password_hash_empty",
            Self::InvalidAdminPasswordHash => "local_config_admin_password_hash_invalid",
        }
    }
}

#[derive(Clone, Default)]
pub struct Settings {
    pub production: bool,
    pub server: ServerSettings,
    pub database: DatabaseSettings,
    pub sunshine: SunshineSettings,
    pub platform: PlatformSettings,
}

#[derive(Clone, Default)]
pub struct PlatformSettings {
    /// Browser-visible and server-probeable base URLs, indexed by stable module id.
    /// Values are deployment configuration and are never persisted in a business database.
    pub service_urls: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct ServerSettings {
    pub bind: String,
    pub port: u16,
    /// Shared proof added by the trusted reverse proxy. It is deployment-only
    /// and must never be serialized into application data or responses.
    pub proxy_secret: String,
}

#[derive(Clone, Default)]
pub struct DatabaseSettings {
    pub url: String,
}

#[derive(Clone)]
pub struct SunshineHostConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    pub web_port: u16,
    pub username: String,
    pub password: String,
    pub verify_tls: bool,
}

impl Default for SunshineHostConfig {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Sunshine".to_string(),
            host: "127.0.0.1".to_string(),
            web_port: 47990,
            username: "admin".to_string(),
            password: String::new(),
            verify_tls: true,
        }
    }
}

#[derive(Clone)]
pub struct SunshineSettings {
    pub hosts: Vec<SunshineHostConfig>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".to_string(),
            port: 8081,
            proxy_secret: String::new(),
        }
    }
}

/// 全新安装从**空列表**开始。
///
/// 不 seed 任何示例主机。一台预置的演示主机（如 127.0.0.1:47990 / admin / 空密码）
/// 会出现在主机列表里、被后台任务每 5 秒 TCP 探测一次，且是一条谁都能看见的空密码
/// 配置——正式版不应携带演示数据。
impl Default for SunshineSettings {
    fn default() -> Self {
        Self { hosts: Vec::new() }
    }
}

mod layout;
mod runtime;

pub(crate) use layout::{LayoutIntent, ensure_layout};
pub use runtime::{
    RetentionSettings, RuntimeEnvironment, RuntimeMode, load_local_config, save_local_config,
};
