#![cfg_attr(
    not(any(
        feature = "module-sentinel-monitor",
        feature = "module-photo-backup",
        feature = "module-dufs",
        feature = "module-sunshine",
        feature = "module-host-monitoring"
    )),
    allow(unreachable_code, unused_imports, unused_variables)
)]

#[cfg(any(feature = "module-photo-backup", feature = "module-dufs"))]
use std::os::unix::fs::DirBuilderExt;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, bail};
use sarmg_platform_core::ModuleHealthState;
use tokio::{process::Command, time::Instant};

use super::{ExternalService, PlatformState, WorkerPhase, gateway, spec::WorkerKind};
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const STABLE_PROCESS_AGE: Duration = Duration::from_secs(60);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(15);

pub(super) async fn supervise(platform: PlatformState, service: ExternalService) {
    let mut shutdown = platform.subscribe_worker_shutdown();
    let mut restart_count = 0_u64;
    let mut backoff = INITIAL_BACKOFF;

    loop {
        if *shutdown.borrow() {
            publish_stopped(&platform, &service, restart_count).await;
            return;
        }
        platform
            .publish_lifecycle(
                service.spec.id,
                WorkerPhase::Starting,
                None,
                restart_count,
                ModuleHealthState::Probing,
                "worker supervisor 正在验证发行布局与私有配置",
            )
            .await;

        let mut command = match worker_command(&service) {
            Ok(command) => command,
            Err(error) => {
                let message = format!("worker 配置或发行布局无效：{error:#}");
                tracing::error!(module = service.spec.id, "{message}");
                platform
                    .publish_lifecycle(
                        service.spec.id,
                        WorkerPhase::Backoff,
                        None,
                        restart_count,
                        ModuleHealthState::Unconfigured,
                        message,
                    )
                    .await;
                if wait_or_shutdown(&mut shutdown, MAX_BACKOFF).await {
                    publish_stopped(&platform, &service, restart_count).await;
                    return;
                }
                continue;
            }
        };

        let started_at = Instant::now();
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let message = format!("无法启动私有 worker：{error}");
                tracing::error!(module = service.spec.id, "{message}");
                platform
                    .publish_lifecycle(
                        service.spec.id,
                        WorkerPhase::Backoff,
                        None,
                        restart_count,
                        ModuleHealthState::Degraded,
                        message,
                    )
                    .await;
                restart_count = restart_count.saturating_add(1);
                if wait_or_shutdown(&mut shutdown, backoff).await {
                    publish_stopped(&platform, &service, restart_count).await;
                    return;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };
        let pid = child.id();
        platform
            .publish_lifecycle(
                service.spec.id,
                WorkerPhase::Running,
                pid,
                restart_count,
                ModuleHealthState::Probing,
                "worker 已启动；等待带 audience 的 gateway-v1 探测",
            )
            .await;
        tracing::info!(
            module = service.spec.id,
            pid,
            bind = %service.spec.bind,
            "private module worker started"
        );

        let status = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow_and_update() {
                    stop_child(&mut child, service.spec.id).await;
                    publish_stopped(&platform, &service, restart_count).await;
                    return;
                }
                continue;
            }
            status = child.wait() => status,
        };

        let age = started_at.elapsed();
        restart_count = restart_count.saturating_add(1);
        let message = match status {
            Ok(status) => format!("worker 意外退出（{status}）；将在退避后重启"),
            Err(error) => format!("等待 worker 退出状态失败（{error}）；将在退避后重启"),
        };
        tracing::warn!(module = service.spec.id, restart_count, "{message}");
        if age >= STABLE_PROCESS_AGE {
            backoff = INITIAL_BACKOFF;
        }
        platform
            .publish_lifecycle(
                service.spec.id,
                WorkerPhase::Backoff,
                None,
                restart_count,
                ModuleHealthState::Degraded,
                message,
            )
            .await;
        if wait_or_shutdown(&mut shutdown, backoff).await {
            publish_stopped(&platform, &service, restart_count).await;
            return;
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn publish_stopped(platform: &PlatformState, service: &ExternalService, restart_count: u64) {
    platform
        .publish_lifecycle(
            service.spec.id,
            WorkerPhase::Stopped,
            None,
            restart_count,
            ModuleHealthState::Degraded,
            "Union 正在关闭；私有 worker 已停止",
        )
        .await;
}

async fn wait_or_shutdown(
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
    delay: Duration,
) -> bool {
    tokio::select! {
        biased;
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow_and_update(),
        () = tokio::time::sleep(delay) => false,
    }
}

async fn stop_child(child: &mut tokio::process::Child, module: &str) {
    if let Some(pid) = child.id() {
        // SAFETY: `pid` came from this live Child. SIGTERM is the worker contract's graceful
        // shutdown trigger; ESRCH only means the process won the race and already exited.
        let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                tracing::warn!(module, pid, "failed to send worker SIGTERM: {error}");
            }
        }
    }
    match tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await {
        Ok(Ok(status)) => tracing::info!(module, %status, "private worker stopped"),
        Ok(Err(error)) => tracing::warn!(module, "failed waiting for worker shutdown: {error}"),
        Err(_) => {
            tracing::warn!(module, "worker exceeded graceful shutdown; sending SIGKILL");
            if let Err(error) = child.start_kill()
                && error.kind() != std::io::ErrorKind::InvalidInput
            {
                tracing::warn!(module, "failed to kill worker: {error}");
            }
            let _ = child.wait().await;
        }
    }
}

#[derive(Debug)]
struct DistributionLayout {
    root: PathBuf,
    modules: PathBuf,
    #[cfg(feature = "module-sentinel-monitor")]
    shared_assets: PathBuf,
}

impl DistributionLayout {
    fn discover() -> anyhow::Result<Self> {
        Self::from_union_executable(&std::env::current_exe()?)
    }

    fn from_union_executable(executable: &Path) -> anyhow::Result<Self> {
        let bin = executable
            .parent()
            .context("Union executable path has no bin directory")?;
        if bin.file_name().and_then(|name| name.to_str()) != Some("bin") {
            bail!(
                "Union executable must run from <distribution>/bin; got {}",
                executable.display()
            );
        }
        let root = bin
            .parent()
            .context("Union bin directory has no distribution root")?
            .to_path_buf();
        Ok(Self {
            modules: root.join("libexec/union/modules"),
            #[cfg(feature = "module-sentinel-monitor")]
            shared_assets: root.join("share/union/modules"),
            root,
        })
    }

    fn worker_binary(&self, module: &str) -> anyhow::Result<PathBuf> {
        let binary = self.modules.join(module);
        let metadata = fs::symlink_metadata(&binary)
            .with_context(|| format!("missing compiled worker {}", binary.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("worker binary must be a non-symlink regular file");
        }
        if metadata.mode() & 0o111 == 0 {
            bail!("worker binary is not executable: {}", binary.display());
        }
        if metadata.nlink() != 1 {
            bail!("worker binary must have exactly one hard link");
        }
        Ok(binary)
    }
}

fn worker_command(service: &ExternalService) -> anyhow::Result<Command> {
    debug_assert_eq!(service.credential.audience, service.spec.id);
    let layout = DistributionLayout::discover()?;
    let binary = layout.worker_binary(service.spec.id)?;
    #[cfg(any(feature = "module-photo-backup", feature = "module-dufs"))]
    let data = module_data_directory(service.spec.id)?;
    let mut command = Command::new(binary);
    command
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .current_dir("/")
        .env("UNION_MODULE_PROTOCOL", gateway::PROTOCOL_VERSION)
        .env("UNION_MODULE_AUDIENCE", service.spec.id)
        .env("UNION_MODULE_TOKEN", service.credential.token.as_ref())
        .env("UNION_MODULE_PREFIX", service.spec.gateway_prefix);
    copy_optional(&mut command, "UNIONC_MODULE_RUST_LOG", "RUST_LOG");

    match service.spec.kind {
        #[cfg(feature = "module-sunshine")]
        WorkerKind::Sunshine => {
            command
                .env("SUNSHINE_BIND", service.spec.bind.to_string())
                .env("SUNSHINE_PRODUCTION", "true");
            copy_required(
                &mut command,
                "UNIONC_SUNSHINE_DATABASE_URL",
                "SUNSHINE_DATABASE_URL",
            )?;
            copy_required(
                &mut command,
                "UNIONC_SUNSHINE_CREDENTIAL_KEY",
                "SUNSHINE_CREDENTIAL_KEY",
            )?;
            copy_module_optional(&mut command, "SUNSHINE", "CREDENTIAL_KEY_ID");
        }
        #[cfg(feature = "module-host-monitoring")]
        WorkerKind::HostMonitoring => {
            command
                .arg("serve")
                .env("UNION_HOST_MONITORING_BIND", service.spec.bind.to_string());
            copy_required(
                &mut command,
                "UNIONC_HOST_MONITORING_DATABASE_URL",
                "UNION_HOST_MONITORING_DATABASE_URL",
            )?;
        }
        #[cfg(feature = "module-sentinel-monitor")]
        WorkerKind::SentinelMonitor => {
            command
                .env("BIND_ADDR", service.spec.bind.to_string())
                .env("SESSION_COOKIE_SECURE", "true")
                .env("STATIC_DIR", layout.shared_assets.join(service.spec.id));
            copy_required(&mut command, "UNIONC_SENTINEL_DATABASE_URL", "DATABASE_URL")?;
            copy_required(
                &mut command,
                "UNIONC_SENTINEL_APP_JWT_SECRET",
                "APP_JWT_SECRET",
            )?;
            copy_required(
                &mut command,
                "UNIONC_SENTINEL_CREDENTIALS_KEY",
                "CREDENTIALS_KEY",
            )?;
            for name in [
                "BOOTSTRAP_ADMIN_EMAIL",
                "BOOTSTRAP_ADMIN_PASSWORD",
                "MEDIA_TOKEN_TTL_SECS",
                "STATUS_INTERVAL_SECS",
                "RECONCILE_INTERVAL_SECS",
                "REQUEST_TIMEOUT_SECS",
                "ONVIF_DISCOVERY_TIMEOUT_MS",
            ] {
                copy_module_optional(&mut command, "SENTINEL", name);
            }
        }
        #[cfg(feature = "module-photo-backup")]
        WorkerKind::PhotoBackup => {
            command
                .env("BIND", service.spec.bind.to_string())
                .env("DATA_DIR", data.join("content"))
                .env("REQUIRE_HTTPS", "true");
            copy_required(
                &mut command,
                "UNIONC_PHOTO_BACKUP_DATABASE_URL",
                "DATABASE_URL",
            )?;
            copy_required(
                &mut command,
                "UNIONC_PHOTO_BACKUP_ADMIN_USERNAME",
                "ADMIN_USERNAME",
            )?;
            copy_required(
                &mut command,
                "UNIONC_PHOTO_BACKUP_ADMIN_PASSWORD",
                "ADMIN_PASSWORD",
            )?;
            for name in ["MAX_PART_BYTES", "METRICS_TOKEN"] {
                copy_module_optional(&mut command, "PHOTO_BACKUP", name);
            }
        }
        #[cfg(feature = "module-dufs")]
        WorkerKind::Dufs => {
            let config = data.join("dufs.yaml");
            validate_private_config(&config)?;
            command
                .current_dir(&data)
                .arg("--config")
                .arg(config)
                .arg("--bind")
                .arg(service.spec.bind.ip().to_string())
                .arg("--port")
                .arg(service.spec.bind.port().to_string());
        }
    }
    // Keep the root alive in the command construction scope and document that relative lookup is
    // never used. The executable itself is always the validated absolute path above.
    debug_assert!(layout.root.is_absolute());
    Ok(command)
}

#[cfg(any(feature = "module-photo-backup", feature = "module-dufs"))]
fn module_data_directory(module: &str) -> anyhow::Result<PathBuf> {
    let modules = crate::infra::paths::data_dir().join("modules");
    let data = modules.join(module);
    create_private_directory(&modules)?;
    create_private_directory(&data)?;
    Ok(data)
}

#[cfg(any(feature = "module-photo-backup", feature = "module-dufs"))]
fn create_private_directory(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_directory(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
                .create(path)
                .with_context(|| format!("failed to create {}", path.display()))?;
            let metadata = fs::symlink_metadata(path)?;
            validate_private_directory(path, &metadata)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(any(feature = "module-photo-backup", feature = "module-dufs"))]
fn validate_private_directory(path: &Path, metadata: &fs::Metadata) -> anyhow::Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "module data path is not a non-symlink directory: {}",
            path.display()
        );
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        bail!("module data directory is not owned by the Union service user");
    }
    if metadata.mode() & 0o7777 != 0o700 {
        bail!("module data directory must have exact permissions 0700");
    }
    Ok(())
}

#[cfg(feature = "module-dufs")]
fn validate_private_config(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("missing Dufs module config {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.nlink() != 1 {
        bail!("Dufs module config must be a single-link non-symlink regular file");
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        bail!("Dufs module config is not owned by the Union service user");
    }
    if !matches!(metadata.mode() & 0o7777, 0o400 | 0o600) {
        bail!("Dufs module config must have exact permissions 0400 or 0600");
    }
    Ok(())
}

fn copy_required(command: &mut Command, source: &str, target: &str) -> anyhow::Result<()> {
    let value = std::env::var_os(source).filter(|value| !value.is_empty());
    let Some(value) = value else {
        bail!("required module setting {source} is missing");
    };
    command.env(target, value);
    Ok(())
}

fn copy_optional(command: &mut Command, source: &str, target: &str) {
    if let Some(value) = std::env::var_os(source).filter(|value| !value.is_empty()) {
        command.env(target, value);
    }
}

fn copy_module_optional(command: &mut Command, module: &str, target: &str) {
    let source = format!("UNIONC_{module}_{target}");
    copy_optional(command, &source, target);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn distribution_layout_resolves_only_the_builder_shape() {
        let layout =
            DistributionLayout::from_union_executable(Path::new("/opt/union-release/bin/unionc"))
                .unwrap();
        assert_eq!(
            layout.modules,
            PathBuf::from("/opt/union-release/libexec/union/modules")
        );
        #[cfg(feature = "module-sentinel-monitor")]
        assert_eq!(
            layout.shared_assets,
            PathBuf::from("/opt/union-release/share/union/modules")
        );
        assert!(
            DistributionLayout::from_union_executable(Path::new("/usr/local/sbin/unionc")).is_err()
        );
    }

    #[test]
    fn exponential_backoff_is_bounded() {
        let mut delay = INITIAL_BACKOFF;
        for _ in 0..20 {
            delay = (delay * 2).min(MAX_BACKOFF);
        }
        assert_eq!(delay, MAX_BACKOFF);
    }

    #[test]
    fn module_environment_names_are_namespaced() {
        let mut command = Command::new("/does/not/run");
        // An absent optional input must not invent or inherit a generic child setting.
        copy_module_optional(&mut command, "SENTINEL", "REQUEST_TIMEOUT_SECS");
        let configured = command.as_std().get_envs().collect::<Vec<_>>();
        assert!(
            configured
                .iter()
                .all(|(name, _)| name != &OsString::from("BIND_ADDR"))
        );
    }
}
