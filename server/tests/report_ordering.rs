//! 乱序 / 重放报文不得回写主机的当前状态。
//!
//! 断线恢复时 spool 会补传一批**历史**报文，重放也可能把同一份报文再送一次。
//! 无条件覆盖 `os` / `capabilities` / `last_seen_at` 会导致：
//!
//! * 一份小时前的旧报文能把刚更新的能力清单覆盖回去；
//! * 任何一次重放都会把 `last_seen_at` 刷成当前时间，让离线主机显示为 online。
//!
//! 报文本身仍要入库（历史曲线需要它），只是不该回写"当前状态"。

use chrono::{Duration, Utc};
use sqlx_core::query::query;
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

/// `capability` 用于区分"新报文"与"旧报文"写入的元数据。
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

#[tokio::test]
async fn a_late_arriving_old_report_is_stored_without_rewriting_host_state() {
    let url = common::test_database_url(
        "a_late_arriving_old_report_is_stored_without_rewriting_host_state",
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

    let now = Utc::now();

    // ── 1. 当前报文：确立主机的当前状态 ────────────────────────────────────
    let current: unionc::monitoring::AgentReport =
        serde_json::from_value(report(host_id, now, "capability.current")).expect("valid report");
    unionc::monitoring::store::store_monitoring_report(&pool, &current)
        .await
        .expect("store current");

    let after_current = unionc::monitoring::store::get_monitored_host(&pool, &host_id.to_string())
        .await
        .expect("read")
        .expect("host exists");
    assert_eq!(after_current.name, "测试实例");
    let last_seen_after_current = after_current.last_seen_at;
    let latest_collected_after_current = after_current.latest_collected_at;

    // ── 2. 补传一份一小时前的旧报文 ────────────────────────────────────────
    let stale: unionc::monitoring::AgentReport = serde_json::from_value(report(
        host_id,
        now - Duration::hours(1),
        "capability.stale",
    ))
    .expect("valid report");
    let (accepted, _) = unionc::monitoring::store::store_monitoring_report(&pool, &stale)
        .await
        .expect("store stale");
    assert!(accepted, "旧报文本身仍应入库——历史曲线需要它");

    // ── 3. 主机的"当前状态"必须纹丝不动 ────────────────────────────────────
    let after_stale = unionc::monitoring::store::get_monitored_host(&pool, &host_id.to_string())
        .await
        .expect("read")
        .expect("host exists");
    assert_eq!(after_stale.name, "测试实例", "旧报文不得覆盖 Server 名称");
    assert_eq!(
        after_stale
            .capabilities
            .first()
            .map(|capability| capability.name.as_str()),
        Some("capability.current"),
        "旧报文不得把能力清单覆盖回旧版本"
    );
    assert_eq!(
        after_stale.last_seen_at, last_seen_after_current,
        "重放旧报文不得刷新 last_seen_at——否则离线主机会显示为 online"
    );
    assert_eq!(
        after_stale.latest_collected_at, latest_collected_after_current,
        "latest_collected_at 必须仍指向更新的那份报文"
    );

    // ── 4. 但旧报文确实进了历史 ────────────────────────────────────────────
    let history =
        unionc::monitoring::store::monitoring_history(&pool, &host_id.to_string(), None, None, 100)
            .await
            .expect("history")
            .expect("host exists");
    assert_eq!(history.len(), 2, "两份报文都应出现在历史中");
    assert!(
        history[0].collected_at < history[1].collected_at,
        "历史应按时间升序返回"
    );

    // ── 5. 更新的报文仍然可以正常推进状态 ──────────────────────────────────
    let newer: unionc::monitoring::AgentReport = serde_json::from_value(report(
        host_id,
        now + Duration::seconds(10),
        "capability.newer",
    ))
    .expect("valid report");
    unionc::monitoring::store::store_monitoring_report(&pool, &newer)
        .await
        .expect("store newer");
    let after_newer = unionc::monitoring::store::get_monitored_host(&pool, &host_id.to_string())
        .await
        .expect("read")
        .expect("host exists");
    assert_eq!(
        after_newer.name, "测试实例",
        "更新的报文也不得覆盖 Server 名称"
    );
    assert!(
        after_newer.last_seen_at >= last_seen_after_current,
        "更新的报文应刷新 last_seen_at"
    );

    query("DELETE FROM monitored_hosts WHERE host_id=?1")
        .bind(host_id.to_string())
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn latest_report_never_moves_last_seen_backwards() {
    let url = common::test_database_url("latest_report_never_moves_last_seen_backwards");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let host_id = Uuid::new_v4();
    common::insert_active_monitoring_host(
        &pool,
        &identity(host_id),
        &hex_hash("monotonic-last-seen-token"),
    )
    .await
    .expect("register");

    // Simulate either a clock correction or an earlier task that captured its timestamp before
    // waiting for the write lock. The new report must still advance latest_* without lowering
    // the already-observed liveness timestamp.
    let future_last_seen = database::to_epoch_micros(Utc::now() + Duration::days(1));
    query("UPDATE monitored_hosts SET last_seen_at=?2 WHERE host_id=?1")
        .bind(host_id.to_string())
        .bind(future_last_seen)
        .execute(&pool)
        .await
        .expect("move liveness marker forward");

    let current: unionc::monitoring::AgentReport =
        serde_json::from_value(report(host_id, Utc::now(), "capability.monotonic"))
            .expect("valid report");
    let report_id = current.report_id.clone();
    let (accepted, received_at) =
        unionc::monitoring::store::store_monitoring_report(&pool, &current)
            .await
            .expect("store current report");
    assert!(accepted);
    assert!(database::to_epoch_micros(received_at) < future_last_seen);

    let row = query("SELECT last_seen_at,latest_report_id FROM monitored_hosts WHERE host_id=?1")
        .bind(host_id.to_string())
        .fetch_one(&pool)
        .await
        .expect("read host state");
    assert_eq!(
        sqlx_core::row::Row::try_get::<i64, _>(&row, "last_seen_at").unwrap(),
        future_last_seen,
        "committing a latest report must not move liveness backwards"
    );
    assert_eq!(
        sqlx_core::row::Row::try_get::<String, _>(&row, "latest_report_id").unwrap(),
        report_id,
        "the monotonic guard must not block latest report advancement"
    );

    query("DELETE FROM monitored_hosts WHERE host_id=?1")
        .bind(host_id.to_string())
        .execute(&pool)
        .await
        .expect("cleanup");
}

/// Equal collection timestamps use the same `report_id DESC` tie-break as the
/// history query. Arrival order must not change which report represents the
/// host's current state.
#[tokio::test]
async fn equal_timestamps_choose_the_same_latest_report_in_both_arrival_orders() {
    let url = common::test_database_url("equal_timestamp_report_ordering");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let collected_at = Utc::now();

    for (high_arrives_first, lower_id, higher_id) in [
        (
            false,
            "00000000-0000-4000-8000-000000000001",
            "ffffffff-ffff-4fff-bfff-fffffffffff1",
        ),
        (
            true,
            "00000000-0000-4000-8000-000000000002",
            "ffffffff-ffff-4fff-bfff-fffffffffff2",
        ),
    ] {
        let host_id = Uuid::new_v4();
        let marker = Uuid::new_v4();
        common::insert_active_monitoring_host(
            &pool,
            &identity(host_id),
            &hex_hash(&format!("{marker}-token")),
        )
        .await
        .expect("register");

        let mut lower: unionc::monitoring::AgentReport =
            serde_json::from_value(report(host_id, collected_at, "capability.lower"))
                .expect("valid lower report");
        lower.report_id = lower_id.to_string();
        let mut higher: unionc::monitoring::AgentReport =
            serde_json::from_value(report(host_id, collected_at, "capability.higher"))
                .expect("valid higher report");
        higher.report_id = higher_id.to_string();

        let reports = if high_arrives_first {
            [&higher, &lower]
        } else {
            [&lower, &higher]
        };
        for report in reports {
            let (accepted, _) = unionc::monitoring::store::store_monitoring_report(&pool, report)
                .await
                .expect("store tied report");
            assert!(accepted);
        }

        let detail = unionc::monitoring::store::get_monitored_host(&pool, &host_id.to_string())
            .await
            .expect("detail query")
            .expect("host exists");
        assert_eq!(detail.name, "测试实例");
        assert_eq!(
            detail
                .latest
                .as_ref()
                .map(|report| report.report_id.as_str()),
            Some(higher_id),
            "detail latest must use the history tie-break regardless of arrival order"
        );

        let history = unionc::monitoring::store::monitoring_history(
            &pool,
            &host_id.to_string(),
            None,
            None,
            100,
        )
        .await
        .expect("history query")
        .expect("host exists");
        assert_eq!(history.len(), 2);
        assert_eq!(
            history.last().map(|point| point.report_id.as_str()),
            Some(higher_id),
            "the final history point and detail latest must agree"
        );
    }
}

/// 报文体只为每台主机的**最新一份**保留。
///
/// `payload` 在代码里只有一个读取点（详情接口经 latest_report_id 主键 JOIN），
/// 若为每一份历史报文都留一份完整 JSON：100 台主机 / 10 秒周期 / 30 天保留
/// 约 420 GB，其中会被读到的只有 100 份。这里锁住"写入即决策、接替即释放"的不变量。
#[tokio::test]
async fn only_the_latest_report_keeps_its_payload() {
    let url = common::test_database_url("only_the_latest_report_keeps_its_payload");
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

    // 统计该主机名下 payload 非空的报文数。
    let non_null_payloads = |pool: database::DbPool| async move {
        query(
            "SELECT count(*) AS total FROM agent_metric_reports \
             WHERE host_id=?1 AND payload IS NOT NULL",
        )
        .bind(host_id.to_string())
        .fetch_one(&pool)
        .await
        .map(|row| sqlx_core::row::Row::try_get::<i64, _>(&row, "total").unwrap())
        .expect("count payloads")
    };

    let base = Utc::now();

    // ── 连续 5 份递增报文：任何时刻都只应有一份 payload ────────────────────
    for step in 0..5 {
        let report: unionc::monitoring::AgentReport = serde_json::from_value(report(
            host_id,
            base + Duration::seconds(step * 10),
            "capability.only",
        ))
        .expect("valid report");
        unionc::monitoring::store::store_monitoring_report(&pool, &report)
            .await
            .expect("store");
        assert_eq!(
            non_null_payloads(pool.clone()).await,
            1,
            "第 {} 份报文写入后，非空 payload 应恒为 1 份",
            step + 1
        );
    }

    // 历史点一个不少——摘要列不受影响。
    let history =
        unionc::monitoring::store::monitoring_history(&pool, &host_id.to_string(), None, None, 100)
            .await
            .expect("history")
            .expect("host exists");
    assert_eq!(history.len(), 5, "释放报文体不得影响历史曲线");
    assert!(
        history
            .iter()
            .all(|point| point.metrics.cpu_usage_percent.is_some()),
        "历史点的摘要指标必须仍然可用"
    );

    // 详情接口仍能拿到完整报文体。
    let detail = unionc::monitoring::store::get_monitored_host(&pool, &host_id.to_string())
        .await
        .expect("detail")
        .expect("host exists");
    assert!(
        detail.latest.is_some(),
        "详情接口必须仍能取到最新报文的完整报文体"
    );
    assert_eq!(
        detail.latest.as_ref().map(|latest| latest.collected_at),
        Some(base + Duration::seconds(40)),
        "详情返回的应当是最新那一份"
    );

    // ── 补传的历史报文自始至终不占报文体 ──────────────────────────────────
    let stale: unionc::monitoring::AgentReport = serde_json::from_value(report(
        host_id,
        base - Duration::hours(1),
        "capability.stale",
    ))
    .expect("valid report");
    unionc::monitoring::store::store_monitoring_report(&pool, &stale)
        .await
        .expect("store stale");
    assert_eq!(
        non_null_payloads(pool.clone()).await,
        1,
        "补传的历史报文不应带上报文体——它永远不会被读取"
    );
    assert!(
        unionc::monitoring::store::get_monitored_host(&pool, &host_id.to_string())
            .await
            .expect("detail")
            .expect("host exists")
            .latest
            .is_some(),
        "补传不得影响详情接口"
    );

    query("DELETE FROM monitored_hosts WHERE host_id=?1")
        .bind(host_id.to_string())
        .execute(&pool)
        .await
        .expect("cleanup");
}

/// 重放**同一份**报文（相同 report_id）应当被幂等地忽略，同样不刷新 last_seen_at。
#[tokio::test]
async fn replaying_the_same_report_is_idempotent() {
    let url = common::test_database_url("replaying_the_same_report_is_idempotent");
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

    let value = report(host_id, Utc::now(), "capability.only");
    let parsed: unionc::monitoring::AgentReport =
        serde_json::from_value(value.clone()).expect("valid report");

    let (first_accepted, first_received) =
        unionc::monitoring::store::store_monitoring_report(&pool, &parsed)
            .await
            .expect("first store");
    assert!(first_accepted, "首次投递应被接受");

    let before = query(
        "SELECT name,last_seen_at,capabilities \
         FROM monitored_hosts WHERE host_id=?1",
    )
    .bind(host_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("host before replay");
    let before_name: String = sqlx_core::row::Row::try_get(&before, "name").unwrap();
    let before_last_seen: i64 = sqlx_core::row::Row::try_get(&before, "last_seen_at").unwrap();
    let before_capabilities: String =
        sqlx_core::row::Row::try_get(&before, "capabilities").unwrap();

    // Reusing the same id with a different body is still a duplicate and must
    // not mutate host identity, capabilities, Server remark or heartbeat state.
    let mut replay = parsed.clone();
    replay.host.os = "replay-must-not-overwrite".to_string();
    replay.capabilities.clear();
    replay.collected_at += Duration::minutes(1);

    let (second_accepted, second_received) =
        unionc::monitoring::store::store_monitoring_report(&pool, &replay)
            .await
            .expect("replay");
    assert!(!second_accepted, "重复的 report_id 必须被识别为重放");
    assert_eq!(
        first_received, second_received,
        "重放应返回首次入库的 received_at，而不是一个新的时间戳"
    );

    let history =
        unionc::monitoring::store::monitoring_history(&pool, &host_id.to_string(), None, None, 100)
            .await
            .expect("history")
            .expect("host exists");
    assert_eq!(history.len(), 1, "重放不得在历史里留下第二个点");

    let after = query(
        "SELECT name,last_seen_at,capabilities \
         FROM monitored_hosts WHERE host_id=?1",
    )
    .bind(host_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("host after replay");
    assert_eq!(
        sqlx_core::row::Row::try_get::<String, _>(&after, "name").unwrap(),
        before_name
    );
    assert_eq!(
        sqlx_core::row::Row::try_get::<i64, _>(&after, "last_seen_at").unwrap(),
        before_last_seen
    );
    assert_eq!(
        sqlx_core::row::Row::try_get::<String, _>(&after, "capabilities").unwrap(),
        before_capabilities
    );

    query("DELETE FROM monitored_hosts WHERE host_id=?1")
        .bind(host_id.to_string())
        .execute(&pool)
        .await
        .expect("cleanup");
}

/// SQLite has one writer per database file. Two HTTP tasks may still enter the
/// persistence API concurrently, so the write gate and `BEGIN IMMEDIATE` must
/// preserve report idempotency and cross-host conflict semantics.
#[tokio::test]
async fn concurrent_duplicate_reports_are_serialized_without_hiding_cross_host_conflicts() {
    let url = common::test_database_url("concurrent_report_idempotency");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let first_host = Uuid::new_v4();
    let second_host = Uuid::new_v4();
    let marker = Uuid::new_v4();
    for host_id in [first_host, second_host] {
        common::insert_active_monitoring_host(
            &pool,
            &identity(host_id),
            &hex_hash(&format!("{marker}-{host_id}-token")),
        )
        .await
        .expect("register");
    }

    let first: unionc::monitoring::AgentReport =
        serde_json::from_value(report(first_host, Utc::now(), "capability.concurrent"))
            .expect("valid report");
    let report_id = first.report_id.clone();
    let (left, right) = tokio::join!(
        unionc::monitoring::store::store_monitoring_report(&pool, &first),
        unionc::monitoring::store::store_monitoring_report(&pool, &first),
    );
    let left = left.expect("left store");
    let right = right.expect("right store");
    assert_eq!(usize::from(left.0) + usize::from(right.0), 1);
    assert_eq!(left.1, right.1, "duplicate ACK must reuse received_at");

    let mut collision: unionc::monitoring::AgentReport =
        serde_json::from_value(report(second_host, Utc::now(), "capability.concurrent"))
            .expect("valid collision report");
    collision.report_id = report_id;
    let error = unionc::monitoring::store::store_monitoring_report(&pool, &collision)
        .await
        .expect_err("another host must not claim an existing report id");
    assert!(matches!(
        error.downcast_ref::<unionc::monitoring::store::StoreReportError>(),
        Some(unionc::monitoring::store::StoreReportError::ReportIdBelongsToAnotherHost)
    ));

    for host_id in [first_host, second_host] {
        query("DELETE FROM monitored_hosts WHERE host_id=?1")
            .bind(host_id.to_string())
            .execute(&pool)
            .await
            .expect("cleanup");
    }
}
