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
        ..
    } = state
    else {
        bail!("internal error: expected an activating pairing state");
    };
    validate_state_version(version)?;
    config
        .validate_durable_report_endpoint(&report_endpoint)
        .context("stored report endpoint is unsafe")?;
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
    persist_active_binding_unlocked(
        config,
        &ActiveBinding {
            version: PAIRING_STATE_VERSION,
            generation,
            request_id,
            instance_id,
            report_endpoint: report_endpoint.clone(),
        },
    )?;
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
