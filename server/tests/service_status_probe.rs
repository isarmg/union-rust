//! 服务状态探测的读写分离。
//!
//! 回归目标：
//! 1. `all_services`（HTTP `/api/services` 与 SSE 两条读路径共用）过去会**自己发起
//!    探测**，导致每个 SSE 连接各跑一遍，对被监控主机的压力随客户端数放大；
//! 2. 并发上限和快/慢探测解耦在 `sunshine::status` 的单元测试中用可控
//!    future 覆盖，不依赖公网路由、TCP 超时或 CI 机器的墙钟负载。

use unionc::{
    config::{LocalConfig, Settings, SunshineHostConfig},
    infra::database,
    state::AppState,
    sunshine::status::all_services,
    system::ServiceStatus,
};

fn state_with_configured_hosts(count: usize) -> AppState {
    let mut settings = Settings::default();
    settings.sunshine.hosts = (0..count)
        .map(|index| SunshineHostConfig {
            id: format!("host-{index}"),
            name: format!("configured-{index}"),
            host: format!("host-{index}.invalid"),
            web_port: 47990,
            ..SunshineHostConfig::default()
        })
        .collect();

    AppState::new(
        settings,
        database::in_memory_pool().expect("in-memory test pool"),
        "unused".into(),
        LocalConfig {
            application_version: env!("CARGO_PKG_VERSION").to_string(),
            admin_username: "admin".into(),
            admin_password_hash: "unused".into(),
        },
        unionc::system::ResourceMonitor::frozen(Default::default()),
    )
}

/// 读路径必须只消费快照，不得触发任何网络探测。
///
/// 这是消除"客户端数 × 主机数"放大的关键：只要读路径自己会探测，
/// 每个 SSE 连接就会各自产生一轮压力。
///
/// 预先写入一条与主机配置无关的哨兵快照；读取结果必须原样返回它。
/// 若读路径偷偷发起探测，返回值会变成按主机配置生成的状态。
#[tokio::test]
async fn reading_statuses_never_probes_the_network() {
    let state = state_with_configured_hosts(8);
    let sentinel = ServiceStatus {
        name: "cached-sentinel".into(),
        kind: "test".into(),
        runtime_state: "cached".into(),
        healthy: true,
        address: None,
        pid: None,
        message: "must be returned verbatim".into(),
        updated_at: "fixed-test-time".into(),
    };
    *state.services.snapshot.write().await = vec![sentinel];

    let statuses = all_services(&state).await;

    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].name, "cached-sentinel");
    assert_eq!(statuses[0].updated_at, "fixed-test-time");
}
