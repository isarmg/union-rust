//! Agent 端点的限流。
//!
//! 限流最大的风险是**误伤合法流量**，因此这里既验证"超额被拦"，也验证
//! "合法配置与断线补传不受影响"。

use std::time::{Duration, Instant};

use unionc::{
    config::{LocalConfig, Settings},
    infra::database,
    state::{AppState, TokenBucket},
};
use uuid::Uuid;

// ─── 限流常量的护栏（编译期断言）────────────────────────────────────────────
//
// 这两条约束是"不能误伤合法流量"的底线，用编译期断言表达：一旦有人把常量调到
// 危险区间，构建直接失败，而不是等到线上出现莫名其妙的 429。

/// `AgentReport::validate()` 允许的最小上报间隔是 0.1 秒，即 10 次/秒。
/// 补充速率必须高于它，否则配置合法的 Agent 会被自家限流挡住。
const _: () = assert!(TokenBucket::REFILL_PER_SECOND > 10.0);

/// Agent 断线恢复时一轮最多补传 32 个批次
/// （见 agent/src/agent_app/delivery/spool.rs 的 flush_spool）。
/// 桶容量小于这个数，恢复过程就会被限流打断。
const _: () = assert!(TokenBucket::CAPACITY >= 32.0);

fn state() -> AppState {
    AppState::new(
        Settings::default(),
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

// ─── 上报令牌桶 ──────────────────────────────────────────────────────────────

/// 合法配置永远不应被限流。
///
/// `AgentReport::validate()` 允许的最小间隔是 0.1 秒，即 10 次/秒；令牌桶的补充
/// 速率必须高于这个上限，否则一个配置合法的 Agent 会被自己人挡在门外。
#[tokio::test]
async fn the_fastest_legitimate_reporting_rate_is_never_throttled() {
    let state = state();
    let host = Uuid::new_v4().to_string();
    // 以最快合法速率持续上报 30 秒（用虚拟时间推进，不真的等待）。
    let start = Instant::now();
    for tick in 0..300 {
        let now = start + Duration::from_millis(tick * 100);
        assert!(
            state.agents.allow_report(&host, now).await,
            "第 {tick} 次上报被限流——最快合法速率不应触发限流"
        );
    }
}

/// 断线恢复时的批量补传（一轮最多 32 个批次）必须能一次通过。
#[tokio::test]
async fn a_spool_recovery_burst_passes_in_one_round() {
    let state = state();
    let host = Uuid::new_v4().to_string();
    let now = Instant::now();
    for index in 0..32 {
        assert!(
            state.agents.allow_report(&host, now).await,
            "补传第 {index} 个批次被限流"
        );
    }
}

/// 超出配额的滥用速率必须被拦下。
#[tokio::test]
async fn an_abusive_burst_is_throttled_once_the_bucket_empties() {
    let state = state();
    let host = Uuid::new_v4().to_string();
    let now = Instant::now();

    // 同一瞬间狂发：先耗尽容量，随后必被拒绝。
    let mut allowed = 0;
    for _ in 0..1000 {
        if state.agents.allow_report(&host, now).await {
            allowed += 1;
        }
    }
    assert_eq!(
        allowed,
        TokenBucket::CAPACITY as usize,
        "瞬时放行量应恰为桶容量"
    );
    assert!(
        !state.agents.allow_report(&host, now).await,
        "桶耗尽后必须继续拒绝"
    );
}

/// 一台主机被限流不应影响其他主机。
#[tokio::test]
async fn throttling_one_host_does_not_affect_others() {
    let state = state();
    let noisy = Uuid::new_v4().to_string();
    let quiet = Uuid::new_v4().to_string();
    let now = Instant::now();

    while state.agents.allow_report(&noisy, now).await {}

    assert!(
        state.agents.allow_report(&quiet, now).await,
        "一台主机耗尽配额后，其他主机被连带限流了"
    );
}

/// 桶应随时间回填，短暂超速不会造成长期封禁。
#[tokio::test]
async fn the_bucket_refills_over_time() {
    let state = state();
    let host = Uuid::new_v4().to_string();
    let now = Instant::now();

    while state.agents.allow_report(&host, now).await {}
    assert!(!state.agents.allow_report(&host, now).await);

    // 一秒后应至少回填 REFILL_PER_SECOND 个令牌。
    let later = now + Duration::from_secs(1);
    let mut refilled = 0;
    while state.agents.allow_report(&host, later).await {
        refilled += 1;
    }
    assert!(
        refilled >= TokenBucket::REFILL_PER_SECOND as usize - 1,
        "一秒后仅回填 {refilled} 个令牌，低于预期的 {}",
        TokenBucket::REFILL_PER_SECOND
    );
}
