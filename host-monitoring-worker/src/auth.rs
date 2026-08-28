use axum::http::{HeaderMap, header};

use crate::error::{Error, Result};

pub const MODULE_AUDIENCE: &str = "host-monitoring";
pub const MODULE_PREFIX: &str = "/api/modules/host-monitoring";
pub use sarmg_platform_gateway::{
    AUDIENCE_HEADER as MODULE_AUDIENCE_HEADER, PREFIX_HEADER as FORWARDED_PREFIX_HEADER,
    PRINCIPAL_HEADER, PROTOCOL as MODULE_PROTOCOL, PROTOCOL_HEADER as MODULE_PROTOCOL_HEADER,
    TOKEN_HEADER as MODULE_TOKEN_HEADER, parse_principal,
};

#[derive(Debug, Clone)]
pub struct Principal {
    pub subject: String,
}

pub fn require(headers: &HeaderMap, state: &crate::http::AppState) -> Result<()> {
    if !state.gateway.validates(headers) {
        return Err(Error::Unauthorized);
    }
    Ok(())
}

pub fn require_console(headers: &HeaderMap, state: &crate::http::AppState) -> Result<Principal> {
    if headers.contains_key(header::COOKIE) {
        return Err(Error::GatewayRequired(
            "Union cookies must be stripped before forwarding to host-monitoring".into(),
        ));
    }
    require(headers, state)?;
    let subject = parse_principal(headers)
        .map_err(|_| Error::Unauthorized)?
        .to_owned();
    Ok(Principal { subject })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[tokio::test]
    async fn shared_gateway_comparison_is_exact() {
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
        headers.insert(PRINCIPAL_HEADER, HeaderValue::from_static("operator"));
        let state = crate::http::AppState::new(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgresql://localhost/unused")
                .unwrap(),
            identity.clone(),
        );
        assert_eq!(
            require_console(&headers, &state).unwrap().subject,
            "operator"
        );

        headers.insert(
            PRINCIPAL_HEADER,
            HeaderValue::from_bytes("管理员".as_bytes()).unwrap(),
        );
        assert_eq!(require_console(&headers, &state).unwrap().subject, "管理员");

        headers.remove(PRINCIPAL_HEADER);
        assert!(require_console(&headers, &state).is_err());
        headers.append(PRINCIPAL_HEADER, HeaderValue::from_static("operator-one"));
        headers.append(PRINCIPAL_HEADER, HeaderValue::from_static("operator-two"));
        assert!(require_console(&headers, &state).is_err());

        headers.insert(
            MODULE_AUDIENCE_HEADER,
            HeaderValue::from_static("photo-backup"),
        );
        assert!(!identity.validates(&headers));
    }
}
