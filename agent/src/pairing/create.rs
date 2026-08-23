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
        }) if !config.replace_pending_pairing
            && pairing_endpoints_match(config, &pairing_endpoint, &report_endpoint) =>
        {
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
            if !config.replace_pending_pairing
                && pairing_endpoints_match(config, pairing_endpoint, report_endpoint)
            {
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
        Some(state @ StoredPairingState::Active { .. }) => {
            // Preserve the old credential's endpoint before a new Creating state replaces the
            // only journal that carried it. Existing installations migrate lazily here.
            let expected = binding_from_active_state(&state)?;
            load_or_migrate_active_binding_unlocked(config, &expected)?;
        }
        _ => {}
    }

    let creating = StoredPairingState::Creating {
        version: PAIRING_STATE_VERSION,
        generation: Uuid::new_v4(),
        pairing_endpoint: config.pairing_endpoint(),
        report_endpoint: config.endpoint.clone(),
        host: host.clone(),
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
        bearer_secret,
        polling_secret,
    } = state
    else {
        bail!("internal error: expected a creating pairing state");
    };
    validate_state_version(version)?;
    // Pairing state survives upgrades and is an input in its own right. Do not
    // rely only on validation of today's config: an older state file may have
    // persisted a remote plaintext bootstrap endpoint under looser rules.
    crate::config::validate_pairing_endpoint(&pairing_endpoint)
        .context("stored pairing endpoint is unsafe")?;
    config
        .validate_durable_report_endpoint(&report_endpoint)
        .context("stored report endpoint is unsafe")?;
    let client = build_client(config)?;
    let response = client
        .post(&pairing_endpoint)
        .json(&CreatePairingRequest {
            host: host.clone(),
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
    let created_request_id = Uuid::parse_str(&created.request_id)
        .expect("protocol rejected a non-canonical pairing request UUID");
    if created.expires_in == 0 || created.expires_in > 7 * 24 * 60 * 60 {
        bail!("UnionC returned an invalid pairing expiration");
    }
    if created.poll_interval == 0 || created.poll_interval > 300 {
        bail!("UnionC returned an invalid pairing poll interval");
    }
    let activation_url = resolve_activation_url(&pairing_endpoint, &created.activation_url)?;
    // Validate the browser destination before it is persisted or shown. Waiting
    // until the user submits the local activation code would already have
    // exposed them to a cross-origin or request-swapping URL from a bad peer.
    validate_activation_url_request(&activation_url, &pairing_endpoint, created_request_id)?;
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
        request_id: created_request_id,
        activation_url: activation_url.clone(),
        expires_at,
        poll_interval: created.poll_interval,
        pairing_endpoint,
        report_endpoint,
        bearer_secret,
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
        request_id: created_request_id,
        activation_url,
        expires_at,
        poll_interval: created.poll_interval,
    })
}
