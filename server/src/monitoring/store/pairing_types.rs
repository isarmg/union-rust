use chrono::{DateTime, Utc};

use crate::monitoring::{AgentInstanceSummary, PairingStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeInviteResult {
    Revoked,
    NotFound,
    NotPending,
}

#[derive(Debug)]
pub enum CreateInviteResult {
    Created(AgentInstanceSummary),
    InstanceNotFound,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivatePairingResult {
    Active(String),
    RequestNotFound,
    InvalidCode,
    Expired,
    Conflict,
}
