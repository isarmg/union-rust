//! UnionC 控制台配置模型。

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
    pub platform: PlatformSettings,
}

/// Product-neutral platform settings placeholder.
///
/// Module configuration is persisted separately by the runtime registry. Package locations,
/// executables and gateway routes come only from Builder-bundled, validated Manifests rather than
/// arbitrary upstream URLs in the Core configuration.
#[derive(Clone, Default)]
pub struct PlatformSettings;

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

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".to_string(),
            port: 8081,
            proxy_secret: String::new(),
        }
    }
}

mod layout;
mod runtime;

pub(crate) use layout::{LayoutIntent, ensure_layout};
pub use runtime::{
    RetentionSettings, RuntimeEnvironment, RuntimeMode, load_local_config, save_local_config,
};
