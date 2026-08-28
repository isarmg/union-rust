//! Product-neutral Core Platform and runtime plugin composition.
//!
//! Business modules are discovered from validated versioned packages. This crate owns only the
//! common runtime, RBAC, configuration, lifecycle, event, task, notification and gateway
//! mechanisms; it contains no module-id switch and no business handler.

mod configuration;
mod event_bus;
mod gateway;
mod http;
mod notifications;
mod package_store;
mod rbac;
mod runtime;
mod sdk_bridge;
mod tasks;

pub use event_bus::{EventBus, EventBusError, EventContract, EventSubscription, PlatformEvent};
pub use notifications::{Notification, NotificationCenter, NotificationSeverity};
pub use package_store::{ActiveSelection, PackageSource, PackageStore};
pub use rbac::{
    AuthenticatedPrincipal, PermissionDefinition, PermissionError, PermissionRegistry,
    RegisteredPermission,
};
pub use runtime::{
    AdapterHandle, InProcessFactory, MigrationRunner, ModuleRuntimeView, PlatformState,
    PluginBackend, PluginContext, PluginFuture, PluginResponseFuture, ServiceEndpoint,
    ServiceLifecycleAdapter,
};
pub use sarmg_platform_sdk::InProcessPlugin;
pub use tasks::{
    TaskDefinition, TaskError, TaskFuture, TaskHandler, TaskRun, TaskRunState, TaskScheduler,
};

pub(crate) use gateway::{module_api_router, module_asset_router};
pub(crate) use http::console_router;

use crate::state::AppState;

/// Discover and activate the immutable distribution graph without delaying the Core listener.
///
/// The runtime's operation mutex still serializes this bootstrap with rescan/config/enable/disable;
/// only the potentially long process readiness gates move off the HTTP startup path. A malformed
/// package graph is reported through logs while the product-neutral control plane remains usable.
pub fn start_external_modules(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let startup = state.platform.start();
        tokio::pin!(startup);
        let mut startup_finished = false;
        let mut shutdown = state.subscribe_shutdown();
        let mut publisher = tokio::time::interval(std::time::Duration::from_secs(1));
        publisher.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_snapshot = None;

        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow_and_update() {
                        break;
                    }
                }
                result = &mut startup, if !startup_finished => {
                    startup_finished = true;
                    if let Err(error) = result {
                        tracing::error!(
                            "module runtime initialization failed without stopping Core: {error:#}"
                        );
                    }
                    publish_module_statuses(&state, &mut last_snapshot).await;
                }
                _ = publisher.tick() => {
                    publish_module_statuses(&state, &mut last_snapshot).await;
                }
            }
        }
    })
}

async fn publish_module_statuses(
    state: &AppState,
    last_snapshot: &mut Option<Vec<crate::system::ServiceStatus>>,
) {
    let snapshot = state
        .platform
        .modules()
        .await
        .into_iter()
        .map(module_service_status)
        .collect::<Vec<_>>();
    if last_snapshot.as_ref() != Some(&snapshot) {
        *last_snapshot = Some(snapshot.clone());
        state.services.publish("plugin-runtime", snapshot).await;
    }
}

fn module_service_status(module: ModuleRuntimeView) -> crate::system::ServiceStatus {
    use sarmg_platform_core::{Execution, PluginHealthState};

    let runtime_state = match module.lifecycle_state {
        PluginHealthState::Discovered => "discovered",
        PluginHealthState::Installing => "installing",
        PluginHealthState::Starting => "starting",
        PluginHealthState::Available => "available",
        PluginHealthState::Degraded => "degraded",
        PluginHealthState::Backoff => "backoff",
        PluginHealthState::Incompatible => "incompatible",
        PluginHealthState::Stopped => "stopped",
        PluginHealthState::Failed => "failed",
    };
    let kind = match module.manifest.execution {
        Execution::InProcess { .. } => "module-in-process",
        Execution::Process { .. } => "module-process",
        Execution::Container { .. } => "module-container",
        Execution::Service { .. } => "module-service",
    };
    crate::system::ServiceStatus {
        name: module.manifest.display_name,
        kind: kind.into(),
        runtime_state: runtime_state.into(),
        healthy: module.enabled && module.lifecycle_state == PluginHealthState::Available,
        // Internal worker addresses and per-process credentials are deliberately absent from the
        // public control-plane status projection. Clients always use the Manifest gateway prefix.
        address: None,
        pid: module.pid,
        message: module.health_message,
        // Keep never-checked states stable so the publisher does not manufacture a new snapshot
        // every second merely by changing a presentation timestamp.
        updated_at: module.checked_at.unwrap_or_default(),
    }
}

#[cfg(test)]
fn spawn_external_module_startup<F>(startup: F) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(error) = startup.await {
            tracing::error!(
                "module runtime initialization failed without stopping Core: {error:#}"
            );
        }
    })
}

#[cfg(test)]
use std::future::Future;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[tokio::test]
    async fn slow_module_startup_is_detached_from_core_readiness() {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (_release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

        let task = super::spawn_external_module_startup(async move {
            let _ = entered_tx.send(());
            let _ = release_rx.await;
            Ok(())
        });

        tokio::time::timeout(Duration::from_secs(1), entered_rx)
            .await
            .expect("detached startup was scheduled")
            .expect("startup task reached the readiness gate");
        assert!(
            !task.is_finished(),
            "the simulated module is still waiting while Core can start serving"
        );
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
}
