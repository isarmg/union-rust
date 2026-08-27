//! Compile-time platform composition, private worker health and gateway state.
//!
//! A selected service module has exactly one binding, binary name and public prefix compiled into
//! Union. Runtime configuration may provide the module's database, storage and business secrets,
//! but it cannot replace the worker executable or point Union at another upstream URL.

#![cfg_attr(
    not(any(
        feature = "module-sentinel-monitor",
        feature = "module-photo-backup",
        feature = "module-dufs",
        feature = "module-sunshine",
        feature = "module-host-monitoring"
    )),
    allow(dead_code)
)]

mod gateway;
mod spec;
mod supervisor;

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{Json, Router, extract::State, routing::get};
use futures_util::stream::{self, StreamExt};
#[cfg(test)]
use sarmg_platform_core::ModuleExecution;
#[cfg(any(
    feature = "module-sentinel-monitor",
    feature = "module-photo-backup",
    feature = "module-dufs",
    feature = "module-sunshine",
    feature = "module-host-monitoring"
))]
use sarmg_platform_core::manifests;
use sarmg_platform_core::{ModuleCatalog, ModuleDescriptor, ModuleHealthState, ModuleInstance};
use tokio::{sync::RwLock, task::JoinHandle};

use crate::{state::AppState, system::ServiceStatus};

pub(crate) use gateway::{console_gateway_router, gateway_router, public_worker_router};
use spec::{InternalCredential, ModuleSpec, compiled_specs};

const PROBE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerPhase {
    Compiled,
    Starting,
    Running,
    Backoff,
    Stopped,
}

#[derive(Debug, Clone)]
struct HealthSnapshot {
    state: ModuleHealthState,
    message: String,
    checked_at: Option<String>,
    phase: WorkerPhase,
    pid: Option<u32>,
    restart_count: u64,
    /// Set only after an authenticated liveness response proves the exact gateway-v1 contract.
    gateway_compatible: bool,
}

impl HealthSnapshot {
    fn compiled() -> Self {
        Self {
            state: ModuleHealthState::Probing,
            message: "已编译；等待 Union worker supervisor 启动".into(),
            checked_at: None,
            phase: WorkerPhase::Compiled,
            pid: None,
            restart_count: 0,
            gateway_compatible: false,
        }
    }
}

#[derive(Debug, Clone)]
struct ExternalService {
    descriptor: ModuleDescriptor,
    spec: ModuleSpec,
    credential: InternalCredential,
}

#[derive(Clone)]
pub struct PlatformState {
    catalog: Arc<ModuleCatalog>,
    external: Arc<Vec<ExternalService>>,
    health: Arc<RwLock<BTreeMap<String, HealthSnapshot>>>,
    probe_client: reqwest::Client,
    gateway_client: reqwest::Client,
    worker_shutdown: tokio::sync::watch::Sender<bool>,
    started: Arc<AtomicBool>,
    tasks: Arc<tokio::sync::Mutex<Vec<JoinHandle<()>>>>,
}

impl PlatformState {
    pub fn new() -> anyhow::Result<Self> {
        let catalog = catalog()?;
        let external = compiled_specs()
            .into_iter()
            .map(|spec| ExternalService {
                descriptor: catalog
                    .get(spec.id)
                    .expect("every compiled worker has a shipped descriptor")
                    .clone(),
                credential: InternalCredential::new(spec.id),
                spec,
            })
            .collect::<Vec<_>>();
        let health = external
            .iter()
            .map(|service| (service.spec.id.to_string(), HealthSnapshot::compiled()))
            .collect();
        let common_client = || {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .user_agent(concat!("unionc-platform/", env!("CARGO_PKG_VERSION")))
        };
        let (worker_shutdown, _) = tokio::sync::watch::channel(false);
        Ok(Self {
            catalog: Arc::new(catalog),
            external: Arc::new(external),
            health: Arc::new(RwLock::new(health)),
            probe_client: common_client().timeout(Duration::from_secs(3)).build()?,
            // Uploads, downloads and SSE have module-specific lifetimes. The Union gateway does
            // not impose a whole-response timeout; connect failure is still bounded above.
            gateway_client: common_client().build()?,
            worker_shutdown,
            started: Arc::new(AtomicBool::new(false)),
            tasks: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        })
    }

    pub async fn instances(&self) -> Vec<ModuleInstance> {
        let health = self.health.read().await;
        self.catalog
            .modules()
            .iter()
            .map(|descriptor| {
                let snapshot = health.get(&descriptor.id);
                ModuleInstance {
                    descriptor: descriptor.clone(),
                    health: snapshot
                        .map(|value| value.state)
                        .unwrap_or(ModuleHealthState::Unconfigured),
                    health_message: snapshot
                        .map(|value| value.message.clone())
                        .unwrap_or_else(|| "当前发行版未编译此模块".into()),
                    pid: snapshot.and_then(|value| value.pid),
                    restart_count: snapshot.map(|value| value.restart_count).unwrap_or(0),
                    checked_at: snapshot.and_then(|value| value.checked_at.clone()),
                }
            })
            .collect()
    }

    fn external_services(&self) -> Vec<ExternalService> {
        self.external.as_ref().clone()
    }

    async fn service_for_gateway(&self, id: &str) -> Option<ExternalService> {
        let service = self.external.iter().find(|service| service.spec.id == id)?;
        self.health.read().await.get(id).filter(|snapshot| {
            snapshot.phase == WorkerPhase::Running && snapshot.gateway_compatible
        })?;
        Some(service.clone())
    }

    async fn publish_lifecycle(
        &self,
        id: &str,
        phase: WorkerPhase,
        pid: Option<u32>,
        restart_count: u64,
        state: ModuleHealthState,
        message: impl Into<String>,
    ) {
        if let Some(snapshot) = self.health.write().await.get_mut(id) {
            snapshot.phase = phase;
            snapshot.pid = pid;
            snapshot.restart_count = restart_count;
            snapshot.state = state;
            snapshot.message = message.into();
            snapshot.checked_at = Some(chrono::Utc::now().to_rfc3339());
            // Every child generation must re-prove the contract before receiving traffic. The
            // credential itself is scoped to this Union process and rotates on Union restart.
            snapshot.gateway_compatible = false;
        }
    }

    async fn running_pid(&self, id: &str) -> Option<u32> {
        self.health
            .read()
            .await
            .get(id)
            .filter(|snapshot| snapshot.phase == WorkerPhase::Running)
            .and_then(|snapshot| snapshot.pid)
    }

    async fn apply_probe(&self, id: &str, pid: u32, result: ProbeResult) {
        let mut health = self.health.write().await;
        let Some(snapshot) = health.get_mut(id) else {
            return;
        };
        // Do not publish a late response from a process generation that has already exited.
        if snapshot.phase != WorkerPhase::Running || snapshot.pid != Some(pid) {
            return;
        }
        snapshot.state = if result.healthy {
            ModuleHealthState::Available
        } else {
            ModuleHealthState::Degraded
        };
        snapshot.message = result.message;
        snapshot.checked_at = Some(chrono::Utc::now().to_rfc3339());
        snapshot.gateway_compatible = result.gateway_compatible;
    }

    async fn probe(&self) -> Vec<ServiceStatus> {
        stream::iter(self.external.iter().cloned().map(|service| {
            let client = self.probe_client.clone();
            let platform = self.clone();
            async move {
                let pid = platform.running_pid(service.spec.id).await?;
                let result = probe_service(client, &service).await;
                platform
                    .apply_probe(service.spec.id, pid, result.clone())
                    .await;
                Some(service_status(&service, pid, &result))
            }
        }))
        .buffer_unordered(3)
        .filter_map(async move |value| value)
        .collect::<Vec<_>>()
        .await
    }

    async fn start(self, app: AppState) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut tasks = self.tasks.lock().await;
        for service in self.external_services() {
            tasks.push(tokio::spawn(supervisor::supervise(self.clone(), service)));
        }
        let probe_app = app.clone();
        let platform = self.clone();
        tasks.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(PROBE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut shutdown = probe_app.subscribe_shutdown();
            loop {
                tokio::select! {
                    biased;
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow_and_update() {
                            break;
                        }
                    }
                    _ = interval.tick() => {
                        let statuses = platform.probe().await;
                        probe_app.services.publish("external-modules", statuses).await;
                    }
                }
            }
        }));
    }

    fn subscribe_worker_shutdown(&self) -> tokio::sync::watch::Receiver<bool> {
        self.worker_shutdown.subscribe()
    }

    /// Stop workers only after Axum has drained gateway responses. Using the application's earlier
    /// shutdown notification here would terminate a worker underneath an in-flight upload.
    pub async fn stop_workers(&self) {
        self.worker_shutdown.send_replace(true);
        let tasks = std::mem::take(&mut *self.tasks.lock().await);
        for task in tasks {
            if let Err(error) = task.await {
                tracing::error!("platform worker task failed: {error}");
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ProbeResult {
    healthy: bool,
    gateway_compatible: bool,
    runtime_state: &'static str,
    message: String,
}

async fn probe_service(client: reqwest::Client, service: &ExternalService) -> ProbeResult {
    let live = probe_endpoint(&client, service, service.spec.liveness_path).await;
    let Ok(live) = live else {
        return live.unwrap_err();
    };
    if !live.gateway_compatible {
        return live;
    }
    let Some(readiness) = service.spec.readiness_path else {
        return live;
    };
    match probe_endpoint(&client, service, readiness).await {
        Ok(ready) => ready,
        Err(error) => error,
    }
}

async fn probe_endpoint(
    client: &reqwest::Client,
    service: &ExternalService,
    path: &str,
) -> Result<ProbeResult, ProbeResult> {
    let endpoint = format!("http://{}{}", service.spec.bind, path);
    let response = client
        .get(endpoint)
        .header(gateway::PROTOCOL_HEADER, gateway::PROTOCOL_VERSION)
        .header(gateway::AUDIENCE_HEADER, service.spec.id)
        .header(gateway::TOKEN_HEADER, service.credential.token.as_ref())
        .header(gateway::PREFIX_HEADER, service.spec.gateway_prefix)
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) if error.is_timeout() => {
            return Err(failed_probe("存活探测超时"));
        }
        Err(error) if error.is_connect() => {
            return Err(failed_probe("worker 尚未接受连接"));
        }
        Err(_) => return Err(failed_probe("worker 存活探测失败")),
    };
    if !response.status().is_success() {
        return Err(failed_probe(format!(
            "worker 探测返回 HTTP {}",
            response.status().as_u16()
        )));
    }
    let protocol = response
        .headers()
        .get(gateway::PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok());
    let audience = response
        .headers()
        .get(gateway::AUDIENCE_HEADER)
        .and_then(|value| value.to_str().ok());
    if protocol != Some(gateway::PROTOCOL_VERSION) || audience != Some(service.spec.id) {
        return Ok(ProbeResult {
            healthy: false,
            gateway_compatible: false,
            runtime_state: "incompatible",
            message: "worker 未证明 gateway-v1 audience/token 契约；代理保持关闭".into(),
        });
    }
    Ok(ProbeResult {
        healthy: true,
        gateway_compatible: true,
        runtime_state: "available",
        message: "私有 worker、就绪探测和 gateway-v1 内部凭据均通过".into(),
    })
}

fn failed_probe(message: impl Into<String>) -> ProbeResult {
    ProbeResult {
        healthy: false,
        gateway_compatible: false,
        runtime_state: "degraded",
        message: message.into(),
    }
}

fn service_status(service: &ExternalService, pid: u32, result: &ProbeResult) -> ServiceStatus {
    ServiceStatus {
        name: service.descriptor.display_name.clone(),
        kind: "platform-service-module".into(),
        runtime_state: result.runtime_state.into(),
        healthy: result.healthy,
        address: Some(format!("http://{}", service.spec.bind)),
        pid: Some(pid),
        message: result.message.clone(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    }
}

fn catalog() -> anyhow::Result<ModuleCatalog> {
    let manifests = vec![
        #[cfg(feature = "module-sunshine")]
        manifests::SUNSHINE,
        #[cfg(feature = "module-host-monitoring")]
        manifests::HOST_MONITORING,
        #[cfg(feature = "module-sentinel-monitor")]
        manifests::SENTINEL_MONITOR,
        #[cfg(feature = "module-photo-backup")]
        manifests::PHOTO_BACKUP,
        #[cfg(feature = "module-dufs")]
        manifests::DUFS,
    ];
    let modules = manifests
        .into_iter()
        .map(serde_json::from_str)
        .collect::<Result<Vec<ModuleDescriptor>, _>>()?;
    Ok(ModuleCatalog::new(modules)?)
}

pub(crate) fn console_router() -> Router<AppState> {
    Router::new().route("/api/platform/modules", get(list_modules))
}

async fn list_modules(State(state): State<AppState>) -> Json<Vec<ModuleInstance>> {
    Json(state.platform.instances().await)
}

pub async fn start_external_modules(state: AppState) {
    let platform = state.platform.clone();
    platform.start(state).await;
}

pub(crate) fn is_compiled_gateway_path(path: &str) -> bool {
    compiled_specs().into_iter().any(|spec| {
        path == spec.gateway_prefix
            || path
                .strip_prefix(spec.gateway_prefix)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "module-dufs")]
    use sarmg_platform_core::UiContribution;

    #[test]
    fn catalog_and_runtime_table_have_the_same_compile_time_membership() {
        let catalog = catalog().unwrap();
        let expected = usize::from(cfg!(feature = "module-sunshine"))
            + usize::from(cfg!(feature = "module-host-monitoring"))
            + usize::from(cfg!(feature = "module-dufs"))
            + usize::from(cfg!(feature = "module-photo-backup"))
            + usize::from(cfg!(feature = "module-sentinel-monitor"));
        assert_eq!(catalog.modules().len(), expected);
        assert_eq!(compiled_specs().len(), expected);
        for spec in compiled_specs() {
            assert_eq!(
                catalog.get(spec.id).unwrap().execution,
                ModuleExecution::PrivateProcess
            );
            assert!(spec.bind.ip().is_loopback());
            assert!(spec.gateway_prefix.starts_with("/modules/"));
        }
        #[cfg(feature = "module-dufs")]
        assert!(matches!(
            catalog.get("dufs").unwrap().ui,
            UiContribution::Gateway { .. }
        ));
        #[cfg(not(feature = "module-dufs"))]
        assert!(catalog.get("dufs").is_none());
    }

    #[tokio::test]
    async fn compiled_service_is_not_launchable_before_contract_probe() {
        let state = PlatformState::new().unwrap();
        let modules = state.instances().await;
        #[cfg(feature = "module-sentinel-monitor")]
        {
            let sentinel = modules
                .iter()
                .find(|module| module.descriptor.id == "sentinel-monitor")
                .unwrap();
            assert_eq!(sentinel.health, ModuleHealthState::Probing);
            assert!(sentinel.pid.is_none());
        }
        #[cfg(not(feature = "module-sentinel-monitor"))]
        assert!(
            modules
                .iter()
                .all(|module| module.descriptor.id != "sentinel-monitor")
        );
    }

    #[test]
    fn omitted_features_have_no_gateway_path() {
        #[cfg(not(feature = "module-dufs"))]
        assert!(!is_compiled_gateway_path("/modules/dufs/file"));
        #[cfg(feature = "module-dufs")]
        assert!(is_compiled_gateway_path("/modules/dufs/file"));
        assert!(!is_compiled_gateway_path("/modules/dufs-evil/file"));
    }
}
