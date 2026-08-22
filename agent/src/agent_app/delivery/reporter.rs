pub(super) async fn prepare_reporter(
    config: &mut AgentConfig,
    host: &mut unionc_agent::HostIdentity,
    command: AgentCommand,
    shutdown: &ShutdownSignal,
) -> anyhow::Result<Option<(Reporter, Option<(Uuid, Uuid)>)>> {
    let mut backoff = Duration::from_secs(1);
    let mut last_authorization_notice: Option<(&'static str, Option<Uuid>)> = None;
    loop {
        if command == AgentCommand::Run
            && let Some(reporter) = pairing::existing_reporter_for_run(config)?
        {
            return Ok(Some((reporter, None)));
        }
        let progress = tokio::select! {
            result = pairing::poll_existing(config) => result,
            _ = shutdown.cancelled() => return Ok(None),
        };
        match progress {
            Ok(Some(PairingProgress::Creating { .. })) => {
                continue;
            }
            Ok(Some(PairingProgress::Waiting(waiting))) => {
                if command != AgentCommand::Run {
                    anyhow::bail!(
                        "browser authorization is still pending; open {}",
                        waiting.activation_url
                    );
                }
                backoff = Duration::from_secs(1);
                let notice = ("pending", Some(waiting.request_id));
                if last_authorization_notice != Some(notice) {
                    info!(
                        agent_state = "awaiting_authorization",
                        request_id = %waiting.request_id,
                        activation_url = %waiting.activation_url,
                        "browser authorization is pending"
                    );
                    last_authorization_notice = Some(notice);
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(waiting.poll_interval)) => {},
                    _ = shutdown.cancelled() => return Ok(None),
                }
                continue;
            }
            Ok(Some(PairingProgress::Active {
                generation,
                request_id,
                instance_id,
                report_endpoint,
            })) => {
                let reporter = pairing::activate_reporter_snapshot(
                    config,
                    host,
                    generation,
                    request_id,
                    instance_id,
                    &report_endpoint,
                )
                .context(
                    "paired host credential could not be loaded; run `unionc-agent pair` again",
                )?;
                return Ok(Some((reporter, Some((generation, request_id)))));
            }
            Ok(Some(PairingProgress::Denied {
                generation: _,
                request_id,
                activation_url,
            })) => {
                if command != AgentCommand::Run {
                    anyhow::bail!(
                        "browser authorization request {request_id} was denied; run pair again \
                         ({activation_url})"
                    );
                }
                let notice = ("denied", Some(request_id));
                if last_authorization_notice != Some(notice) {
                    info!(
                        agent_state = "awaiting_authorization",
                        %request_id,
                        "browser authorization was denied; run `unionc-agent pair --server <url>`"
                    );
                    last_authorization_notice = Some(notice);
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {},
                    _ = shutdown.cancelled() => return Ok(None),
                }
                continue;
            }
            Ok(Some(PairingProgress::Expired {
                generation: _,
                request_id,
                activation_url,
            })) => {
                if command != AgentCommand::Run {
                    anyhow::bail!(
                        "browser authorization request {request_id} expired; run pair again \
                         ({activation_url})"
                    );
                }
                let notice = ("expired", Some(request_id));
                if last_authorization_notice != Some(notice) {
                    info!(
                        agent_state = "awaiting_authorization",
                        %request_id,
                        "browser authorization expired; run `unionc-agent pair --server <url>`"
                    );
                    last_authorization_notice = Some(notice);
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {},
                    _ = shutdown.cancelled() => return Ok(None),
                }
                continue;
            }
            Ok(None) => {}
            Err(error) if command != AgentCommand::Run => {
                return Err(error.context("failed to resume browser pairing"));
            }
            Err(error) => {
                let delay = jitter(backoff, config.jitter_percent);
                warn!(
                    retry_seconds = delay.as_secs_f64(),
                    "browser pairing state could not be checked; retrying: {error}"
                );
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {},
                    _ = shutdown.cancelled() => return Ok(None),
                }
                backoff = (backoff * 2).min(Duration::from_secs(300));
                continue;
            }
        }

        if command != AgentCommand::Run {
            anyhow::bail!(
                "this host is not authorized; run `unionc-agent pair --server <url>` first"
            );
        }
        let delay = jitter(backoff, config.jitter_percent);
        let notice = ("unconfigured", None);
        if last_authorization_notice != Some(notice) {
            info!(
                agent_state = "awaiting_authorization",
                retry_seconds = delay.as_secs_f64(),
                "no host credential or pending pairing request; run `unionc-agent pair --server <url>`"
            );
            last_authorization_notice = Some(notice);
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {},
            _ = shutdown.cancelled() => return Ok(None),
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}
