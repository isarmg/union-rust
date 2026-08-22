use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Context;
use rand::random;
use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};
use unionc_agent::{
    AgentCommand, AgentConfig, OutputMode, SystemSampler,
    collectors::{load_host_identity, transient_host_identity},
    model::AgentReport,
    pairing::{self, PairingProgress},
    service::ShutdownSignal,
    spool::Spool,
    transport::Reporter,
};
use uuid::Uuid;

#[cfg(windows)]
use unionc_agent::service;

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    if service::windows_service_requested(std::env::args_os()) {
        return windows_service_host::dispatch();
    }

    init_tracing()?;
    build_runtime()?.block_on(run_agent(None))
}

fn init_tracing() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "unionc_agent=info".into()),
        )
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialize logging: {error}"))?;
    Ok(())
}

fn build_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to initialize the async runtime")
}

async fn run_agent(ready: Option<fn() -> anyhow::Result<bool>>) -> anyhow::Result<()> {
    let (mut config, command) = AgentConfig::load_from_args()?;
    if command == AgentCommand::Status {
        return print_local_status(&config);
    }
    if command == AgentCommand::Doctor && !config.doctor_delivery {
        return run_read_only_doctor(&config).await;
    }
    let mut host = if command == AgentCommand::Probe {
        transient_host_identity(Uuid::new_v4())
    } else if pairing::has_current_authorized_identity(&config)? {
        load_host_identity(&config.state_dir)?
    } else {
        transient_host_identity(Uuid::new_v4())
    };
    if let Some(name) = &config.host_name {
        host.name.clone_from(name);
    }
    if command == AgentCommand::Pair {
        return run_pairing(&mut config, host).await;
    }

    let mut sampler = SystemSampler::new();
    tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;

    if command == AgentCommand::Probe {
        let report = sampler.collect(host, config.slow_interval_seconds, 0);
        match config.output_mode {
            OutputMode::Json => println!("{}", serde_json::to_string_pretty(&report)?),
            OutputMode::Human => println!(
                "Local collection succeeded: {} logical CPUs, {} network interfaces, {} disks, {} capabilities ({} collector errors).",
                report.system.cpu.logical_count,
                report.system.networks.len(),
                report.system.disks.len(),
                report.capabilities.len(),
                report.agent.collector_errors
            ),
        }
        return Ok(());
    }

    let spool = Spool::open(&config.state_dir, config.spool_max_bytes)
        .with_context(|| format!("failed to open spool in {}", config.state_dir.display()))?;
    // A Windows service becomes RUNNING only after configuration, host identity,
    // collectors and durable spool have all initialized. Network authorization
    // is deliberately not part of bootstrap: an unpaired service must remain
    // healthy while it waits for browser approval.
    if let Some(report_ready) = ready
        && !report_ready()?
    {
        return Ok(());
    }
    let Some((reporter, active_pairing)) =
        prepare_reporter(&mut config, &mut host, command).await?
    else {
        info!("shutdown signal received while waiting for browser pairing");
        return Ok(());
    };

    if matches!(command, AgentCommand::Once | AgentCommand::Doctor) {
        let result = run_once(&config, host.clone(), &mut sampler, &spool, reporter).await;
        if command == AgentCommand::Doctor {
            result?;
            let delivery = serde_json::json!({
                "schema_version": 1,
                "command": "doctor",
                "status": "healthy",
                "mode": "delivery",
                "host_id": host.id,
                "host_name": host.name,
                "endpoint": config.endpoint,
                "spool_pending_batches": spool.pending_count()?,
                "checks": [
                    "configuration",
                    "state-directory",
                    "local-collection",
                    "host-credential",
                    "server-delivery",
                    "spool"
                ]
            });
            match config.output_mode {
                OutputMode::Json => {
                    println!("{}", serde_json::to_string_pretty(&delivery)?)
                }
                OutputMode::Human => println!(
                    "UnionC Agent doctor: healthy; end-to-end delivery succeeded and the spool is drained."
                ),
            }
            return Ok(());
        }
        result?;
        match config.output_mode {
            OutputMode::Json => println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": 1,
                    "command": "once",
                    "status": "delivered",
                    "host_id": host.id,
                    "spool_pending_batches": spool.pending_count()?
                }))?
            ),
            OutputMode::Human => println!("One telemetry snapshot was delivered successfully."),
        }
        return Ok(());
    }

    info!(host_id = %host.id, host_name = %host.name, "read-only telemetry agent started");
    run_loop(config, host, sampler, spool, reporter, active_pairing).await
}

async fn run_pairing(
    config: &mut AgentConfig,
    host: unionc_agent::HostIdentity,
) -> anyhow::Result<()> {
    let tray_cancel = tray_pair_cancel_signal(config.tray_cancel_event.as_deref())?;
    let tray_deadline = config
        .tray_deadline_seconds
        .map(|seconds| Instant::now() + Duration::from_secs(seconds));
    let session = tokio::select! {
        result = pairing::start_or_resume(config, &host) => result?,
        outcome = wait_for_pairing_abort(tray_cancel.as_ref(), tray_deadline) => {
            match outcome? {
                PairingWait::Shutdown => return Ok(()),
                outcome => stop_tray_pairing(config, outcome)?,
            }
            unreachable!()
        }
    };
    let expected_generation = session.generation;
    let expected_request_id = session.request_id;
    // The pending state carries its exact report/pairing endpoints. Keep the
    // currently active credential bound to its current endpoint until the new
    // pairing transaction reaches Active. Denial or expiry changes neither.
    if config.tray_events {
        emit_tray_pair_event(serde_json::json!({
            "event": "pairing_waiting",
            "version": env!("CARGO_PKG_VERSION"),
            "request_id": session.request_id,
            "generation": session.generation,
            "activation_url": session.activation_url,
            "pairing_endpoint": config.pairing_endpoint(),
            "expires_at": session.expires_at,
            "poll_interval": session.poll_interval
        }))?;
    } else {
        match config.output_mode {
            OutputMode::Human => println!(
                "Authorize this host in your browser:\n{}\n\nThe request expires at {}. This command may be interrupted; `run` will continue polling the saved request.",
                session.activation_url, session.expires_at
            ),
            OutputMode::Json => println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "schema_version": 1,
                    "command": "pair",
                    "status": "pairing_waiting",
                    "request_id": session.request_id,
                    "activation_url": session.activation_url,
                    "expires_at": session.expires_at,
                    "resumable": true
                }))?
            ),
        }
    }

    let expected_activation_instance = if config.tray_activation_stdin {
        let activation_code = read_tray_activation_code()?;
        let activated = tokio::select! {
            result = pairing::activate_pending_with_code(
                config,
                expected_generation,
                expected_request_id,
                activation_code.as_str()?,
            ) => result?,
            outcome = wait_for_pairing_abort(tray_cancel.as_ref(), tray_deadline) => {
                match outcome? {
                    PairingWait::Shutdown => return Ok(()),
                    outcome => stop_tray_pairing(config, outcome)?,
                }
                unreachable!()
            }
        };
        drop(activation_code);
        activated
    } else {
        None
    };

    let mut network_backoff = Duration::from_secs(1);
    loop {
        let polled = tokio::select! {
            result = pairing::poll_existing(config) => result,
            outcome = wait_for_pairing_abort(tray_cancel.as_ref(), tray_deadline) => {
                match outcome? {
                    PairingWait::Shutdown => return Ok(()),
                    outcome => stop_tray_pairing(config, outcome)?,
                }
                unreachable!()
            }
        };
        match polled {
            Ok(Some(PairingProgress::Creating { generation, .. })) => {
                if generation != expected_generation {
                    anyhow::bail!(
                        "this pairing request was superseded by another request; no credentials were changed"
                    );
                }
                anyhow::bail!(
                    "current pairing poll left the request in its creating phase; no credentials were changed"
                );
            }
            Ok(Some(PairingProgress::Waiting(waiting))) => {
                if waiting.generation != expected_generation
                    || waiting.request_id != expected_request_id
                {
                    anyhow::bail!(
                        "this pairing request was superseded by another request; no credentials were changed"
                    );
                }
                network_backoff = Duration::from_secs(1);
                let delay = Duration::from_secs(waiting.poll_interval);
                match wait_for_pairing_control(delay, tray_cancel.as_ref(), tray_deadline).await? {
                    PairingWait::Elapsed => {}
                    PairingWait::Shutdown => {
                        if config.tray_events {
                            emit_tray_pair_event(serde_json::json!({
                                "event": "pairing_interrupted",
                                "version": env!("CARGO_PKG_VERSION"),
                                "request_id": waiting.request_id
                            }))?;
                        } else {
                            match config.output_mode {
                                OutputMode::Human => println!(
                                    "Pairing remains pending. Re-run `unionc-agent pair` to continue this saved request.\n{}",
                                    waiting.activation_url
                                ),
                                OutputMode::Json => println!(
                                    "{}",
                                    serde_json::to_string(&serde_json::json!({
                                        "schema_version": 1,
                                        "command": "pair",
                                        "status": "interrupted",
                                        "request_id": waiting.request_id,
                                        "activation_url": waiting.activation_url,
                                        "resumable": true
                                    }))?
                                ),
                            }
                        }
                        return Ok(());
                    }
                    outcome => stop_tray_pairing(config, outcome)?,
                }
            }
            Ok(Some(PairingProgress::Active {
                generation,
                request_id,
                instance_id,
                report_endpoint,
            })) => {
                if generation != expected_generation
                    || request_id != expected_request_id
                    || report_endpoint != config.endpoint
                    || expected_activation_instance.is_some_and(|expected| expected != instance_id)
                {
                    anyhow::bail!(
                        "this pairing request was superseded or returned a different server endpoint; no configuration was committed"
                    );
                }
                let config_path = pairing::commit_active_configuration(
                    config,
                    generation,
                    request_id,
                    instance_id,
                    &report_endpoint,
                )?;
                let result = serde_json::json!({
                    "status": "paired",
                    "request_id": request_id,
                    "instance_id": instance_id,
                    "config": config_path,
                    "endpoint": report_endpoint,
                    "message": "browser authorization succeeded; the host credential was stored privately"
                });
                if config.tray_events {
                    emit_tray_pair_event(serde_json::json!({
                        "event": "paired",
                        "version": env!("CARGO_PKG_VERSION"),
                        "request_id": request_id,
                        "instance_id": instance_id,
                        "endpoint": report_endpoint
                    }))?;
                } else {
                    match config.output_mode {
                        OutputMode::Json => {
                            println!("{}", serde_json::to_string_pretty(&result)?)
                        }
                        OutputMode::Human => println!(
                            "Pairing succeeded. Host credential stored in {} and reporting to {}.",
                            config_path.display(),
                            report_endpoint
                        ),
                    }
                }
                return Ok(());
            }
            Ok(Some(PairingProgress::Denied {
                generation,
                request_id,
                activation_url,
            })) => {
                if generation != expected_generation || request_id != expected_request_id {
                    anyhow::bail!(
                        "this pairing request was superseded by another request; no credentials were changed"
                    );
                }
                anyhow::bail!(
                    "browser pairing request {request_id} was denied; run `unionc-agent pair` to \
                     create a new request ({activation_url})"
                );
            }
            Ok(Some(PairingProgress::Expired {
                generation,
                request_id,
                activation_url,
            })) => {
                if generation != expected_generation || request_id != expected_request_id {
                    anyhow::bail!(
                        "this pairing request was superseded by another request; no credentials were changed"
                    );
                }
                anyhow::bail!(
                    "browser pairing request {request_id} expired; run `unionc-agent pair` to \
                     create a new request ({activation_url})"
                );
            }
            Ok(None) => anyhow::bail!("the persisted pairing request disappeared"),
            Err(error) => {
                if chrono::Utc::now() >= session.expires_at {
                    if !config.tray_events && config.output_mode == OutputMode::Json {
                        println!(
                            "{}",
                            serde_json::to_string(&serde_json::json!({
                                "schema_version": 1,
                                "command": "pair",
                                "status": "expired",
                                "request_id": session.request_id,
                                "resumable": false,
                                "error": {
                                    "code": "pairing_expired",
                                    "message": "the saved pairing request expired while the server was unreachable"
                                }
                            }))?
                        );
                    }
                    anyhow::bail!(
                        "browser pairing request {} expired while the server was unreachable; run pair again",
                        session.request_id
                    );
                }
                let delay = jitter(network_backoff, config.jitter_percent);
                warn!(
                    retry_seconds = delay.as_secs_f64(),
                    "pairing status could not be checked; the saved request will be retried: {error}"
                );
                match wait_for_pairing_control(delay, tray_cancel.as_ref(), tray_deadline).await? {
                    PairingWait::Elapsed => {}
                    PairingWait::Shutdown => return Ok(()),
                    outcome => stop_tray_pairing(config, outcome)?,
                }
                network_backoff = (network_backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

/// A bounded, in-memory authorization key received only from the elevated
/// tray broker's anonymous stdin. The backing allocation is overwritten when
/// it leaves scope; the value is never formatted or included in an error.
struct TrayActivationCode(Vec<u8>);

impl TrayActivationCode {
    fn as_str(&self) -> anyhow::Result<&str> {
        std::str::from_utf8(&self.0).context("authorization key is not valid UTF-8")
    }
}

impl Drop for TrayActivationCode {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

fn read_tray_activation_code() -> anyhow::Result<TrayActivationCode> {
    let mut bytes = Vec::with_capacity(258);
    std::io::stdin()
        .lock()
        .take(258)
        .read_to_end(&mut bytes)
        .context("failed to receive the authorization key from the tray broker")?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > 256 {
        bytes.fill(0);
        anyhow::bail!("authorization key exceeds the 256-byte limit");
    }
    let code = TrayActivationCode(bytes);
    crate_validate_tray_activation_code(code.as_str()?)?;
    Ok(code)
}

fn crate_validate_tray_activation_code(value: &str) -> anyhow::Result<()> {
    unionc_agent::tray_support::validate_activation_code(value).map(|_| ())
}

async fn wait_for_pairing_abort(
    tray_cancel: Option<&ShutdownSignal>,
    tray_deadline: Option<Instant>,
) -> anyhow::Result<PairingWait> {
    tokio::select! {
        result = shutdown_signal() => {
            result?;
            Ok(PairingWait::Shutdown)
        },
        _ = optional_pair_cancel(tray_cancel) => Ok(PairingWait::Cancelled),
        _ = optional_pair_deadline(tray_deadline) => Ok(PairingWait::Deadline),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PairingWait {
    Elapsed,
    Shutdown,
    Cancelled,
    Deadline,
}

async fn wait_for_pairing_control(
    delay: Duration,
    tray_cancel: Option<&ShutdownSignal>,
    tray_deadline: Option<Instant>,
) -> anyhow::Result<PairingWait> {
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(PairingWait::Elapsed),
        result = shutdown_signal() => {
            result?;
            Ok(PairingWait::Shutdown)
        },
        _ = optional_pair_cancel(tray_cancel) => Ok(PairingWait::Cancelled),
        _ = optional_pair_deadline(tray_deadline) => Ok(PairingWait::Deadline),
    }
}

async fn optional_pair_cancel(signal: Option<&ShutdownSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
    }
}

async fn optional_pair_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

fn stop_tray_pairing(config: &AgentConfig, outcome: PairingWait) -> anyhow::Result<()> {
    let (event, message) = match outcome {
        PairingWait::Cancelled => ("pairing_cancelled", "tray pairing was cancelled safely"),
        PairingWait::Deadline => (
            "pairing_timeout",
            "tray pairing reached its configured safety deadline",
        ),
        _ => return Ok(()),
    };
    if config.tray_events {
        emit_tray_pair_event(serde_json::json!({
            "event": event,
            "version": env!("CARGO_PKG_VERSION")
        }))?;
    }
    anyhow::bail!(message)
}

fn tray_pair_cancel_signal(name: Option<&str>) -> anyhow::Result<Option<ShutdownSignal>> {
    let Some(name) = name else {
        return Ok(None);
    };
    #[cfg(not(windows))]
    {
        let _ = name;
        anyhow::bail!("tray cancellation events are available only on Windows");
    }
    #[cfg(windows)]
    {
        windows_pair_cancel::open(name).map(Some)
    }
}

fn emit_tray_pair_event(event: serde_json::Value) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &event)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct DiagnosticCheck {
    id: &'static str,
    status: &'static str,
    code: Option<&'static str>,
    message: String,
    remediation: Option<String>,
    duration_ms: u64,
}

impl DiagnosticCheck {
    fn new(
        id: &'static str,
        status: &'static str,
        code: Option<&'static str>,
        message: impl Into<String>,
        remediation: Option<impl Into<String>>,
        started: Instant,
    ) -> Self {
        Self {
            id,
            status,
            code,
            message: message.into(),
            remediation: remediation.map(Into::into),
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

struct HostInspection {
    id: Option<String>,
    check: DiagnosticCheck,
}

fn inspect_host_identity(config: &AgentConfig) -> HostInspection {
    let started = Instant::now();
    let path = config.state_dir.join("host-id");
    match fs::read_to_string(&path) {
        Ok(value) => {
            let value = value.trim();
            match Uuid::parse_str(value) {
                Ok(id) => HostInspection {
                    id: Some(id.to_string()),
                    check: DiagnosticCheck::new(
                        "identity",
                        "ok",
                        None,
                        "host identity is readable and valid",
                        None::<String>,
                        started,
                    ),
                },
                Err(_) => HostInspection {
                    id: None,
                    check: DiagnosticCheck::new(
                        "identity",
                        "error",
                        Some("identity_invalid"),
                        format!("{} does not contain a valid UUID", path.display()),
                        Some("repair the state directory or pair this host again"),
                        started,
                    ),
                },
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => HostInspection {
            id: None,
            check: DiagnosticCheck::new(
                "identity",
                "missing",
                Some("identity_missing"),
                "host identity has not been created yet",
                Some("pair this host before expecting authenticated reports"),
                started,
            ),
        },
        Err(error) => HostInspection {
            id: None,
            check: DiagnosticCheck::new(
                "identity",
                "error",
                Some("identity_unreadable"),
                format!("failed to read {}: {error}", path.display()),
                Some("check the state-directory owner and permissions"),
                started,
            ),
        },
    }
}

struct CredentialInspection {
    present: bool,
    check: DiagnosticCheck,
}

fn inspect_credential(config: &AgentConfig) -> CredentialInspection {
    let started = Instant::now();
    let path = config.state_dir.join("agent-token");
    match fs::read_to_string(&path) {
        Ok(value) if !value.trim().is_empty() => CredentialInspection {
            present: true,
            check: DiagnosticCheck::new(
                "credential",
                "ok",
                None,
                "the private host credential is readable",
                None::<String>,
                started,
            ),
        },
        Ok(_) => CredentialInspection {
            present: false,
            check: DiagnosticCheck::new(
                "credential",
                "error",
                Some("credential_empty"),
                format!("{} is empty", path.display()),
                Some("pair this host again"),
                started,
            ),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => CredentialInspection {
            present: false,
            check: DiagnosticCheck::new(
                "credential",
                "missing",
                Some("credential_missing"),
                "no host credential is stored yet",
                Some("pair this host before expecting authenticated reports"),
                started,
            ),
        },
        Err(error) => CredentialInspection {
            present: false,
            check: DiagnosticCheck::new(
                "credential",
                "error",
                Some("credential_unreadable"),
                format!("failed to read {}: {error}", path.display()),
                Some("check the state-directory owner and permissions"),
                started,
            ),
        },
    }
}

#[derive(Default, Serialize)]
struct SpoolInspection {
    pending_batches: usize,
    invalid_batches: usize,
    total_bytes: u64,
    #[serde(skip)]
    check: Option<DiagnosticCheck>,
}

fn inspect_spool(state_dir: &Path) -> SpoolInspection {
    let started = Instant::now();
    let path = state_dir.join("spool");
    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SpoolInspection {
                check: Some(DiagnosticCheck::new(
                    "spool",
                    "missing",
                    None,
                    "the spool has not been created yet",
                    None::<String>,
                    started,
                )),
                ..Default::default()
            };
        }
        Err(error) => {
            return SpoolInspection {
                check: Some(DiagnosticCheck::new(
                    "spool",
                    "error",
                    Some("spool_unreadable"),
                    format!("failed to read {}: {error}", path.display()),
                    Some("check the state-directory owner, permissions, and disk health"),
                    started,
                )),
                ..Default::default()
            };
        }
    };
    let mut result = SpoolInspection::default();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                result.check = Some(DiagnosticCheck::new(
                    "spool",
                    "error",
                    Some("spool_unreadable"),
                    format!("failed to enumerate {}: {error}", path.display()),
                    Some("check the state-directory owner, permissions, and disk health"),
                    started,
                ));
                return result;
            }
        };
        let entry_path = entry.path();
        let extension = entry_path.extension().and_then(|value| value.to_str());
        match extension {
            Some("json") => result.pending_batches += 1,
            Some("invalid") => result.invalid_batches += 1,
            _ => continue,
        }
        match entry.metadata() {
            Ok(metadata) => result.total_bytes = result.total_bytes.saturating_add(metadata.len()),
            Err(error) => {
                result.check = Some(DiagnosticCheck::new(
                    "spool",
                    "error",
                    Some("spool_unreadable"),
                    format!("failed to inspect {}: {error}", entry_path.display()),
                    Some("check the state-directory owner, permissions, and disk health"),
                    started,
                ));
                return result;
            }
        }
    }
    result.check = Some(DiagnosticCheck::new(
        "spool",
        "ok",
        None,
        format!(
            "{} pending, {} invalid, {} bytes",
            result.pending_batches, result.invalid_batches, result.total_bytes
        ),
        None::<String>,
        started,
    ));
    result
}

fn print_local_status(config: &AgentConfig) -> anyhow::Result<()> {
    let config_started = Instant::now();
    let configured = config
        .config_path
        .as_ref()
        .is_some_and(|path| path.is_file());
    let config_check = if let Some(issue) = config.diagnostic_config_issue() {
        DiagnosticCheck::new(
            "configuration",
            "error",
            Some("config_invalid"),
            issue,
            Some("repair the configuration file, then run status again"),
            config_started,
        )
    } else if configured {
        DiagnosticCheck::new(
            "configuration",
            "ok",
            None,
            "configuration file is present",
            None::<String>,
            config_started,
        )
    } else {
        DiagnosticCheck::new(
            "configuration",
            "missing",
            Some("config_missing"),
            "configuration file is not present",
            Some("pair this host to create its private configuration"),
            config_started,
        )
    };
    let host = inspect_host_identity(config);
    let credential = inspect_credential(config);
    let mut spool = inspect_spool(&config.state_dir);
    let spool_check = spool
        .check
        .take()
        .expect("spool inspection always produces a check");

    let pairing_result = pairing::local_progress(config);
    let authorization_result = pairing::local_auth_state(config);
    let pairing_error = pairing_result.as_ref().err().map(ToString::to_string);
    let authorization_error = authorization_result.as_ref().err().map(ToString::to_string);
    let pairing = pairing_result.ok().flatten();
    let authorization = authorization_result.ok().flatten();
    let reauth_required = authorization
        .as_ref()
        .is_some_and(|state| state.status == "reauth_required");
    let pairing_pending = pairing.as_ref().is_some_and(|progress| {
        matches!(
            progress,
            PairingProgress::Creating { .. } | PairingProgress::Waiting(_)
        )
    });
    let has_error = [
        config_check.status,
        host.check.status,
        credential.check.status,
        spool_check.status,
    ]
    .contains(&"error")
        || pairing_error.is_some()
        || authorization_error.is_some();
    let overall_state = if has_error {
        "degraded"
    } else if reauth_required {
        "reauth_required"
    } else if pairing_pending {
        "pairing"
    } else if configured && host.id.is_some() && credential.present {
        "configured"
    } else {
        "unconfigured"
    };
    let next_action = match overall_state {
        "degraded" => "repair the failed local check, then run `unionc-agent doctor`",
        "reauth_required" => "create a new pairing invitation in UnionC and pair this host again",
        "pairing" => "complete or resume the saved browser pairing request",
        "unconfigured" => "run `unionc-agent pair --server https://your-console`",
        _ => "run `unionc-agent doctor --delivery` for an explicit end-to-end delivery test",
    };
    let checks = serde_json::json!({
        "configuration": config_check,
        "identity": host.check,
        "credential": credential.check,
        "spool": spool_check,
        "pairing": {
            "status": if pairing_error.is_some() { "error" } else { "ok" },
            "code": pairing_error.as_ref().map(|_| "pairing_state_invalid"),
            "message": pairing_error
        },
        "authorization": {
            "status": if authorization_error.is_some() { "error" } else { "ok" },
            "code": authorization_error.as_ref().map(|_| "authorization_state_invalid"),
            "message": authorization_error
        }
    });
    let snapshot = serde_json::json!({
        "schema_version": 1,
        "command": "status",
        "status": overall_state,
        "configured": configured,
        "config": config.config_path,
        "endpoint": config.endpoint,
        "state_dir": config.state_dir,
        "host_id": host.id,
        "host_name": config.host_name,
        "credential_present": credential.present,
        "spool_pending_batches": spool.pending_batches,
        "spool_invalid_batches": spool.invalid_batches,
        "spool_bytes": spool.total_bytes,
        "pairing": pairing,
        "authorization": authorization,
        "checks": &checks,
        "next_action": next_action
    });
    match config.output_mode {
        OutputMode::Json => println!("{}", serde_json::to_string_pretty(&snapshot)?),
        OutputMode::Human => {
            println!("UnionC Agent: {overall_state}");
            println!(
                "  Configuration: {}",
                if configured { "present" } else { "missing" }
            );
            println!(
                "  Identity: {}",
                snapshot["host_id"].as_str().unwrap_or("not available")
            );
            println!(
                "  Credential: {}",
                if credential.present {
                    "present"
                } else {
                    "missing"
                }
            );
            println!(
                "  Spool: {} pending, {} invalid, {} bytes",
                spool.pending_batches, spool.invalid_batches, spool.total_bytes
            );
            println!("  Next: {next_action}");
        }
    }
    Ok(())
}

async fn run_read_only_doctor(config: &AgentConfig) -> anyhow::Result<()> {
    let mut checks = Vec::new();

    let started = Instant::now();
    checks.push(match config.validate_for_diagnostics() {
        Ok(()) => DiagnosticCheck::new(
            "configuration",
            "ok",
            None,
            "effective configuration is valid",
            None::<String>,
            started,
        ),
        Err(error) => DiagnosticCheck::new(
            "configuration",
            "error",
            Some("config_invalid"),
            error.to_string(),
            Some("repair the reported setting before starting the service"),
            started,
        ),
    });

    let started = Instant::now();
    checks.push(match fs::metadata(&config.state_dir) {
        Ok(metadata) if metadata.is_dir() => DiagnosticCheck::new(
            "state_directory",
            "ok",
            None,
            format!("{} is accessible", config.state_dir.display()),
            None::<String>,
            started,
        ),
        Ok(_) => DiagnosticCheck::new(
            "state_directory",
            "error",
            Some("state_directory_invalid"),
            format!("{} is not a directory", config.state_dir.display()),
            Some("restore the package-managed private state directory"),
            started,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DiagnosticCheck::new(
            "state_directory",
            "missing",
            None,
            "state directory has not been created yet",
            Some("pair this host or start the packaged service"),
            started,
        ),
        Err(error) => DiagnosticCheck::new(
            "state_directory",
            "error",
            Some("state_directory_unreadable"),
            format!("failed to inspect {}: {error}", config.state_dir.display()),
            Some("check the service account, owner, permissions, and disk health"),
            started,
        ),
    });

    let host = inspect_host_identity(config);
    let diagnostic_id = host
        .id
        .as_deref()
        .and_then(|value| Uuid::parse_str(value).ok())
        .unwrap_or_else(Uuid::new_v4);
    checks.push(host.check);
    checks.push(inspect_credential(config).check);
    let mut spool = inspect_spool(&config.state_dir);
    checks.push(
        spool
            .check
            .take()
            .expect("spool inspection always produces a check"),
    );

    let started = Instant::now();
    let mut collection_host = transient_host_identity(diagnostic_id);
    if let Some(name) = &config.host_name {
        collection_host.name.clone_from(name);
    }
    let mut sampler = SystemSampler::new();
    let report = sampler.collect(
        collection_host,
        config.slow_interval_seconds,
        spool.pending_batches as u64,
    );
    let capabilities = report.capabilities.len();
    let collector_errors = report.agent.collector_errors;
    checks.push(DiagnosticCheck::new(
        "local_collection",
        if collector_errors == 0 {
            "ok"
        } else {
            "warning"
        },
        (collector_errors != 0).then_some("collector_degraded"),
        format!(
            "local snapshot completed with {capabilities} capabilities and {collector_errors} collector errors"
        ),
        (collector_errors != 0)
            .then_some("inspect capability details with `unionc-agent probe --output json`"),
        started,
    ));
    checks.push(DiagnosticCheck::new(
        "server_delivery",
        "skipped",
        None,
        "no report was sent; read-only doctor never drains the spool or changes credentials",
        Some("use `unionc-agent doctor --delivery` for an explicit end-to-end test"),
        Instant::now(),
    ));

    let has_errors = checks.iter().any(|check| check.status == "error");
    let has_warnings = checks
        .iter()
        .any(|check| matches!(check.status, "warning" | "missing"));
    let status = if has_errors {
        "unhealthy"
    } else if has_warnings {
        "attention"
    } else {
        "healthy"
    };
    let result = serde_json::json!({
        "schema_version": 1,
        "command": "doctor",
        "status": status,
        "mode": "read_only",
        "checks": &checks,
        "next_action": if has_errors {
            "repair the failed checks and run doctor again"
        } else {
            "use --delivery only when a real server write is intended"
        }
    });
    match config.output_mode {
        OutputMode::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        OutputMode::Human => {
            println!("UnionC Agent doctor: {status} (read-only)");
            for check in &checks {
                println!("  {:<18} {:<9} {}", check.id, check.status, check.message);
                if let Some(remediation) = &check.remediation {
                    println!("    Next: {remediation}");
                }
            }
        }
    }
    if has_errors {
        anyhow::bail!("one or more read-only diagnostic checks failed");
    }
    Ok(())
}

/// 采样一次，并保证之前由 `once`/`run` 留下的积压得到补传。
async fn run_once(
    config: &AgentConfig,
    host: unionc_agent::HostIdentity,
    sampler: &mut SystemSampler,
    spool: &Spool,
    reporter: Reporter,
) -> anyhow::Result<()> {
    let pending = spool.pending_count()?;
    let report = sampler.collect(host.clone(), config.slow_interval_seconds, pending);
    // `flush_spool` 单轮最多发 32 份；once 是显式的一次性投递命令，因此循环到队列
    // 清空。若网络仍不可用，当前采样也入队后退出，下一次 once 可以继续恢复。
    while spool.pending_count()? > 0 {
        match flush_spool(spool, &reporter, None).await {
            Ok(FlushOutcome::Drained | FlushOutcome::BatchComplete) => {}
            Ok(FlushOutcome::Failed(error)) => {
                spool.enqueue(&report)?;
                return Err(anyhow::anyhow!(error)
                    .context("current report was retained while stored reports remain pending"));
            }
            Err(error) => {
                spool.enqueue(&report)?;
                return Err(error.context(
                    "current report was retained because the local spool could not be flushed",
                ));
            }
        }
    }

    let send = reporter.send_unionc(&report).await;
    if let Err(error) = send {
        if error.is_permanent() {
            return Err(anyhow::anyhow!(error)
                .context("report was rejected permanently and was not spooled"));
        }
        spool.enqueue(&report)?;
        return Err(anyhow::anyhow!(error).context("report was retained in the local spool"));
    }
    if let Err(error) = reporter.send_otlp(&report).await {
        warn!("optional OTLP export failed: {error}");
    }
    Ok(())
}

struct OtlpQueue {
    sender: mpsc::Sender<AgentReport>,
    worker: tokio::task::JoinHandle<()>,
}

impl OtlpQueue {
    fn spawn(reporter: Reporter) -> Self {
        // OTLP is an optional secondary output. A bounded worker prevents a slow
        // collector from delaying host sampling or primary UnionC delivery.
        let (sender, mut receiver) = mpsc::channel::<AgentReport>(128);
        let worker = tokio::spawn(async move {
            while let Some(report) = receiver.recv().await {
                if let Err(error) = reporter.send_otlp(&report).await {
                    warn!(report_id = %report.report_id, "optional OTLP export failed: {error}");
                }
            }
        });
        Self { sender, worker }
    }

    fn try_export(&self, report: &AgentReport) {
        if let Err(error) = self.sender.try_send(report.clone()) {
            warn!(report_id = %report.report_id, "optional OTLP queue rejected a report: {error}");
        }
    }

    fn abort(self) {
        // OTLP is best-effort and every primary report has already been
        // acknowledged before it reaches this queue. Do not let a Collector
        // timeout extend service shutdown by as much as 300 seconds.
        self.worker.abort();
    }
}

async fn prepare_reporter(
    config: &mut AgentConfig,
    host: &mut unionc_agent::HostIdentity,
    command: AgentCommand,
) -> anyhow::Result<Option<(Reporter, Option<(Uuid, Uuid)>)>> {
    let mut backoff = Duration::from_secs(1);
    let mut last_authorization_notice: Option<(&'static str, Option<Uuid>)> = None;
    loop {
        if command == AgentCommand::Run
            && let Some(reporter) = pairing::existing_reporter_for_run(config)?
        {
            return Ok(Some((reporter, None)));
        }
        match pairing::poll_existing(config).await {
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
                    result = shutdown_signal() => {
                        result?;
                        return Ok(None);
                    }
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
                    result = shutdown_signal() => {
                        result?;
                        return Ok(None);
                    }
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
                    result = shutdown_signal() => {
                        result?;
                        return Ok(None);
                    }
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
                    result = shutdown_signal() => {
                        result?;
                        return Ok(None);
                    }
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
            result = shutdown_signal() => {
                result?;
                return Ok(None);
            }
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

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

async fn run_loop(
    config: AgentConfig,
    host: unionc_agent::HostIdentity,
    mut sampler: SystemSampler,
    spool: Spool,
    reporter: Reporter,
    active_pairing: Option<(Uuid, Uuid)>,
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
            result = shutdown_signal() => {
                if let Err(error) = result { error!("shutdown handler failed: {error}"); }
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
            let delivery_ready = !authorization_blocked && retry_at.is_some_and(|at| now >= at);
            let next_retry = (!authorization_blocked).then_some(retry_at).flatten();
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
                                        "browser reauthorization completed; queued reports will be retried"
                                    );
                                }
                                Err(error) => {
                                    pairing_retry_at = Instant::now();
                                    warn!("pairing changed before its Reporter snapshot was loaded: {error}");
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
                            let delay = jitter(pairing_backoff, config.jitter_percent);
                            pairing_retry_at = Instant::now() + delay;
                            pairing_backoff =
                                (pairing_backoff * 2).min(Duration::from_secs(300));
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
                        Ok(FlushOutcome::Failed(error)) if error.is_revoked() => {
                            spool_flush_health.record_success();
                            let rendered = anyhow::anyhow!(error);
                            let reporter_is_current = match pairing::mark_reauth_required_if_current(
                                &config,
                                active_pairing,
                                format!("the server revoked this host credential: {rendered}"),
                            ) {
                                Ok(true) => {
                                    authorization_blocked = true;
                                    retry_at = None;
                                    pairing_retry_at = Instant::now();
                                    true
                                }
                                Ok(false) => {
                                    pairing_retry_at = Instant::now();
                                    warn!("ignored a stale 403 from a reporter superseded by newer pairing state");
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
                                    "the server revoked this host credential. The Agent will keep a bounded local \
                                     spool but will not recreate the host automatically; run `unionc-agent pair \
                                     --server <url>`: {rendered}"
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

/// 单类 spool 磁盘操作的健康度跟踪。
///
/// 单次 I/O 失败（磁盘瞬时写满、目录被误删、权限被改动）不应终止一个常驻守护进程：
/// 退出只会表现为反复崩溃重启，且期间连内存直传都停了。这里改为降级续跑，只有在
/// **同类操作连续**失败到阈值时才退出，把持续性故障交给服务管理器处理。主循环为
/// 读、写和补传各持有一个实例，避免“读取成功”掩盖“持续不可写”。
#[derive(Default)]
struct SpoolHealth {
    consecutive_failures: u32,
}

impl SpoolHealth {
    /// 连续失败多少次后放弃。按 10 秒采集间隔算约合 15 分钟持续故障。
    const MAX_CONSECUTIVE_FAILURES: u32 = 100;

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// 记录一次失败。仅当连续失败达到阈值时才返回 `Err`（从而终止主循环）。
    fn record_failure(
        &mut self,
        operation: &str,
        error: &dyn std::fmt::Display,
    ) -> anyhow::Result<()> {
        self.consecutive_failures += 1;
        warn!(
            consecutive_failures = self.consecutive_failures,
            "{operation}失败，已降级继续运行：{error}"
        );
        if self.consecutive_failures >= Self::MAX_CONSECUTIVE_FAILURES {
            anyhow::bail!(
                "spool 连续 {} 次操作失败，判定为持续性故障；退出并交由服务管理器处理",
                self.consecutive_failures
            );
        }
        Ok(())
    }

    /// 尝试把报文写入 spool。写不进去时丢弃该报文并继续，而不是终止进程。
    fn try_enqueue(&mut self, spool: &Spool, report: &AgentReport) -> anyhow::Result<()> {
        match spool.enqueue(report) {
            Ok(()) => {
                self.record_success();
                Ok(())
            }
            Err(error) => {
                self.record_failure("写入 spool", &error)?;
                warn!(report_id = %report.report_id, "本次采样未能持久化，已丢弃");
                Ok(())
            }
        }
    }
}

/// 补传 spool 中积压的报文。
///
/// 返回值区分四种结局：队列排空、32 条批次额度用尽、保留具体性质的网络失败，以及
/// spool 自身的磁盘 I/O 故障。批次边界会主动让出调度，但下一批无需等待采样 ticker。
enum FlushOutcome {
    Drained,
    BatchComplete,
    Failed(unionc_agent::transport::SendError),
}

async fn flush_spool(
    spool: &Spool,
    reporter: &Reporter,
    otlp_queue: Option<&OtlpQueue>,
) -> anyhow::Result<FlushOutcome> {
    // 每轮最多补传 32 个批次，避免长时间断线恢复后独占网络和采样线程。
    for _ in 0..32 {
        let Some(pending) = spool.oldest()? else {
            return Ok(FlushOutcome::Drained);
        };
        match reporter.send_unionc(&pending.report).await {
            Ok(()) => {
                // 顺序很重要：**先确认出队，再导出 OTLP**。
                //
                // 反过来的话，一旦 acknowledge 失败（文件已被删、权限变更等），
                // 报文会留在 spool 里，下一轮重新读取并再次导出，在 Collector
                // 侧产生重复数据点。先出队则最坏只是漏导一次——OTLP 本就是
                // 尽力而为的次要输出，漏一个点远好过重复计数。
                let report = pending.report.clone();
                spool.acknowledge(pending)?;
                if let Some(queue) = otlp_queue {
                    queue.try_export(&report);
                }
            }
            // 永久拒绝：确认出队并丢弃，否则队首这条会永远阻塞后面所有报文的补传。
            Err(error) if error.is_permanent() => {
                error!(
                    report_id = %pending.report.report_id,
                    "spool 中的报文被永久拒绝，已丢弃：{error}"
                );
                spool.acknowledge(pending)?;
            }
            Err(error) => return Ok(FlushOutcome::Failed(error)),
        }
    }
    Ok(FlushOutcome::BatchComplete)
}

fn jitter(base: Duration, percent: u8) -> Duration {
    if percent == 0 {
        return base;
    }
    let range = percent as f64 / 100.0;
    let factor = (1.0 - range) + random::<f64>() * range * 2.0;
    Duration::from_secs_f64((base.as_secs_f64() * factor).max(0.05))
}

#[cfg(unix)]
async fn shutdown_signal() -> anyhow::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result?,
        _ = terminate.recv() => {},
    }
    Ok(())
}

#[cfg(not(unix))]
async fn shutdown_signal() -> anyhow::Result<()> {
    #[cfg(windows)]
    if let Some(signal) = windows_service_host::shutdown_signal() {
        signal.cancelled().await;
        return Ok(());
    }
    tokio::signal::ctrl_c().await?;
    Ok(())
}

#[cfg(windows)]
mod windows_pair_cancel {
    use std::{
        ffi::{OsStr, c_void},
        os::windows::ffi::OsStrExt,
        thread,
    };

    use anyhow::{Context, ensure};
    use unionc_agent::service::{ShutdownSignal, shutdown_channel};
    use windows::{
        Win32::{
            Foundation::{CloseHandle, WAIT_OBJECT_0},
            System::Threading::{OpenEventW, SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject},
        },
        core::PCWSTR,
    };

    pub(super) fn open(name: &str) -> anyhow::Result<ShutdownSignal> {
        let name = OsStr::new(name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let event =
            unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(name.as_ptr())) }
                .context("failed to open the tray pairing cancellation event")?;
        let event_raw = event.0 as isize;
        let (controller, signal) = shutdown_channel();
        thread::Builder::new()
            .name("unionc-pair-cancel".into())
            .spawn(move || {
                let event = windows::Win32::Foundation::HANDLE(event_raw as *mut c_void);
                let result = unsafe { WaitForSingleObject(event, u32::MAX) };
                let _ = unsafe { CloseHandle(event) };
                if result == WAIT_OBJECT_0 {
                    controller.request_shutdown();
                }
            })
            .context("failed to start the tray cancellation waiter")?;
        ensure!(!signal.is_requested(), "tray pairing was already cancelled");
        Ok(signal)
    }
}

#[cfg(windows)]
mod windows_service_host {
    use std::{
        ffi::c_void,
        panic::{AssertUnwindSafe, catch_unwind},
        ptr,
        sync::{
            Mutex, OnceLock,
            atomic::{AtomicPtr, AtomicU32, Ordering},
        },
        time::Duration,
    };

    use anyhow::Context;
    use unionc_agent::service::{
        ShutdownController, ShutdownSignal, WINDOWS_SERVICE_NAME, shutdown_channel,
    };
    use windows::{
        Win32::{
            Foundation::{ERROR_SERVICE_SPECIFIC_ERROR, NO_ERROR},
            System::Services::{
                RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
                SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP,
                SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS,
                SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING,
                SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
                StartServiceCtrlDispatcherW,
            },
        },
        core::PWSTR,
    };

    static STATUS_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    static CURRENT_STATE: AtomicU32 = AtomicU32::new(0);
    static EXIT_CODE: AtomicU32 = AtomicU32::new(0);
    static SERVICE_EXIT_CODE: AtomicU32 = AtomicU32::new(0);
    static CHECKPOINT: AtomicU32 = AtomicU32::new(0);
    static WAIT_HINT: AtomicU32 = AtomicU32::new(0);
    static TRANSITION: Mutex<()> = Mutex::new(());
    static SHUTDOWN_CONTROLLER: OnceLock<ShutdownController> = OnceLock::new();
    static SHUTDOWN_SIGNAL: OnceLock<ShutdownSignal> = OnceLock::new();

    const START_WAIT_HINT_MS: u32 = 30_000;
    const STOP_WAIT_HINT_MS: u32 = 30_000;
    const SERVICE_FAILURE_RUNTIME: u32 = 1;
    const SERVICE_FAILURE_PANIC: u32 = 2;

    pub(super) fn dispatch() -> anyhow::Result<()> {
        let mut service_name = WINDOWS_SERVICE_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let table = [
            SERVICE_TABLE_ENTRYW {
                lpServiceName: PWSTR(service_name.as_mut_ptr()),
                lpServiceProc: Some(service_main),
            },
            SERVICE_TABLE_ENTRYW::default(),
        ];
        // The SCM owns the calling thread until the service main function exits.
        unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }
            .context("failed to connect UnionC Agent to the Windows Service Control Manager")
    }

    pub(super) fn shutdown_signal() -> Option<ShutdownSignal> {
        SHUTDOWN_SIGNAL.get().cloned()
    }

    unsafe extern "system" fn service_main(_argument_count: u32, _arguments: *mut PWSTR) {
        let outcome = catch_unwind(AssertUnwindSafe(service_main_inner));
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                eprintln!("UnionC Agent service failed: {error:#}");
                if CURRENT_STATE.load(Ordering::Acquire) != SERVICE_STOPPED.0 {
                    let _ = report_stopped(SERVICE_FAILURE_RUNTIME);
                }
            }
            Err(_) => {
                eprintln!("UnionC Agent service panicked");
                if CURRENT_STATE.load(Ordering::Acquire) != SERVICE_STOPPED.0 {
                    let _ = report_stopped(SERVICE_FAILURE_PANIC);
                }
            }
        }
    }

    fn service_main_inner() -> anyhow::Result<()> {
        let (controller, signal) = shutdown_channel();
        SHUTDOWN_CONTROLLER
            .set(controller)
            .map_err(|_| anyhow::anyhow!("SCM shutdown controller was already initialized"))?;
        SHUTDOWN_SIGNAL
            .set(signal.clone())
            .map_err(|_| anyhow::anyhow!("SCM shutdown signal was already initialized"))?;

        let service_name = WINDOWS_SERVICE_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let handle = unsafe {
            RegisterServiceCtrlHandlerExW(
                windows::core::PCWSTR(service_name.as_ptr()),
                Some(control_handler),
                None,
            )
        }
        .context("failed to register the UnionC Agent service control handler")?;
        STATUS_HANDLE.store(handle.0, Ordering::Release);
        report_status(SERVICE_START_PENDING, 0, 1, START_WAIT_HINT_MS)?;

        super::init_tracing()?;
        let runtime = super::build_runtime()?;

        match runtime.block_on(super::run_agent(Some(report_running))) {
            Ok(()) => report_stopped(0),
            Err(error) => {
                eprintln!("UnionC Agent runtime failed: {error:#}");
                report_stopped(SERVICE_FAILURE_RUNTIME)?;
                Err(error)
            }
        }
    }

    unsafe extern "system" fn control_handler(
        control: u32,
        _event_type: u32,
        _event_data: *mut c_void,
        _context: *mut c_void,
    ) -> u32 {
        match control {
            SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
                let _transition = TRANSITION.lock().unwrap_or_else(|error| error.into_inner());
                if CURRENT_STATE.load(Ordering::Acquire) != SERVICE_STOPPED.0 {
                    let _ = report_status(SERVICE_STOP_PENDING, 0, 1, STOP_WAIT_HINT_MS);
                    if let Some(controller) = SHUTDOWN_CONTROLLER.get() {
                        controller.request_shutdown();
                    }
                    start_stop_progress_reporter();
                }
            }
            SERVICE_CONTROL_INTERROGATE => {
                let _transition = TRANSITION.lock().unwrap_or_else(|error| error.into_inner());
                let _ = repeat_current_status();
            }
            _ => {}
        }
        NO_ERROR.0
    }

    fn report_running() -> anyhow::Result<bool> {
        let _transition = TRANSITION.lock().unwrap_or_else(|error| error.into_inner());
        if SHUTDOWN_SIGNAL
            .get()
            .is_some_and(ShutdownSignal::is_requested)
        {
            report_status(SERVICE_STOP_PENDING, 0, 1, STOP_WAIT_HINT_MS)?;
            start_stop_progress_reporter();
            return Ok(false);
        }
        report_status(SERVICE_RUNNING, 0, 0, 0)?;
        Ok(true)
    }

    fn start_stop_progress_reporter() {
        static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if STARTED.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Err(error) = std::thread::Builder::new()
            .name("unionc-service-stop-progress".into())
            .spawn(|| {
                let mut checkpoint = 2;
                loop {
                    std::thread::sleep(Duration::from_secs(5));
                    let _transition = TRANSITION.lock().unwrap_or_else(|error| error.into_inner());
                    if CURRENT_STATE.load(Ordering::Acquire) != SERVICE_STOP_PENDING.0 {
                        return;
                    }
                    let _ = report_status(SERVICE_STOP_PENDING, 0, checkpoint, STOP_WAIT_HINT_MS);
                    checkpoint = checkpoint.saturating_add(1);
                }
            })
        {
            // This function is called from an extern "system" SCM callback.
            // Never panic across that FFI boundary if the OS cannot allocate a
            // progress thread; the already-published STOP_PENDING status and
            // main shutdown signal remain valid.
            eprintln!("failed to start service stop progress reporter: {error}");
        }
    }

    fn report_stopped(service_exit_code: u32) -> anyhow::Result<()> {
        let _transition = TRANSITION.lock().unwrap_or_else(|error| error.into_inner());
        let win32_exit_code = if service_exit_code == 0 {
            NO_ERROR.0
        } else {
            ERROR_SERVICE_SPECIFIC_ERROR.0
        };
        set_service_status(SERVICE_STOPPED, service_exit_code, 0, 0, win32_exit_code)
    }

    fn repeat_current_status() -> anyhow::Result<()> {
        let state = SERVICE_STATUS_CURRENT_STATE(CURRENT_STATE.load(Ordering::Acquire));
        set_service_status(
            state,
            SERVICE_EXIT_CODE.load(Ordering::Acquire),
            CHECKPOINT.load(Ordering::Acquire),
            WAIT_HINT.load(Ordering::Acquire),
            EXIT_CODE.load(Ordering::Acquire),
        )
    }

    fn report_status(
        state: SERVICE_STATUS_CURRENT_STATE,
        service_exit_code: u32,
        checkpoint: u32,
        wait_hint: u32,
    ) -> anyhow::Result<()> {
        set_service_status(state, service_exit_code, checkpoint, wait_hint, NO_ERROR.0)
    }

    fn set_service_status(
        state: SERVICE_STATUS_CURRENT_STATE,
        service_exit_code: u32,
        checkpoint: u32,
        wait_hint: u32,
        win32_exit_code: u32,
    ) -> anyhow::Result<()> {
        let raw_handle = STATUS_HANDLE.load(Ordering::Acquire);
        if raw_handle.is_null() {
            anyhow::bail!("the Windows service status handle is unavailable");
        }
        let controls = if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        };
        let status = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: state,
            dwControlsAccepted: controls,
            dwWin32ExitCode: win32_exit_code,
            dwServiceSpecificExitCode: service_exit_code,
            dwCheckPoint: checkpoint,
            dwWaitHint: wait_hint,
        };
        unsafe { SetServiceStatus(SERVICE_STATUS_HANDLE(raw_handle), &status) }
            .context("failed to report UnionC Agent service status")?;
        CURRENT_STATE.store(state.0, Ordering::Release);
        EXIT_CODE.store(win32_exit_code, Ordering::Release);
        SERVICE_EXIT_CODE.store(service_exit_code, Ordering::Release);
        CHECKPOINT.store(checkpoint, Ordering::Release);
        WAIT_HINT.store(wait_hint, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_jitter_is_exact() {
        assert_eq!(jitter(Duration::from_secs(10), 0), Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_delivery_notification_channel_does_not_shift_sampling_cadence() {
        let (sender, _receiver) = mpsc::channel(1);
        let trigger = DeliveryTrigger { sender };
        let mut cadence = SamplingCadence::starting_now();
        let start = cadence.deadline();

        for index in 0..4 {
            tokio::time::sleep_until(cadence.deadline()).await;
            assert_eq!(
                tokio::time::Instant::now(),
                start + Duration::from_secs(index * 10),
                "a blocked delivery consumer must not move sampling tick {index}"
            );
            assert!(trigger.notify());
            cadence.schedule_next(Duration::from_secs(10), tokio::time::Instant::now());
        }
    }

    #[test]
    fn cadence_skips_an_overrun_instead_of_bursting_missed_samples() {
        let mut cadence = SamplingCadence::starting_now();
        let start = cadence.deadline();
        cadence.schedule_next(Duration::from_secs(10), start + Duration::from_secs(25));
        assert_eq!(cadence.deadline(), start + Duration::from_secs(35));
    }

    #[tokio::test(start_paused = true)]
    async fn delivery_worker_shutdown_has_a_hard_upper_bound() {
        let worker = tokio::spawn(async {
            std::future::pending::<()>().await;
            Ok(())
        });
        let started = tokio::time::Instant::now();
        stop_delivery_worker(worker).await.unwrap();
        assert_eq!(
            tokio::time::Instant::now().duration_since(started),
            Duration::from_secs(5)
        );
    }

    #[tokio::test]
    async fn tray_pairing_cancel_wakes_without_terminating_the_process() {
        let (controller, signal) = unionc_agent::service::shutdown_channel();
        controller.request_shutdown();
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_pairing_abort(Some(&signal), None),
        )
        .await
        .expect("graceful pairing cancellation should wake")
        .unwrap();
        assert_eq!(outcome, PairingWait::Cancelled);
    }

    #[tokio::test]
    async fn tray_pairing_deadline_is_observed_at_network_select_boundaries() {
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_pairing_abort(None, Some(Instant::now())),
        )
        .await
        .expect("expired pairing deadline should wake")
        .unwrap();
        assert_eq!(outcome, PairingWait::Deadline);
    }

    #[test]
    fn pairing_activation_loads_the_server_assigned_identity() {
        let directory = std::env::temp_dir().join(format!("unionc-active-host-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let instance_id = Uuid::new_v4();
        fs::write(directory.join("host-id"), instance_id.to_string()).unwrap();
        let stale_id = Uuid::new_v4();
        let mut config = AgentConfig::default();
        config.state_dir = directory.clone();
        config.config_path = Some(directory.join("config.json"));
        config.host_name = Some("paired-name".into());
        let mut host = unionc_agent::HostIdentity {
            id: stale_id.to_string(),
            name: "stale".into(),
            os: "test".into(),
            os_version: None,
            kernel_version: None,
            arch: "test".into(),
            agent_version: "test".into(),
        };

        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        fs::write(directory.join("agent-token"), "paired-token").unwrap();
        fs::write(directory.join("host-id"), instance_id.to_string()).unwrap();
        fs::write(
            directory.join("auth-state.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "status": "authorized",
                "reason": "current pairing completed",
                "changed_at": chrono::Utc::now()
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            directory.join("pairing-state.json"),
            serde_json::to_vec(&serde_json::json!({
                "phase": "active",
                "version": env!("CARGO_PKG_VERSION"),
                "generation": generation,
                "request_id": request_id,
                "activation_url": "https://unionc.example/agent/activate/test",
                "instance_id": instance_id,
                "report_endpoint": "https://unionc.example/api/agent/v1/report",
                "host_name": "paired-name",
                "completed_at": chrono::Utc::now()
            }))
            .unwrap(),
        )
        .unwrap();

        let _reporter = pairing::activate_reporter_snapshot(
            &mut config,
            &mut host,
            generation,
            request_id,
            instance_id,
            "https://unionc.example/api/agent/v1/report",
        )
        .unwrap();

        assert_eq!(host.id, instance_id.to_string());
        assert_eq!(host.name, "paired-name");
        assert_eq!(
            config.endpoint,
            "https://unionc.example/api/agent/v1/report"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    /// 偶发 I/O 失败必须降级续跑，不能终止常驻进程。
    #[test]
    fn transient_spool_failures_do_not_stop_the_agent() {
        let mut health = SpoolHealth::default();
        for _ in 0..(SpoolHealth::MAX_CONSECUTIVE_FAILURES - 1) {
            health
                .record_failure("测试", &"disk full")
                .expect("未达阈值前必须继续运行");
        }
    }

    /// 但持续性故障要退出，交给服务管理器处理——否则会静默地一直丢数据。
    #[test]
    fn sustained_spool_failures_eventually_stop_the_agent() {
        let mut health = SpoolHealth::default();
        for _ in 0..(SpoolHealth::MAX_CONSECUTIVE_FAILURES - 1) {
            health.record_failure("测试", &"disk full").unwrap();
        }
        let error = health
            .record_failure("测试", &"disk full")
            .expect_err("达到阈值必须返回错误以终止主循环");
        assert!(
            error.to_string().contains("持续性故障"),
            "错误信息应说明这是持续性故障而非偶发，实际为：{error}"
        );
    }

    /// 中间只要成功一次，计数就归零——阈值针对的是**连续**失败。
    #[test]
    fn a_single_success_resets_the_failure_streak() {
        let mut health = SpoolHealth::default();
        for _ in 0..(SpoolHealth::MAX_CONSECUTIVE_FAILURES - 1) {
            health.record_failure("测试", &"transient").unwrap();
        }
        health.record_success();
        // 归零后应能再撑满一整轮，说明计数确实被重置了。
        for _ in 0..(SpoolHealth::MAX_CONSECUTIVE_FAILURES - 1) {
            health
                .record_failure("测试", &"transient")
                .expect("成功一次后计数应归零");
        }
    }
}
