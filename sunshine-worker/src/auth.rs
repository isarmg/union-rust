//! Sunshine constants and request identity layered on the shared gateway
//! contract. Validation itself is owned by `sarmg-platform-gateway`.

pub use sarmg_platform_gateway::{
    AUDIENCE_HEADER, GatewayIdentity as InternalAuth, PREFIX_HEADER, PROTOCOL, PROTOCOL_HEADER,
    TOKEN_HEADER,
};

pub const AUDIENCE: &str = "sunshine";
pub const PREFIX: &str = "/modules/sunshine";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InternalIdentity {
    pub subject: String,
}
