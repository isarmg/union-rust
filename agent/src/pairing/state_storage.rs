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

// These fragments intentionally remain in this module scope. Pairing commit
// and compare-and-swap helpers share private state-machine invariants; an
// `include!` split keeps those boundaries private while making the source
// navigable and keeping tests out of the production flow file.
include!("state_io.rs");
include!("tests.rs");
