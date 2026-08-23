//! 历史查询的三种结果与单次往返。
//!
//! 「先 `get_monitored_host` 判存在，再查历史」是两次往返，而且存在性检查会
//! 连带把完整的 `latest_report` JSON 读出来——仅仅为了判断一行是否存在。
//! 现在合并为一条“限量 CTE + LEFT JOIN”查询，需要区分三种结果：
//!   * 主机不存在        → `None`（上层返回 404）
//!   * 主机存在但无历史  → `Some(空列表)`
//!   * 主机存在且有历史  → `Some(数据点)`

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

async fn register(pool: &database::DbPool, host_id: Uuid) {
    common::insert_active_monitoring_host(
        pool,
        &HostIdentity {
            id: host_id.to_string(),
            os: "linux".into(),
            os_version: None,
            kernel_version: None,
            arch: "x86_64".into(),
            agent_version: "0.3.4".into(),
        },
        &hex_hash(&format!("{host_id}-token")),
    )
    .await
    .expect("register host");
}

fn report(host_id: Uuid, collected_at: chrono::DateTime<Utc>, cpu: f64) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "report_id": Uuid::new_v4(),
        "collected_at": collected_at,
        "host": {
            "id": host_id, "os": "linux",
            "os_version": null, "kernel_version": null,
            "arch": "x86_64", "agent_version": "0.3.4"
        },
        "interval_seconds": 10.0,
        "system": {
            "uptime_seconds": 1,
            "cpu": { "usage_percent": cpu, "logical_count": 2,
                     "physical_count": 1, "per_core_percent": [cpu, cpu] },
            "memory": { "total_bytes": 1000, "used_bytes": 500, "available_bytes": 500,
                        "swap_total_bytes": 0, "swap_used_bytes": 0 },
            "networks": [], "disks": [], "temperatures": [], "gpus": []
        },
        "capabilities": [],
        "agent": { "spool_pending_batches": 0, "collector_errors": 0 }
    })
}

#[tokio::test]
async fn history_distinguishes_missing_host_from_empty_history() {
    let url = common::test_database_url("history_distinguishes_missing_host");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    // ── 1. 主机不存在 → None ──────────────────────────────────────────────
    let missing = Uuid::new_v4();
    assert!(
        unionc::monitoring::store::monitoring_history(&pool, &missing.to_string(), None, None, 100)
            .await
            .expect("query should succeed")
            .is_none(),
        "不存在的主机应返回 None，供上层区分 404"
    );

    // ── 2. 主机存在但无历史 → Some(空列表) ────────────────────────────────
    let empty_host = Uuid::new_v4();
    register(&pool, empty_host).await;
    let points = unionc::monitoring::store::monitoring_history(
        &pool,
        &empty_host.to_string(),
        None,
        None,
        100,
    )
    .await
    .expect("query should succeed")
    .expect("已注册的主机不应被当作不存在");
    assert!(
        points.is_empty(),
        "刚注册、尚无上报的主机应返回空列表而非 404，实际有 {} 个点",
        points.len()
    );

    // ── 3. 主机存在且有历史 → Some(按时间升序的数据点) ────────────────────
    let host = Uuid::new_v4();
    register(&pool, host).await;
    for index in 0..5 {
        let parsed = serde_json::from_value(report(
            host,
            Utc::now() - Duration::seconds((5 - index) * 10),
            10.0 + index as f64,
        ))
        .expect("valid report");
        unionc::monitoring::store::store_monitoring_report(&pool, &parsed)
            .await
            .expect("store report");
    }
    let points =
        unionc::monitoring::store::monitoring_history(&pool, &host.to_string(), None, None, 100)
            .await
            .expect("query should succeed")
            .expect("host exists");
    assert_eq!(points.len(), 5);
    assert!(
        points
            .windows(2)
            .all(|w| w[0].collected_at <= w[1].collected_at),
        "历史点应按采集时间升序返回，供前端直接画曲线"
    );
    assert_eq!(
        points.first().unwrap().metrics.cpu_usage_percent,
        Some(10.0),
        "最早的点应排在最前"
    );

    // ── 4. 时间范围过滤仍然生效 ───────────────────────────────────────────
    let recent = unionc::monitoring::store::monitoring_history(
        &pool,
        &host.to_string(),
        Some(Utc::now() - Duration::seconds(25)),
        None,
        100,
    )
    .await
    .expect("query should succeed")
    .expect("host exists");
    assert!(
        recent.len() < 5 && !recent.is_empty(),
        "时间范围过滤应缩小结果集，实际返回 {} 个点",
        recent.len()
    );

    // ── 5. limit 生效，且返回的是**最近**的点 ─────────────────────────────
    let limited =
        unionc::monitoring::store::monitoring_history(&pool, &host.to_string(), None, None, 2)
            .await
            .expect("query should succeed")
            .expect("host exists");
    assert_eq!(limited.len(), 2);
    assert_eq!(
        limited.last().unwrap().metrics.cpu_usage_percent,
        Some(14.0),
        "limit 应保留最近的点（而非最早的）"
    );

    for id in [empty_host, host] {
        query("DELETE FROM monitored_hosts WHERE host_id=?1")
            .bind(id.to_string())
            .execute(&pool)
            .await
            .expect("cleanup");
    }
}

#[tokio::test]
async fn history_range_boundaries_are_inclusive_for_every_query_shape() {
    let url = common::test_database_url("history_range_boundaries_are_inclusive");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let host = Uuid::new_v4();
    register(&pool, host).await;
    let first = Utc::now() - Duration::minutes(2);
    let middle = first + Duration::seconds(10);
    let last = middle + Duration::seconds(10);
    for (collected_at, cpu) in [(first, 10.0), (middle, 20.0), (last, 30.0)] {
        let parsed = serde_json::from_value(report(host, collected_at, cpu)).expect("valid report");
        unionc::monitoring::store::store_monitoring_report(&pool, &parsed)
            .await
            .expect("store report");
    }

    for (label, from, to, expected) in [
        ("unbounded", None, None, vec![10.0, 20.0, 30.0]),
        ("from only", Some(middle), None, vec![20.0, 30.0]),
        ("to only", None, Some(middle), vec![10.0, 20.0]),
        ("both", Some(middle), Some(middle), vec![20.0]),
    ] {
        let points =
            unionc::monitoring::store::monitoring_history(&pool, &host.to_string(), from, to, 100)
                .await
                .unwrap_or_else(|error| panic!("{label} query failed: {error}"))
                .expect("registered host must exist");
        let actual = points
            .iter()
            .map(|point| point.metrics.cpu_usage_percent.expect("stored CPU metric"))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "{label} range returned wrong points");

        let limited =
            unionc::monitoring::store::monitoring_history(&pool, &host.to_string(), from, to, 1)
                .await
                .unwrap_or_else(|error| panic!("limited {label} query failed: {error}"))
                .expect("registered host must exist");
        assert_eq!(limited.len(), 1, "{label} limit was not applied");
        assert_eq!(
            limited[0].metrics.cpu_usage_percent,
            expected.last().copied(),
            "{label} limit did not retain the newest in-range point"
        );
    }

    query("DELETE FROM monitored_hosts WHERE host_id=?1")
        .bind(host.to_string())
        .execute(&pool)
        .await
        .expect("cleanup");
}
