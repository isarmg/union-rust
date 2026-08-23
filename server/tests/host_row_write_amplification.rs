//! 上报路径对 `monitored_hosts` 的写放大。
//!
//! # 为什么值得单独守一条
//!
//! 上报是全系统频率最高的写路径，而 `monitored_hosts` 是一张**很小**的表
//! （行数 = 主机数）。二者相乘就是问题所在：500 台 / 10 秒 = 50 次/秒 打在
//! 500 行上，任何一点每报文的额外写入都会放大 SQLite 单写者的持锁时间
//! 与 WAL 写入量。
//!
//! 每份报文对该行做**两次** UPDATE（INSERT 前写 identity，INSERT 后写
//! latest_* 指针），且无条件重写 `capabilities` JSON——即使它一个字节
//! 都没变。前者会延长单写事务，后者还会制造无效的 WAL 与页面改写。
//!
//! 这类退化不会让功能测试失败。测试用 SQLite trigger 对目标表的实际
//! UPDATE 语句同步计数，直接把每份报文仅更新一次的契约钉死。

use chrono::Utc;
use sqlx_core::{query::query, row::Row};
use unionc::monitoring::HostIdentity;
use unionc::{config::Settings, infra::database};
use uuid::Uuid;

mod common;

fn hex_hash(seed: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(seed.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn identity(host_id: Uuid) -> HostIdentity {
    HostIdentity {
        id: host_id.to_string(),
        os: "linux".into(),
        os_version: Some("6.1.0".into()),
        kernel_version: Some("6.1.0".into()),
        arch: "x86_64".into(),
        agent_version: "0.3.4".into(),
    }
}

/// 造一份报文。`capability` 变化即代表 identity 侧发生了真实变更。
fn report(
    host_id: Uuid,
    collected_at: chrono::DateTime<Utc>,
    capability: &str,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "report_id": Uuid::new_v4(),
        "collected_at": collected_at,
        "host": {
            "id": host_id, "os": "linux",
            "os_version": "6.1.0", "kernel_version": "6.1.0",
            "arch": "x86_64", "agent_version": "0.3.4"
        },
        "interval_seconds": 10.0,
        "system": {
            "uptime_seconds": 60,
            "cpu": {"usage_percent": 10.0, "logical_count": 4, "physical_count": 2,
                    "per_core_percent": [10.0, 10.0, 10.0, 10.0]},
            "memory": {"total_bytes": 1000, "used_bytes": 500, "available_bytes": 500,
                       "swap_total_bytes": 0, "swap_used_bytes": 0},
            "networks": [], "disks": [], "temperatures": [], "gpus": []
        },
        "capabilities": [{
            "name": capability, "available": true,
            "source": "test", "error_kind": null, "message": null
        }],
        "agent": {"spool_pending_batches": 0, "collector_errors": 0}
    })
}

async fn read_updates(pool: &database::DbPool) -> i64 {
    query("SELECT updates FROM _test_host_update_probe")
        .fetch_one(pool)
        .await
        .expect("read update probe")
        .try_get("updates")
        .expect("updates column")
}

/// 稳态上报（identity 不变）每份报文只能产生**一次** UPDATE。
#[tokio::test]
async fn a_steady_state_report_updates_the_host_row_exactly_once() {
    let url = common::test_database_url("a_steady_state_report_updates_the_host_row_exactly_once");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let host_id = Uuid::new_v4();
    let marker = Uuid::new_v4();
    common::insert_active_monitoring_host(
        &pool,
        &identity(host_id),
        &hex_hash(&format!("{marker}-token")),
    )
    .await
    .expect("register");

    // 第一份报文会真正写入 identity（注册时的值与报文里的 capabilities 不同），
    // 因此从**第二份**开始才是稳态。
    let base = Utc::now();
    let first: unionc::monitoring::AgentReport =
        serde_json::from_value(report(host_id, base, "capability.stable")).expect("valid");
    unionc::monitoring::store::store_monitoring_report(&pool, &first)
        .await
        .expect("store first");

    // 每个测试都使用独立 SQLite 文件，因此可安全安装一个仅用于本用例的
    // 持久 trigger。它在同一写事务内同步增计，不存在统计延迟或连接快照。
    query("CREATE TABLE _test_host_update_probe(updates INTEGER NOT NULL)")
        .execute(&pool)
        .await
        .expect("create update probe");
    query("INSERT INTO _test_host_update_probe(updates) VALUES(0)")
        .execute(&pool)
        .await
        .expect("initialize update probe");
    query(
        "CREATE TRIGGER _test_count_monitored_host_updates \
         AFTER UPDATE ON monitored_hosts \
         WHEN NEW.host_id = OLD.host_id \
         BEGIN UPDATE _test_host_update_probe SET updates=updates+1; END",
    )
    .execute(&pool)
    .await
    .expect("create update trigger");

    const REPORTS: i64 = 20;
    let before = read_updates(&pool).await;
    for index in 1..=REPORTS {
        let next: unionc::monitoring::AgentReport = serde_json::from_value(report(
            host_id,
            base + chrono::Duration::seconds(index * 10),
            // 完全相同的 capability —— 稳态。
            "capability.stable",
        ))
        .expect("valid");
        unionc::monitoring::store::store_monitoring_report(&pool, &next)
            .await
            .expect("store");
    }
    let after = read_updates(&pool).await;
    let per_report = (after - before) as f64 / REPORTS as f64;

    eprintln!(
        "稳态 {REPORTS} 份报文：trigger 计数 {before} → {after}，共 {} 次 UPDATE（{per_report:.2} 次/报文）",
        after - before
    );
    // 断言**精确等于**而非"不超过"：读漏时差值会是 0，若写成 `<=` 就会
    // 被当成"写得更少"而通过。上界与下界都必须钉死。
    assert_eq!(
        after - before,
        REPORTS,
        "每份报文对 monitored_hosts 应恰好一次 UPDATE。实测 {REPORTS} 份报文产生了 {} 次\
         （trigger {before} → {after}）。合并前是两次——INSERT 前写 identity、\
         INSERT 后写 latest_*——会让这张小表的行版本翻倍。",
        after - before
    );
}

/// identity 真的变化时必须写进去——省写入不能省掉正确性。
#[tokio::test]
async fn a_changed_capability_set_is_still_persisted() {
    let url = common::test_database_url("a_changed_capability_set_is_still_persisted");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let host_id = Uuid::new_v4();
    let marker = Uuid::new_v4();
    common::insert_active_monitoring_host(
        &pool,
        &identity(host_id),
        &hex_hash(&format!("{marker}-token")),
    )
    .await
    .expect("register");

    let base = Utc::now();
    for (offset, capability) in [(0, "capability.before"), (10, "capability.after")] {
        let value: unionc::monitoring::AgentReport = serde_json::from_value(report(
            host_id,
            base + chrono::Duration::seconds(offset),
            capability,
        ))
        .expect("valid");
        unionc::monitoring::store::store_monitoring_report(&pool, &value)
            .await
            .expect("store");
    }

    let stored = unionc::monitoring::store::get_monitored_host(&pool, &host_id.to_string())
        .await
        .expect("read")
        .expect("host exists");
    assert_eq!(
        stored.capabilities.first().map(|c| c.name.as_str()),
        Some("capability.after"),
        "capabilities 变化后必须落库——跳过写入的前提是**值没变**，而不是懒得写"
    );
}

/// 稳态上报仍必须推进 `last_seen_at` 与 `latest_*` 指针。
///
/// 这是上一条优化的边界：跳过的只能是 identity 列，"这台机器还活着"和
/// "最新报文是哪一份"两件事每份报文都要更新，否则主机会被判成离线。
#[tokio::test]
async fn skipping_identity_writes_still_advances_liveness_and_latest_pointer() {
    let url = common::test_database_url(
        "skipping_identity_writes_still_advances_liveness_and_latest_pointer",
    );
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let host_id = Uuid::new_v4();
    let marker = Uuid::new_v4();
    common::insert_active_monitoring_host(
        &pool,
        &identity(host_id),
        &hex_hash(&format!("{marker}-token")),
    )
    .await
    .expect("register");

    let base = Utc::now();
    let first: unionc::monitoring::AgentReport =
        serde_json::from_value(report(host_id, base, "capability.stable")).expect("valid");
    unionc::monitoring::store::store_monitoring_report(&pool, &first)
        .await
        .expect("store first");
    let after_first = unionc::monitoring::store::get_monitored_host(&pool, &host_id.to_string())
        .await
        .expect("read")
        .expect("host exists");

    // 第二份：identity 完全相同，只有时间推进。
    let second: unionc::monitoring::AgentReport = serde_json::from_value(report(
        host_id,
        base + chrono::Duration::seconds(30),
        "capability.stable",
    ))
    .expect("valid");
    unionc::monitoring::store::store_monitoring_report(&pool, &second)
        .await
        .expect("store second");
    let after_second = unionc::monitoring::store::get_monitored_host(&pool, &host_id.to_string())
        .await
        .expect("read")
        .expect("host exists");

    assert!(
        after_second.last_seen_at >= after_first.last_seen_at,
        "identity 没变也必须刷新 last_seen_at，否则主机会被误判为离线"
    );
    assert!(
        after_second.latest_collected_at > after_first.latest_collected_at,
        "latest_* 指针必须随新报文推进：{:?} → {:?}",
        after_first.latest_collected_at,
        after_second.latest_collected_at
    );
}
