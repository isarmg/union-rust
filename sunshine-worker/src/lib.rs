//! Sunshine's private UnionC worker.
//!
//! The worker owns its PostgreSQL schema and upstream Sunshine credentials. It
//! deliberately has no dependency on UnionC's SQLite database, sessions,
//! cookies or `AppState`.

pub mod auth;
pub mod client;
pub mod config;
pub mod crypto;
pub mod db;
pub mod error;
pub mod http;
pub mod migration;
pub mod model;

pub use auth::{InternalAuth, InternalIdentity};
pub use config::ServeConfig;
pub use error::{AppError, AppResult};
