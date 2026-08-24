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

/// Read pairing progress and its active endpoint binding as one stable, byte-for-byte read-only
/// snapshot. Re-reading the journal prevents an atomic replacement racing with the binding read
/// from manufacturing a false mismatch in diagnostics.
pub fn local_status(config: &AgentConfig) -> anyhow::Result<LocalPairingStatus> {
    const MAX_SNAPSHOT_ATTEMPTS: usize = 3;

    for _ in 0..MAX_SNAPSHOT_ATTEMPTS {
        let state = load_state(config)?;
        if !matches!(state.as_ref(), Some(StoredPairingState::Active { .. })) {
            return Ok(LocalPairingStatus {
                progress: state.map(progress_from_terminal),
                active_report_endpoint: None,
                active_binding_persisted: false,
            });
        }
        let active = state.expect("checked Active pairing state above");
        let expected = binding_from_active_state(&active)?;
        let binding = load_active_binding(config);
        let current = load_state(config)?;
        let current_binding = match current.as_ref() {
            Some(current @ StoredPairingState::Active { .. }) => {
                Some(binding_from_active_state(current)?)
            }
            _ => None,
        };
        if current_binding.as_ref() != Some(&expected) {
            continue;
        }
        let persisted = match binding? {
            Some(binding) if binding == expected => true,
            Some(_) => bail!("active binding does not match the current Active pairing state"),
            // A current-version Active journal from an installation upgraded in place remains
            // usable; run/pair will migrate it while holding the transaction lock.
            None => false,
        };
        return Ok(LocalPairingStatus {
            progress: Some(progress_from_terminal(active)),
            active_report_endpoint: Some(expected.report_endpoint),
            active_binding_persisted: persisted,
        });
    }
    bail!("pairing state changed repeatedly while reading local status")
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
                | StoredPairingState::Denied { .. }
                | StoredPairingState::Expired { .. }
        )
    ))
}

fn config_for_active_binding(config: &AgentConfig, binding: &ActiveBinding) -> AgentConfig {
    let mut active = config.clone();
    apply_active_config(&mut active, &binding.report_endpoint);
    active
}

fn reporter_for_active_binding_unlocked(
    config: &AgentConfig,
    binding: &ActiveBinding,
) -> anyhow::Result<Option<Reporter>> {
    let durable_host = load_host_identity(&config.state_dir)?;
    if durable_host.id != binding.instance_id.to_string() {
        bail!("stored host identity does not match the active endpoint binding");
    }
    Reporter::for_existing_credential(&config_for_active_binding(config, binding))
}

/// Return a consistent snapshot of the previously active reporter while a
/// new pairing attempt is incomplete. Reading the durable endpoint binding
/// and token under the same cross-process lock prevents observing a token with
/// an unrelated base configuration endpoint.
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
        Some(
            StoredPairingState::Creating { .. }
            | StoredPairingState::Pending { .. }
            | StoredPairingState::Denied { .. }
            | StoredPairingState::Expired { .. },
        ) => match load_active_binding(config)? {
            Some(binding) => reporter_for_active_binding_unlocked(config, &binding),
            // Compatibility for an upgrade that was already mid-pairing before bindings existed.
            None => Reporter::for_existing_credential(config),
        },
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
    let Some(state @ StoredPairingState::Active { .. }) = load_state(config)?
    else {
        return Ok(None);
    };
    let expected = binding_from_active_state(&state)?;
    let binding = load_or_migrate_active_binding_unlocked(config, &expected)?;
    reporter_for_active_binding_unlocked(config, &binding)
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
    let expected =
        ensure_active_is_current(config, generation, request_id, instance_id, report_endpoint)?;
    let binding = load_or_migrate_active_binding_unlocked(config, &expected)?;
    let path = persist_active_config_unlocked(config, &binding.report_endpoint)?;
    apply_active_config(config, &binding.report_endpoint);
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
    let expected =
        ensure_active_is_current(config, generation, request_id, instance_id, report_endpoint)?;
    let binding = load_or_migrate_active_binding_unlocked(config, &expected)?;
    let reporter_config = config_for_active_binding(config, &binding);
    apply_active_config(config, &binding.report_endpoint);
    let durable_host = load_host_identity(&config.state_dir)?;
    let durable_host_id = Uuid::parse_str(&durable_host.id)
        .context("durable host identity contains an invalid UUID")?;
    if durable_host_id != binding.instance_id {
        bail!(
            "paired host identity mismatch: state contains {}, server assigned {instance_id}; run pair again",
            durable_host.id
        );
    }
    *host = durable_host;
    Reporter::for_existing_credential(&reporter_config)?
        .context("paired host credential is missing after the Active pairing transaction")
}

fn ensure_active_is_current(
    config: &AgentConfig,
    generation: Uuid,
    request_id: Uuid,
    instance_id: Uuid,
    report_endpoint: &str,
) -> anyhow::Result<ActiveBinding> {
    let current = load_state(config)?;
    match current.as_ref() {
        Some(state @ StoredPairingState::Active {
            generation: current_generation,
            request_id: current_request_id,
            instance_id: current_instance_id,
            report_endpoint: current_report_endpoint,
            ..
        }) if *current_generation == generation
            && *current_request_id == request_id
            && *current_instance_id == instance_id
            && current_report_endpoint == report_endpoint
        => binding_from_active_state(state),
        _ => Err(PairingSuperseded.into()),
    }
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
