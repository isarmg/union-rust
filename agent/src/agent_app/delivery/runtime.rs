/// Capacity-one notifications deliberately coalesce. Reports are durable in
/// the spool before notification, so the channel is only a wake-up edge rather
/// than the source of truth and can never apply network backpressure to sampling.
#[derive(Clone)]
struct DeliveryTrigger {
    sender: mpsc::Sender<()>,
}

impl DeliveryTrigger {
    fn notify(&self) -> bool {
        match self.sender.try_send(()) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(())) => true,
            Err(mpsc::error::TrySendError::Closed(())) => false,
        }
    }
}

/// A cadence is anchored to its previous deadline, not to the end of sampling.
/// If collection itself overruns a full interval, missed ticks are skipped
/// instead of emitted in a burst.
struct SamplingCadence {
    deadline: tokio::time::Instant,
}

impl SamplingCadence {
    fn starting_now() -> Self {
        Self {
            deadline: tokio::time::Instant::now(),
        }
    }

    fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }

    fn schedule_next(&mut self, interval: Duration, now: tokio::time::Instant) {
        let anchored = self.deadline + interval;
        self.deadline = if anchored > now {
            anchored
        } else {
            now + interval
        };
    }
}

pub(super) async fn run_loop(
    config: AgentConfig,
    host: unionc_agent::HostIdentity,
    mut sampler: SystemSampler,
    spool: Spool,
    reporter: Reporter,
    active_pairing: Option<(Uuid, Uuid)>,
    process_shutdown: &ShutdownSignal,
) -> anyhow::Result<()> {
    let (delivery_sender, delivery_receiver) = mpsc::channel(1);
    let delivery_trigger = DeliveryTrigger {
        sender: delivery_sender,
    };
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let (host_sender, host_receiver) = watch::channel(host.clone());
    let mut delivery_worker = tokio::spawn(
        DeliveryWorker {
            config: config.clone(),
            host,
            spool: spool.clone(),
            reporter,
            active_pairing,
            wake: delivery_receiver,
            shutdown: shutdown_receiver,
            host_updates: host_sender,
        }
        .run(),
    );

    let mut spool_read_health = SpoolHealth::default();
    let mut spool_write_health = SpoolHealth::default();
    let mut cadence = SamplingCadence::starting_now();

    loop {
        tokio::select! {
            biased;
            result = &mut delivery_worker => {
                return delivery_worker_result(result);
            }
            _ = process_shutdown.cancelled() => {
                info!("shutdown signal received");
                let _ = shutdown_sender.send(true);
                drop(delivery_trigger);
                return stop_delivery_worker(delivery_worker).await;
            }
            _ = tokio::time::sleep_until(cadence.deadline()) => {
                // Only bounded local disk work is allowed on the cadence path.
                // Every network operation, retry and 32-report backlog batch is
                // owned by `delivery_loop` and cannot shift the next deadline.
                let pending = match spool.pending_count() {
                    Ok(count) => {
                        spool_read_health.record_success();
                        count
                    }
                    Err(error) => {
                        spool_read_health.record_failure("读取 spool 队列长度", &error)?;
                        0
                    }
                };
                let report = sampler.collect(
                    host_receiver.borrow().clone(),
                    config.slow_interval_seconds,
                    pending,
                );
                spool_write_health.try_enqueue(&spool, &report)?;
                if !delivery_trigger.notify() {
                    warn!(report_id = %report.report_id, "delivery worker stopped before notification");
                }
                cadence.schedule_next(
                    jitter(config.interval(), config.jitter_percent),
                    tokio::time::Instant::now(),
                );
            }
        }
    }
}

fn delivery_worker_result(
    result: Result<anyhow::Result<()>, tokio::task::JoinError>,
) -> anyhow::Result<()> {
    result.map_err(|error| anyhow::anyhow!("delivery worker task failed: {error}"))?
}

async fn stop_delivery_worker(
    mut worker: tokio::task::JoinHandle<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    match tokio::time::timeout(Duration::from_secs(5), &mut worker).await {
        Ok(result) => delivery_worker_result(result),
        Err(_) => {
            warn!(
                "delivery worker did not stop within 5 seconds; cancelling it with reports preserved in spool"
            );
            worker.abort();
            let _ = worker.await;
            Ok(())
        }
    }
}

struct DeliveryWorker {
    config: AgentConfig,
    host: unionc_agent::HostIdentity,
    spool: Spool,
    reporter: Reporter,
    active_pairing: Option<(Uuid, Uuid)>,
    wake: mpsc::Receiver<()>,
    shutdown: watch::Receiver<bool>,
    host_updates: watch::Sender<unionc_agent::HostIdentity>,
}

fn delivery_timing(
    authorization_blocked: bool,
    retry_at: Option<Instant>,
    now: Instant,
) -> (bool, Option<Instant>) {
    if authorization_blocked {
        return (false, None);
    }
    match retry_at {
        Some(at) if now >= at => (true, None),
        Some(at) => (false, Some(at)),
        None => (false, None),
    }
}

fn pairing_failure_schedule(
    now: Instant,
    backoff: Duration,
    jitter_percent: u8,
) -> (Instant, Duration, Duration) {
    let delay = jitter(backoff, jitter_percent);
    let next_backoff = (backoff * 2).min(Duration::from_secs(300));
    (now + delay, next_backoff, delay)
}

impl DeliveryWorker {
    async fn run(self) -> anyhow::Result<()> {
        let Self {
            mut config,
            mut host,
            spool,
            mut reporter,
            mut active_pairing,
            mut wake,
            mut shutdown,
            host_updates,
        } = self;
        let mut retry_at = Some(Instant::now());
        let mut backoff = Duration::from_secs(1);
        let mut spool_flush_health = SpoolHealth::default();
        let mut authorization_blocked = false;
        let mut pairing_retry_at = Instant::now();
        let mut pairing_backoff = Duration::from_secs(1);
        let mut pairing_probe = None;
        let otlp_queue = config
            .otlp_endpoint
            .as_ref()
            .map(|_| OtlpQueue::spawn(reporter.clone()));

        let outcome: anyhow::Result<()> = async {
        loop {
            if *shutdown.borrow() {
                break Ok(());
            }
            if pairing_probe.is_none() && Instant::now() >= pairing_retry_at {
                let probe_config = config.clone();
                pairing_probe = Some(tokio::spawn(async move {
                    pairing::poll_existing(&probe_config).await
                }));
            }

            let now = Instant::now();
            // Once a retry becomes ready it must stop participating as a timer in this
            // `select!`. Keeping an expired deadline enabled would let `sleep_until` complete
            // immediately and cancel a still-pending HTTP delivery future on every loop.
            let (delivery_ready, next_retry) =
                delivery_timing(authorization_blocked, retry_at, now);
            let next_pairing = pairing_probe.is_none().then_some(pairing_retry_at);
            let deadline = match (next_retry, next_pairing) {
                (Some(left), Some(right)) => left.min(right),
                (Some(value), None) | (None, Some(value)) => value,
                (None, None) => now + Duration::from_secs(60 * 60),
            };

            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break Ok(());
                    }
                }
                joined = async {
                    pairing_probe
                        .as_mut()
                        .expect("pairing probe branch requires a task")
                        .await
                }, if pairing_probe.is_some() => {
                    pairing_probe = None;
                    let outcome = joined
                        .context("pairing probe task failed")?;
                    match outcome {
                        Ok(Some(PairingProgress::Active {
                            generation,
                            request_id,
                            instance_id,
                            report_endpoint,
                        })) if Some((generation, request_id)) != active_pairing => {
                            match pairing::activate_reporter_snapshot(
                                &mut config,
                                &mut host,
                                generation,
                                request_id,
                                instance_id,
                                &report_endpoint,
                            ) {
                                Ok(fresh) => {
                                    reporter = fresh;
                                    active_pairing = Some((generation, request_id));
                                    authorization_blocked = false;
                                    retry_at = Some(Instant::now());
                                    backoff = Duration::from_secs(1);
                                    pairing_backoff = Duration::from_secs(1);
                                    pairing_retry_at = Instant::now() + Duration::from_secs(60);
                                    let _ = host_updates.send(host.clone());
                                    info!(
                                        agent_state = "authorized",
                                        %request_id,
                                        "new instance pairing completed; queued reports will be retried"
                                    );
                                }
                                Err(error) => {
                                    let delay;
                                    (pairing_retry_at, pairing_backoff, delay) =
                                        pairing_failure_schedule(
                                            Instant::now(),
                                            pairing_backoff,
                                            config.jitter_percent,
                                        );
                                    warn!(
                                        retry_seconds = delay.as_secs_f64(),
                                        "pairing changed before its Reporter snapshot was loaded: {error}"
                                    );
                                }
                            }
                        }
                        Ok(Some(PairingProgress::Waiting(waiting))) => {
                            pairing_backoff = Duration::from_secs(1);
                            pairing_retry_at =
                                Instant::now() + Duration::from_secs(waiting.poll_interval);
                            if authorization_blocked {
                                warn!(
                                    agent_state = "awaiting_authorization",
                                    activation_url = %waiting.activation_url,
                                    "open the activation URL to restore telemetry delivery"
                                );
                            }
                        }
                        Ok(Some(PairingProgress::Creating { .. })) => {
                            pairing_retry_at = Instant::now() + Duration::from_secs(2);
                        }
                        Ok(Some(PairingProgress::Active { .. }))
                        | Ok(Some(PairingProgress::Denied { .. }))
                        | Ok(Some(PairingProgress::Expired { .. }))
                        | Ok(None) => {
                            pairing_retry_at = Instant::now() + Duration::from_secs(60);
                        }
                        Err(error) => {
                            let delay;
                            (pairing_retry_at, pairing_backoff, delay) =
                                pairing_failure_schedule(
                                    Instant::now(),
                                    pairing_backoff,
                                    config.jitter_percent,
                                );
                            warn!(
                                retry_seconds = delay.as_secs_f64(),
                                "browser pairing state could not be checked: {error}"
                            );
                        }
                    }
                }
                flushed = flush_spool(&spool, &reporter, otlp_queue.as_ref()), if delivery_ready => {
                    match flushed {
                        Ok(FlushOutcome::Drained) => {
                            spool_flush_health.record_success();
                            retry_at = None;
                            backoff = Duration::from_secs(1);
                        }
                        Ok(FlushOutcome::BatchComplete) => {
                            spool_flush_health.record_success();
                            retry_at = Some(Instant::now());
                            backoff = Duration::from_secs(1);
                            tokio::task::yield_now().await;
                        }
                        Err(error) => {
                            spool_flush_health.record_failure("补传 spool 队列", &error)?;
                            warn!(
                                pending = spool.pending_count().unwrap_or(0),
                                "telemetry delivery failed: {error}"
                            );
                            retry_at = Some(Instant::now() + jitter(backoff, config.jitter_percent));
                            backoff = (backoff * 2).min(Duration::from_secs(300));
                        }
                        Ok(FlushOutcome::Failed(error)) if error.is_unauthorized() => {
                            spool_flush_health.record_success();
                            let rendered = anyhow::anyhow!(error);
                            let reporter_is_current = match pairing::mark_reauth_required_if_current(
                                &config,
                                active_pairing,
                                format!("the host credential was rejected with HTTP 401: {rendered}"),
                            ) {
                                Ok(true) => {
                                    authorization_blocked = true;
                                    retry_at = None;
                                    pairing_retry_at = Instant::now();
                                    true
                                }
                                Ok(false) => {
                                    pairing_retry_at = Instant::now();
                                    warn!("ignored a stale 401 from a reporter superseded by newer pairing state");
                                    false
                                }
                                Err(state_error) => {
                                    authorization_blocked = true;
                                    retry_at = None;
                                    pairing_retry_at = Instant::now();
                                    warn!("failed to validate reauth_required state: {state_error}");
                                    true
                                }
                            };
                            if reporter_is_current {
                                error!(
                                    agent_state = "reauth_required",
                                    "the paired credential is no longer accepted. Run `unionc-agent \
                                     pair --server <url>`: {rendered}"
                                );
                            } else {
                                retry_at = Some(
                                    Instant::now() + jitter(backoff, config.jitter_percent),
                                );
                            }
                        }
                        Ok(FlushOutcome::Failed(error)) => {
                            spool_flush_health.record_success();
                            warn!(
                                pending = spool.pending_count().unwrap_or(0),
                                "telemetry delivery failed: {error}"
                            );
                            retry_at = Some(
                                Instant::now() + jitter(backoff, config.jitter_percent),
                            );
                            backoff = (backoff * 2).min(Duration::from_secs(300));
                        }
                    }
                }
                message = wake.recv() => {
                    if message.is_none() {
                        break Ok(());
                    }
                    // A wake edge is needed only after the queue was drained.
                    // New samples must not collapse an active network backoff
                    // into one request per sampling interval.
                    if !authorization_blocked && retry_at.is_none() {
                        retry_at = Some(Instant::now());
                    }
                }
                _ = tokio::time::sleep_until(deadline.into()) => {}
            }
        }
    }
    .await;

        if let Some(probe) = pairing_probe {
            probe.abort();
        }
        if let Some(queue) = otlp_queue {
            queue.abort();
        }
        outcome
    }
}
