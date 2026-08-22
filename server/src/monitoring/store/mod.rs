//! Persistence for read-only host metric reports.

use chrono::{DateTime, Utc};
use sqlx_core::{query::query, row::Row};
use sqlx_sqlite::{SqliteConnection, SqliteRow};

use crate::monitoring::{
    AgentInstanceSummary, AgentPairingPublicSummary, AgentPairingRequest, AgentReport,
    AgentReportExt, PairingStatus,
};

use crate::infra::database::{self, DbPool};

mod host_types;
mod pairing_types;
mod report_types;
mod rows;

pub use host_types::*;
pub use pairing_types::*;
pub use report_types::*;
use rows::*;

// Persistence flows stay in this module scope so SQLite transaction helpers
// and row decoders remain private without widening the store API. The source
// is split by domain while preserving the original transaction boundaries.
include!("pairing.rs");
include!("reports.rs");
include!("hosts.rs");
include!("retention.rs");
