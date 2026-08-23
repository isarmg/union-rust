use super::*;

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
                Ok(id) if id.to_string() == value => HostInspection {
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
                _ => HostInspection {
                    id: None,
                    check: DiagnosticCheck::new(
                        "identity",
                        "error",
                        Some("identity_invalid"),
                        format!(
                            "{} does not contain a canonical lowercase, hyphenated UUID",
                            path.display()
                        ),
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

fn inspect_configuration(config: &AgentConfig, configured: bool) -> DiagnosticCheck {
    let started = Instant::now();
    match config.validate_for_diagnostics() {
        Err(error) => DiagnosticCheck::new(
            "configuration",
            "error",
            Some("config_invalid"),
            error.to_string(),
            Some("repair the configuration file, then run status again"),
            started,
        ),
        Ok(()) if configured => DiagnosticCheck::new(
            "configuration",
            "ok",
            None,
            "configuration file is present and its effective settings are valid",
            None::<String>,
            started,
        ),
        Ok(()) => DiagnosticCheck::new(
            "configuration",
            "missing",
            Some("config_missing"),
            "configuration file is not present",
            Some("pair this host to create its private configuration"),
            started,
        ),
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

pub(super) fn print_local_status(config: &AgentConfig) -> anyhow::Result<()> {
    let configured = config
        .config_path
        .as_ref()
        .is_some_and(|path| path.is_file());
    let config_check = inspect_configuration(config, configured);
    let host = inspect_host_identity(config);
    let credential = inspect_credential(config);
    let mut spool = inspect_spool(&config.state_dir);
    let spool_check = spool
        .check
        .take()
        .expect("spool inspection always produces a check");

    let pairing_result = pairing::local_status(config);
    let authorization_result = pairing::local_auth_state(config);
    let pairing_error = pairing_result.as_ref().err().map(ToString::to_string);
    let authorization_error = authorization_result.as_ref().err().map(ToString::to_string);
    let pairing_status = pairing_result.ok();
    let pairing = pairing_status
        .as_ref()
        .and_then(|status| status.progress.as_ref());
    let active_endpoint = pairing_status
        .as_ref()
        .and_then(|status| status.active_report_endpoint.as_deref());
    let active_binding_persisted = pairing_status
        .as_ref()
        .is_some_and(|status| status.active_binding_persisted);
    let status_endpoint = pairing_error
        .is_none()
        .then(|| active_endpoint.unwrap_or(config.endpoint.as_str()));
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
    let config_status = config_check.status;
    let (binding_status, binding_code, binding_message) = if let Some(error) = &pairing_error {
        (
            "error",
            Some("active_pairing_snapshot_invalid"),
            error.clone(),
        )
    } else if active_endpoint.is_some() && !active_binding_persisted {
        (
            "warning",
            Some("active_binding_missing"),
            "legacy Active state will create its private endpoint binding on the next run".into(),
        )
    } else if active_endpoint.is_some() {
        (
            "ok",
            None,
            "active credential endpoint binding is readable and current".into(),
        )
    } else {
        (
            "skipped",
            None,
            "there is no Active pairing endpoint to inspect".into(),
        )
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
        "active_binding": {
            "status": binding_status,
            "code": binding_code,
            "message": binding_message
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
        "endpoint": status_endpoint,
        "state_dir": config.state_dir,
        "host_id": host.id,
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
            println!("  Configuration: {config_status}");
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
            println!("  Endpoint: {}", status_endpoint.unwrap_or("not available"));
            println!(
                "  Spool: {} pending, {} invalid, {} bytes",
                spool.pending_batches, spool.invalid_batches, spool.total_bytes
            );
            println!("  Next: {next_action}");
        }
    }
    Ok(())
}

pub(super) async fn run_read_only_doctor(config: &AgentConfig) -> anyhow::Result<()> {
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
    let collection_host = transient_host_identity(diagnostic_id);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_inspection_rejects_noncanonical_uuid_text() {
        let directory =
            std::env::temp_dir().join(format!("unionc-diagnostic-host-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("host-id"),
            "BBBBBBBB-BBBB-4BBB-8BBB-BBBBBBBBBBBB",
        )
        .unwrap();
        let mut config = AgentConfig::default();
        config.state_dir = directory.clone();

        let inspection = inspect_host_identity(&config);
        fs::remove_dir_all(directory).unwrap();

        assert!(inspection.id.is_none());
        assert_eq!(inspection.check.status, "error");
        assert_eq!(inspection.check.code, Some("identity_invalid"));
    }

    #[test]
    fn status_configuration_check_validates_effective_settings() {
        let mut config = AgentConfig::default();
        config.interval_seconds = 0;

        let check = inspect_configuration(&config, true);

        assert_eq!(check.status, "error");
        assert_eq!(check.code, Some("config_invalid"));
        assert!(check.message.contains("interval_seconds"));
    }
}
