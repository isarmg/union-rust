//! Browser-authorized, zero-copy host pairing.
//!
//! The browser approves a pending request but never receives either secret.
//! Both the future report bearer token and the independent polling secret are
//! generated locally. Only their SHA-256 hashes leave this process.

use std::{fs, path::PathBuf};

use anyhow::{Context, bail};
use chrono::{TimeDelta, Utc};
use reqwest::{StatusCode, header};
use unionc_protocol::{
    ActivateAgentRequestRef as ActivatePairingRequest,
    ActivateAgentResponse as ActivatePairingResponse, ActivatePairingStatus,
    AgentPairingRequest as CreatePairingRequest, AgentPairingResponse as CreatePairingResponse,
    AgentPairingStatusResponse as PairingStatusResponse, PairingStatus,
};
use uuid::Uuid;

use crate::{
    collectors::load_host_identity,
    config::AgentConfig,
    model::HostIdentity,
    state_lock,
    transport::{Reporter, build_client, persist_private_value},
};

mod activation;
mod client;
mod commit;
mod state;

use activation::*;
use client::*;
use commit::*;
use state::*;
pub use state::{LocalAuthState, LocalPairingStatus, PairingProgress, PairingSession};

// Flow fragments stay in this module scope so the state machine retains its
// existing private visibility and compare-and-swap transaction invariants.
include!("create.rs");
include!("activation_flow.rs");
include!("polling.rs");
include!("commit_flow.rs");
include!("local.rs");
include!("state_storage.rs");
