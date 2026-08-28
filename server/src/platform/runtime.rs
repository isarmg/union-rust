use std::{
    collections::BTreeMap,
    future::Future,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    pin::Pin,
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use axum::{body::Body, extract::Request, response::Response};
use sarmg_platform_core::{
    DatabaseMigration, Execution, HealthDefinition, MigrationEngine, PlatformVersions,
    PluginCatalog, PluginHealthState, PluginManifest, RestartPolicy, RouteAuth, ServiceProtocol,
};
use serde::Serialize;
use tokio::{
    process::Command,
    sync::{Mutex, RwLock, watch},
    task::JoinHandle,
};

use super::{
    configuration::ConfigurationRegistry,
    event_bus::{EventBus, EventContract},
    notifications::NotificationCenter,
    package_store::{PackageSource, PackageStore, SelectedPackage, resolve_bundle_path},
    rbac::{PermissionDefinition, PermissionRegistry},
    tasks::TaskScheduler,
};

pub const CORE_VERSION: &str = "0.5.0";

pub type PluginFuture<T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send>>;
pub type PluginResponseFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

#[derive(Clone)]
pub struct PluginContext {
    pub id: String,
    pub version: String,
    pub manifest: PluginManifest,
    pub package_root: PathBuf,
    pub events: EventBus,
    pub tasks: TaskScheduler,
    pub notifications: NotificationCenter,
}

/// Only factories linked into Union by trusted Rust code can satisfy an `in_process` manifest.
pub trait InProcessFactory: Send + Sync {
    fn create(
        &self,
        context: PluginContext,
        artifact: PathBuf,
        entrypoint: String,
    ) -> PluginFuture<Arc<dyn sarmg_platform_sdk::InProcessPlugin>>;
}

#[derive(Debug, Clone)]
pub struct ServiceEndpoint {
    pub base_url: url::Url,
    /// Headers are supplied by trusted adapter code, never by a manifest or lifecycle request.
    pub headers: BTreeMap<String, String>,
}

impl ServiceEndpoint {
    pub fn validate(&self) -> anyhow::Result<()> {
        if !matches!(self.base_url.scheme(), "http" | "https")
            || self.base_url.cannot_be_a_base()
            || self.base_url.host_str().is_none()
            || self.base_url.query().is_some()
            || self.base_url.fragment().is_some()
        {
            anyhow::bail!("trusted service adapter returned an invalid HTTP endpoint");
        }
        for (name, value) in &self.headers {
            axum::http::HeaderName::from_bytes(name.as_bytes())?;
            axum::http::HeaderValue::from_str(value)?;
        }
        Ok(())
    }
}

pub struct AdapterHandle {
    pub services: BTreeMap<String, ServiceEndpoint>,
    stop: Arc<dyn Fn() -> PluginFuture<()> + Send + Sync>,
}

impl AdapterHandle {
    pub fn new(
        services: BTreeMap<String, ServiceEndpoint>,
        stop: impl Fn() -> PluginFuture<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            services,
            stop: Arc::new(stop),
        }
    }

    async fn stop(&self) -> anyhow::Result<()> {
        (self.stop)().await
    }
}

/// Container and already-independent services are controlled only through a trusted adapter
/// registered by host code. Image names and service ids in a manifest are metadata, not commands.
pub trait ServiceLifecycleAdapter: Send + Sync {
    fn start(
        &self,
        context: PluginContext,
        manifest: PluginManifest,
    ) -> PluginFuture<AdapterHandle>;
}

pub trait MigrationRunner: Send + Sync {
    fn apply(&self, context: PluginContext, migration: DatabaseMigration) -> PluginFuture<()>;
}

#[derive(Default)]
struct TrustedComponents {
    factories: BTreeMap<String, Arc<dyn InProcessFactory>>,
    adapters: BTreeMap<String, Arc<dyn ServiceLifecycleAdapter>>,
    migrations: BTreeMap<&'static str, Arc<dyn MigrationRunner>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedFrontend {
    pub entry: String,
    pub styles: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleRuntimeView {
    #[serde(flatten)]
    pub manifest: PluginManifest,
    pub package_source: PackageSource,
    pub enabled: bool,
    pub lifecycle_state: PluginHealthState,
    pub health_message: String,
    pub pid: Option<u32>,
    pub restart_count: u64,
    pub checked_at: Option<String>,
    pub resolved_frontend: ResolvedFrontend,
}

#[derive(Debug, Clone)]
struct RuntimeSnapshot {
    state: PluginHealthState,
    message: String,
    pid: Option<u32>,
    restart_count: u64,
    checked_at: Option<String>,
}

impl RuntimeSnapshot {
    fn discovered(enabled: bool) -> Self {
        Self {
            state: if enabled {
                PluginHealthState::Discovered
            } else {
                PluginHealthState::Stopped
            },
            message: if enabled {
                "manifest discovered and validated".into()
            } else {
                "module is included in this distribution but disabled".into()
            },
            pid: None,
            restart_count: 0,
            checked_at: Some(chrono::Utc::now().to_rfc3339()),
        }
    }
}

#[derive(Clone)]
enum RunningBackend {
    InProcess(Arc<dyn sarmg_platform_sdk::InProcessPlugin>),
    Http(Arc<HttpBackend>),
}

#[derive(Clone)]
struct HttpBackend {
    endpoint: ServiceEndpoint,
    process: Option<ManagedProcess>,
}

const HEALTH_FAILURE_THRESHOLD: u32 = 2;

#[derive(Debug, Clone)]
struct HealthMonitorSnapshot {
    state: PluginHealthState,
    message: String,
    checked_at: Option<String>,
}

#[derive(Clone)]
struct HealthMonitor {
    snapshot: watch::Receiver<HealthMonitorSnapshot>,
    shutdown: watch::Sender<bool>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl HealthMonitor {
    fn start(manifest: PluginManifest, backend: RunningBackend) -> anyhow::Result<Self> {
        let interval = match manifest.health {
            HealthDefinition::Callback {
                interval_seconds, ..
            }
            | HealthDefinition::Http {
                interval_seconds, ..
            } => Duration::from_secs(interval_seconds.into()),
        };
        Self::start_with_interval(manifest, backend, interval)
    }

    fn start_with_interval(
        manifest: PluginManifest,
        backend: RunningBackend,
        interval: Duration,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(
                health_timeout_seconds(&manifest).into(),
            ))
            .build()?;
        let (snapshot_tx, snapshot) = watch::channel(HealthMonitorSnapshot {
            state: PluginHealthState::Available,
            message: "startup health gate passed; periodic monitoring is active".into(),
            checked_at: Some(chrono::Utc::now().to_rfc3339()),
        });
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut consecutive_failures = 0_u32;
            loop {
                tokio::select! {
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow_and_update() {
                            break;
                        }
                    }
                    () = tokio::time::sleep(interval) => {
                        let result = probe_backend_once(&manifest, &backend, &client).await;
                        let checked_at = Some(chrono::Utc::now().to_rfc3339());
                        let snapshot = match result {
                            Ok(message) => {
                                consecutive_failures = 0;
                                HealthMonitorSnapshot {
                                    state: PluginHealthState::Available,
                                    message,
                                    checked_at,
                                }
                            }
                            Err(error) => {
                                consecutive_failures = consecutive_failures.saturating_add(1);
                                let degraded = consecutive_failures >= HEALTH_FAILURE_THRESHOLD;
                                HealthMonitorSnapshot {
                                    state: if degraded {
                                        PluginHealthState::Degraded
                                    } else {
                                        PluginHealthState::Available
                                    },
                                    message: format!(
                                        "periodic health check failed ({consecutive_failures}/{HEALTH_FAILURE_THRESHOLD}): {error}"
                                    ),
                                    checked_at,
                                }
                            }
                        };
                        snapshot_tx.send_replace(snapshot);
                    }
                }
            }
        });
        Ok(Self {
            snapshot,
            shutdown,
            task: Arc::new(Mutex::new(Some(task))),
        })
    }

    fn snapshot(&self) -> HealthMonitorSnapshot {
        self.snapshot.borrow().clone()
    }

    fn is_available(&self) -> bool {
        self.snapshot.borrow().state == PluginHealthState::Available
    }

    async fn stop(self) {
        self.shutdown.send_replace(true);
        if let Some(task) = self.task.lock().await.take()
            && let Err(error) = task.await
        {
            tracing::warn!("module health monitor failed: {error}");
        }
    }
}

#[derive(Clone)]
struct ModuleRecord {
    package: SelectedPackage,
    snapshot: RuntimeSnapshot,
    backend: Option<RunningBackend>,
    adapter: Option<Arc<AdapterHandle>>,
    health_monitor: Option<HealthMonitor>,
}

impl ModuleRecord {
    fn view(&self) -> ModuleRuntimeView {
        let frontend = &self.package.manifest.frontend;
        let mut snapshot = self.snapshot.clone();
        if let Some(RunningBackend::Http(http)) = self.backend.as_ref()
            && let Some(process) = http.process.as_ref()
            && let Ok(process) = process.snapshot.try_read()
        {
            snapshot.pid = process.pid;
            snapshot.restart_count = process.restart_count;
            if !process.ready && snapshot.state == PluginHealthState::Available {
                snapshot.state = process_display_state(&process);
                snapshot.message = process.message.clone();
                snapshot.checked_at = Some(chrono::Utc::now().to_rfc3339());
            }
        }
        if snapshot.state == PluginHealthState::Available
            && let Some(monitor) = self.health_monitor.as_ref()
        {
            let health = monitor.snapshot();
            snapshot.state = health.state;
            snapshot.message = health.message;
            snapshot.checked_at = health.checked_at;
        }
        ModuleRuntimeView {
            manifest: self.package.manifest.clone(),
            package_source: self.package.source,
            enabled: self.package.enabled,
            lifecycle_state: snapshot.state,
            health_message: snapshot.message,
            pid: snapshot.pid,
            restart_count: snapshot.restart_count,
            checked_at: snapshot.checked_at,
            resolved_frontend: ResolvedFrontend {
                entry: frontend
                    .public_asset_path(&self.package.manifest.id, &frontend.entry)
                    .expect("validated frontend entry has a public path"),
                styles: frontend
                    .styles
                    .iter()
                    .map(|style| {
                        frontend
                            .public_asset_path(&self.package.manifest.id, style)
                            .expect("validated frontend style has a public path")
                    })
                    .collect(),
            },
        }
    }
}

#[derive(Clone)]
pub struct PlatformState {
    store: PackageStore,
    records: Arc<RwLock<BTreeMap<String, ModuleRecord>>>,
    trusted: Arc<RwLock<TrustedComponents>>,
    operation: Arc<Mutex<()>>,
    pub permissions: PermissionRegistry,
    pub events: EventBus,
    pub tasks: TaskScheduler,
    pub notifications: NotificationCenter,
    pub configuration: ConfigurationRegistry,
    gateway_client: reqwest::Client,
    service_directory: Arc<RwLock<BTreeMap<String, String>>>,
}

impl PlatformState {
    pub fn new(store: PackageStore, administrator: &str) -> anyhow::Result<Self> {
        let configuration = ConfigurationRegistry::new(store.configuration_directory());
        Ok(Self {
            store,
            records: Arc::new(RwLock::new(BTreeMap::new())),
            trusted: Arc::new(RwLock::new(TrustedComponents::default())),
            operation: Arc::new(Mutex::new(())),
            permissions: PermissionRegistry::for_administrator(administrator)?,
            events: EventBus::default(),
            tasks: TaskScheduler::default(),
            notifications: NotificationCenter::default(),
            configuration,
            gateway_client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .user_agent(concat!("unionc-plugin-gateway/", env!("CARGO_PKG_VERSION")))
                .build()?,
            service_directory: Arc::new(RwLock::new(BTreeMap::new())),
        })
    }

    pub fn from_environment(administrator: &str) -> anyhow::Result<Self> {
        Self::new(PackageStore::from_environment()?, administrator)
    }

    pub async fn register_in_process_factory(
        &self,
        runtime: sarmg_platform_core::InProcessRuntime,
        factory: Arc<dyn InProcessFactory>,
    ) -> anyhow::Result<()> {
        let runtime = in_process_runtime_key(runtime);
        let mut trusted = self.trusted.write().await;
        if trusted.factories.insert(runtime.into(), factory).is_some() {
            anyhow::bail!("in-process host runtime factory already registered: {runtime}");
        }
        Ok(())
    }

    pub async fn register_service_adapter(
        &self,
        module: &str,
        adapter: Arc<dyn ServiceLifecycleAdapter>,
    ) -> anyhow::Result<()> {
        if trusted_module_id(module).is_none() {
            anyhow::bail!("invalid trusted adapter module id: {module}");
        }
        let mut trusted = self.trusted.write().await;
        if trusted.adapters.insert(module.into(), adapter).is_some() {
            anyhow::bail!("service lifecycle adapter already registered: {module}");
        }
        Ok(())
    }

    pub async fn register_migration_runner(
        &self,
        engine: MigrationEngine,
        runner: Arc<dyn MigrationRunner>,
    ) -> anyhow::Result<()> {
        if engine == MigrationEngine::Embedded {
            anyhow::bail!("embedded migrations are owned by the plugin backend");
        }
        let mut trusted = self.trusted.write().await;
        if trusted
            .migrations
            .insert(migration_key(engine), runner)
            .is_some()
        {
            anyhow::bail!("migration runner already registered for {engine:?}");
        }
        Ok(())
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        let _operation = self.operation.lock().await;
        let packages = self.store.discover()?;
        self.initialize(packages).await
    }

    pub async fn rescan(&self) -> anyhow::Result<Vec<ModuleRuntimeView>> {
        let _operation = self.operation.try_lock().map_err(|_| {
            anyhow::anyhow!("another module lifecycle operation is in progress; retry")
        })?;
        let packages = self.store.discover()?;
        validate_packages(&packages)?;
        self.ensure_rescan_preserves_release(&packages).await?;
        Ok(self.modules().await)
    }

    pub async fn enable(&self, id: &str) -> anyhow::Result<Vec<ModuleRuntimeView>> {
        self.change_enabled(id, true).await
    }

    pub async fn disable(&self, id: &str) -> anyhow::Result<Vec<ModuleRuntimeView>> {
        self.change_enabled(id, false).await
    }

    async fn change_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> anyhow::Result<Vec<ModuleRuntimeView>> {
        let _operation = self.operation.try_lock().map_err(|_| {
            anyhow::anyhow!("another module lifecycle operation is in progress; retry")
        })?;
        let packages = self.store.discover()?;
        validate_packages(&packages)?;
        self.ensure_rescan_preserves_release(&packages).await?;
        let package = packages
            .into_iter()
            .find(|package| package.manifest.id == id)
            .ok_or_else(|| {
                anyhow::anyhow!("module is not included in this Union distribution: {id}")
            })?;
        let current_enabled = self
            .records
            .read()
            .await
            .get(id)
            .map(|record| record.package.enabled)
            .ok_or_else(|| anyhow::anyhow!("module runtime record does not exist: {id}"))?;
        if current_enabled == enabled {
            return Ok(self.modules().await);
        }
        if enabled {
            self.configuration
                .validate_resource_isolation()
                .await
                .context("module data-resource isolation validation failed")?;
            if !self.configuration.is_configured(id).await {
                anyhow::bail!(
                    "module configuration is not complete: configure {id} before enabling it"
                );
            }
            self.ensure_activation_ready(&package.manifest).await?;
        } else {
            let dependents = self.enabled_dependents(id).await;
            if !dependents.is_empty() {
                anyhow::bail!(
                    "module {id} is required by enabled dependents: {}",
                    dependents.join(", ")
                );
            }
        }

        let previous = self.store.selection(id)?;
        self.store.set_enabled(id, enabled)?;
        let mut record = self
            .records
            .read()
            .await
            .get(id)
            .cloned()
            .expect("module record remained present while lifecycle operation was serialized");
        record.package.enabled = enabled;
        if enabled {
            record.backend = None;
            record.adapter = None;
            record.health_monitor = None;
            record.snapshot = RuntimeSnapshot {
                state: PluginHealthState::Starting,
                message: "module enable is preparing its runtime".into(),
                pid: None,
                restart_count: 0,
                checked_at: Some(chrono::Utc::now().to_rfc3339()),
            };
            // Keep the catalog record visible throughout the asynchronous health gate. Gateway
            // traffic sees no backend until activation completes.
            self.records.write().await.insert(id.into(), record.clone());
            if let Err(error) = self.activate(&mut record).await {
                self.deactivate_runtime(&mut record).await;
                record.package.enabled = false;
                record.snapshot = RuntimeSnapshot {
                    state: PluginHealthState::Failed,
                    message: error.to_string(),
                    pid: None,
                    restart_count: 0,
                    checked_at: Some(chrono::Utc::now().to_rfc3339()),
                };
                self.records.write().await.insert(id.into(), record);
                self.store.restore_selection(id, previous.as_ref())?;
                return Err(error.context(
                    "plugin enable failed; only the target was stopped and its enable state was restored",
                ));
            }
        } else {
            // Remove the routable backend before waiting for graceful worker shutdown. The local
            // clone retains the lifecycle handles needed to terminate it.
            let mut stopping = record.clone();
            stopping.backend = None;
            stopping.adapter = None;
            stopping.health_monitor = None;
            stopping.snapshot = RuntimeSnapshot {
                state: PluginHealthState::Stopped,
                message: "module disable is stopping its private runtime".into(),
                pid: None,
                restart_count: record.snapshot.restart_count,
                checked_at: Some(chrono::Utc::now().to_rfc3339()),
            };
            self.records.write().await.insert(id.into(), stopping);
            self.deactivate_runtime(&mut record).await;
            record.snapshot = RuntimeSnapshot::discovered(false);
        }
        self.records.write().await.insert(id.into(), record);
        Ok(self.modules().await)
    }

    /// Configuration is a lifecycle operation. v1 deliberately accepts online changes only for
    /// stopped modules; a future manifest revision may opt into a module-specific transactional
    /// reload hook, but silently changing a running process' persisted configuration is unsafe.
    pub async fn set_configuration(
        &self,
        id: &str,
        value: serde_json::Value,
    ) -> anyhow::Result<super::configuration::ModuleConfiguration> {
        let _operation = self.operation.try_lock().map_err(|_| {
            anyhow::anyhow!("another module lifecycle operation is in progress; retry")
        })?;
        let record = self
            .records
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("module runtime record does not exist: {id}"))?;
        if record.package.enabled {
            anyhow::bail!("module {id} must be disabled before its configuration can be replaced");
        }
        self.configuration.set(id, value).await
    }

    async fn initialize(&self, packages: Vec<SelectedPackage>) -> anyhow::Result<()> {
        let catalog = validate_packages(&packages)?;
        if !self.records.read().await.is_empty() {
            anyhow::bail!("plugin runtime is already initialized");
        }

        let by_id = packages
            .into_iter()
            .map(|package| (package.manifest.id.clone(), package))
            .collect::<BTreeMap<_, _>>();
        let mut next = BTreeMap::new();
        for manifest in catalog.activation_order() {
            let package = by_id
                .get(&manifest.id)
                .expect("validated catalog and package table agree")
                .clone();
            let mut record = ModuleRecord {
                snapshot: RuntimeSnapshot::discovered(package.enabled),
                package,
                backend: None,
                adapter: None,
                health_monitor: None,
            };
            if let Err(error) = self.register_metadata(&record.package).await {
                self.unregister_metadata(&record.package.manifest.id).await;
                record.snapshot = RuntimeSnapshot {
                    state: PluginHealthState::Failed,
                    message: format!("module metadata registration failed: {error}"),
                    pid: None,
                    restart_count: 0,
                    checked_at: Some(chrono::Utc::now().to_rfc3339()),
                };
            }
            next.insert(record.package.manifest.id.clone(), record);
        }
        *self.records.write().await = next;

        // Activate each dependency layer concurrently. Required dependencies finish before their
        // dependents, while unrelated slow readiness probes cannot add their deadlines together.
        // This whole bootstrap still owns `operation`, so control-plane lifecycle mutations see a
        // complete, serially consistent catalog rather than half-applied records.
        let mut levels = BTreeMap::<usize, Vec<&PluginManifest>>::new();
        let mut level_by_id = BTreeMap::<String, usize>::new();
        for manifest in catalog.activation_order() {
            let level = manifest
                .dependencies
                .iter()
                .filter(|dependency| !dependency.optional)
                .filter_map(|dependency| level_by_id.get(&dependency.id))
                .max()
                .map_or(0, |level| level + 1);
            level_by_id.insert(manifest.id.clone(), level);
            levels.entry(level).or_default().push(manifest);
        }
        for manifests in levels.into_values() {
            futures_util::future::join_all(
                manifests
                    .into_iter()
                    .map(|manifest| self.activate_startup_module(manifest)),
            )
            .await;
        }
        Ok(())
    }

    async fn activate_startup_module(&self, manifest: &PluginManifest) {
        let should_activate = self
            .records
            .read()
            .await
            .get(&manifest.id)
            .is_some_and(|record| {
                record.package.enabled && record.snapshot.state != PluginHealthState::Failed
            });
        if !should_activate {
            return;
        }
        let readiness = self.ensure_activation_ready(manifest).await;
        let mut record = self
            .records
            .read()
            .await
            .get(&manifest.id)
            .cloned()
            .expect("startup record exists for every validated manifest");
        record.snapshot = RuntimeSnapshot {
            state: PluginHealthState::Starting,
            message: "module startup is entering health gates".into(),
            pid: None,
            restart_count: 0,
            checked_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        self.records
            .write()
            .await
            .insert(manifest.id.clone(), record.clone());
        let result = match readiness {
            Ok(()) => self.activate(&mut record).await,
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            self.deactivate_runtime(&mut record).await;
            record.snapshot = RuntimeSnapshot {
                state: PluginHealthState::Failed,
                message: error.to_string(),
                pid: None,
                restart_count: 0,
                checked_at: Some(chrono::Utc::now().to_rfc3339()),
            };
            tracing::error!(module = manifest.id, "module activation failed: {error:#}");
        }
        self.records
            .write()
            .await
            .insert(manifest.id.clone(), record);
    }

    async fn ensure_rescan_preserves_release(
        &self,
        packages: &[SelectedPackage],
    ) -> anyhow::Result<()> {
        let records = self.records.read().await;
        if records.len() != packages.len() {
            anyhow::bail!(
                "bundled module membership changed while Core is running; activate the complete release and restart Core"
            );
        }
        for package in packages {
            let record = records.get(&package.manifest.id).ok_or_else(|| {
                anyhow::anyhow!(
                    "bundled module {} appeared while Core is running; restart into the complete release",
                    package.manifest.id
                )
            })?;
            if record.package.root != package.root
                || record.package.manifest != package.manifest
                || record.package.enabled != package.enabled
            {
                anyhow::bail!(
                    "bundled module {} or its enable-state file changed outside the serialized lifecycle API; existing runtimes were left untouched",
                    package.manifest.id
                );
            }
        }
        Ok(())
    }

    async fn ensure_activation_ready(&self, manifest: &PluginManifest) -> anyhow::Result<()> {
        if matches!(manifest.execution, Execution::InProcess { .. })
            && manifest
                .backend
                .routes
                .iter()
                .any(|route| route.auth == RouteAuth::Module)
        {
            anyhow::bail!(
                "in-process plugin {} cannot own authentication secrets; use platform-auth routes or process/service execution",
                manifest.id
            );
        }
        for dependency in manifest
            .dependencies
            .iter()
            .filter(|dependency| !dependency.optional)
        {
            let available = self.backend(&dependency.id).await.is_some();
            if !available {
                anyhow::bail!(
                    "module {} requires enabled and healthy dependency {}",
                    manifest.id,
                    dependency.id
                );
            }
        }
        Ok(())
    }

    async fn enabled_dependents(&self, id: &str) -> Vec<String> {
        self.records
            .read()
            .await
            .values()
            .filter(|record| record.package.enabled)
            .filter(|record| {
                record
                    .package
                    .manifest
                    .dependencies
                    .iter()
                    .any(|dependency| !dependency.optional && dependency.id == id)
            })
            .map(|record| record.package.manifest.id.clone())
            .collect()
    }

    async fn register_metadata(&self, package: &SelectedPackage) -> anyhow::Result<()> {
        let manifest = &package.manifest;
        let permissions = manifest
            .permissions
            .iter()
            .map(|permission| PermissionDefinition {
                id: permission.id.clone(),
                description: permission.description.clone(),
                default_roles: permission.default_roles.clone(),
            })
            .collect::<Vec<_>>();
        self.permissions
            .register_module(&manifest.id, &permissions)
            .await?;
        self.events
            .register_module(
                &manifest.id,
                EventContract {
                    publishes: manifest
                        .events
                        .publishes
                        .iter()
                        .map(|event| versioned_topic(&event.topic, event.version))
                        .collect(),
                    subscribes: manifest
                        .events
                        .subscribes
                        .iter()
                        .map(|event| event.topic.clone())
                        .collect(),
                },
            )
            .await?;
        let schema_path = resolve_bundle_path(&package.root, &manifest.configuration.schema)?;
        let metadata = std::fs::metadata(&schema_path)?;
        if !metadata.is_file() || metadata.len() > 1024 * 1024 {
            anyhow::bail!("configuration schema must be a small regular file");
        }
        let schema = serde_json::from_slice(&std::fs::read(schema_path)?)?;
        self.configuration
            .register(
                &manifest.id,
                manifest.configuration.version,
                schema,
                manifest.configuration.secret_fields.clone(),
            )
            .await?;
        Ok(())
    }

    async fn activate(&self, record: &mut ModuleRecord) -> anyhow::Result<()> {
        let manifest = &record.package.manifest;
        if !self.configuration.is_configured(&manifest.id).await {
            anyhow::bail!(
                "module configuration is not complete: configure {} before enabling it",
                manifest.id
            );
        }
        record.snapshot.state = PluginHealthState::Installing;
        record.snapshot.message = "preparing module-owned database migrations".into();
        self.apply_migrations(&record.package).await?;
        record.snapshot.state = PluginHealthState::Starting;
        record.snapshot.message = "starting plugin runtime".into();

        let context = self.context(&record.package);
        match &manifest.execution {
            Execution::InProcess {
                runtime,
                artifact,
                entrypoint,
            } => {
                let runtime_key = in_process_runtime_key(*runtime);
                let factory = self
                    .trusted
                    .read()
                    .await
                    .factories
                    .get(runtime_key)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!(
                        "in-process host runtime adapter is not in the trusted whitelist: {runtime_key}"
                    ))?;
                let artifact = resolve_bundle_path(&record.package.root, artifact)?;
                let plugin = factory
                    .create(context, artifact, entrypoint.clone())
                    .await?;
                if plugin.manifest().id != manifest.id
                    || plugin.manifest().version != manifest.version
                {
                    anyhow::bail!(
                        "trusted in-process factory returned a plugin with the wrong identity"
                    );
                }
                let sdk_context = self.sdk_context(&record.package);
                plugin
                    .start(&sdk_context)
                    .await
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                if let Err(error) = self.probe_in_process(manifest, &plugin).await {
                    if let Err(stop_error) = plugin.stop().await {
                        tracing::warn!(
                            module = manifest.id,
                            "failed to stop plugin after startup health failure: {stop_error}"
                        );
                    }
                    return Err(error);
                }
                let mut services = self.service_directory.write().await;
                for service in &manifest.services {
                    services.insert(
                        service.name.clone(),
                        format!("in-process://{}/{}", manifest.id, service.name),
                    );
                }
                record.backend = Some(RunningBackend::InProcess(plugin));
            }
            Execution::Process {
                executable,
                args,
                environment: _,
                bind,
            } => {
                let configuration = self
                    .configuration
                    .raw_value(&manifest.id)
                    .await
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "module configuration disappeared while starting {}",
                            manifest.id
                        )
                    })?;
                let environment =
                    sarmg_platform_sdk::resolve_environment_bindings(manifest, &configuration)?
                        .into_iter()
                        .collect();
                let process = ManagedProcess::start(
                    &record.package,
                    executable,
                    args,
                    environment,
                    bind,
                    self.store
                        .configuration_directory()
                        .join(format!("{}.json", manifest.id)),
                )
                .await?;
                let endpoint = process.endpoint();
                if let Err(error) = self.probe_http(manifest, &endpoint).await {
                    process.stop().await;
                    return Err(error);
                }
                let mut services = self.service_directory.write().await;
                for service in &manifest.services {
                    services.insert(service.name.clone(), endpoint.base_url.to_string());
                }
                let snapshot = process.snapshot().await;
                record.snapshot.pid = snapshot.pid;
                record.snapshot.restart_count = snapshot.restart_count;
                record.backend = Some(RunningBackend::Http(Arc::new(HttpBackend {
                    endpoint,
                    process: Some(process),
                })));
            }
            Execution::Container { .. } | Execution::Service { .. } => {
                let adapter = self
                    .trusted
                    .read()
                    .await
                    .adapters
                    .get(&manifest.id)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "{} plugin {} has no trusted lifecycle/service-discovery adapter",
                            execution_label(&manifest.execution),
                            manifest.id
                        )
                    })?;
                let handle = Arc::new(adapter.start(context, manifest.clone()).await?);
                let endpoint = (|| -> anyhow::Result<ServiceEndpoint> {
                    for endpoint in handle.services.values() {
                        endpoint.validate()?;
                    }
                    handle
                        .services
                        .get(&manifest.backend.service)
                        .cloned()
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "trusted adapter did not discover backend service {}",
                                manifest.backend.service
                            )
                        })
                })();
                let endpoint = match endpoint {
                    Ok(endpoint) => endpoint,
                    Err(error) => {
                        let _ = handle.stop().await;
                        return Err(error);
                    }
                };
                if let Err(error) = self.probe_http(manifest, &endpoint).await {
                    let _ = handle.stop().await;
                    return Err(error);
                }
                let mut directory = self.service_directory.write().await;
                for (service, endpoint) in &handle.services {
                    directory.insert(service.clone(), endpoint.base_url.to_string());
                }
                record.backend = Some(RunningBackend::Http(Arc::new(HttpBackend {
                    endpoint,
                    process: None,
                })));
                record.adapter = Some(handle);
            }
        }
        record.snapshot.state = PluginHealthState::Available;
        record.snapshot.message =
            "manifest, migrations, registrations and health gates passed".into();
        record.snapshot.checked_at = Some(chrono::Utc::now().to_rfc3339());
        record.health_monitor = Some(HealthMonitor::start(
            manifest.clone(),
            record
                .backend
                .clone()
                .context("activated module has no backend for health monitoring")?,
        )?);
        let _ = self
            .events
            .publish(
                "platform",
                "platform.module.enabled.v1",
                serde_json::json!({"id":manifest.id,"version":manifest.version}),
            )
            .await;
        Ok(())
    }

    async fn apply_migrations(&self, package: &SelectedPackage) -> anyhow::Result<()> {
        for migration in &package.manifest.migrations {
            if migration.engine == MigrationEngine::Embedded {
                continue;
            }
            if let Some(directory) = &migration.directory {
                let path = resolve_bundle_path(&package.root, directory)?;
                if !path.is_dir() {
                    anyhow::bail!("migration directory is not a directory: {}", path.display());
                }
            }
            // Current process modules own their SQLx migrator and run it before binding their
            // readiness endpoint. Running the same SQL in Core would use a separate ledger/search
            // path and then make the worker replay it. Successful readiness is therefore the v1
            // migration completion gate. Other execution modes require an explicitly trusted host
            // runner until Manifest v2 can name an executor unambiguously.
            if matches!(package.manifest.execution, Execution::Process { .. }) {
                continue;
            }
            let runner = self
                .trusted
                .read()
                .await
                .migrations
                .get(migration_key(migration.engine))
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no trusted {:?} migration runner is registered for {}",
                        migration.engine,
                        package.manifest.id
                    )
                })?;
            runner
                .apply(self.context(package), migration.clone())
                .await
                .with_context(|| {
                    format!(
                        "migration {} failed for {}",
                        migration.id, package.manifest.id
                    )
                })?;
        }
        Ok(())
    }

    async fn probe_in_process(
        &self,
        manifest: &PluginManifest,
        plugin: &Arc<dyn sarmg_platform_sdk::InProcessPlugin>,
    ) -> anyhow::Result<()> {
        let HealthDefinition::Callback {
            liveness_hook,
            readiness_hook,
            timeout_seconds,
            ..
        } = &manifest.health
        else {
            anyhow::bail!("in-process plugin has non-callback health definition")
        };
        let health = tokio::time::timeout(
            Duration::from_secs((*timeout_seconds).into()),
            plugin.health(),
        )
        .await
        .context("in-process health callback timed out")?;
        if health.status != sarmg_platform_sdk::HealthStatus::Ready {
            anyhow::bail!(
                "in-process health callbacks {liveness_hook}/{readiness_hook} reported {:?}: {}",
                health.status,
                health.message
            );
        }
        Ok(())
    }

    async fn probe_http(
        &self,
        manifest: &PluginManifest,
        endpoint: &ServiceEndpoint,
    ) -> anyhow::Result<()> {
        probe_http_until_ready(manifest, endpoint).await
    }

    fn context(&self, package: &SelectedPackage) -> PluginContext {
        PluginContext {
            id: package.manifest.id.clone(),
            version: package.manifest.version.clone(),
            manifest: package.manifest.clone(),
            package_root: package.root.clone(),
            events: self.events.clone(),
            tasks: self.tasks.clone(),
            notifications: self.notifications.clone(),
        }
    }

    fn sdk_context(&self, package: &SelectedPackage) -> super::sdk_bridge::SdkPlatformContext {
        super::sdk_bridge::SdkPlatformContext::new(
            package.manifest.id.clone(),
            self.configuration.clone(),
            self.tasks.clone(),
            self.notifications.clone(),
            self.events.clone(),
            self.service_directory.clone(),
        )
    }

    pub async fn modules(&self) -> Vec<ModuleRuntimeView> {
        self.records
            .read()
            .await
            .values()
            .map(ModuleRecord::view)
            .collect()
    }

    pub(crate) fn gateway_client(&self) -> &reqwest::Client {
        &self.gateway_client
    }

    pub async fn module(&self, id: &str) -> Option<ModuleRuntimeView> {
        self.records.read().await.get(id).map(ModuleRecord::view)
    }

    pub async fn backend(&self, id: &str) -> Option<PluginBackend> {
        let records = self.records.read().await;
        let record = records.get(id)?;
        if !record.package.enabled || record.snapshot.state != PluginHealthState::Available {
            return None;
        }
        if record
            .health_monitor
            .as_ref()
            .is_some_and(|monitor| !monitor.is_available())
        {
            return None;
        }
        if let Some(RunningBackend::Http(http)) = record.backend.as_ref()
            && let Some(process) = http.process.as_ref()
            && !process.snapshot.read().await.ready
        {
            return None;
        }
        Some(PluginBackend {
            manifest: record.package.manifest.clone(),
            backend: record.backend.clone()?,
        })
    }

    pub(crate) async fn route_auth(
        &self,
        id: &str,
        method: &axum::http::Method,
        path: &str,
    ) -> Option<RouteAuth> {
        let records = self.records.read().await;
        let record = records.get(id)?;
        select_route(&record.package.manifest, method, path).map(|(route, _)| route.auth)
    }

    /// Return the Core-enforced ingress limits for the most-specific registered module route.
    /// Unknown module paths deliberately fall back to the conservative global policy; the
    /// gateway will subsequently return its normal 404/503 response.
    pub(crate) async fn route_request_body_policy(
        &self,
        id: &str,
        method: &axum::http::Method,
        path: &str,
    ) -> Option<sarmg_platform_core::RequestBodyPolicy> {
        let records = self.records.read().await;
        let record = records.get(id)?;
        select_route(&record.package.manifest, method, path).map(|(route, _)| route.request_body)
    }

    pub async fn asset(&self, id: &str, relative: &str) -> anyhow::Result<Option<PathBuf>> {
        let records = self.records.read().await;
        let Some(record) = records.get(id) else {
            return Ok(None);
        };
        if !record.package.enabled || !relative.starts_with("frontend/") {
            return Ok(None);
        }
        Ok(Some(resolve_bundle_path(&record.package.root, relative)?))
    }

    async fn deactivate_runtime(&self, record: &mut ModuleRecord) {
        if let Some(monitor) = record.health_monitor.take() {
            monitor.stop().await;
        }
        if let Some(backend) = record.backend.take() {
            match backend {
                RunningBackend::InProcess(plugin) => {
                    if let Err(error) = plugin.stop().await {
                        tracing::warn!(
                            module = record.package.manifest.id,
                            "in-process plugin stop failed: {error:#}"
                        );
                    }
                }
                RunningBackend::Http(http) => {
                    if let Some(process) = http.process.as_ref() {
                        process.stop().await;
                    }
                }
            }
        }
        if let Some(adapter) = record.adapter.take()
            && let Err(error) = adapter.stop().await
        {
            tracing::warn!(
                module = record.package.manifest.id,
                "service adapter stop failed: {error:#}"
            );
        }
        let mut services = self.service_directory.write().await;
        for service in &record.package.manifest.services {
            services.remove(&service.name);
        }
        record.snapshot.pid = None;
    }

    async fn unregister_metadata(&self, id: &str) {
        self.permissions.unregister_module(id).await;
        self.events.unregister_module(id).await;
        self.configuration.unregister(id).await;
        self.tasks.unregister_owner(id).await;
    }

    pub async fn stop_all(&self) {
        let mut records = std::mem::take(&mut *self.records.write().await);
        let order = PluginCatalog::new(
            records
                .values()
                .map(|record| record.package.manifest.clone())
                .collect(),
        )
        .map(|catalog| {
            catalog
                .deactivation_order()
                .map(|manifest| manifest.id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|_| records.keys().rev().cloned().collect());
        for id in order {
            let Some(mut record) = records.remove(&id) else {
                continue;
            };
            self.deactivate_runtime(&mut record).await;
            self.unregister_metadata(&id).await;
        }
        self.service_directory.write().await.clear();
    }
}

fn health_timeout_seconds(manifest: &PluginManifest) -> u32 {
    match manifest.health {
        HealthDefinition::Callback {
            timeout_seconds, ..
        }
        | HealthDefinition::Http {
            timeout_seconds, ..
        } => timeout_seconds,
    }
}

async fn probe_http_until_ready(
    manifest: &PluginManifest,
    endpoint: &ServiceEndpoint,
) -> anyhow::Result<()> {
    let HealthDefinition::Http {
        liveness_path,
        readiness_path,
        timeout_seconds,
        ..
    } = &manifest.health
    else {
        anyhow::bail!("external plugin has non-HTTP health definition")
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs((*timeout_seconds).into()))
        .build()?;
    let deadline = tokio::time::Instant::now()
        + Duration::from_secs(manifest.lifecycle.startup_timeout_seconds.into());
    loop {
        let mut healthy = true;
        for path in [liveness_path, readiness_path] {
            let url = endpoint.base_url.join(path.trim_start_matches('/'))?;
            let mut request = client.get(url);
            for (name, value) in &endpoint.headers {
                request = request.header(name, value);
            }
            match request.send().await {
                Ok(response)
                    if response.status().is_success()
                        && response
                            .headers()
                            .get("x-union-module-protocol")
                            .and_then(|value| value.to_str().ok())
                            == Some("gateway-v1")
                        && response
                            .headers()
                            .get("x-union-module-audience")
                            .and_then(|value| value.to_str().ok())
                            == Some(manifest.id.as_str()) => {}
                _ => {
                    healthy = false;
                    break;
                }
            }
        }
        if healthy {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "plugin HTTP liveness/readiness did not become healthy before startup timeout"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn probe_backend_once(
    manifest: &PluginManifest,
    backend: &RunningBackend,
    client: &reqwest::Client,
) -> anyhow::Result<String> {
    match (&manifest.health, backend) {
        (
            HealthDefinition::Callback {
                liveness_hook,
                readiness_hook,
                timeout_seconds,
                ..
            },
            RunningBackend::InProcess(plugin),
        ) => {
            let report = tokio::time::timeout(
                Duration::from_secs((*timeout_seconds).into()),
                plugin.health(),
            )
            .await
            .context("in-process health callback timed out")?;
            if report.status != sarmg_platform_sdk::HealthStatus::Ready {
                anyhow::bail!(
                    "callbacks {liveness_hook}/{readiness_hook} reported {:?}: {}",
                    report.status,
                    report.message
                );
            }
            Ok(format!(
                "periodic callbacks {liveness_hook}/{readiness_hook} passed: {}",
                report.message
            ))
        }
        (
            HealthDefinition::Http {
                liveness_path,
                readiness_path,
                ..
            },
            RunningBackend::Http(http),
        ) => {
            if let Some(process) = http.process.as_ref()
                && !process.snapshot.read().await.ready
            {
                anyhow::bail!("supervised process has not passed its restart readiness gate");
            }
            let endpoint = &http.endpoint;
            for (kind, path) in [
                ("liveness", liveness_path.as_str()),
                ("readiness", readiness_path.as_str()),
            ] {
                let url = endpoint.base_url.join(path.trim_start_matches('/'))?;
                let mut request = client.get(url);
                for (name, value) in &endpoint.headers {
                    request = request.header(name, value);
                }
                let response = request
                    .send()
                    .await
                    .with_context(|| format!("{kind} request failed"))?;
                if !response.status().is_success() {
                    anyhow::bail!("{kind} returned HTTP {}", response.status());
                }
                if response
                    .headers()
                    .get("x-union-module-protocol")
                    .and_then(|value| value.to_str().ok())
                    != Some("gateway-v1")
                    || response
                        .headers()
                        .get("x-union-module-audience")
                        .and_then(|value| value.to_str().ok())
                        != Some(manifest.id.as_str())
                {
                    anyhow::bail!("{kind} response failed gateway identity validation");
                }
            }
            Ok("periodic HTTP liveness/readiness checks passed".into())
        }
        (HealthDefinition::Callback { .. }, RunningBackend::Http(_)) => {
            anyhow::bail!("external backend cannot use callback health checks")
        }
        (HealthDefinition::Http { .. }, RunningBackend::InProcess(_)) => {
            anyhow::bail!("in-process backend cannot use HTTP health checks")
        }
    }
}

#[derive(Clone)]
pub struct PluginBackend {
    pub manifest: PluginManifest,
    backend: RunningBackend,
}

pub(crate) struct MatchedRoute<'a> {
    pub id: &'a str,
    pub auth: RouteAuth,
    pub permission: Option<&'a str>,
    pub request_body: sarmg_platform_core::RequestBodyPolicy,
    pub upstream_path: String,
}

impl PluginBackend {
    pub fn protocol(&self) -> ServiceProtocol {
        self.manifest
            .services
            .iter()
            .find(|service| service.name == self.manifest.backend.service)
            .expect("validated manifest backend service exists")
            .protocol
    }

    pub async fn call(
        &self,
        request: Request<Body>,
        route_id: String,
        request_max_bytes: u64,
        actor: Option<sarmg_platform_sdk::Actor>,
    ) -> Response {
        match &self.backend {
            RunningBackend::InProcess(plugin) => {
                let (parts, body) = request.into_parts();
                let collection_limit = usize::try_from(request_max_bytes).unwrap_or(usize::MAX);
                let body = match axum::body::to_bytes(body, collection_limit).await {
                    Ok(body) => body.to_vec(),
                    Err(_) => {
                        return in_process_error(
                            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                            "in-process request body exceeds its Manifest route limit",
                        );
                    }
                };
                let method = match core_http_method(&parts.method) {
                    Some(method) => method,
                    None => {
                        return in_process_error(
                            axum::http::StatusCode::METHOD_NOT_ALLOWED,
                            "unsupported in-process HTTP method",
                        );
                    }
                };
                let mut headers = BTreeMap::<String, Vec<String>>::new();
                for (name, value) in &parts.headers {
                    if matches!(
                        name.as_str(),
                        "authorization"
                            | "cookie"
                            | "x-union-module-token"
                            | "x-union-plugin-token"
                    ) {
                        continue;
                    }
                    if let Ok(value) = value.to_str() {
                        headers
                            .entry(name.as_str().into())
                            .or_default()
                            .push(value.into());
                    }
                }
                let plugin_request = sarmg_platform_sdk::InProcessHttpRequest {
                    route_id,
                    method,
                    path: parts.uri.path().into(),
                    query: parts.uri.query().map(str::to_owned),
                    headers,
                    body,
                    actor,
                };
                match plugin.handle_http(plugin_request).await {
                    Ok(response) => in_process_response(response),
                    Err(error) => {
                        tracing::warn!(
                            module = self.manifest.id,
                            "in-process HTTP handler failed: {error}"
                        );
                        in_process_error(
                            axum::http::StatusCode::BAD_GATEWAY,
                            "in-process plugin handler failed",
                        )
                    }
                }
            }
            RunningBackend::Http(_) => unreachable!("HTTP proxying is handled by gateway"),
        }
    }

    pub fn endpoint(&self) -> Option<&ServiceEndpoint> {
        match &self.backend {
            RunningBackend::Http(http) => Some(&http.endpoint),
            RunningBackend::InProcess(_) => None,
        }
    }

    pub(crate) fn route_for(
        &self,
        method: &axum::http::Method,
        path: &str,
    ) -> Option<MatchedRoute<'_>> {
        select_route(&self.manifest, method, path).and_then(|(route, captures)| {
            let upstream_path = render_route(&route.upstream_path, &captures)?;
            Some(MatchedRoute {
                id: &route.id,
                auth: route.auth,
                permission: route.permission.as_deref(),
                request_body: route.request_body,
                upstream_path,
            })
        })
    }
}

fn validate_packages(packages: &[SelectedPackage]) -> anyhow::Result<PluginCatalog> {
    let catalog = PluginCatalog::new(
        packages
            .iter()
            .map(|package| package.manifest.clone())
            .collect(),
    )?;
    catalog.ensure_platform_compatible(&PlatformVersions::parse(
        CORE_VERSION,
        sarmg_platform_core::PLATFORM_API_VERSION,
        sarmg_platform_core::PLUGIN_API_VERSION,
    )?)?;
    Ok(catalog)
}

fn select_route<'a>(
    manifest: &'a PluginManifest,
    method: &axum::http::Method,
    path: &str,
) -> Option<(
    &'a sarmg_platform_core::BackendRoute,
    BTreeMap<String, String>,
)> {
    manifest
        .backend
        .routes
        .iter()
        .filter(|route| route_method_matches(route, method))
        .filter_map(|route| capture_route(&route.path, path).map(|captures| (route, captures)))
        .max_by_key(|(route, _)| sarmg_platform_core::route_specificity(&route.path))
}

fn route_method_matches(
    route: &sarmg_platform_core::BackendRoute,
    method: &axum::http::Method,
) -> bool {
    route.methods.iter().any(|candidate| match candidate {
        sarmg_platform_core::HttpMethod::Delete => method == axum::http::Method::DELETE,
        sarmg_platform_core::HttpMethod::Get => method == axum::http::Method::GET,
        sarmg_platform_core::HttpMethod::Head => method == axum::http::Method::HEAD,
        sarmg_platform_core::HttpMethod::Options => method == axum::http::Method::OPTIONS,
        sarmg_platform_core::HttpMethod::Patch => method == axum::http::Method::PATCH,
        sarmg_platform_core::HttpMethod::Post => method == axum::http::Method::POST,
        sarmg_platform_core::HttpMethod::Put => method == axum::http::Method::PUT,
    })
}

fn capture_route(pattern: &str, path: &str) -> Option<BTreeMap<String, String>> {
    let pattern = route_segments(pattern);
    let path = safe_request_segments(path)?;
    let mut captures = BTreeMap::new();
    let mut index = 0;
    while index < pattern.len() {
        let expected = pattern[index];
        if let Some(name) = expected
            .strip_prefix("{*")
            .and_then(|value| value.strip_suffix('}'))
        {
            if index >= path.len() {
                return None;
            }
            captures.insert(name.into(), path[index..].join("/"));
            return Some(captures);
        }
        let actual = path.get(index)?;
        if let Some(name) = expected
            .strip_prefix('{')
            .and_then(|value| value.strip_suffix('}'))
        {
            captures.insert(name.into(), (*actual).into());
        } else if expected != *actual {
            return None;
        }
        index += 1;
    }
    (index == path.len()).then_some(captures)
}

fn route_segments(pattern: &str) -> Vec<&str> {
    if pattern == "/" {
        Vec::new()
    } else {
        pattern.trim_start_matches('/').split('/').collect()
    }
}

fn safe_request_segments(path: &str) -> Option<Vec<&str>> {
    if path == "/" {
        return Some(Vec::new());
    }
    if !path.starts_with('/') {
        return None;
    }
    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    if segments.iter().any(|segment| {
        segment.is_empty()
            || matches!(*segment, "." | "..")
            || segment
                .chars()
                .any(|character| character.is_control() || matches!(character, '\\' | '?' | '#'))
    }) {
        return None;
    }
    Some(segments)
}

fn render_route(template: &str, captures: &BTreeMap<String, String>) -> Option<String> {
    let mut rendered = Vec::new();
    for segment in template.trim_start_matches('/').split('/') {
        if let Some(name) = segment
            .strip_prefix("{*")
            .and_then(|value| value.strip_suffix('}'))
        {
            rendered.push(
                captures
                    .get(name)?
                    .split('/')
                    .map(encode_path_segment)
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        } else if let Some(name) = segment
            .strip_prefix('{')
            .and_then(|value| value.strip_suffix('}'))
        {
            rendered.push(encode_path_segment(captures.get(name)?));
        } else {
            rendered.push(segment.into());
        }
    }
    Some(format!("/{}", rendered.join("/")))
}

/// Axum's `Path` extractor has already percent-decoded captures. Encode each UTF-8 byte back into
/// an RFC 3986 path segment before constructing the upstream URI; unlike form encoding, spaces are
/// `%20` and never `+`. Encoding every non-unreserved byte also prevents decoded delimiters from
/// changing the route structure.
fn encode_path_segment(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn versioned_topic(topic: &str, version: u32) -> String {
    format!("{topic}.v{version}")
}

fn trusted_module_id(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'))
    .then_some(value)
}

fn migration_key(engine: MigrationEngine) -> &'static str {
    match engine {
        MigrationEngine::Postgresql => "postgresql",
        MigrationEngine::Sqlite => "sqlite",
        MigrationEngine::Embedded => "embedded",
    }
}

fn execution_label(execution: &Execution) -> &'static str {
    match execution {
        Execution::InProcess { .. } => "in_process",
        Execution::Process { .. } => "process",
        Execution::Container { .. } => "container",
        Execution::Service { .. } => "service",
    }
}

fn in_process_runtime_key(runtime: sarmg_platform_core::InProcessRuntime) -> &'static str {
    match runtime {
        sarmg_platform_core::InProcessRuntime::WasiComponentV1 => "wasi_component_v1",
    }
}

fn core_http_method(method: &axum::http::Method) -> Option<sarmg_platform_core::HttpMethod> {
    match *method {
        axum::http::Method::DELETE => Some(sarmg_platform_core::HttpMethod::Delete),
        axum::http::Method::GET => Some(sarmg_platform_core::HttpMethod::Get),
        axum::http::Method::HEAD => Some(sarmg_platform_core::HttpMethod::Head),
        axum::http::Method::OPTIONS => Some(sarmg_platform_core::HttpMethod::Options),
        axum::http::Method::PATCH => Some(sarmg_platform_core::HttpMethod::Patch),
        axum::http::Method::POST => Some(sarmg_platform_core::HttpMethod::Post),
        axum::http::Method::PUT => Some(sarmg_platform_core::HttpMethod::Put),
        _ => None,
    }
}

fn in_process_response(response: sarmg_platform_sdk::InProcessHttpResponse) -> Response {
    let Ok(status) = axum::http::StatusCode::from_u16(response.status) else {
        return in_process_error(
            axum::http::StatusCode::BAD_GATEWAY,
            "in-process plugin returned an invalid status",
        );
    };
    let mut output = Response::new(Body::from(response.body));
    *output.status_mut() = status;
    for (name, values) in response.headers {
        let Ok(name) = axum::http::HeaderName::from_bytes(name.as_bytes()) else {
            return in_process_error(
                axum::http::StatusCode::BAD_GATEWAY,
                "in-process plugin returned an invalid header",
            );
        };
        if matches!(
            name.as_str(),
            "connection"
                | "proxy-connection"
                | "keep-alive"
                | "transfer-encoding"
                | "upgrade"
                | "trailer"
                | "x-union-module-token"
                | "x-union-plugin-token"
        ) {
            continue;
        }
        for value in values {
            let Ok(value) = axum::http::HeaderValue::from_str(&value) else {
                return in_process_error(
                    axum::http::StatusCode::BAD_GATEWAY,
                    "in-process plugin returned an invalid header",
                );
            };
            output.headers_mut().append(name.clone(), value);
        }
    }
    output
}

fn in_process_error(status: axum::http::StatusCode, message: &str) -> Response {
    let mut response = Response::new(Body::from(
        serde_json::json!({"code":"in_process_plugin_error","message":message}).to_string(),
    ));
    *response.status_mut() = status;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    response
}

#[derive(Debug, Clone)]
struct ProcessSnapshot {
    pid: Option<u32>,
    ready: bool,
    /// The supervisor has stopped retrying, so a missing PID is a failure rather than backoff.
    terminal: bool,
    restart_count: u64,
    message: String,
}

fn process_display_state(process: &ProcessSnapshot) -> PluginHealthState {
    if process.terminal {
        PluginHealthState::Failed
    } else if process.pid.is_some() {
        PluginHealthState::Starting
    } else {
        PluginHealthState::Backoff
    }
}

#[derive(Clone)]
struct ManagedProcess {
    endpoint: ServiceEndpoint,
    snapshot: Arc<RwLock<ProcessSnapshot>>,
    shutdown: watch::Sender<bool>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl ManagedProcess {
    async fn start(
        package: &SelectedPackage,
        executable: &str,
        args: &[String],
        environment: Vec<(String, String)>,
        bind: &sarmg_platform_core::ProcessBind,
        configuration_path: PathBuf,
    ) -> anyhow::Result<Self> {
        let executable = resolve_bundle_path(&package.root, executable)?;
        let metadata = std::fs::metadata(&executable)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                anyhow::bail!("plugin process executable is not an executable regular file");
            }
        }
        let ip: IpAddr = bind.host.parse()?;
        let address = if bind.port == 0 {
            let listener = std::net::TcpListener::bind(SocketAddr::new(ip, 0))?;
            listener.local_addr()?
        } else {
            SocketAddr::new(ip, bind.port)
        };
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let endpoint = ServiceEndpoint {
            base_url: url::Url::parse(&format!("http://{address}/"))?,
            headers: BTreeMap::from([
                ("x-union-plugin-token".into(), token.clone()),
                ("x-union-module-protocol".into(), "gateway-v1".into()),
                (
                    "x-union-module-audience".into(),
                    package.manifest.id.clone(),
                ),
                ("x-union-module-token".into(), token.clone()),
                (
                    "x-forwarded-prefix".into(),
                    format!("/api/modules/{}", package.manifest.id),
                ),
            ]),
        };
        let snapshot = Arc::new(RwLock::new(ProcessSnapshot {
            pid: None,
            ready: false,
            terminal: false,
            restart_count: 0,
            message: "process supervisor is starting".into(),
        }));
        let (shutdown, mut receiver) = watch::channel(false);
        let task_snapshot = snapshot.clone();
        let manifest = package.manifest.clone();
        let package_root = package.root.clone();
        let arguments = args.to_vec();
        let configured_environment = environment;
        let bind_environment = bind.environment.clone();
        let task_endpoint = endpoint.clone();
        let task = tokio::spawn(async move {
            let mut restarts = 0_u64;
            let mut backoff = Duration::from_secs(1);
            loop {
                if *receiver.borrow() {
                    break;
                }
                let mut command = Command::new(&executable);
                command
                    .args(&arguments)
                    .env_clear()
                    .envs(configured_environment.iter().cloned())
                    .env("UNION_PLUGIN_ID", &manifest.id)
                    .env("UNION_PLUGIN_VERSION", &manifest.version)
                    .env(
                        "UNION_PLUGIN_API_VERSION",
                        sarmg_platform_core::PLUGIN_API_VERSION,
                    )
                    .env("UNION_PLUGIN_BIND", address.to_string())
                    .env("UNION_PLUGIN_PORT", address.port().to_string())
                    .env("UNION_PLUGIN_TOKEN", &token)
                    .env("UNION_PLUGIN_PACKAGE_ROOT", &package_root)
                    .env("UNION_PLUGIN_CONFIG", &configuration_path)
                    .env("UNION_MODULE_PROTOCOL", "gateway-v1")
                    .env("UNION_MODULE_AUDIENCE", &manifest.id)
                    .env("UNION_MODULE_TOKEN", &token)
                    .env(
                        "UNION_MODULE_PREFIX",
                        format!("/api/modules/{}", manifest.id),
                    )
                    .current_dir(&package_root)
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .kill_on_drop(true);
                #[cfg(unix)]
                command.process_group(0);
                if let Some(name) = &bind_environment {
                    command.env(name, address.to_string());
                }
                let mut child = match command.spawn() {
                    Ok(child) => child,
                    Err(error) => {
                        let mut state = task_snapshot.write().await;
                        state.pid = None;
                        state.terminal = true;
                        state.message = format!("process spawn failed: {error}");
                        break;
                    }
                };
                {
                    let mut state = task_snapshot.write().await;
                    state.pid = child.id();
                    state.ready = false;
                    state.terminal = false;
                    state.restart_count = restarts;
                    state.message = "process started; waiting for liveness/readiness gates".into();
                }
                let readiness = probe_http_until_ready(&manifest, &task_endpoint);
                tokio::pin!(readiness);
                let status = tokio::select! {
                    changed = receiver.changed() => {
                        if changed.is_err() || *receiver.borrow_and_update() {
                            terminate_child(&mut child, manifest.lifecycle.shutdown_timeout_seconds).await;
                            break;
                        }
                        continue;
                    }
                    result = child.wait() => result,
                    result = &mut readiness => {
                        match result {
                            Ok(()) => {
                                let mut state = task_snapshot.write().await;
                                state.ready = true;
                                state.message = "process passed liveness/readiness gates".into();
                                drop(state);
                                tokio::select! {
                                    changed = receiver.changed() => {
                                        if changed.is_err() || *receiver.borrow_and_update() {
                                            terminate_child(&mut child, manifest.lifecycle.shutdown_timeout_seconds).await;
                                            break;
                                        }
                                        continue;
                                    }
                                    result = child.wait() => result,
                                }
                            }
                            Err(error) => {
                                terminate_child(&mut child, manifest.lifecycle.shutdown_timeout_seconds).await;
                                let mut state = task_snapshot.write().await;
                                state.message = format!("process readiness failed: {error}");
                                drop(state);
                                Err(std::io::Error::other(format!(
                                    "process readiness gate failed: {error}"
                                )))
                            }
                        }
                    }
                };
                let successful = status.as_ref().is_ok_and(|status| status.success());
                {
                    let mut state = task_snapshot.write().await;
                    state.pid = None;
                    state.ready = false;
                    state.message = match &status {
                        Ok(status) => format!("process exited: {status}"),
                        Err(error) => format!("process wait failed: {error}"),
                    };
                }
                let restart = match manifest.lifecycle.restart_policy {
                    RestartPolicy::Never => false,
                    RestartPolicy::OnFailure => !successful,
                    RestartPolicy::Always => true,
                };
                if !restart || restarts >= u64::from(manifest.lifecycle.max_restarts) {
                    let mut state = task_snapshot.write().await;
                    state.terminal = true;
                    state.message = if restart {
                        format!(
                            "{}; restart limit {} exhausted",
                            state.message, manifest.lifecycle.max_restarts
                        )
                    } else {
                        format!("{}; restart policy will not restart it", state.message)
                    };
                    break;
                }
                restarts += 1;
                task_snapshot.write().await.restart_count = restarts;
                tokio::select! {
                    changed = receiver.changed() => {
                        if changed.is_err() || *receiver.borrow_and_update() { break; }
                    }
                    () = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        });
        Ok(Self {
            endpoint,
            snapshot,
            shutdown,
            task: Arc::new(Mutex::new(Some(task))),
        })
    }

    fn endpoint(&self) -> ServiceEndpoint {
        self.endpoint.clone()
    }

    async fn snapshot(&self) -> ProcessSnapshot {
        self.snapshot.read().await.clone()
    }

    async fn stop(&self) {
        self.shutdown.send_replace(true);
        if let Some(task) = self.task.lock().await.take()
            && let Err(error) = task.await
        {
            tracing::warn!("plugin process supervisor failed: {error}");
        }
    }
}

async fn terminate_child(child: &mut tokio::process::Child, grace_seconds: u32) {
    if let Some(pid) = child.id() {
        signal_process_tree(pid, libc::SIGTERM);
    }
    if tokio::time::timeout(Duration::from_secs(grace_seconds.into()), child.wait())
        .await
        .is_err()
    {
        if let Some(pid) = child.id() {
            signal_process_tree(pid, libc::SIGKILL);
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

#[cfg(unix)]
fn signal_process_tree(pid: u32, signal: libc::c_int) {
    // Each worker is spawned as the leader of a fresh process group. A negative pid addresses the
    // complete group so helper processes cannot outlive disable/shutdown.
    let _ = unsafe { libc::kill(-(pid as libc::pid_t), signal) };
}

#[cfg(not(unix))]
fn signal_process_tree(pid: u32, signal: libc::c_int) {
    let _ = unsafe { libc::kill(pid as libc::pid_t, signal) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FakeFactory {
        starts: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
    }

    impl InProcessFactory for FakeFactory {
        fn create(
            &self,
            context: PluginContext,
            _artifact: PathBuf,
            _entrypoint: String,
        ) -> PluginFuture<Arc<dyn sarmg_platform_sdk::InProcessPlugin>> {
            let plugin = FakePlugin {
                manifest: context.manifest,
                starts: self.starts.clone(),
                stops: self.stops.clone(),
                ready: Arc::new(AtomicBool::new(true)),
                health_calls: Arc::new(AtomicUsize::new(0)),
            };
            Box::pin(async move {
                let plugin: Arc<dyn sarmg_platform_sdk::InProcessPlugin> = Arc::new(plugin);
                Ok(plugin)
            })
        }
    }

    struct FakePlugin {
        manifest: PluginManifest,
        starts: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
        ready: Arc<AtomicBool>,
        health_calls: Arc<AtomicUsize>,
    }

    impl sarmg_platform_sdk::InProcessHttpHandler for FakePlugin {
        fn handle_http<'a>(
            &'a self,
            request: sarmg_platform_sdk::InProcessHttpRequest,
        ) -> sarmg_platform_sdk::PlatformFuture<
            'a,
            Result<sarmg_platform_sdk::InProcessHttpResponse, sarmg_platform_sdk::PlatformError>,
        > {
            Box::pin(async move {
                Ok(sarmg_platform_sdk::InProcessHttpResponse {
                    status: 200,
                    headers: BTreeMap::from([(
                        "content-type".into(),
                        vec!["application/json".into()],
                    )]),
                    body: serde_json::to_vec(&serde_json::json!({
                        "route": request.route_id,
                        "path": request.path,
                    }))
                    .unwrap(),
                })
            })
        }
    }

    impl sarmg_platform_sdk::InProcessPlugin for FakePlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }

        fn start<'a>(
            &'a self,
            _platform: &'a dyn sarmg_platform_sdk::PlatformContext,
        ) -> sarmg_platform_sdk::PlatformFuture<'a, Result<(), sarmg_platform_sdk::PlatformError>>
        {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }

        fn stop<'a>(
            &'a self,
        ) -> sarmg_platform_sdk::PlatformFuture<'a, Result<(), sarmg_platform_sdk::PlatformError>>
        {
            self.stops.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }

        fn health<'a>(
            &'a self,
        ) -> sarmg_platform_sdk::PlatformFuture<'a, sarmg_platform_sdk::HealthReport> {
            self.health_calls.fetch_add(1, Ordering::SeqCst);
            let ready = self.ready.load(Ordering::SeqCst);
            Box::pin(async move {
                sarmg_platform_sdk::HealthReport {
                    status: if ready {
                        sarmg_platform_sdk::HealthStatus::Ready
                    } else {
                        sarmg_platform_sdk::HealthStatus::Unavailable
                    },
                    message: if ready { "ready" } else { "not ready" }.into(),
                }
            })
        }
    }

    struct FakeMigrationRunner {
        calls: Arc<AtomicUsize>,
        fail: Arc<AtomicBool>,
    }

    impl MigrationRunner for FakeMigrationRunner {
        fn apply(
            &self,
            _context: PluginContext,
            _migration: DatabaseMigration,
        ) -> PluginFuture<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let fail = self.fail.load(Ordering::SeqCst);
            Box::pin(async move {
                if fail {
                    anyhow::bail!("injected migration failure")
                }
                Ok(())
            })
        }
    }

    fn manifest(id: &str, dependency: Option<&str>, core: &str) -> String {
        let dependency = dependency.map_or_else(
            || serde_json::json!([]),
            |dependency| {
                serde_json::json!([{
                    "id": dependency,
                    "version": "^1.0",
                    "optional": false
                }])
            },
        );
        serde_json::to_string_pretty(&serde_json::json!({
            "manifest_version": 1,
            "id": id,
            "display_name": format!("{id} test plugin"),
            "description": "Runtime integration-test plugin",
            "version": "1.0.0",
            "version_metadata": {
                "channel":"development",
                "distribution":"bundled",
                "license":"Apache-2.0"
            },
            "compatibility": {
                "core": core,
                "platform_api": "^1.0",
                "plugin_api": "^1.0"
            },
            "dependencies": dependency,
            "execution": {
                "mode":"in_process",
                "runtime":"wasi_component_v1",
                "artifact":"backend/plugin.wasm",
                "entrypoint":"activate"
            },
            "backend": {
                "api_version":"v1",
                "base_path":format!("/api/modules/{id}"),
                "service":format!("{id}.api"),
                "routes":[{
                    "id":"echo",
                    "path":"/echo/{value}",
                    "upstream_path":"/internal/{value}",
                    "methods":["GET"],
                    "auth":"platform",
                    "permission":format!("{id}.echo.read")
                }]
            },
            "frontend": {
                "entry":"frontend/entry.js",
                "styles":["frontend/styles.css"],
                "components":["TestView"],
                "api_base":format!("/api/modules/{id}"),
                "routes":[{
                    "path":format!("/modules/{id}"),
                    "component":"TestView",
                    "permission":format!("{id}.echo.read")
                }],
                "menu":[{
                    "id":"main",
                    "label":"Test",
                    "route":format!("/modules/{id}"),
                    "permission":format!("{id}.echo.read"),
                    "order":10
                }]
            },
            "permissions":[{
                "id":format!("{id}.echo.read"),
                "description":"Read test echo",
                "default_roles":["admin"]
            }],
            "migrations":[{
                "id":"schema-v1",
                "engine":"sqlite",
                "directory":"migrations"
            }],
            "configuration": {
                "schema":"config/schema.json",
                "version":1,
                "secret_fields":[]
            },
            "health": {
                "kind":"callback",
                "liveness_hook":"live",
                "readiness_hook":"ready",
                "interval_seconds":10,
                "timeout_seconds":2
            },
            "lifecycle": {
                "startup_timeout_seconds":5,
                "shutdown_timeout_seconds":5,
                "restart_policy":"never",
                "max_restarts":0
            },
            "services":[{
                "name":format!("{id}.api"),
                "protocol":"http",
                "visibility":"platform"
            }],
            "events": {
                "publishes":[{"topic":format!("{id}.changed"),"version":1}],
                "subscribes":[]
            }
        }))
        .unwrap()
    }

    fn write_package(root: &std::path::Path, id: &str, dependency: Option<&str>, core: &str) {
        let root = root.join(id);
        std::fs::create_dir_all(root.join("backend")).unwrap();
        std::fs::create_dir_all(root.join("frontend")).unwrap();
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("migrations")).unwrap();
        std::fs::write(root.join("manifest.json"), manifest(id, dependency, core)).unwrap();
        std::fs::write(root.join("backend/plugin.wasm"), b"fake-wasi-component").unwrap();
        std::fs::write(root.join("frontend/entry.js"), b"export default {};").unwrap();
        std::fs::write(root.join("frontend/styles.css"), b"").unwrap();
        std::fs::write(
            root.join("config/schema.json"),
            br#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        )
        .unwrap();
        std::fs::write(root.join("migrations/0001.sql"), b"SELECT 1;").unwrap();
    }

    fn rewrite_execution(bundled: &std::path::Path, id: &str, execution: serde_json::Value) {
        let path = bundled.join(id).join("manifest.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["execution"] = execution;
        value["migrations"] = serde_json::json!([{"id":"state-v1","engine":"embedded"}]);
        if value["execution"]["mode"] != "in_process" {
            value["health"] = serde_json::json!({
                "kind":"http",
                "service":format!("{id}.api"),
                "liveness_path":"/health/live",
                "readiness_path":"/health/ready",
                "interval_seconds":10,
                "timeout_seconds":2
            });
        }
        std::fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }

    #[test]
    fn route_matching_supports_parameters_and_tail_captures() {
        assert!(capture_route("/", "/").is_some());
        assert!(capture_route("/{*path}", "/").is_none());
        assert!(capture_route("/{*path}", "/nested/file").is_some());
        assert!(capture_route("/items/{id}", "/items/42").is_some());
        assert!(capture_route("/files/{*path}", "/files/a/b").is_some());
        assert!(capture_route("/items/{id}", "/items/42/extra").is_none());
        assert!(capture_route("/items", "/other").is_none());
        for unsafe_path in [
            "/items/.",
            "/items/..",
            "/items//value",
            "/items/value\\child",
            "/items/value?query",
            "/items/value#fragment",
        ] {
            assert!(
                capture_route("/items/{id}", unsafe_path).is_none(),
                "unsafe path unexpectedly matched: {unsafe_path}"
            );
        }
        let captures = capture_route("/files/{*path}", "/files/a/b").unwrap();
        assert_eq!(
            render_route("/api/v1/assets/{*path}", &captures).as_deref(),
            Some("/api/v1/assets/a/b")
        );
        let captures =
            capture_route("/files/{*path}", "/files/中文目录/a file/100%/literal+plus").unwrap();
        assert_eq!(
            render_route("/storage/{*path}", &captures).as_deref(),
            Some("/storage/%E4%B8%AD%E6%96%87%E7%9B%AE%E5%BD%95/a%20file/100%25/literal%2Bplus")
        );
        let captures = capture_route("/items/{id}", "/items/中文 100%").unwrap();
        assert_eq!(
            render_route("/internal/{id}", &captures).as_deref(),
            Some("/internal/%E4%B8%AD%E6%96%87%20100%25")
        );
        assert!(
            sarmg_platform_core::route_specificity("/cameras/{camera_id}/ptz")
                > sarmg_platform_core::route_specificity("/cameras/{*path}")
        );
    }

    #[test]
    fn exhausted_process_supervisor_is_reported_as_failed_not_backoff() {
        let mut process = ProcessSnapshot {
            pid: None,
            ready: false,
            terminal: false,
            restart_count: 5,
            message: "waiting".into(),
        };
        assert_eq!(process_display_state(&process), PluginHealthState::Backoff);
        process.pid = Some(42);
        assert_eq!(process_display_state(&process), PluginHealthState::Starting);
        process.pid = None;
        process.terminal = true;
        assert_eq!(process_display_state(&process), PluginHealthState::Failed);
    }

    #[tokio::test]
    async fn periodic_health_degrades_recovers_and_is_cancelled() {
        let plugin_manifest =
            PluginManifest::parse_json(&manifest("health", None, "^0.5")).unwrap();
        let ready = Arc::new(AtomicBool::new(false));
        let health_calls = Arc::new(AtomicUsize::new(0));
        let plugin: Arc<dyn sarmg_platform_sdk::InProcessPlugin> = Arc::new(FakePlugin {
            manifest: plugin_manifest.clone(),
            starts: Arc::new(AtomicUsize::new(0)),
            stops: Arc::new(AtomicUsize::new(0)),
            ready: ready.clone(),
            health_calls: health_calls.clone(),
        });
        let monitor = HealthMonitor::start_with_interval(
            plugin_manifest,
            RunningBackend::InProcess(plugin),
            Duration::from_millis(10),
        )
        .unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if monitor.snapshot().state == PluginHealthState::Degraded {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(health_calls.load(Ordering::SeqCst) >= HEALTH_FAILURE_THRESHOLD as usize);

        ready.store(true, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if monitor.snapshot().state == PluginHealthState::Available {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();

        monitor.stop().await;
        let stopped_at = health_calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(health_calls.load(Ordering::SeqCst), stopped_at);
    }

    #[tokio::test]
    async fn every_execution_mode_has_an_explicit_trusted_adapter_outcome() {
        let temporary = tempfile::tempdir().unwrap();
        let bundled = temporary.path().join("bundled");
        for id in ["wasi", "process", "container", "service"] {
            write_package(&bundled, id, None, "^0.5");
        }
        rewrite_execution(
            &bundled,
            "wasi",
            serde_json::json!({
                "mode":"in_process",
                "runtime":"wasi_component_v1",
                "artifact":"backend/plugin.wasm",
                "entrypoint":"activate"
            }),
        );
        rewrite_execution(
            &bundled,
            "process",
            serde_json::json!({
                "mode":"process",
                "executable":"backend/missing-worker",
                "args":[],
                "environment":[],
                "bind":{"host":"127.0.0.1","port":0}
            }),
        );
        rewrite_execution(
            &bundled,
            "container",
            serde_json::json!({
                "mode":"container",
                "image":"registry.invalid/container:1.0.0",
                "digest":format!("sha256:{}", "f".repeat(64))
            }),
        );
        rewrite_execution(
            &bundled,
            "service",
            serde_json::json!({"mode":"service","service":"service.api"}),
        );

        let runtime = PlatformState::new(
            PackageStore::new(bundled, temporary.path().join("state")),
            "admin",
        )
        .unwrap();
        runtime.start().await.unwrap();
        for id in ["wasi", "process", "container", "service"] {
            runtime
                .set_configuration(id, serde_json::json!({}))
                .await
                .unwrap();
        }

        let wasi = format!("{:#}", runtime.enable("wasi").await.unwrap_err());
        assert!(wasi.contains("trusted whitelist"), "{wasi}");
        let process = format!("{:#}", runtime.enable("process").await.unwrap_err());
        assert!(process.contains("missing bundle path"), "{process}");
        let container = format!("{:#}", runtime.enable("container").await.unwrap_err());
        assert!(
            container.contains("trusted lifecycle/service-discovery adapter"),
            "{container}"
        );
        let service = format!("{:#}", runtime.enable("service").await.unwrap_err());
        assert!(
            service.contains("trusted lifecycle/service-discovery adapter"),
            "{service}"
        );
        assert!(runtime.modules().await.iter().all(|module| !module.enabled));
    }

    #[tokio::test]
    async fn runtime_end_to_end_discovers_validates_registers_and_rolls_back_state_changes() {
        let temporary = tempfile::tempdir().unwrap();
        let bundled = temporary.path().join("bundled");
        write_package(&bundled, "base", None, "^0.5");
        write_package(&bundled, "child", Some("base"), "^0.5");
        let store = PackageStore::new(bundled.clone(), temporary.path().join("state"));
        let runtime = PlatformState::new(store, "admin").unwrap();
        let starts = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        runtime
            .register_in_process_factory(
                sarmg_platform_core::InProcessRuntime::WasiComponentV1,
                Arc::new(FakeFactory {
                    starts: starts.clone(),
                    stops: stops.clone(),
                }),
            )
            .await
            .unwrap();
        let migrations = Arc::new(AtomicUsize::new(0));
        let fail_migrations = Arc::new(AtomicBool::new(false));
        runtime
            .register_migration_runner(
                MigrationEngine::Sqlite,
                Arc::new(FakeMigrationRunner {
                    calls: migrations.clone(),
                    fail: fail_migrations.clone(),
                }),
            )
            .await
            .unwrap();

        runtime.start().await.unwrap();
        let modules = runtime.modules().await;
        assert_eq!(
            modules
                .iter()
                .map(|module| module.manifest.id.as_str())
                .collect::<Vec<_>>(),
            vec!["base", "child"]
        );
        assert!(modules.iter().all(|module| !module.enabled));
        assert!(
            modules
                .iter()
                .all(|module| module.lifecycle_state == PluginHealthState::Stopped)
        );
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        assert_eq!(migrations.load(Ordering::SeqCst), 0);
        assert!(runtime.permissions.allows("admin", "child.echo.read").await);
        let configuration = runtime.configuration.get("base").await.unwrap();
        assert!(!configuration.configured);
        assert_eq!(configuration.value, None);

        // Merely bundling a module never activates it, and enable is rejected without a persisted
        // schema-valid configuration. Configuration is platform state, not package mutation.
        assert!(runtime.enable("base").await.is_err());
        assert!(!runtime.module("base").await.unwrap().enabled);
        runtime
            .set_configuration("base", serde_json::json!({}))
            .await
            .unwrap();
        assert!(
            temporary
                .path()
                .join("state/configuration/base.json")
                .is_file()
        );
        runtime.enable("base").await.unwrap();
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(stops.load(Ordering::SeqCst), 0);
        assert!(
            runtime
                .set_configuration("base", serde_json::json!({}))
                .await
                .is_err(),
            "a running module's persisted configuration must not change underneath it"
        );
        runtime
            .set_configuration("child", serde_json::json!({}))
            .await
            .unwrap();
        runtime.enable("child").await.unwrap();
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(stops.load(Ordering::SeqCst), 0);
        assert!(
            runtime
                .modules()
                .await
                .iter()
                .all(|module| module.lifecycle_state == PluginHealthState::Available)
        );
        assert!(starts.load(Ordering::SeqCst) >= 2);
        assert_eq!(
            starts.load(Ordering::SeqCst),
            migrations.load(Ordering::SeqCst)
        );
        runtime
            .events
            .publish("base", "base.changed.v1", serde_json::json!({"ok":true}))
            .await
            .unwrap();
        let backend = runtime.backend("base").await.unwrap();
        let route = backend
            .route_for(&axum::http::Method::GET, "/echo/value")
            .unwrap();
        assert_eq!(route.upstream_path, "/internal/value");

        // A dependency cannot be disabled while an enabled dependent needs it. Candidate state is
        // rejected before running instances are stopped and the enable-state pointer is restored.
        assert!(runtime.disable("base").await.is_err());
        assert!(runtime.module("base").await.unwrap().enabled);
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(stops.load(Ordering::SeqCst), 0);

        runtime.disable("child").await.unwrap();
        assert!(!runtime.module("child").await.unwrap().enabled);
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(stops.load(Ordering::SeqCst), 1);
        assert_eq!(
            runtime.module("base").await.unwrap().lifecycle_state,
            PluginHealthState::Available,
            "stopping one module must not restart or degrade an unrelated dependency"
        );
        runtime.disable("base").await.unwrap();
        assert!(!runtime.module("base").await.unwrap().enabled);
        assert_eq!(stops.load(Ordering::SeqCst), 2);
        runtime.enable("base").await.unwrap();
        runtime.enable("child").await.unwrap();
        assert_eq!(starts.load(Ordering::SeqCst), 4);
        assert_eq!(stops.load(Ordering::SeqCst), 2);
        assert!(runtime.modules().await.iter().all(|module| module.enabled));

        // A malformed/incompatible replacement in the read-only distribution is rejected on
        // rescan before switching the active graph. Existing instances and registrations remain.
        std::fs::write(
            bundled.join("child/manifest.json"),
            manifest("child", Some("base"), ">=99.0"),
        )
        .unwrap();
        let starts_before = starts.load(Ordering::SeqCst);
        assert!(runtime.rescan().await.is_err());
        assert_eq!(
            runtime.module("child").await.unwrap().manifest.version,
            "1.0.0"
        );
        assert_eq!(starts.load(Ordering::SeqCst), starts_before);

        runtime.stop_all().await;
        assert!(stops.load(Ordering::SeqCst) >= starts.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn one_enabled_module_failure_does_not_prevent_core_or_siblings_from_starting() {
        let temporary = tempfile::tempdir().unwrap();
        let bundled = temporary.path().join("bundled");
        write_package(&bundled, "bad", None, "^0.5");
        write_package(&bundled, "good", None, "^0.5");
        rewrite_execution(
            &bundled,
            "bad",
            serde_json::json!({
                "mode":"process",
                "executable":"backend/missing-worker",
                "args":[],
                "environment":[],
                "bind":{"host":"127.0.0.1","port":0}
            }),
        );
        let store = PackageStore::new(bundled, temporary.path().join("custom-state"));
        store.prepare().unwrap();
        for id in ["bad", "good"] {
            let configuration_path = store.configuration_directory().join(format!("{id}.json"));
            std::fs::write(&configuration_path, b"{}").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &configuration_path,
                    std::fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
            store.set_enabled(id, true).unwrap();
        }
        let runtime = PlatformState::new(store, "admin").unwrap();
        let starts = Arc::new(AtomicUsize::new(0));
        runtime
            .register_in_process_factory(
                sarmg_platform_core::InProcessRuntime::WasiComponentV1,
                Arc::new(FakeFactory {
                    starts: starts.clone(),
                    stops: Arc::new(AtomicUsize::new(0)),
                }),
            )
            .await
            .unwrap();
        runtime
            .register_migration_runner(
                MigrationEngine::Sqlite,
                Arc::new(FakeMigrationRunner {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail: Arc::new(AtomicBool::new(false)),
                }),
            )
            .await
            .unwrap();

        runtime.start().await.unwrap();

        let bad = runtime.module("bad").await.unwrap();
        assert!(bad.enabled);
        assert_eq!(bad.lifecycle_state, PluginHealthState::Failed);
        let good = runtime.module("good").await.unwrap();
        assert!(good.enabled);
        assert_eq!(
            good.lifecycle_state,
            PluginHealthState::Available,
            "healthy sibling failed to activate: {}",
            good.health_message
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(runtime.backend("good").await.is_some());
        assert!(runtime.backend("bad").await.is_none());
        runtime.stop_all().await;
    }
}
