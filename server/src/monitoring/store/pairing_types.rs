use chrono::{DateTime, Utc};

use crate::monitoring::{AgentInstanceSummary, PairingStatus};

/// Hard disk-growth boundary for anonymous, unapproved pairing requests.
/// Active rows represent administrator-approved hosts and are not counted.
pub const MAX_PENDING_PAIRING_REQUESTS: i64 = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelInviteResult {
    Cancelled,
    NotFound,
    NotPending,
}

#[derive(Debug)]
pub enum CreateInviteResult {
    Created(AgentInstanceSummary),
    Conflict,
}

#[derive(Debug)]
pub struct StoredPairingStatus {
    pub status: PairingStatus,
    pub instance_id: Option<String>,
}

#[derive(Debug)]
pub struct StoredPairingCreation {
    pub request_id: String,
    pub expires_at: DateTime<Utc>,
    pub created: bool,
}

#[derive(Debug)]
pub enum CreatePairingResult {
    Ready(StoredPairingCreation),
    Expired,
    Conflict,
    AtCapacity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivatePairingResult {
    Active(String),
    RequestNotFound,
    InvalidCode,
    Expired,
    Conflict,
}
