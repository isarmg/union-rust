pub mod auth;
pub mod config;
pub mod error;
pub mod http;
pub mod import;
pub mod model;
pub mod store;

use sha2::{Digest, Sha256};

pub fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
