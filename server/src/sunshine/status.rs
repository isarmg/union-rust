use std::future::Future;

use crate::sunshine::SunshineStatus;
use crate::system::ServiceStatus;
use crate::{
    config::SunshineHostConfig,
    state::{AppState, SunshineHostHealth},
};

/// 读取最近一次探测结果。**不发起任何网络请求。**
///
/// 探测由 `startup::start_service_status_probe` 启动的后台 worker 负责；
/// HTTP 与 SSE 两条读路径都只消费快照，因此增加客户端不会增加对被监控主机的探测。
pub async fn all_services(state: &AppState) -> Vec<ServiceStatus> {
    state.services.snapshot.read().await.clone()
}

#[derive(Clone)]
pub(crate) struct ServiceProbeBatch {
    hosts: Vec<SunshineHostConfig>,
    reachable: Vec<bool>,
    statuses: Vec<ServiceStatus>,
}

/// Run one background probe and publish it only if its complete input
/// configuration is still current.
///
/// Per-host Web health already rejects a late result in
/// `publish_health_if_current`. The service snapshot needs the same protection
/// at batch granularity: otherwise a slow probe started before a PATCH/DELETE
/// can resurrect an old address (or a deleted host) in `/api/services` and SSE.
pub(crate) async fn probe_and_publish_services(state: &AppState) -> Option<ServiceProbeBatch> {
    let batch = probe_services(state).await;
    if publish_service_batch_if_current(state, &batch).await {
        Some(batch)
    } else {
        None
    }
}

async fn probe_services(state: &AppState) -> ServiceProbeBatch {
    let hosts = state.hosts.sunshine.read().await.clone();
    let results = probe_hosts_with(hosts, |host| async move {
        let reachable = crate::sunshine::client::check_reachable(&host).await;
        (host, reachable)
    })
    .await;
    service_probe_batch(results)
}

const MAX_CONCURRENT_PROBES: usize = 8;

async fn probe_hosts_with<F, Fut>(
    hosts: Vec<SunshineHostConfig>,
    probe: F,
) -> Vec<(SunshineHostConfig, bool)>
where
    F: FnMut(SunshineHostConfig) -> Fut,
    Fut: Future<Output = (SunshineHostConfig, bool)>,
{
    use futures_util::stream::{self, StreamExt};

    stream::iter(hosts.into_iter().map(probe))
        .buffered(MAX_CONCURRENT_PROBES)
        .collect()
        .await
}

fn service_probe_batch(results: Vec<(SunshineHostConfig, bool)>) -> ServiceProbeBatch {
    let (hosts, reachable): (Vec<_>, Vec<_>) = results.into_iter().unzip();
    let statuses = hosts
        .iter()
        .zip(&reachable)
        .map(|(host, reachable)| sunshine_host_service_status(host, *reachable))
        .collect();
    ServiceProbeBatch {
        hosts,
        reachable,
        statuses,
    }
}

/// Refresh the slower Sunshine Web API health snapshot from a previously
/// published TCP batch. The dedicated health worker calls this serially, so a
/// slow upstream round can neither overlap another health round nor delay the
/// five-second TCP publisher.
pub(crate) async fn probe_and_publish_health(state: &AppState, batch: ServiceProbeBatch) {
    use futures_util::stream::{self, StreamExt};

    let production = state.settings.production;
    stream::iter(batch.hosts.into_iter().zip(batch.reachable).map(
        |(host, reachable)| async move {
            let connection = if production && !host.verify_tls {
                Err("生产环境不允许关闭 Sunshine TLS 证书验证".to_string())
            } else if reachable {
                crate::sunshine::client::check_connection(&host).await
            } else {
                Err("Sunshine Web 端口不可达".to_string())
            };
            let snapshot = SunshineHostHealth::completed(reachable, &connection);
            publish_health_if_current(state, &host, snapshot).await;
        },
    ))
    .buffered(MAX_CONCURRENT_PROBES)
    .collect::<Vec<_>>()
    .await;
}

async fn publish_service_batch_if_current(state: &AppState, batch: &ServiceProbeBatch) -> bool {
    let current = state.hosts.sunshine.read().await;
    if current.len() != batch.hosts.len()
        || !current
            .iter()
            .zip(&batch.hosts)
            .all(|(left, right)| same_host_configuration(left, right))
    {
        return false;
    }

    // Keep the configuration read lock through publication. A CRUD writer
    // cannot change the host list between the equality check and snapshot/SSE
    // publication, so a result accepted here was produced for exactly the
    // configuration visible at the publication instant.
    *state.services.snapshot.write().await = batch.statuses.clone();
    let _ = state.services.events.send(batch.statuses.clone());
    true
}

/// 只在被探测的配置仍是当前配置时发布结果。
///
/// 网络请求期间不持锁；返回时逐字段比较（包括密码和 TLS 策略），防止一次针对旧地址
/// 或旧凭据的迟到响应覆盖刚写入的 `pending`，也防止已删除主机被重新插回快照。
async fn publish_health_if_current(
    state: &AppState,
    probed_host: &SunshineHostConfig,
    snapshot: SunshineHostHealth,
) -> bool {
    let hosts = state.hosts.sunshine.read().await;
    let Some(current) = hosts.iter().find(|host| host.id == probed_host.id) else {
        return false;
    };
    if !same_host_configuration(current, probed_host) {
        return false;
    }

    // 保持 hosts 读锁直到健康快照写入完成。配置更新必须先拿 hosts 写锁，因此这里的
    // “确认仍是当前配置 + 发布结果”对更新操作而言是一个原子步骤。
    state
        .hosts
        .sunshine_health
        .write()
        .await
        .insert(probed_host.id.clone(), snapshot);
    true
}

fn same_host_configuration(left: &SunshineHostConfig, right: &SunshineHostConfig) -> bool {
    left.id == right.id
        && left.name == right.name
        && left.host == right.host
        && left.web_port == right.web_port
        && left.username == right.username
        && left.password == right.password
        && left.verify_tls == right.verify_tls
}

fn sunshine_host_service_status(
    host: &crate::config::SunshineHostConfig,
    reachable: bool,
) -> ServiceStatus {
    ServiceStatus {
        name: format!("sunshine:{}", host.name),
        kind: "streaming-host".to_string(),
        runtime_state: if reachable { "reachable" } else { "unknown" }.to_string(),
        healthy: reachable,
        address: Some(crate::sunshine::client::web_url(host)),
        pid: None,
        message: format!(
            "{} — {}",
            host.name,
            if reachable {
                "port reachable"
            } else {
                "unreachable"
            }
        ),
        updated_at: chrono::Utc::now().to_rfc3339(),
    }
}

pub fn sunshine_host_status(
    host: &crate::config::SunshineHostConfig,
    health: Option<&SunshineHostHealth>,
) -> SunshineStatus {
    let reachable = health.and_then(|value| value.reachable).unwrap_or(false);
    SunshineStatus {
        host: host.host.clone(),
        web_port: host.web_port,
        web_url: crate::sunshine::client::web_url(host),
        reachable,
        message: match health.and_then(|value| value.reachable) {
            Some(true) => "Sunshine Web UI port is reachable".to_string(),
            Some(false) => "Sunshine Web UI port is not reachable".to_string(),
            None => "Sunshine Web UI reachability check is pending".to_string(),
        },
    }
}

#[cfg(test)]
mod health_snapshot_tests {
    use super::*;
    use crate::{
        config::{LocalConfig, Settings},
        infra::database,
    };

    fn state_with_host(host: SunshineHostConfig) -> AppState {
        let mut settings = Settings::default();
        settings.database.url = ":memory:".to_string();
        settings.sunshine.hosts = vec![host];
        AppState::new(
            settings,
            database::in_memory_pool().expect("in-memory test pool"),
            "unused".into(),
            LocalConfig {
                application_version: env!("CARGO_PKG_VERSION").to_string(),
                admin_username: "admin".into(),
                admin_password_hash: "unused".into(),
            },
            crate::system::ResourceMonitor::frozen(Default::default()),
        )
        .expect("capture in-memory database identity")
    }

    #[tokio::test]
    async fn stale_probe_cannot_replace_pending_for_updated_configuration() {
        let original = SunshineHostConfig {
            id: "host-1".into(),
            host: "old.example.test".into(),
            password: "old-secret".into(),
            ..SunshineHostConfig::default()
        };
        let state = state_with_host(original.clone());

        {
            let mut hosts = state.hosts.sunshine.write().await;
            let mut health = state.hosts.sunshine_health.write().await;
            hosts[0].host = "new.example.test".into();
            hosts[0].password = "new-secret".into();
            health.insert(original.id.clone(), SunshineHostHealth::pending());
        }

        let old_success = SunshineHostHealth::completed(true, &Ok(()));
        assert!(!publish_health_if_current(&state, &original, old_success).await);
        let health = state.hosts.sunshine_health.read().await;
        let current = health.get(&original.id).expect("pending snapshot");
        assert_eq!(current.reachable, None);
        assert_eq!(current.connected, None);
    }

    #[tokio::test]
    async fn probe_for_deleted_host_is_discarded_instead_of_resurrected() {
        let host = SunshineHostConfig {
            id: "host-1".into(),
            ..SunshineHostConfig::default()
        };
        let state = state_with_host(host.clone());
        state.hosts.sunshine.write().await.clear();
        state.hosts.sunshine_health.write().await.remove(&host.id);

        let success = SunshineHostHealth::completed(true, &Ok(()));
        assert!(!publish_health_if_current(&state, &host, success).await);
        assert!(
            !state
                .hosts
                .sunshine_health
                .read()
                .await
                .contains_key(&host.id)
        );
    }

    #[tokio::test]
    async fn stale_service_batch_cannot_replace_the_current_snapshot() {
        let original = SunshineHostConfig {
            id: "host-1".into(),
            name: "old-name".into(),
            host: "old.example.test".into(),
            ..SunshineHostConfig::default()
        };
        let state = state_with_host(original.clone());
        let stale_status = sunshine_host_service_status(&original, true);
        let stale_batch = ServiceProbeBatch {
            hosts: vec![original],
            reachable: vec![true],
            statuses: vec![stale_status],
        };

        state.hosts.sunshine.write().await[0].host = "new.example.test".into();

        assert!(!publish_service_batch_if_current(&state, &stale_batch).await);
        assert!(state.services.snapshot.read().await.is_empty());
    }

    #[tokio::test]
    async fn reachability_probes_fill_the_concurrency_window_without_real_network() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let hosts = (0..MAX_CONCURRENT_PROBES)
            .map(|index| SunshineHostConfig {
                id: format!("host-{index}"),
                ..SunshineHostConfig::default()
            })
            .collect();
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let observed_active = Arc::clone(&active);
        let observed_maximum = Arc::clone(&maximum);

        let results = probe_hosts_with(hosts, move |host| {
            let active = Arc::clone(&observed_active);
            let maximum = Arc::clone(&observed_maximum);
            async move {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(current, Ordering::SeqCst);
                // Yield once while counted as active. A serial implementation
                // can only reach one; buffered probing polls the whole window.
                tokio::task::yield_now().await;
                active.fetch_sub(1, Ordering::SeqCst);
                (host, false)
            }
        })
        .await;

        assert_eq!(results.len(), MAX_CONCURRENT_PROBES);
        assert_eq!(maximum.load(Ordering::SeqCst), MAX_CONCURRENT_PROBES);
    }

    #[tokio::test]
    async fn unreachable_tcp_batch_completes_health_without_a_second_network_probe() {
        let host = SunshineHostConfig {
            id: "host-1".into(),
            host: "this-name-must-never-be-resolved.invalid".into(),
            ..SunshineHostConfig::default()
        };
        let state = state_with_host(host.clone());
        let batch = service_probe_batch(vec![(host.clone(), false)]);

        probe_and_publish_health(&state, batch).await;

        let health = state.hosts.sunshine_health.read().await;
        let snapshot = health.get(&host.id).expect("completed health snapshot");
        assert_eq!(snapshot.reachable, Some(false));
        assert_eq!(snapshot.connected, Some(false));
        assert_eq!(
            snapshot.connection_error.as_deref(),
            Some("Sunshine Web 端口不可达")
        );
    }
}
