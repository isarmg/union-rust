//! Browser-authorized, zero-copy host pairing.
//!
//! The browser approves a pending request but never receives either secret.
//! Both the future report bearer token and the independent polling secret are
//! generated locally. Only their SHA-256 hashes leave this process.

use std::{fs, net::IpAddr, path::PathBuf};

use anyhow::{Context, bail};
use chrono::{DateTime, TimeDelta, Utc};
use reqwest::{StatusCode, header};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned, de::Error as _,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    collectors::load_host_identity,
    config::AgentConfig,
    model::HostIdentity,
    state_lock,
    transport::{Reporter, build_activation_client, build_client, persist_private_value},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PairingStateVersion;

const PAIRING_STATE_VERSION: PairingStateVersion = PairingStateVersion;

impl Serialize for PairingStateVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(env!("CARGO_PKG_VERSION"))
    }
}

impl<'de> Deserialize<'de> for PairingStateVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = String::deserialize(deserializer)?;
        if version == env!("CARGO_PKG_VERSION") {
            Ok(Self)
        } else {
            Err(D::Error::custom(format!(
                "pairing state belongs to Agent {version}, expected {}",
                env!("CARGO_PKG_VERSION")
            )))
        }
    }
}
const PAIRING_STATE_FILE: &str = "pairing-state.json";
const AUTH_STATE_FILE: &str = "auth-state.json";
const MAX_PAIRING_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum StoredPairingState {
    Creating {
        version: PairingStateVersion,
        generation: Uuid,
        pairing_endpoint: String,
        report_endpoint: String,
        host: HostIdentity,
        host_name: Option<String>,
        bearer_secret: String,
        polling_secret: String,
    },
    Pending {
        version: PairingStateVersion,
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
        expires_at: DateTime<Utc>,
        poll_interval: u64,
        pairing_endpoint: String,
        report_endpoint: String,
        bearer_secret: String,
        host_name: Option<String>,
        polling_secret: String,
    },
    /// Durable local commit journal. Once this phase exists, no network I/O is
    /// allowed; startup idempotently completes the
    /// token/identity/config transition before writing Active last.
    Activating {
        version: PairingStateVersion,
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
        expires_at: DateTime<Utc>,
        poll_interval: u64,
        instance_id: Uuid,
        pairing_endpoint: String,
        report_endpoint: String,
        bearer_secret: String,
        host_name: Option<String>,
    },
    Active {
        version: PairingStateVersion,
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
        instance_id: Uuid,
        report_endpoint: String,
        host_name: Option<String>,
        completed_at: DateTime<Utc>,
    },
    Denied {
        version: PairingStateVersion,
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
        report_endpoint: String,
        completed_at: DateTime<Utc>,
    },
    Expired {
        version: PairingStateVersion,
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
        report_endpoint: String,
        completed_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PairingSession {
    pub generation: Uuid,
    pub request_id: Uuid,
    pub activation_url: String,
    pub expires_at: DateTime<Utc>,
    pub poll_interval: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", content = "details", rename_all = "snake_case")]
pub enum PairingProgress {
    Creating {
        generation: Uuid,
        report_endpoint: String,
    },
    Waiting(PairingSession),
    Active {
        generation: Uuid,
        request_id: Uuid,
        instance_id: Uuid,
        report_endpoint: String,
    },
    Denied {
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
    },
    Expired {
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalAuthState {
    version: PairingStateVersion,
    pub status: String,
    pub reason: String,
    pub changed_at: DateTime<Utc>,
}

impl PairingProgress {
    pub fn active_request_id(&self) -> Option<Uuid> {
        match self {
            Self::Active { request_id, .. } => Some(*request_id),
            _ => None,
        }
    }
}

#[derive(Serialize)]
struct CreatePairingRequest<'a> {
    host: &'a HostIdentity,
    token_hash: String,
    polling_secret_hash: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatePairingResponse {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    request_id: Uuid,
    activation_url: String,
    expires_in: u64,
    poll_interval: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairingStatusResponse {
    status: PairingStatus,
    #[serde(default, deserialize_with = "deserialize_optional_canonical_uuid")]
    instance_id: Option<Uuid>,
}

#[derive(Serialize)]
struct ActivatePairingRequest<'a> {
    request_id: Uuid,
    activation_code: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivatePairingResponse {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    instance_id: Uuid,
    status: ActivatePairingStatus,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ActivatePairingStatus {
    Active,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum PairingStatus {
    Waiting,
    Active,
    Denied,
    Expired,
}

fn deserialize_canonical_uuid<'de, D>(deserializer: D) -> Result<Uuid, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    let parsed = Uuid::parse_str(&value).map_err(D::Error::custom)?;
    if parsed.to_string() != value {
        return Err(D::Error::custom(
            "UUID must use canonical lowercase hyphenated text",
        ));
    }
    Ok(parsed)
}

fn deserialize_optional_canonical_uuid<'de, D>(deserializer: D) -> Result<Option<Uuid>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .map(|value| {
            let parsed = Uuid::parse_str(&value).map_err(D::Error::custom)?;
            if parsed.to_string() != value {
                return Err(D::Error::custom(
                    "UUID must use canonical lowercase hyphenated text",
                ));
            }
            Ok(parsed)
        })
        .transpose()
}

pub async fn start_or_resume(
    config: &AgentConfig,
    host: &HostIdentity,
) -> anyhow::Result<PairingSession> {
    match prepare_start(config, host)? {
        PairingStart::Waiting(session) => Ok(session),
        PairingStart::Create(state) => finish_create_request(config, *state).await,
    }
}

enum PairingStart {
    Waiting(PairingSession),
    Create(Box<StoredPairingState>),
}

/// Select or create the request generation while holding the cross-process
/// state lock. Network I/O happens only after this function releases it.
fn prepare_start(config: &AgentConfig, host: &HostIdentity) -> anyhow::Result<PairingStart> {
    let _lock = lock_state(config)?;
    match load_state(config)? {
        Some(StoredPairingState::Pending {
            generation,
            request_id,
            activation_url,
            expires_at,
            poll_interval,
            pairing_endpoint,
            report_endpoint,
            ..
        }) if pairing_endpoints_match(config, &pairing_endpoint, &report_endpoint) => {
            return Ok(PairingStart::Waiting(PairingSession {
                generation,
                request_id,
                activation_url,
                expires_at,
                poll_interval,
            }));
        }
        Some(StoredPairingState::Pending { expires_at, .. })
            if !config.replace_pending_pairing && expires_at > Utc::now() =>
        {
            bail!(
                "a browser pairing request for a different UnionC server is still pending; \
                 finish it or wait until {expires_at} before changing servers"
            );
        }
        Some(state @ StoredPairingState::Creating { .. }) => {
            let (pairing_endpoint, report_endpoint) = match &state {
                StoredPairingState::Creating {
                    pairing_endpoint,
                    report_endpoint,
                    ..
                } => (pairing_endpoint, report_endpoint),
                _ => unreachable!(),
            };
            if pairing_endpoints_match(config, pairing_endpoint, report_endpoint) {
                return Ok(PairingStart::Create(Box::new(state)));
            }
            if !config.replace_pending_pairing {
                bail!(
                    "a browser pairing request for a different UnionC server is being created; \
                     retry with the original server before changing servers"
                );
            }
        }
        Some(state @ StoredPairingState::Activating { .. }) => {
            let session = session_from_activating(&state)?;
            let same_requested_endpoints = match &state {
                StoredPairingState::Activating {
                    pairing_endpoint,
                    report_endpoint,
                    ..
                } => pairing_endpoints_match(config, pairing_endpoint, report_endpoint),
                _ => unreachable!(),
            };
            finish_activating_unlocked(config, state)?;
            if same_requested_endpoints {
                return Ok(PairingStart::Waiting(session));
            }
            // The journal belonged to another server. It is now fully
            // converged, so this explicitly confirmed request may safely
            // create its own generation below.
        }
        _ => {}
    }

    let creating = StoredPairingState::Creating {
        version: PAIRING_STATE_VERSION,
        generation: Uuid::new_v4(),
        pairing_endpoint: config.pairing_endpoint(),
        report_endpoint: config.endpoint.clone(),
        host: host.clone(),
        host_name: config.host_name.clone(),
        bearer_secret: random_secret(),
        polling_secret: random_secret(),
    };
    // Persist both locally generated secrets and the exact host request before
    // the first POST. If the server commits but the response is lost, retrying
    // uses the same polling_secret_hash and the server returns the same request.
    persist_state_unlocked(config, &creating)?;
    Ok(PairingStart::Create(Box::new(creating)))
}

fn pairing_endpoints_match(
    config: &AgentConfig,
    stored_pairing_endpoint: &str,
    stored_report_endpoint: &str,
) -> bool {
    stored_pairing_endpoint == config.pairing_endpoint()
        && stored_report_endpoint == config.endpoint
}

async fn finish_create_request(
    config: &AgentConfig,
    state: StoredPairingState,
) -> anyhow::Result<PairingSession> {
    let StoredPairingState::Creating {
        version,
        generation,
        pairing_endpoint,
        report_endpoint,
        host,
        host_name,
        bearer_secret,
        polling_secret,
    } = state
    else {
        bail!("internal error: expected a creating pairing state");
    };
    validate_state_version(version)?;
    let client = build_client(config)?;
    let response = client
        .post(&pairing_endpoint)
        .json(&CreatePairingRequest {
            host: &host,
            token_hash: sha256_hex(&bearer_secret),
            polling_secret_hash: sha256_hex(&polling_secret),
        })
        .send()
        .await
        .context("failed to create a browser pairing request")?;
    let status = response.status();
    let content_type = pairing_response_content_type(&response);
    let body = read_limited(response, "pairing request").await?;
    ensure_pairing_status(
        status,
        &[StatusCode::OK, StatusCode::CREATED],
        &body,
        "create pairing request",
    )?;
    let created: CreatePairingResponse =
        parse_pairing_json(&body, &content_type, &pairing_endpoint, "pairing response")?;
    if created.expires_in == 0 || created.expires_in > 7 * 24 * 60 * 60 {
        bail!("UnionC returned an invalid pairing expiration");
    }
    if created.poll_interval == 0 || created.poll_interval > 300 {
        bail!("UnionC returned an invalid pairing poll interval");
    }
    let activation_url = resolve_activation_url(
        &pairing_endpoint,
        &created.activation_url,
        config.allow_insecure_http,
    )?;
    let expires_at = Utc::now()
        .checked_add_signed(TimeDelta::seconds(
            i64::try_from(created.expires_in).context("pairing expiration overflow")?,
        ))
        .context("pairing expiration overflow")?;
    let expected_pairing_endpoint = pairing_endpoint.clone();
    let expected_report_endpoint = report_endpoint.clone();
    let expected_polling_secret = polling_secret.clone();
    let state = StoredPairingState::Pending {
        version: PAIRING_STATE_VERSION,
        generation,
        request_id: created.request_id,
        activation_url: activation_url.clone(),
        expires_at,
        poll_interval: created.poll_interval,
        pairing_endpoint,
        report_endpoint,
        bearer_secret,
        host_name,
        polling_secret,
    };
    compare_and_persist_creating(
        config,
        generation,
        &expected_pairing_endpoint,
        &expected_report_endpoint,
        &expected_polling_secret,
        &state,
    )?;
    Ok(PairingSession {
        generation,
        request_id: created.request_id,
        activation_url,
        expires_at,
        poll_interval: created.poll_interval,
    })
}

/// Submit the one-time authorization key for exactly the pending generation
/// emitted to the trusted Windows tray broker.
///
/// The key stays in memory and is sent only by the Agent's TLS-configured
/// client. Redirects are disabled so an HTTP 307/308 can never replay the JSON
/// body to another endpoint. A 409 is deliberately treated as ambiguous: the
/// server may have committed a previous attempt whose response was lost, so
/// the caller must keep polling this same durable request rather than create a
/// replacement and risk losing the successfully activated generation.
pub async fn activate_pending_with_code(
    config: &AgentConfig,
    generation: Uuid,
    request_id: Uuid,
    activation_code: &str,
) -> anyhow::Result<Option<Uuid>> {
    crate::tray_support::validate_activation_code(activation_code)?;
    let (activation_url, pairing_endpoint, report_endpoint, polling_secret) = {
        let _lock = lock_state(config)?;
        let state = load_state(config)?
            .context("the pending pairing request disappeared before activation")?;
        match state {
            StoredPairingState::Pending {
                version,
                generation: current_generation,
                request_id: current_request_id,
                activation_url,
                pairing_endpoint,
                report_endpoint,
                polling_secret,
                ..
            } => {
                validate_state_version(version)?;
                if current_generation != generation || current_request_id != request_id {
                    return Err(PairingSuperseded.into());
                }
                (
                    activation_url,
                    pairing_endpoint,
                    report_endpoint,
                    polling_secret,
                )
            }
            StoredPairingState::Activating {
                generation: current_generation,
                request_id: current_request_id,
                instance_id,
                ..
            }
            | StoredPairingState::Active {
                generation: current_generation,
                request_id: current_request_id,
                instance_id,
                ..
            } if current_generation == generation && current_request_id == request_id => {
                return Ok(Some(instance_id));
            }
            _ => {
                bail!("the pairing request is no longer pending; authorization key was not sent")
            }
        }
    };
    validate_activation_url_request(&activation_url, &pairing_endpoint, request_id)?;
    let endpoint = activation_endpoint(&pairing_endpoint)?;
    let endpoint_display = endpoint.as_str().to_string();
    let client = build_activation_client(config)?;
    let response = client
        .post(endpoint)
        .header(header::ACCEPT, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .json(&ActivatePairingRequest {
            request_id,
            activation_code,
        })
        .send()
        .await
        .context("failed to submit the one-time authorization key")?;
    let status = response.status();
    let content_type = pairing_response_content_type(&response);
    let body = read_limited(response, "pairing activation").await?;
    let activated_instance = if status == StatusCode::CONFLICT {
        None
    } else {
        ensure_pairing_status(
            status,
            &[StatusCode::OK],
            &body,
            "submit the one-time authorization key",
        )?;
        let activated: ActivatePairingResponse = parse_pairing_json(
            &body,
            &content_type,
            &endpoint_display,
            "pairing activation response",
        )?;
        debug_assert_eq!(activated.status, ActivatePairingStatus::Active);
        Some(activated.instance_id)
    };

    // Network I/O never holds the credential lock. Reacquire and compare the
    // full pending transaction before allowing the caller to trust the result.
    let _lock = lock_state(config)?;
    let current = load_state(config)?.context("the pairing state disappeared after activation")?;
    match current {
        StoredPairingState::Pending { .. } => {
            ensure_pending_is_current(
                config,
                generation,
                request_id,
                &pairing_endpoint,
                &report_endpoint,
                &polling_secret,
            )?;
            Ok(activated_instance)
        }
        StoredPairingState::Activating {
            generation: current_generation,
            request_id: current_request_id,
            instance_id,
            ..
        }
        | StoredPairingState::Active {
            generation: current_generation,
            request_id: current_request_id,
            instance_id,
            ..
        } if current_generation == generation && current_request_id == request_id => {
            if activated_instance.is_some_and(|activated| activated != instance_id) {
                bail!(
                    "activation instance does not match the concurrently committed pairing state"
                );
            }
            Ok(Some(instance_id))
        }
        _ => Err(PairingSuperseded.into()),
    }
}

fn activation_endpoint(pairing_endpoint: &str) -> anyhow::Result<reqwest::Url> {
    let mut endpoint = reqwest::Url::parse(pairing_endpoint)
        .context("stored pairing endpoint is not a valid URL")?;
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        bail!("stored pairing endpoint unexpectedly contains credentials");
    }
    let path = endpoint.path().trim_end_matches('/');
    let base = path
        .strip_suffix("/pairing-requests")
        .context("stored pairing endpoint does not end in /pairing-requests")?;
    endpoint.set_path(&format!("{base}/activate"));
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    Ok(endpoint)
}

fn validate_activation_url_request(
    activation_url: &str,
    pairing_endpoint: &str,
    request_id: Uuid,
) -> anyhow::Result<()> {
    let url =
        reqwest::Url::parse(activation_url).context("stored pairing activation URL is invalid")?;
    let pairing =
        reqwest::Url::parse(pairing_endpoint).context("stored pairing endpoint is invalid")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.scheme() != pairing.scheme()
        || url.host_str().map(str::to_ascii_lowercase)
            != pairing.host_str().map(str::to_ascii_lowercase)
        || url.port_or_known_default() != pairing.port_or_known_default()
        || url.path() != format!("/agent/activate/{request_id}")
    {
        bail!("stored activation URL does not match the pending pairing request");
    }
    Ok(())
}

/// Inspect or advance the locally persisted pairing state once. Callers own
/// scheduling so an interactive `pair` command and an already-running service
/// can safely poll the same request without holding a lock or a socket.
pub async fn poll_existing(config: &AgentConfig) -> anyhow::Result<Option<PairingProgress>> {
    let Some(state) = load_state_for_network(config)? else {
        return Ok(None);
    };
    let state = match state {
        creating @ StoredPairingState::Creating { .. } => {
            let waiting = finish_create_request(config, creating).await?;
            return Ok(Some(PairingProgress::Waiting(waiting)));
        }
        activating @ StoredPairingState::Activating { .. } => {
            return recover_activating(config, activating).map(Some);
        }
        state => state,
    };
    let pending_for_activation = state.clone();
    let StoredPairingState::Pending {
        version,
        generation,
        request_id,
        activation_url,
        expires_at,
        poll_interval,
        pairing_endpoint,
        report_endpoint,
        bearer_secret: _,
        host_name: _,
        polling_secret,
    } = state
    else {
        return Ok(Some(progress_from_terminal(state)));
    };
    validate_state_version(version)?;

    let endpoint = format!(
        "{}/{request_id}/status",
        pairing_endpoint.trim_end_matches('/')
    );
    let client = build_client(config)?;
    let response = client
        .post(&endpoint)
        .header(header::AUTHORIZATION, format!("Pairing {polling_secret}"))
        .send()
        .await
        .context("failed to poll browser pairing status")?;
    let status = response.status();
    let content_type = pairing_response_content_type(&response);
    let body = read_limited(response, "pairing status").await?;
    ensure_pairing_status(status, &[StatusCode::OK], &body, "poll pairing status")?;
    let polled: PairingStatusResponse =
        parse_pairing_json(&body, &content_type, &endpoint, "pairing status response")?;

    match polled.status {
        PairingStatus::Waiting => {
            if polled.instance_id.is_some() {
                bail!("waiting pairing response unexpectedly included instance_id");
            }
            Ok(Some(PairingProgress::Waiting(PairingSession {
                generation,
                request_id,
                activation_url,
                expires_at,
                poll_interval,
            })))
        }
        PairingStatus::Active => {
            let instance_id = polled
                .instance_id
                .context("active pairing response omitted instance_id")?;
            persist_active_credentials(config, pending_for_activation, instance_id).map(Some)
        }
        PairingStatus::Denied => {
            if polled.instance_id.is_some() {
                bail!("denied pairing response unexpectedly included instance_id");
            }
            let expected_report_endpoint = report_endpoint.clone();
            let denied = StoredPairingState::Denied {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: activation_url.clone(),
                report_endpoint,
                completed_at: Utc::now(),
            };
            compare_and_persist_pending(
                config,
                generation,
                request_id,
                &pairing_endpoint,
                &expected_report_endpoint,
                &polling_secret,
                &denied,
            )?;
            Ok(Some(PairingProgress::Denied {
                generation,
                request_id,
                activation_url,
            }))
        }
        PairingStatus::Expired => {
            if polled.instance_id.is_some() {
                bail!("expired pairing response unexpectedly included instance_id");
            }
            let expected_report_endpoint = report_endpoint.clone();
            let expired = StoredPairingState::Expired {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: activation_url.clone(),
                report_endpoint,
                completed_at: Utc::now(),
            };
            compare_and_persist_pending(
                config,
                generation,
                request_id,
                &pairing_endpoint,
                &expected_report_endpoint,
                &polling_secret,
                &expired,
            )?;
            Ok(Some(PairingProgress::Expired {
                generation,
                request_id,
                activation_url,
            }))
        }
    }
}

fn load_state_for_network(config: &AgentConfig) -> anyhow::Result<Option<StoredPairingState>> {
    let _lock = lock_state(config)?;
    load_state(config)
}

fn persist_active_credentials(
    config: &AgentConfig,
    pending: StoredPairingState,
    instance_id: Uuid,
) -> anyhow::Result<PairingProgress> {
    let StoredPairingState::Pending {
        generation,
        request_id,
        activation_url,
        expires_at,
        poll_interval,
        pairing_endpoint,
        report_endpoint,
        bearer_secret,
        host_name,
        polling_secret,
        ..
    } = pending
    else {
        bail!("internal error: expected pending pairing state for activation");
    };
    let _lock = lock_state(config)?;
    ensure_pending_is_current(
        config,
        generation,
        request_id,
        &pairing_endpoint,
        &report_endpoint,
        &polling_secret,
    )?;
    let activating = StoredPairingState::Activating {
        version: PAIRING_STATE_VERSION,
        generation,
        request_id,
        activation_url,
        expires_at,
        poll_interval,
        instance_id,
        pairing_endpoint,
        report_endpoint: report_endpoint.clone(),
        bearer_secret,
        host_name,
    };
    // Commit the journal before touching any long-lived credential. A crash
    // after this write is recovered locally and can never pair the new token
    // with the previous server endpoint.
    persist_state_unlocked(config, &activating)?;
    finish_activating_unlocked(config, activating)
}

fn session_from_activating(state: &StoredPairingState) -> anyhow::Result<PairingSession> {
    let StoredPairingState::Activating {
        generation,
        request_id,
        activation_url,
        expires_at,
        poll_interval,
        ..
    } = state
    else {
        bail!("internal error: expected an activating pairing state");
    };
    Ok(PairingSession {
        generation: *generation,
        request_id: *request_id,
        activation_url: activation_url.clone(),
        expires_at: *expires_at,
        poll_interval: *poll_interval,
    })
}

fn recover_activating(
    config: &AgentConfig,
    expected: StoredPairingState,
) -> anyhow::Result<PairingProgress> {
    let (expected_generation, expected_request_id) = match &expected {
        StoredPairingState::Activating {
            generation,
            request_id,
            ..
        } => (*generation, *request_id),
        _ => bail!("internal error: expected an activating pairing state"),
    };
    let _lock = lock_state(config)?;
    match load_state(config)? {
        Some(
            current @ StoredPairingState::Activating {
                generation,
                request_id,
                ..
            },
        ) if generation == expected_generation && request_id == expected_request_id => {
            finish_activating_unlocked(config, current)
        }
        Some(StoredPairingState::Active {
            generation,
            request_id,
            instance_id,
            report_endpoint,
            ..
        }) if generation == expected_generation && request_id == expected_request_id => {
            Ok(PairingProgress::Active {
                generation,
                request_id,
                instance_id,
                report_endpoint,
            })
        }
        _ => Err(PairingSuperseded.into()),
    }
}

/// Complete an Activating journal while the pairing state lock is held. Every
/// write is idempotent; Active is deliberately last so any earlier crash is
/// recoverable without consulting the remote server.
fn finish_activating_unlocked(
    config: &AgentConfig,
    state: StoredPairingState,
) -> anyhow::Result<PairingProgress> {
    let StoredPairingState::Activating {
        version,
        generation,
        request_id,
        activation_url,
        instance_id,
        report_endpoint,
        bearer_secret,
        host_name,
        ..
    } = state
    else {
        bail!("internal error: expected an activating pairing state");
    };
    validate_state_version(version)?;
    persist_private_value(
        &config.state_dir.join("agent-token"),
        &bearer_secret,
        "paired host token",
    )?;
    persist_private_value(
        &config.state_dir.join("host-id"),
        &instance_id.to_string(),
        "server-assigned host identity",
    )?;
    persist_active_config_unlocked(config, &report_endpoint, &host_name)?;
    persist_auth_state_unlocked(
        config,
        &LocalAuthState {
            version: PAIRING_STATE_VERSION,
            status: "authorized".into(),
            reason: "browser pairing completed".into(),
            changed_at: Utc::now(),
        },
    )?;
    persist_state_unlocked(
        config,
        &StoredPairingState::Active {
            version: PAIRING_STATE_VERSION,
            generation,
            request_id,
            activation_url,
            instance_id,
            report_endpoint: report_endpoint.clone(),
            host_name,
            completed_at: Utc::now(),
        },
    )?;
    Ok(PairingProgress::Active {
        generation,
        request_id,
        instance_id,
        report_endpoint,
    })
}

/// Inspect the durable pairing journal without taking the transaction lock or
/// completing an interrupted current transaction.
///
/// `status` is a diagnostic command and must remain byte-for-byte read-only:
/// taking the normal lock can create the state directory/lock file, while the
/// recovery path can publish a credential and rewrite several state files.
/// Recovery remains the responsibility of `run` and `pair`.
pub fn local_progress(config: &AgentConfig) -> anyhow::Result<Option<PairingProgress>> {
    load_state(config).map(|state| state.map(progress_from_terminal))
}

/// Return whether the durable host-id belongs to a current package-version
/// pairing transaction that still has an authorized credential.
pub fn has_current_authorized_identity(config: &AgentConfig) -> anyhow::Result<bool> {
    let _lock = lock_state(config)?;
    let authorized =
        local_auth_state_unlocked(config)?.is_some_and(|state| state.status == "authorized");
    if !authorized {
        return Ok(false);
    }
    Ok(matches!(
        load_state(config)?,
        Some(
            StoredPairingState::Creating { .. }
                | StoredPairingState::Pending { .. }
                | StoredPairingState::Activating { .. }
                | StoredPairingState::Active { .. }
        )
    ))
}

/// Return a consistent snapshot of the previously active reporter while a
/// re-pair is incomplete. Reading the durable endpoint (already in `config`)
/// and token under the same cross-process lock prevents observing the new
/// token before its matching endpoint/configuration commit.
pub fn existing_reporter_for_run(config: &AgentConfig) -> anyhow::Result<Option<Reporter>> {
    let _lock = lock_state(config)?;
    if local_auth_state_unlocked(config)?.is_none_or(|state| state.status != "authorized") {
        return Ok(None);
    }
    match load_state(config)? {
        Some(StoredPairingState::Active { .. }) => Ok(None),
        Some(activating @ StoredPairingState::Activating { .. }) => {
            finish_activating_unlocked(config, activating)?;
            Ok(None)
        }
        Some(StoredPairingState::Creating { .. } | StoredPairingState::Pending { .. }) => {
            Reporter::for_existing_credential(config)
        }
        _ => Ok(None),
    }
}

/// Build a low-level transport only when every durable identity component is
/// bound to the current package's completed pairing transaction.
pub(crate) fn reporter_for_current_active_state(
    config: &AgentConfig,
) -> anyhow::Result<Option<Reporter>> {
    let _lock = lock_state(config)?;
    if local_auth_state_unlocked(config)?.is_none_or(|state| state.status != "authorized") {
        return Ok(None);
    }
    let Some(StoredPairingState::Active {
        instance_id,
        report_endpoint,
        ..
    }) = load_state(config)?
    else {
        return Ok(None);
    };
    if report_endpoint != config.endpoint {
        bail!("active pairing state does not match the configured report endpoint");
    }
    let host_id_path = config.state_dir.join("host-id");
    let host_id = fs::read_to_string(&host_id_path)
        .with_context(|| format!("failed to read {}", host_id_path.display()))?;
    let host_id = host_id.trim();
    let parsed = Uuid::parse_str(host_id).context("stored host identity is not a UUID")?;
    if parsed.to_string() != host_id || parsed != instance_id {
        bail!("stored host identity does not match the current Active pairing state");
    }
    Reporter::for_existing_credential(config)
}

/// Revalidate the exact Active generation and durably converge the main
/// configuration before a caller starts using its token.
pub fn commit_active_configuration(
    config: &mut AgentConfig,
    generation: Uuid,
    request_id: Uuid,
    instance_id: Uuid,
    report_endpoint: &str,
) -> anyhow::Result<PathBuf> {
    let _lock = lock_state(config)?;
    let host_name =
        ensure_active_is_current(config, generation, request_id, instance_id, report_endpoint)?;
    let path = persist_active_config_unlocked(config, report_endpoint, &host_name)?;
    apply_active_config(config, report_endpoint, &host_name);
    Ok(path)
}

/// Atomically snapshot the Active generation's config, identity and token into
/// an in-memory Reporter before allowing another pairing transaction to
/// replace them on disk.
pub fn activate_reporter_snapshot(
    config: &mut AgentConfig,
    host: &mut HostIdentity,
    generation: Uuid,
    request_id: Uuid,
    instance_id: Uuid,
    report_endpoint: &str,
) -> anyhow::Result<Reporter> {
    let _lock = lock_state(config)?;
    if local_auth_state_unlocked(config)?.is_none_or(|state| state.status != "authorized") {
        bail!("current Active pairing state has no current authorized identity state");
    }
    let host_name =
        ensure_active_is_current(config, generation, request_id, instance_id, report_endpoint)?;
    // Active is written only after config in the journal transaction. Do not
    // rewrite it from this potentially stale service snapshot.
    apply_active_config(config, report_endpoint, &host_name);
    let mut durable_host = load_host_identity(&config.state_dir)?;
    let durable_host_id = Uuid::parse_str(&durable_host.id)
        .context("durable host identity contains an invalid UUID")?;
    if durable_host_id != instance_id {
        bail!(
            "paired host identity mismatch: state contains {}, server assigned {instance_id}; run pair again",
            durable_host.id
        );
    }
    if let Some(name) = &host_name {
        durable_host.name.clone_from(name);
    }
    *host = durable_host;
    Reporter::for_existing_credential(config)?
        .context("paired host credential is missing after the Active pairing transaction")
}

fn ensure_active_is_current(
    config: &AgentConfig,
    generation: Uuid,
    request_id: Uuid,
    instance_id: Uuid,
    report_endpoint: &str,
) -> anyhow::Result<Option<String>> {
    let current = load_state(config)?;
    let host_name = match current {
        Some(StoredPairingState::Active {
            generation: current_generation,
            request_id: current_request_id,
            instance_id: current_instance_id,
            report_endpoint: current_report_endpoint,
            host_name,
            ..
        }) if current_generation == generation
            && current_request_id == request_id
            && current_instance_id == instance_id
            && current_report_endpoint == report_endpoint =>
        {
            host_name
        }
        _ => return Err(PairingSuperseded.into()),
    };
    Ok(host_name)
}

pub fn mark_reauth_required(config: &AgentConfig, reason: impl Into<String>) -> anyhow::Result<()> {
    persist_auth_state(
        config,
        &LocalAuthState {
            version: PAIRING_STATE_VERSION,
            status: "reauth_required".into(),
            reason: reason.into(),
            changed_at: Utc::now(),
        },
    )
}

/// Mark the reporter credential blocked only if no newer Active transaction
/// superseded the in-memory reporter while its HTTP request was in flight.
pub fn mark_reauth_required_if_current(
    config: &AgentConfig,
    active_pairing: Option<(Uuid, Uuid)>,
    reason: impl Into<String>,
) -> anyhow::Result<bool> {
    let _lock = lock_state(config)?;
    let state = load_state(config)?;
    let current = match active_pairing {
        Some((expected_generation, expected_request_id)) => match state {
            Some(StoredPairingState::Activating { .. }) => false,
            Some(StoredPairingState::Active {
                generation,
                request_id,
                ..
            }) => generation == expected_generation && request_id == expected_request_id,
            _ => true,
        },
        None => !matches!(
            state,
            Some(StoredPairingState::Activating { .. } | StoredPairingState::Active { .. })
        ),
    };
    if !current {
        return Ok(false);
    }
    persist_auth_state_unlocked(
        config,
        &LocalAuthState {
            version: PAIRING_STATE_VERSION,
            status: "reauth_required".into(),
            reason: reason.into(),
            changed_at: Utc::now(),
        },
    )?;
    Ok(true)
}

pub fn mark_authorized(config: &AgentConfig) -> anyhow::Result<()> {
    persist_auth_state(
        config,
        &LocalAuthState {
            version: PAIRING_STATE_VERSION,
            status: "authorized".into(),
            reason: "browser pairing completed".into(),
            changed_at: Utc::now(),
        },
    )
}

pub fn local_auth_state(config: &AgentConfig) -> anyhow::Result<Option<LocalAuthState>> {
    local_auth_state_unlocked(config)
}

fn local_auth_state_unlocked(config: &AgentConfig) -> anyhow::Result<Option<LocalAuthState>> {
    let path = config.state_dir.join(AUTH_STATE_FILE);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("auth state {} is invalid", path.display()))
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn persist_auth_state(config: &AgentConfig, state: &LocalAuthState) -> anyhow::Result<()> {
    let _lock = lock_state(config)?;
    persist_auth_state_unlocked(config, state)
}

fn persist_auth_state_unlocked(config: &AgentConfig, state: &LocalAuthState) -> anyhow::Result<()> {
    persist_private_value(
        &config.state_dir.join(AUTH_STATE_FILE),
        &serde_json::to_string_pretty(state)?,
        "local authorization state",
    )
}

fn progress_from_terminal(state: StoredPairingState) -> PairingProgress {
    match state {
        StoredPairingState::Creating {
            generation,
            report_endpoint,
            ..
        } => PairingProgress::Creating {
            generation,
            report_endpoint,
        },
        StoredPairingState::Pending {
            generation,
            request_id,
            activation_url,
            expires_at,
            poll_interval,
            ..
        } => PairingProgress::Waiting(PairingSession {
            generation,
            request_id,
            activation_url,
            expires_at,
            poll_interval,
        }),
        StoredPairingState::Activating {
            generation,
            report_endpoint,
            ..
        } => PairingProgress::Creating {
            generation,
            report_endpoint,
        },
        StoredPairingState::Active {
            generation,
            request_id,
            instance_id,
            report_endpoint,
            ..
        } => PairingProgress::Active {
            generation,
            request_id,
            instance_id,
            report_endpoint,
        },
        StoredPairingState::Denied {
            generation,
            request_id,
            activation_url,
            ..
        } => PairingProgress::Denied {
            generation,
            request_id,
            activation_url,
        },
        StoredPairingState::Expired {
            generation,
            request_id,
            activation_url,
            ..
        } => PairingProgress::Expired {
            generation,
            request_id,
            activation_url,
        },
    }
}

fn state_path(config: &AgentConfig) -> PathBuf {
    config.state_dir.join(PAIRING_STATE_FILE)
}

fn lock_state(config: &AgentConfig) -> anyhow::Result<state_lock::CredentialStateLock> {
    state_lock::lock(&config.state_dir)
}

fn load_state(config: &AgentConfig) -> anyhow::Result<Option<StoredPairingState>> {
    let path = state_path(config);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read pairing state {}", path.display()));
        }
    };
    let state: StoredPairingState = serde_json::from_slice(&bytes)
        .with_context(|| format!("pairing state {} is invalid", path.display()))?;
    let (version, generation) = match &state {
        StoredPairingState::Creating {
            version,
            generation,
            ..
        }
        | StoredPairingState::Pending {
            version,
            generation,
            ..
        }
        | StoredPairingState::Activating {
            version,
            generation,
            ..
        }
        | StoredPairingState::Active {
            version,
            generation,
            ..
        }
        | StoredPairingState::Denied {
            version,
            generation,
            ..
        }
        | StoredPairingState::Expired {
            version,
            generation,
            ..
        } => (*version, *generation),
    };
    validate_state_version(version)?;
    if generation.is_nil() {
        bail!("pairing state contains an invalid nil generation; start a new pairing request");
    }
    Ok(Some(state))
}

#[cfg(test)]
fn persist_state(config: &AgentConfig, state: &StoredPairingState) -> anyhow::Result<()> {
    let _lock = lock_state(config)?;
    persist_state_unlocked(config, state)
}

fn persist_state_unlocked(config: &AgentConfig, state: &StoredPairingState) -> anyhow::Result<()> {
    let serialized = serde_json::to_string_pretty(state)?;
    persist_private_value(&state_path(config), &serialized, "browser pairing state")
}

fn compare_and_persist_creating(
    config: &AgentConfig,
    generation: Uuid,
    pairing_endpoint: &str,
    report_endpoint: &str,
    polling_secret: &str,
    next: &StoredPairingState,
) -> anyhow::Result<()> {
    let _lock = lock_state(config)?;
    let current = load_state(config)?;
    if !matches!(
        current,
        Some(StoredPairingState::Creating {
            generation: current_generation,
            pairing_endpoint: current_pairing_endpoint,
            report_endpoint: current_report_endpoint,
            polling_secret: current_polling_secret,
            ..
        }) if current_generation == generation
            && current_pairing_endpoint == pairing_endpoint
            && current_report_endpoint == report_endpoint
            && current_polling_secret == polling_secret
    ) {
        return Err(PairingSuperseded.into());
    }
    persist_state_unlocked(config, next)
}

fn compare_and_persist_pending(
    config: &AgentConfig,
    generation: Uuid,
    request_id: Uuid,
    pairing_endpoint: &str,
    report_endpoint: &str,
    polling_secret: &str,
    next: &StoredPairingState,
) -> anyhow::Result<()> {
    let _lock = lock_state(config)?;
    ensure_pending_is_current(
        config,
        generation,
        request_id,
        pairing_endpoint,
        report_endpoint,
        polling_secret,
    )?;
    persist_state_unlocked(config, next)
}

fn ensure_pending_is_current(
    config: &AgentConfig,
    generation: Uuid,
    request_id: Uuid,
    pairing_endpoint: &str,
    report_endpoint: &str,
    polling_secret: &str,
) -> anyhow::Result<()> {
    let current = load_state(config)?;
    if !matches!(
        current,
        Some(StoredPairingState::Pending {
            generation: current_generation,
            request_id: current_request_id,
            pairing_endpoint: current_pairing_endpoint,
            report_endpoint: current_report_endpoint,
            polling_secret: current_polling_secret,
            ..
        }) if current_generation == generation
            && current_request_id == request_id
            && current_pairing_endpoint == pairing_endpoint
            && current_report_endpoint == report_endpoint
            && current_polling_secret == polling_secret
    ) {
        return Err(PairingSuperseded.into());
    }
    Ok(())
}

fn apply_active_config(
    config: &mut AgentConfig,
    report_endpoint: &str,
    host_name: &Option<String>,
) {
    config.endpoint = report_endpoint.to_string();
    config.pairing_endpoint = None;
    config.host_name.clone_from(host_name);
}

fn persist_active_config_unlocked(
    config: &AgentConfig,
    report_endpoint: &str,
    host_name: &Option<String>,
) -> anyhow::Result<PathBuf> {
    let mut active = config.clone();
    apply_active_config(&mut active, report_endpoint, host_name);
    active.persist_after_pairing()
}

#[derive(Debug, thiserror::Error)]
#[error("browser pairing operation was superseded by a newer request; reloading saved state")]
struct PairingSuperseded;

fn validate_state_version(version: PairingStateVersion) -> anyhow::Result<()> {
    if version != PAIRING_STATE_VERSION {
        bail!("pairing state does not belong to the current Agent package");
    }
    Ok(())
}

fn random_secret() -> String {
    hex(&rand::random::<[u8; 32]>())
}

fn sha256_hex(secret: &str) -> String {
    hex(&Sha256::digest(secret.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(DIGITS[(byte >> 4) as usize] as char);
        value.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    value
}

fn resolve_activation_url(
    pairing_endpoint: &str,
    activation_url: &str,
    allow_insecure_http: bool,
) -> anyhow::Result<String> {
    let base = reqwest::Url::parse(pairing_endpoint).context("invalid pairing endpoint URL")?;
    let url = match reqwest::Url::parse(activation_url) {
        Ok(url) => url,
        Err(_) => base
            .join(activation_url)
            .context("invalid activation URL returned by UnionC")?,
    };
    if !url.username().is_empty() || url.password().is_some() {
        bail!("UnionC returned an activation URL containing credentials");
    }
    match url.scheme() {
        "https" => {}
        "http" if allow_insecure_http || is_loopback_host(url.host_str()) => {}
        "http" => bail!("UnionC returned an insecure non-loopback activation URL"),
        scheme => bail!("UnionC returned an unsupported activation URL scheme: {scheme}"),
    }
    Ok(url.to_string())
}

fn is_loopback_host(host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

async fn read_limited(response: reqwest::Response, target: &str) -> anyhow::Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PAIRING_RESPONSE_BYTES as u64)
    {
        bail!("{target} response exceeds the 64 KiB limit");
    }
    let mut response = response;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_PAIRING_RESPONSE_BYTES {
            bail!("{target} response exceeds the 64 KiB limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn pairing_response_content_type(response: &reqwest::Response) -> String {
    let raw = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("<missing>");
    pairing_content_type_for_diagnostics(raw)
}

fn pairing_content_type_for_diagnostics(content_type: &str) -> String {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match media_type.as_str() {
        "application/json" | "text/html" | "text/plain" | "application/octet-stream" => media_type,
        "<missing>" => media_type,
        value if value.starts_with("application/") && value.ends_with("+json") => {
            "application/*+json".to_string()
        }
        _ => "<unexpected>".to_string(),
    }
}

fn pairing_origin_for_diagnostics(endpoint: &str) -> String {
    reqwest::Url::parse(endpoint)
        .ok()
        .map(|url| url.origin().ascii_serialization())
        .filter(|origin| origin != "null")
        .unwrap_or_else(|| "<invalid Server origin>".to_string())
}

fn parse_pairing_json<T: DeserializeOwned>(
    body: &[u8],
    content_type: &str,
    endpoint: &str,
    response_kind: &str,
) -> anyhow::Result<T> {
    let invalid_response = || {
        let origin = pairing_origin_for_diagnostics(endpoint);
        let content_type = pairing_content_type_for_diagnostics(content_type);
        anyhow::anyhow!(
            "UnionC returned an unexpected or malformed {response_kind} from Server origin {origin} (HTTP 2xx Content-Type: {content_type}); the configured Server address or port may be wrong. Use the complete UnionC management-console origin, including its port"
        )
    };
    if pairing_content_type_for_diagnostics(content_type) != "application/json" {
        return Err(invalid_response());
    }
    serde_json::from_slice(body).map_err(|_| invalid_response())
}

fn ensure_pairing_status(
    status: StatusCode,
    allowed: &[StatusCode],
    body: &[u8],
    operation: &str,
) -> anyhow::Result<()> {
    if allowed.contains(&status) {
        return Ok(());
    }
    let detail: String = String::from_utf8_lossy(body).chars().take(512).collect();
    if status.is_success() {
        bail!("UnionC returned an unexpected HTTP {status} while attempting to {operation}");
    }
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        bail!(
            "UnionC refused to {operation}: HTTP {status}; start a new browser pairing ({detail})"
        );
    }
    bail!("UnionC failed to {operation}: HTTP {status}: {detail}")
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        path::PathBuf,
        sync::mpsc,
        thread,
    };

    use super::*;

    fn test_config(directory: PathBuf) -> AgentConfig {
        let config_path = directory.join("config.json");
        AgentConfig {
            endpoint: "https://unionc.example/api/agent/v1/report".into(),
            config_path: Some(config_path),
            state_dir: directory,
            ..AgentConfig::default()
        }
    }

    fn test_host() -> HostIdentity {
        HostIdentity {
            id: Uuid::new_v4().to_string(),
            name: "pairing-contract-test".into(),
            os: "test".into(),
            os_version: None,
            kernel_version: None,
            arch: "test".into(),
            agent_version: "test".into(),
        }
    }

    fn one_shot_pairing_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let request_id = Uuid::new_v4();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0_u8; 16 * 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(
                std::str::from_utf8(&request[..read])
                    .unwrap()
                    .starts_with("POST /api/agent/v2/pairing-requests ")
            );
            let body = serde_json::to_vec(&serde_json::json!({
                "request_id": request_id,
                "activation_url": format!("/agent/activate/{request_id}"),
                "expires_in": 600,
                "poll_interval": 1
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{address}"), handle)
    }

    fn delayed_active_server(
        instance_id: Uuid,
    ) -> (
        String,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0_u8; 16 * 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(
                std::str::from_utf8(&request[..read])
                    .unwrap()
                    .contains("/status ")
            );
            seen_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            let body = serde_json::to_vec(&serde_json::json!({
                "status": "active",
                "instance_id": instance_id
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{address}"), seen_rx, release_tx, handle)
    }

    fn delayed_activation_server(
        instance_id: Uuid,
    ) -> (
        String,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0_u8; 16 * 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(
                std::str::from_utf8(&request[..read])
                    .unwrap()
                    .starts_with("POST /api/agent/v2/activate ")
            );
            seen_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            let body = serde_json::to_vec(&serde_json::json!({
                "status": "active",
                "instance_id": instance_id
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{address}"), seen_rx, release_tx, handle)
    }

    #[test]
    fn generated_secrets_have_256_bits_and_hash_the_transmitted_form() {
        let secret = random_secret();
        assert_eq!(secret.len(), 64);
        assert!(secret.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(sha256_hex(&secret).len(), 64);
        assert_ne!(secret, sha256_hex(&secret));
    }

    #[test]
    fn create_request_contract_contains_hashes_but_not_raw_secrets() {
        let host = HostIdentity {
            id: Uuid::new_v4().to_string(),
            name: "contract-test".into(),
            os: "test".into(),
            os_version: None,
            kernel_version: None,
            arch: "test".into(),
            agent_version: "test".into(),
        };
        let bearer_secret = random_secret();
        let polling_secret = random_secret();
        let value = serde_json::to_value(CreatePairingRequest {
            host: &host,
            token_hash: sha256_hex(&bearer_secret),
            polling_secret_hash: sha256_hex(&polling_secret),
        })
        .unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 3);
        assert!(object.contains_key("host"));
        assert_eq!(object["token_hash"], sha256_hex(&bearer_secret));
        assert_eq!(object["polling_secret_hash"], sha256_hex(&polling_secret));
        let serialized = serde_json::to_string(&value).unwrap();
        assert!(!serialized.contains(&bearer_secret));
        assert!(!serialized.contains(&polling_secret));
    }

    #[test]
    fn status_contract_accepts_only_current_waiting_value() {
        let response: PairingStatusResponse = serde_json::from_value(serde_json::json!({
            "status": "waiting"
        }))
        .unwrap();
        assert!(matches!(response.status, PairingStatus::Waiting));
        assert!(response.instance_id.is_none());
        assert!(
            serde_json::from_value::<PairingStatusResponse>(serde_json::json!({
                "status": "pending"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PairingStatusResponse>(serde_json::json!({
                "status": "waiting",
                "pending": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PairingStatusResponse>(serde_json::json!({
                "status": "active",
                "instance_id": Uuid::new_v4().to_string().to_uppercase()
            }))
            .is_err()
        );
    }

    #[test]
    fn current_pairing_responses_and_local_auth_state_reject_unknown_fields() {
        assert!(
            serde_json::from_value::<CreatePairingResponse>(serde_json::json!({
                "request_id": Uuid::new_v4(),
                "activation_url": "https://unionc.example/agent/activate/request",
                "expires_in": 300,
                "poll_interval": 2,
                "enrollment_secret": "obsolete"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<CreatePairingResponse>(serde_json::json!({
                "request_id": Uuid::new_v4().to_string().replace('-', ""),
                "activation_url": "https://unionc.example/agent/activate/request",
                "expires_in": 300,
                "poll_interval": 2
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivatePairingResponse>(serde_json::json!({
                "instance_id": Uuid::new_v4(),
                "status": "active",
                "token": "obsolete"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivatePairingResponse>(serde_json::json!({
                "instance_id": Uuid::new_v4(),
                "status": "pending"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivatePairingResponse>(serde_json::json!({
                "instance_id": Uuid::new_v4().to_string().to_uppercase(),
                "status": "active"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<LocalAuthState>(serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "status": "authorized",
                "reason": "browser pairing completed",
                "changed_at": Utc::now(),
                "legacy": true
            }))
            .is_err()
        );
        for version in [
            serde_json::Value::Null,
            serde_json::json!(1),
            serde_json::json!("0.1.0"),
        ] {
            let mut state = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "status": "authorized",
                "reason": "browser pairing completed",
                "changed_at": Utc::now()
            });
            if version.is_null() {
                state.as_object_mut().unwrap().remove("version");
            } else {
                state["version"] = version;
            }
            assert!(serde_json::from_value::<LocalAuthState>(state).is_err());
        }
    }

    #[test]
    fn non_json_success_points_to_the_server_origin_without_leaking_the_body() {
        let endpoint = "http://127.0.0.1/api/agent/v2/pairing-requests";
        let body = b"<!doctype html><title>POETIZE private marker</title>";
        let error = parse_pairing_json::<CreatePairingResponse>(
            body,
            "text/html; charset=utf-8",
            endpoint,
            "pairing response",
        )
        .err()
        .expect("HTML must not be accepted as a pairing response");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("Server origin http://127.0.0.1"));
        assert!(!rendered.contains("/api/agent/v2/pairing-requests"));
        assert!(rendered.contains("Content-Type: text/html"));
        assert!(rendered.contains("address or port may be wrong"));
        assert!(rendered.contains("including its port"));
        assert!(!rendered.contains("POETIZE"));
        assert!(!rendered.contains("private marker"));

        let valid_json = serde_json::to_vec(&serde_json::json!({
            "request_id": Uuid::new_v4(),
            "activation_url": "/agent/activate/request",
            "expires_in": 600,
            "poll_interval": 2
        }))
        .unwrap();
        assert!(
            parse_pairing_json::<CreatePairingResponse>(
                &valid_json,
                "text/plain",
                endpoint,
                "pairing response"
            )
            .is_err()
        );
        assert!(
            parse_pairing_json::<CreatePairingResponse>(
                &valid_json,
                "application/vnd.unionc+json",
                endpoint,
                "pairing response"
            )
            .is_err()
        );
    }

    #[test]
    fn pairing_operations_accept_only_their_current_http_statuses() {
        assert!(
            ensure_pairing_status(
                StatusCode::OK,
                &[StatusCode::OK, StatusCode::CREATED],
                b"",
                "create pairing request"
            )
            .is_ok()
        );
        assert!(
            ensure_pairing_status(
                StatusCode::CREATED,
                &[StatusCode::OK, StatusCode::CREATED],
                b"",
                "create pairing request"
            )
            .is_ok()
        );
        for operation in [
            "poll pairing status",
            "submit the one-time authorization key",
        ] {
            assert!(
                ensure_pairing_status(StatusCode::OK, &[StatusCode::OK], b"", operation).is_ok()
            );
            assert!(
                ensure_pairing_status(StatusCode::NO_CONTENT, &[StatusCode::OK], b"", operation)
                    .is_err()
            );
        }
    }

    #[test]
    fn malformed_json_source_and_endpoint_secrets_are_fully_redacted() {
        let marker = "uci_SECRET_MARKER_MUST_NOT_LEAK";
        let body = format!(r#"{{"status":"{marker}"}}"#);
        let endpoint = format!(
            "https://user:{marker}@unionc.example/api/agent/v2/pairing-requests?key={marker}#{marker}"
        );
        let error = parse_pairing_json::<PairingStatusResponse>(
            body.as_bytes(),
            "application/json",
            &endpoint,
            "pairing status response",
        )
        .err()
        .expect("an unknown status must not be accepted");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("Server origin https://unionc.example"));
        assert!(rendered.contains("Content-Type: application/json"));
        assert!(!rendered.contains(marker));
        assert!(!rendered.contains("unknown variant"));
        assert!(!rendered.contains("Caused by"));
    }

    #[test]
    fn diagnostic_content_type_does_not_echo_parameters_or_unknown_values() {
        let marker = "uci_SECRET_MARKER_MUST_NOT_LEAK";
        assert_eq!(
            pairing_content_type_for_diagnostics(&format!("text/html; reflected={marker}")),
            "text/html"
        );
        assert_eq!(
            pairing_content_type_for_diagnostics(&format!("application/{marker}")),
            "<unexpected>"
        );
    }

    #[test]
    fn relative_activation_url_is_resolved_to_the_console_origin() {
        assert_eq!(
            resolve_activation_url(
                "https://unionc.example/api/agent/v2/pairing-requests",
                "/agent/activate/00000000-0000-4000-8000-000000000001",
                false,
            )
            .unwrap(),
            "https://unionc.example/agent/activate/00000000-0000-4000-8000-000000000001"
        );
    }

    #[test]
    fn activation_endpoint_and_public_url_stay_bound_to_the_pairing_origin() {
        let request_id = Uuid::new_v4();
        assert_eq!(
            activation_endpoint("https://unionc.example/prefix/api/agent/v2/pairing-requests")
                .unwrap()
                .as_str(),
            "https://unionc.example/prefix/api/agent/v2/activate"
        );
        validate_activation_url_request(
            &format!("https://unionc.example/agent/activate/{request_id}"),
            "https://unionc.example/prefix/api/agent/v2/pairing-requests",
            request_id,
        )
        .unwrap();
        assert!(
            validate_activation_url_request(
                &format!("https://attacker.example/agent/activate/{request_id}"),
                "https://unionc.example/api/agent/v2/pairing-requests",
                request_id,
            )
            .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn service_activation_commit_wins_the_post_response_race_idempotently() {
        let directory =
            std::env::temp_dir().join(format!("unionc-activation-race-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let instance_id = Uuid::new_v4();
        let (server, request_seen, release_response, server_thread) =
            delayed_activation_server(instance_id);
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let config = AgentConfig {
            endpoint: format!("{server}/api/agent/v1/report"),
            pairing_endpoint: Some(format!("{server}/api/agent/v2/pairing-requests")),
            state_dir: directory.clone(),
            allow_insecure_http: true,
            ..AgentConfig::default()
        };
        persist_state(
            &config,
            &StoredPairingState::Pending {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: format!("{server}/agent/activate/{request_id}"),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 1,
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: random_secret(),
                host_name: None,
                polling_secret: random_secret(),
            },
        )
        .unwrap();

        let activation_config = config.clone();
        let activation = tokio::spawn(async move {
            activate_pending_with_code(
                &activation_config,
                generation,
                request_id,
                "uci_test_authorization_key",
            )
            .await
        });
        request_seen
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        persist_state(
            &config,
            &StoredPairingState::Active {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: format!("{server}/agent/activate/{request_id}"),
                instance_id,
                report_endpoint: config.endpoint.clone(),
                host_name: None,
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        release_response.send(()).unwrap();
        assert_eq!(activation.await.unwrap().unwrap(), Some(instance_id));
        server_thread.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pending_state_round_trips_privately() {
        let directory = std::env::temp_dir().join(format!("unionc-pairing-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        let state = StoredPairingState::Pending {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            activation_url: "https://unionc.example/agent/activate/test".into(),
            expires_at: Utc::now(),
            poll_interval: 5,
            pairing_endpoint: config.pairing_endpoint(),
            report_endpoint: config.endpoint.clone(),
            bearer_secret: random_secret(),
            host_name: None,
            polling_secret: random_secret(),
        };
        persist_state(&config, &state).unwrap();
        assert!(matches!(
            load_state(&config).unwrap(),
            Some(StoredPairingState::Pending { .. })
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(state_path(&config))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn creating_state_round_trips_the_same_secrets_for_idempotent_retry() {
        let directory = std::env::temp_dir().join(format!("unionc-creating-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        let bearer_secret = random_secret();
        let polling_secret = random_secret();
        let state = StoredPairingState::Creating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            pairing_endpoint: config.pairing_endpoint(),
            report_endpoint: config.endpoint.clone(),
            host: HostIdentity {
                id: Uuid::new_v4().to_string(),
                name: "resume-test".into(),
                os: "test".into(),
                os_version: None,
                kernel_version: None,
                arch: "test".into(),
                agent_version: "test".into(),
            },
            host_name: None,
            bearer_secret: bearer_secret.clone(),
            polling_secret: polling_secret.clone(),
        };
        let mut encoded = serde_json::to_value(&state).unwrap();
        assert_eq!(encoded["version"], env!("CARGO_PKG_VERSION"));
        encoded["version"] = serde_json::json!(1);
        assert!(serde_json::from_value::<StoredPairingState>(encoded).is_err());
        persist_state(&config, &state).unwrap();
        assert!(matches!(
            load_state(&config).unwrap(),
            Some(StoredPairingState::Creating {
                bearer_secret: saved_bearer,
                polling_secret: saved_polling,
                ..
            }) if saved_bearer == bearer_secret && saved_polling == polling_secret
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn live_pending_request_cannot_be_silently_moved_to_another_server() {
        let directory =
            std::env::temp_dir().join(format!("unionc-pending-origin-{}", Uuid::new_v4()));
        let mut config = test_config(directory.clone());
        fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.json");
        config.config_path = Some(config_path.clone());
        let old_config = serde_json::to_vec(&config).unwrap();
        fs::write(&config_path, &old_config).unwrap();
        fs::write(directory.join("agent-token"), "existing-long-lived-token").unwrap();
        let state = StoredPairingState::Pending {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            activation_url: "https://old.example/agent/activate/test".into(),
            expires_at: Utc::now() + TimeDelta::minutes(10),
            poll_interval: 5,
            pairing_endpoint: "https://old.example/api/agent/v2/pairing-requests".into(),
            report_endpoint: "https://old.example/api/agent/v1/report".into(),
            bearer_secret: random_secret(),
            host_name: None,
            polling_secret: random_secret(),
        };
        persist_state(&config, &state).unwrap();
        config.endpoint = "https://new.example/api/agent/v1/report".into();
        let error = start_or_resume(&config, &test_host())
            .await
            .expect_err("a live request must stay bound to its original server");
        assert!(error.to_string().contains("different UnionC server"));
        assert!(matches!(
            load_state(&config).unwrap(),
            Some(StoredPairingState::Pending { pairing_endpoint, .. })
                if pairing_endpoint.starts_with("https://old.example/")
        ));
        assert_eq!(fs::read(&config_path).unwrap(), old_config);
        assert_eq!(
            fs::read_to_string(directory.join("agent-token")).unwrap(),
            "existing-long-lived-token"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn interrupted_create_cannot_be_silently_moved_to_another_server() {
        let directory =
            std::env::temp_dir().join(format!("unionc-creating-origin-{}", Uuid::new_v4()));
        let mut config = test_config(directory.clone());
        let state = StoredPairingState::Creating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            pairing_endpoint: "https://old.example/api/agent/v2/pairing-requests".into(),
            report_endpoint: "https://old.example/api/agent/v1/report".into(),
            host: test_host(),
            host_name: None,
            bearer_secret: random_secret(),
            polling_secret: random_secret(),
        };
        persist_state(&config, &state).unwrap();
        config.endpoint = "https://new.example/api/agent/v1/report".into();
        let error = start_or_resume(&config, &test_host())
            .await
            .expect_err("an interrupted create must stay bound to its original server");
        assert!(error.to_string().contains("different UnionC server"));
        assert!(matches!(
            load_state(&config).unwrap(),
            Some(StoredPairingState::Creating { pairing_endpoint, .. })
                if pairing_endpoint.starts_with("https://old.example/")
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn confirmed_tray_replacement_can_replace_mismatched_incomplete_states() {
        for old_state in ["creating", "pending"] {
            let directory = std::env::temp_dir().join(format!(
                "unionc-confirmed-replace-{old_state}-{}",
                Uuid::new_v4()
            ));
            let (server, server_thread) = one_shot_pairing_server();
            let mut config = AgentConfig {
                endpoint: format!("{server}/api/agent/v1/report"),
                state_dir: directory.clone(),
                replace_pending_pairing: true,
                ..AgentConfig::default()
            };
            config.pairing_endpoint = Some(format!("{server}/api/agent/v2/pairing-requests"));
            let state = if old_state == "creating" {
                StoredPairingState::Creating {
                    version: PAIRING_STATE_VERSION,
                    generation: Uuid::new_v4(),
                    pairing_endpoint: "https://old.example/api/agent/v2/pairing-requests".into(),
                    report_endpoint: "https://old.example/api/agent/v1/report".into(),
                    host: test_host(),
                    host_name: None,
                    bearer_secret: random_secret(),
                    polling_secret: random_secret(),
                }
            } else {
                StoredPairingState::Pending {
                    version: PAIRING_STATE_VERSION,
                    generation: Uuid::new_v4(),
                    request_id: Uuid::new_v4(),
                    activation_url: "https://old.example/agent/activate/test".into(),
                    expires_at: Utc::now() + TimeDelta::minutes(10),
                    poll_interval: 5,
                    pairing_endpoint: "https://old.example/api/agent/v2/pairing-requests".into(),
                    report_endpoint: "https://old.example/api/agent/v1/report".into(),
                    bearer_secret: random_secret(),
                    host_name: None,
                    polling_secret: random_secret(),
                }
            };
            persist_state(&config, &state).unwrap();
            let session = start_or_resume(&config, &test_host())
                .await
                .expect("the explicitly confirmed new origin should replace incomplete state");
            assert!(session.activation_url.starts_with(&server));
            assert!(matches!(
                load_state(&config).unwrap(),
                Some(StoredPairingState::Pending { pairing_endpoint, .. })
                    if pairing_endpoint == format!("{server}/api/agent/v2/pairing-requests")
            ));
            server_thread.join().unwrap();
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delayed_old_activation_cannot_overwrite_a_replacement_generation() {
        let directory =
            std::env::temp_dir().join(format!("unionc-delayed-active-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let old_instance_id = Uuid::new_v4();
        let (old_server, request_seen, release_response, old_thread) =
            delayed_active_server(old_instance_id);
        let old_config_path = directory.join("config.json");
        let old_pairing_endpoint = format!("{old_server}/api/agent/v2/pairing-requests");
        let old_report_endpoint = format!("{old_server}/api/agent/v1/report");
        let old_config = AgentConfig {
            endpoint: old_report_endpoint.clone(),
            pairing_endpoint: Some(old_pairing_endpoint.clone()),
            state_dir: directory.clone(),
            config_path: Some(old_config_path.clone()),
            ..AgentConfig::default()
        };
        let old_config_bytes = serde_json::to_vec(&old_config).unwrap();
        fs::write(&old_config_path, &old_config_bytes).unwrap();
        fs::write(directory.join("agent-token"), "old-long-lived-token").unwrap();
        let old_host_id = Uuid::new_v4();
        fs::write(directory.join("host-id"), old_host_id.to_string()).unwrap();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let old_state = StoredPairingState::Pending {
            version: PAIRING_STATE_VERSION,
            generation,
            request_id,
            activation_url: format!("{old_server}/agent/activate/{request_id}"),
            expires_at: Utc::now() + TimeDelta::minutes(10),
            poll_interval: 1,
            pairing_endpoint: old_pairing_endpoint.clone(),
            report_endpoint: old_report_endpoint.clone(),
            bearer_secret: random_secret(),
            host_name: None,
            polling_secret: random_secret(),
        };
        persist_state(&old_config, &old_state).unwrap();
        let polling_config = old_config.clone();
        let stale_poll = tokio::spawn(async move { poll_existing(&polling_config).await });
        request_seen
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();

        let (new_server, new_thread) = one_shot_pairing_server();
        let mut new_config = AgentConfig {
            endpoint: format!("{new_server}/api/agent/v1/report"),
            pairing_endpoint: Some(format!("{new_server}/api/agent/v2/pairing-requests")),
            state_dir: directory.clone(),
            config_path: Some(old_config_path.clone()),
            replace_pending_pairing: true,
            ..AgentConfig::default()
        };
        new_config.allow_insecure_http = true;
        let new_session = start_or_resume(&new_config, &test_host()).await.unwrap();
        release_response.send(()).unwrap();
        let stale_error = stale_poll
            .await
            .unwrap()
            .expect_err("the delayed old Active response must lose its generation CAS");
        assert!(stale_error.is::<PairingSuperseded>());
        assert!(matches!(
            load_state(&new_config).unwrap(),
            Some(StoredPairingState::Pending {
                generation: saved_generation,
                pairing_endpoint,
                ..
            }) if saved_generation == new_session.generation
                && pairing_endpoint.starts_with(&new_server)
        ));
        assert_eq!(
            fs::read_to_string(directory.join("agent-token")).unwrap(),
            "old-long-lived-token"
        );
        assert_eq!(
            fs::read_to_string(directory.join("host-id")).unwrap(),
            old_host_id.to_string()
        );
        assert_eq!(fs::read(&old_config_path).unwrap(), old_config_bytes);
        old_thread.join().unwrap();
        new_thread.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn activating_journal_recovers_all_endpoint_bound_files() {
        for preexisting in [false, true] {
            let directory = std::env::temp_dir().join(format!(
                "unionc-activating-recovery-{preexisting}-{}",
                Uuid::new_v4()
            ));
            fs::create_dir_all(&directory).unwrap();
            let config_path = directory.join("config.json");
            let mut config = AgentConfig {
                endpoint: "https://old.example/api/agent/v1/report".into(),
                host_name: Some("old-service-name".into()),
                state_dir: directory.clone(),
                config_path: Some(config_path.clone()),
                ..AgentConfig::default()
            };
            if preexisting {
                fs::write(directory.join("agent-token"), "old-token").unwrap();
                fs::write(directory.join("host-id"), Uuid::new_v4().to_string()).unwrap();
                fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            }
            let generation = Uuid::new_v4();
            let request_id = Uuid::new_v4();
            let instance_id = Uuid::new_v4();
            let new_token = random_secret();
            persist_state(
                &config,
                &StoredPairingState::Activating {
                    version: PAIRING_STATE_VERSION,
                    generation,
                    request_id,
                    activation_url: "https://new.example/agent/activate/test".into(),
                    expires_at: Utc::now() + TimeDelta::minutes(10),
                    poll_interval: 1,
                    instance_id,
                    pairing_endpoint: "https://new.example/api/agent/v2/pairing-requests".into(),
                    report_endpoint: "https://new.example/api/agent/v1/report".into(),
                    bearer_secret: new_token.clone(),
                    host_name: Some("tray-selected-name".into()),
                },
            )
            .unwrap();

            let progress = poll_existing(&config).await.unwrap().unwrap();
            assert!(matches!(
                progress,
                PairingProgress::Active {
                    generation: saved_generation,
                    request_id: saved_request,
                    instance_id: saved_instance,
                    ..
                } if saved_generation == generation
                    && saved_request == request_id
                    && saved_instance == instance_id
            ));
            assert_eq!(
                fs::read_to_string(directory.join("agent-token")).unwrap(),
                new_token
            );
            assert_eq!(
                fs::read_to_string(directory.join("host-id")).unwrap(),
                instance_id.to_string()
            );
            let durable: AgentConfig =
                serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
            assert_eq!(durable.endpoint, "https://new.example/api/agent/v1/report");
            assert_eq!(durable.host_name.as_deref(), Some("tray-selected-name"));
            assert!(matches!(
                load_state(&config).unwrap(),
                Some(StoredPairingState::Active {
                    generation: saved_generation,
                    ..
                }) if saved_generation == generation
            ));
            let mut host = test_host();
            activate_reporter_snapshot(
                &mut config,
                &mut host,
                generation,
                request_id,
                instance_id,
                "https://new.example/api/agent/v1/report",
            )
            .unwrap();
            assert_eq!(config.host_name.as_deref(), Some("tray-selected-name"));
            assert_eq!(host.name, "tray-selected-name");
            let durable_after_snapshot: AgentConfig =
                serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
            assert_eq!(
                durable_after_snapshot.host_name.as_deref(),
                Some("tray-selected-name")
            );
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn run_keeps_the_current_credential_during_a_non_active_repair() {
        let directory =
            std::env::temp_dir().join(format!("unionc-current-reporter-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("agent-token"), "current-long-lived-token").unwrap();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let states = [
            StoredPairingState::Creating {
                version: PAIRING_STATE_VERSION,
                generation,
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                host: test_host(),
                host_name: None,
                bearer_secret: random_secret(),
                polling_secret: random_secret(),
            },
            StoredPairingState::Pending {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://unionc.example/agent/activate/test".into(),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 5,
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: random_secret(),
                host_name: None,
                polling_secret: random_secret(),
            },
        ];
        persist_state(&config, &states[1]).unwrap();
        assert!(
            existing_reporter_for_run(&config).unwrap().is_none(),
            "a token and pairing journal without current authorized state must be rejected"
        );
        persist_auth_state(
            &config,
            &LocalAuthState {
                version: PAIRING_STATE_VERSION,
                status: "authorized".into(),
                reason: "current pairing completed".into(),
                changed_at: Utc::now(),
            },
        )
        .unwrap();
        for state in states {
            persist_state(&config, &state).unwrap();
            assert!(existing_reporter_for_run(&config).unwrap().is_some());
        }

        persist_state(
            &config,
            &StoredPairingState::Denied {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://unionc.example/agent/activate/test".into(),
                report_endpoint: config.endpoint.clone(),
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        assert!(existing_reporter_for_run(&config).unwrap().is_none());

        fs::remove_file(directory.join(PAIRING_STATE_FILE)).unwrap();
        assert!(
            existing_reporter_for_run(&config).unwrap().is_none(),
            "a raw token without current package-version pairing state must be rejected"
        );

        fs::write(directory.join("agent-token"), "active-token").unwrap();
        persist_state(
            &config,
            &StoredPairingState::Active {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://unionc.example/agent/activate/test".into(),
                instance_id: Uuid::new_v4(),
                report_endpoint: config.endpoint.clone(),
                host_name: None,
                completed_at: Utc::now(),
            },
        )
        .unwrap();
        assert!(existing_reporter_for_run(&config).unwrap().is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stale_delivery_cannot_block_a_new_active_generation() {
        let directory = std::env::temp_dir().join(format!("unionc-reauth-cas-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        let old_generation = Uuid::new_v4();
        let old_request = Uuid::new_v4();
        let active = |generation, request_id| StoredPairingState::Active {
            version: PAIRING_STATE_VERSION,
            generation,
            request_id,
            activation_url: "https://unionc.example/agent/activate/test".into(),
            instance_id: Uuid::new_v4(),
            report_endpoint: config.endpoint.clone(),
            host_name: None,
            completed_at: Utc::now(),
        };
        persist_state(&config, &active(old_generation, old_request)).unwrap();
        assert!(
            mark_reauth_required_if_current(
                &config,
                Some((old_generation, old_request)),
                "current 401",
            )
            .unwrap()
        );

        persist_state(&config, &active(Uuid::new_v4(), Uuid::new_v4())).unwrap();
        assert!(
            !mark_reauth_required_if_current(
                &config,
                Some((old_generation, old_request)),
                "stale 403",
            )
            .unwrap()
        );

        persist_state(
            &config,
            &StoredPairingState::Pending {
                version: PAIRING_STATE_VERSION,
                generation: Uuid::new_v4(),
                request_id: Uuid::new_v4(),
                activation_url: "https://unionc.example/agent/activate/test".into(),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 5,
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: random_secret(),
                host_name: None,
                polling_secret: random_secret(),
            },
        )
        .unwrap();
        assert!(
            mark_reauth_required_if_current(
                &config,
                Some((old_generation, old_request)),
                "old reporter rejected during pending repair",
            )
            .unwrap()
        );

        persist_state(
            &config,
            &StoredPairingState::Activating {
                version: PAIRING_STATE_VERSION,
                generation: Uuid::new_v4(),
                request_id: Uuid::new_v4(),
                activation_url: "https://unionc.example/agent/activate/test".into(),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 5,
                instance_id: Uuid::new_v4(),
                pairing_endpoint: config.pairing_endpoint(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: random_secret(),
                host_name: None,
            },
        )
        .unwrap();
        assert!(
            !mark_reauth_required_if_current(
                &config,
                Some((old_generation, old_request)),
                "stale while new activation commits",
            )
            .unwrap()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn revoked_authorization_state_is_explicit() {
        let directory = std::env::temp_dir().join(format!("unionc-revoked-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        mark_reauth_required(&config, "HTTP 403 revoked").unwrap();
        let state = local_auth_state(&config).unwrap().unwrap();
        assert_eq!(state.status, "reauth_required");
        assert!(state.reason.contains("403"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn local_inspection_does_not_create_a_lock_or_state_directory() {
        let directory =
            std::env::temp_dir().join(format!("unionc-read-only-status-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());

        assert!(local_progress(&config).unwrap().is_none());
        assert!(local_auth_state(&config).unwrap().is_none());
        assert!(
            !directory.exists(),
            "read-only status inspection must not create the state directory"
        );
    }

    #[test]
    fn local_inspection_does_not_publish_an_activating_credential() {
        let directory =
            std::env::temp_dir().join(format!("unionc-read-only-activating-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        let state = StoredPairingState::Activating {
            version: PAIRING_STATE_VERSION,
            generation: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            activation_url: "https://unionc.example/agent/activate/test".into(),
            expires_at: Utc::now() + TimeDelta::minutes(10),
            poll_interval: 5,
            instance_id: Uuid::new_v4(),
            pairing_endpoint: config.pairing_endpoint(),
            report_endpoint: config.endpoint.clone(),
            bearer_secret: random_secret(),
            host_name: None,
        };
        persist_state(&config, &state).unwrap();
        let state_path = state_path(&config);
        let before = fs::read(&state_path).unwrap();

        assert!(matches!(
            local_progress(&config).unwrap(),
            Some(PairingProgress::Creating { .. })
        ));
        assert_eq!(fs::read(&state_path).unwrap(), before);
        assert!(!directory.join("agent-token").exists());
        assert!(!directory.join("host-id").exists());
        assert!(!directory.join("auth-state.json").exists());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn activation_atomically_commits_server_identity_and_token() {
        let directory = std::env::temp_dir().join(format!("unionc-activation-{}", Uuid::new_v4()));
        let config = test_config(directory.clone());
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("host-id"), Uuid::new_v4().to_string()).unwrap();
        fs::write(directory.join("agent-token"), "old-token").unwrap();
        let instance_id = Uuid::new_v4();
        let bearer_secret = random_secret();
        let polling_secret = random_secret();
        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let pairing_endpoint = config.pairing_endpoint();
        persist_state(
            &config,
            &StoredPairingState::Pending {
                version: PAIRING_STATE_VERSION,
                generation,
                request_id,
                activation_url: "https://unionc.example/agent/activate/test".into(),
                expires_at: Utc::now() + TimeDelta::minutes(10),
                poll_interval: 5,
                pairing_endpoint: pairing_endpoint.clone(),
                report_endpoint: config.endpoint.clone(),
                bearer_secret: bearer_secret.clone(),
                host_name: None,
                polling_secret: polling_secret.clone(),
            },
        )
        .unwrap();

        persist_active_credentials(&config, load_state(&config).unwrap().unwrap(), instance_id)
            .unwrap();

        assert_eq!(
            fs::read_to_string(directory.join("host-id")).unwrap(),
            instance_id.to_string()
        );
        assert_eq!(
            fs::read_to_string(directory.join("agent-token")).unwrap(),
            bearer_secret
        );
        assert!(matches!(
            load_state(&config).unwrap(),
            Some(StoredPairingState::Active {
                instance_id: saved,
                ..
            }) if saved == instance_id
        ));
        assert_eq!(
            local_auth_state(&config).unwrap().unwrap().status,
            "authorized"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
