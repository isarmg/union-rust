//! Sunshine constants and request identity layered on the shared gateway
//! contract. Validation itself is owned by `sarmg-platform-gateway`.

pub use sarmg_platform_gateway::{
    AUDIENCE_HEADER, GatewayIdentity as InternalAuth, PREFIX_HEADER, PRINCIPAL_HEADER, PROTOCOL,
    PROTOCOL_HEADER, TOKEN_HEADER, parse_principal,
};

pub const AUDIENCE: &str = "sunshine";
pub const PREFIX: &str = "/api/modules/sunshine";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InternalIdentity {
    pub subject: String,
}
