#[cfg(test)]
fn persist_state(config: &AgentConfig, state: &StoredPairingState) -> anyhow::Result<()> {
    let _lock = lock_state(config)?;
    persist_state_unlocked(config, state)
}

fn persist_state_unlocked(config: &AgentConfig, state: &StoredPairingState) -> anyhow::Result<()> {
    let serialized = serde_json::to_string_pretty(state)?;
    persist_private_value(&state_path(config), &serialized, "browser pairing state")
}

fn persist_active_binding_unlocked(
    config: &AgentConfig,
    binding: &ActiveBinding,
) -> anyhow::Result<()> {
    validate_active_binding(config, binding)?;
    persist_private_value(
        &active_binding_path(config),
        &serde_json::to_string_pretty(binding)?,
        "active credential endpoint binding",
    )
}

fn load_or_migrate_active_binding_unlocked(
    config: &AgentConfig,
    expected: &ActiveBinding,
) -> anyhow::Result<ActiveBinding> {
    match load_active_binding(config)? {
        Some(binding) if binding == *expected => Ok(binding),
        Some(_) => bail!("active binding does not match the current Active pairing state"),
        None => {
            persist_active_binding_unlocked(config, expected)?;
            Ok(expected.clone())
        }
    }
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

#[derive(Debug, thiserror::Error)]
#[error("browser pairing operation was superseded by a newer request; reloading saved state")]
struct PairingSuperseded;

fn validate_state_version(version: PairingStateVersion) -> anyhow::Result<()> {
    if version != PAIRING_STATE_VERSION {
        bail!("pairing state does not belong to the current Agent package");
    }
    Ok(())
}
