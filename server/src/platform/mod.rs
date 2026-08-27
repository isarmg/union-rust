//! Platform composition root.
//!
//! The generic contracts live in the sibling `platform` project. This module is the only place
//! that knows which business modules are linked into the Union distribution.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use axum::{Json, Router, extract::State, routing::get};
use futures_util::stream::{self, StreamExt};
use sarmg_platform_axum::{AxumModule, assemble};
use sarmg_platform_core::{
    ModuleCatalog, ModuleDescriptor, ModuleExecution, ModuleHealthState, ModuleInstance, manifests,
};
use tokio::sync::RwLock;

use crate::{config::PlatformSettings, state::AppState, system::ServiceStatus};

#[derive(Debug, Clone)]
struct HealthSnapshot {
    state: ModuleHealthState,
    message: String,
    checked_at: Option<String>,
}

#[derive(Debug, Clone)]
struct ExternalService {
    descriptor: ModuleDescriptor,
    base_url: Option<reqwest::Url>,
}

#[derive(Clone)]
pub struct PlatformState {
    catalog: Arc<ModuleCatalog>,
    external: Arc<Vec<ExternalService>>,
    health: Arc<RwLock<BTreeMap<String, HealthSnapshot>>>,
    probe_client: reqwest::Client,
}

impl PlatformState {
    pub fn new(settings: &PlatformSettings) -> anyhow::Result<Self> {
        let catalog = catalog()?;
        let mut external = Vec::new();
        let mut health = BTreeMap::new();
        for descriptor in catalog.modules() {
            if descriptor.execution != ModuleExecution::Service {
                continue;
            }
            let base_url = settings
                .service_urls
                .get(&descriptor.id)
                .map(|value| reqwest::Url::parse(value))
                .transpose()?;
            health.insert(
                descriptor.id.clone(),
                if base_url.is_some() {
                    HealthSnapshot {
                        state: ModuleHealthState::Probing,
                        message: "等待首次存活探测".into(),
                        checked_at: None,
                    }
                } else {
                    HealthSnapshot {
                        state: ModuleHealthState::Unconfigured,
                        message: "未配置服务 URL".into(),
                        checked_at: None,
                    }
                },
            );
            external.push(ExternalService {
                descriptor: descriptor.clone(),
                base_url,
            });
        }
        let probe_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .user_agent(concat!("unionc-platform/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            catalog: Arc::new(catalog),
            external: Arc::new(external),
            health: Arc::new(RwLock::new(health)),
            probe_client,
        })
    }

    pub async fn instances(&self) -> Vec<ModuleInstance> {
        let health = self.health.read().await;
        self.catalog
            .modules()
            .iter()
            .map(|descriptor| {
                let external = self
                    .external
                    .iter()
                    .find(|service| service.descriptor.id == descriptor.id);
                let snapshot = health.get(&descriptor.id);
                let in_process = descriptor.execution == ModuleExecution::InProcess;
                ModuleInstance {
                    descriptor: descriptor.clone(),
                    configured: in_process
                        || external.and_then(|value| value.base_url.as_ref()).is_some(),
                    health: if in_process {
                        ModuleHealthState::Available
                    } else {
                        snapshot
                            .map(|value| value.state)
                            .unwrap_or(ModuleHealthState::Unconfigured)
                    },
                    health_message: if in_process {
                        "已编译到当前发行版".into()
                    } else {
                        snapshot
                            .map(|value| value.message.clone())
                            .unwrap_or_else(|| "未配置服务 URL".into())
                    },
                    launch_url: external
                        .and_then(|value| value.base_url.as_ref())
                        .map(ToString::to_string),
                    checked_at: snapshot.and_then(|value| value.checked_at.clone()),
                }
            })
            .collect()
    }

    async fn probe(&self) -> Vec<ServiceStatus> {
        let results = stream::iter(self.external.iter().cloned().map(|service| {
            let client = self.probe_client.clone();
            async move { probe_service(client, service).await }
        }))
        .buffer_unordered(3)
        .collect::<Vec<_>>()
        .await;

        let mut health = self.health.write().await;
        let mut statuses = Vec::with_capacity(results.len());
        for (id, snapshot, status) in results {
            health.insert(id, snapshot);
            statuses.push(status);
        }
        statuses
    }
}

fn catalog() -> anyhow::Result<ModuleCatalog> {
    let manifests = vec![
        manifests::SUNSHINE,
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

fn descriptor(id: &str) -> ModuleDescriptor {
    catalog()
        .expect("shipped platform manifests are valid")
        .get(id)
        .expect("linked module has a shipped manifest")
        .clone()
}

fn in_process_modules() -> Vec<AxumModule<AppState>> {
    vec![
        AxumModule {
            descriptor: descriptor("sunshine"),
            console_routes: crate::sunshine::http::router,
            public_routes: None,
        },
        AxumModule {
            descriptor: descriptor("host-monitoring"),
            console_routes: crate::monitoring::http::console_router,
            public_routes: Some(crate::monitoring::http::agent_router),
        },
    ]
}

pub(crate) fn console_router() -> Router<AppState> {
    let assembled = assemble(in_process_modules()).expect("linked module descriptors are valid");
    Router::new()
        .route("/api/platform/modules", get(list_modules))
        .merge(assembled.console_routes)
}

pub(crate) fn public_router() -> Router<AppState> {
    assemble(in_process_modules())
        .expect("linked module descriptors are valid")
        .public_routes
}

async fn list_modules(State(state): State<AppState>) -> Json<Vec<ModuleInstance>> {
    Json(state.platform.instances().await)
}

pub(crate) fn start_external_service_probes(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut shutdown = state.subscribe_shutdown();
        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow_and_update() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    let statuses = state.platform.probe().await;
                    state.services.publish("external-modules", statuses).await;
                }
            }
        }
    });
}

async fn probe_service(
    client: reqwest::Client,
    service: ExternalService,
) -> (String, HealthSnapshot, ServiceStatus) {
    let now = chrono::Utc::now().to_rfc3339();
    let Some(base_url) = service.base_url else {
        let snapshot = HealthSnapshot {
            state: ModuleHealthState::Unconfigured,
            message: "未配置服务 URL".into(),
            checked_at: None,
        };
        return (
            service.descriptor.id.clone(),
            snapshot,
            service_status(
                &service.descriptor,
                None,
                false,
                "not-configured",
                "未配置服务 URL",
                &now,
            ),
        );
    };
    let binding = service
        .descriptor
        .service
        .as_ref()
        .expect("external service manifest has a binding");
    let endpoint = base_url
        .join(binding.liveness_path.trim_start_matches('/'))
        .expect("validated liveness path joins a validated base URL");
    let result = client.get(endpoint).send().await;
    let (healthy, runtime_state, message) = match result {
        Ok(response) if response.status().is_success() => {
            (true, "available", "公开存活探测通过".to_string())
        }
        Ok(response) => (
            false,
            "degraded",
            format!("存活探测返回 HTTP {}", response.status().as_u16()),
        ),
        Err(error) if error.is_timeout() => (false, "degraded", "存活探测超时".to_string()),
        Err(error) if error.is_connect() => (false, "degraded", "无法连接服务".to_string()),
        Err(_) => (false, "degraded", "存活探测失败".to_string()),
    };
    let snapshot = HealthSnapshot {
        state: if healthy {
            ModuleHealthState::Available
        } else {
            ModuleHealthState::Degraded
        },
        message: message.clone(),
        checked_at: Some(now.clone()),
    };
    let status = service_status(
        &service.descriptor,
        Some(base_url.to_string()),
        healthy,
        runtime_state,
        &message,
        &now,
    );
    (service.descriptor.id, snapshot, status)
}

fn service_status(
    descriptor: &ModuleDescriptor,
    address: Option<String>,
    healthy: bool,
    runtime_state: &str,
    message: &str,
    updated_at: &str,
) -> ServiceStatus {
    ServiceStatus {
        name: descriptor.display_name.clone(),
        kind: "platform-service-module".into(),
        runtime_state: runtime_state.into(),
        healthy,
        address,
        pid: None,
        message: message.into(),
        updated_at: updated_at.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "module-dufs")]
    use sarmg_platform_core::UiContribution;

    #[test]
    fn catalog_keeps_business_data_ownership_separate() {
        let catalog = catalog().unwrap();
        let expected = 2
            + usize::from(cfg!(feature = "module-dufs"))
            + usize::from(cfg!(feature = "module-photo-backup"))
            + usize::from(cfg!(feature = "module-sentinel-monitor"));
        assert_eq!(catalog.modules().len(), expected);
        #[cfg(feature = "module-dufs")]
        assert!(matches!(
            catalog.get("dufs").unwrap().ui,
            UiContribution::External { .. }
        ));
        #[cfg(not(feature = "module-dufs"))]
        assert!(catalog.get("dufs").is_none());
        assert_eq!(
            catalog.get("host-monitoring").unwrap().execution,
            ModuleExecution::InProcess
        );
    }

    #[tokio::test]
    async fn unconfigured_services_are_visible_but_not_launchable() {
        let state = PlatformState::new(&PlatformSettings::default()).unwrap();
        let modules = state.instances().await;
        #[cfg(feature = "module-sentinel-monitor")]
        {
            let sentinel = modules
                .iter()
                .find(|module| module.descriptor.id == "sentinel-monitor")
                .unwrap();
            assert!(!sentinel.configured);
            assert_eq!(sentinel.health, ModuleHealthState::Unconfigured);
            assert!(sentinel.launch_url.is_none());
        }
        #[cfg(not(feature = "module-sentinel-monitor"))]
        assert!(
            modules
                .iter()
                .all(|module| module.descriptor.id != "sentinel-monitor")
        );
    }
}
