use axum::http::{HeaderMap, header};

use crate::error::{Error, Result};

pub const MODULE_AUDIENCE: &str = "host-monitoring";
pub const MODULE_PREFIX: &str = "/modules/host-monitoring";
pub use sarmg_platform_gateway::{
    AUDIENCE_HEADER as MODULE_AUDIENCE_HEADER, PREFIX_HEADER as FORWARDED_PREFIX_HEADER,
    PROTOCOL as MODULE_PROTOCOL, PROTOCOL_HEADER as MODULE_PROTOCOL_HEADER,
    TOKEN_HEADER as MODULE_TOKEN_HEADER,
};

#[derive(Debug, Clone)]
pub struct Principal {
    pub subject: String,
}

pub fn require(headers: &HeaderMap, state: &crate::http::AppState) -> Result<Principal> {
    if !state.gateway.validates(headers) {
        return Err(Error::Unauthorized);
    }
    Ok(Principal {
        subject: "union-gateway".into(),
    })
}

pub fn require_console(headers: &HeaderMap, state: &crate::http::AppState) -> Result<Principal> {
    if headers.contains_key(header::COOKIE) {
        return Err(Error::GatewayRequired(
            "Union cookies must be stripped before forwarding to host-monitoring".into(),
        ));
    }
    require(headers, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn shared_gateway_comparison_is_exact() {
        let token = "ab".repeat(32);
        let identity = sarmg_platform_gateway::GatewayIdentity::new(
            MODULE_PROTOCOL,
            MODULE_AUDIENCE,
            &token,
            MODULE_PREFIX,
            MODULE_AUDIENCE,
            MODULE_PREFIX,
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            MODULE_PROTOCOL_HEADER,
            HeaderValue::from_static(MODULE_PROTOCOL),
        );
        headers.insert(
            MODULE_AUDIENCE_HEADER,
            HeaderValue::from_static(MODULE_AUDIENCE),
        );
        headers.insert(MODULE_TOKEN_HEADER, HeaderValue::from_str(&token).unwrap());
        headers.insert(
            FORWARDED_PREFIX_HEADER,
            HeaderValue::from_static(MODULE_PREFIX),
        );
        assert!(identity.validates(&headers));
        headers.insert(
            MODULE_AUDIENCE_HEADER,
            HeaderValue::from_static("photo-backup"),
        );
        assert!(!identity.validates(&headers));
    }
}
