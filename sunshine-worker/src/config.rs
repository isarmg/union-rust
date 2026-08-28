use std::{net::SocketAddr, str::FromStr};

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::{auth::InternalAuth, crypto::SecretBox};

pub const DEFAULT_BIND: &str = "127.0.0.1:18104";

#[derive(Clone)]
pub struct ServeConfig {
    pub bind: SocketAddr,
    pub database_url: String,
    pub production: bool,
    pub internal_auth: InternalAuth,
    pub secrets: SecretBox,
}

impl ServeConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind = std::env::var("UNION_PLUGIN_BIND")
            .or_else(|_| std::env::var("SUNSHINE_BIND"))
            .unwrap_or_else(|_| DEFAULT_BIND.to_string());
        let bind = SocketAddr::from_str(&bind)
            .context("UNION_PLUGIN_BIND/SUNSHINE_BIND must be a socket address")?;
        if !bind.ip().is_loopback() {
            bail!("plugin bind must be a loopback address; the worker is not a public service");
        }
        let database_url =
            std::env::var("SUNSHINE_DATABASE_URL").context("SUNSHINE_DATABASE_URL is required")?;
        if database_url.trim().is_empty() {
            bail!("SUNSHINE_DATABASE_URL must not be empty");
        }
        let production = parse_bool_env("SUNSHINE_PRODUCTION", true)?;
        let credential_key = decode_key_env("SUNSHINE_CREDENTIAL_KEY")?;
        let credential_key_id =
            std::env::var("SUNSHINE_CREDENTIAL_KEY_ID").unwrap_or_else(|_| "primary".to_string());
        Ok(Self {
            bind,
            database_url,
            production,
            internal_auth: InternalAuth::from_env(crate::auth::AUDIENCE, crate::auth::PREFIX)?,
            secrets: SecretBox::new(credential_key_id, credential_key)?,
        })
    }
}

pub fn decode_key_env(name: &str) -> anyhow::Result<[u8; 32]> {
    let value = std::env::var(name).with_context(|| format!("{name} is required"))?;
    let decoded = STANDARD
        .decode(value.trim())
        .with_context(|| format!("{name} must be base64"))?;
    decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("{name} must decode to exactly 32 bytes"))
}

fn parse_bool_env(name: &str, default: bool) -> anyhow::Result<bool> {
    match std::env::var(name) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Ok(true),
            "0" | "false" | "no" => Ok(false),
            _ => bail!("{name} must be true or false"),
        },
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AUDIENCE, PREFIX, PROTOCOL};

    #[test]
    fn default_bind_is_the_reserved_loopback_endpoint() {
        assert_eq!(DEFAULT_BIND, "127.0.0.1:18104");
        let parsed: SocketAddr = DEFAULT_BIND.parse().unwrap();
        assert!(parsed.ip().is_loopback());
    }

    #[test]
    fn compiled_gateway_identity_is_fixed() {
        assert_eq!(PROTOCOL, "gateway-v1");
        assert_eq!(AUDIENCE, "sunshine");
        assert_eq!(PREFIX, "/api/modules/sunshine");
    }
}
