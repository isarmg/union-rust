//! 保留期清理与"最新报告"的相互作用。
//!
//! 报文体不在 `monitored_hosts` 里存副本，详情接口的报文体改为
//! 通过 `latest_report_id` 关联到 `agent_metric_reports`。这带来一个新的失效模式：
//! 保留期清理若把被引用的那一份删掉，长期离线主机的详情页会变成空白。
//!
//! 本测试把两道保险都钉住：
//!   1. 清理**跳过**仍被引用的最新报告；
//!   2. 外键 `ON DELETE SET NULL` 兜底，杜绝悬空 id。

use chrono::{Duration, Utc};
use sqlx_core::{query::query, query_builder::QueryBuilder, row::Row};
use sqlx_sqlite::Sqlite;
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

fn report(host_id: Uuid, collected_at: chrono::DateTime<Utc>) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "report_id": Uuid::new_v4(),
        "collected_at": collected_at,
        "host": {
            "id": host_id, "os": "linux",
            "os_version": null, "kernel_version": null,
            "arch": "x86_64", "agent_version": "0.3.5"
        },
        "interval_seconds": 10.0,
        "system": {
            "uptime_seconds": 1,
            "cpu": { "usage_percent": 12.5, "logical_count": 2,
                     "physical_count": 1, "per_core_percent": [10.0, 15.0] },
            "memory": { "total_bytes": 1000, "used_bytes": 500, "available_bytes": 500,
                        "swap_total_bytes": 0, "swap_used_bytes": 0 },
            "networks": [], "disks": [], "temperatures": [], "gpus": []
        },
        "capabilities": [],
        "agent": { "spool_pending_batches": 0, "collector_errors": 0 }
    })
}

/// 一台早已离线的主机：它所有的报告都超出了保留期。
#[tokio::test]
async fn retention_keeps_the_latest_report_of_a_long_offline_host() {
    let url = common::test_database_url("retention_keeps_the_latest_report");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let host_id = Uuid::new_v4();
    common::insert_active_monitoring_host(
        &pool,
        &HostIdentity {
            id: host_id.to_string(),
            os: "linux".into(),
            os_version: None,
            kernel_version: None,
            arch: "x86_64".into(),
            agent_version: "0.3.5".into(),
        },
        &hex_hash(&format!("{host_id}-token")),
    )
    .await
    .expect("register host");

    // 写入 3 份报告，随后把 received_at 整体改到 100 天前——模拟一台早已停止上报的主机。
    for index in 0..3 {
        let parsed = serde_json::from_value(report(
            host_id,
            Utc::now() - Duration::seconds(300 - index * 10),
        ))
        .expect("valid report");
        unionc::monitoring::store::store_monitoring_report(&pool, &parsed)
            .await
            .expect("store report");
    }
    query("UPDATE agent_metric_reports SET received_at=?2 WHERE host_id=?1")
        .bind(host_id.to_string())
        .bind(database::to_epoch_micros(Utc::now() - Duration::days(100)))
        .execute(&pool)
        .await
        .expect("age the reports");

    let latest_before: Option<String> =
        query("SELECT latest_report_id FROM monitored_hosts WHERE host_id=?1")
            .bind(host_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("read latest id")
            .try_get("latest_report_id")
            .expect("column present");
    assert!(latest_before.is_some(), "写入后应记录最新报告 id");

    unionc::monitoring::store::prune_monitoring_history(&pool, 30)
        .await
        .expect("prune");

    // ── 保险 1：最新那份必须留下 ──────────────────────────────────────────
    let remaining: i64 = query("SELECT COUNT(*) AS c FROM agent_metric_reports WHERE host_id=?1")
        .bind(host_id.to_string())
        .fetch_one(&pool)
        .await
        .expect("count")
        .try_get("c")
        .expect("column");
    assert_eq!(
        remaining, 1,
        "超期的旧报告应被清理，但最新一份必须保留（实际剩余 {remaining} 份）"
    );

    // ── 保险 2：引用没有变成悬空，详情接口仍能拿到报文体 ──────────────────
    let stored = unionc::monitoring::store::get_monitored_host(&pool, &host_id.to_string())
        .await
        .expect("detail query")
        .expect("host exists");
    assert!(
        stored.latest.is_some(),
        "长期离线主机的详情页丢失了报文体——保留期清理删掉了被引用的最新报告"
    );
    assert_eq!(
        stored.metrics.cpu_usage_percent,
        Some(12.5),
        "摘要指标同样应保留"
    );

    query("DELETE FROM monitored_hosts WHERE host_id=?1")
        .bind(host_id.to_string())
        .execute(&pool)
        .await
        .expect("cleanup");
}

/// 外键必须声明 `ON DELETE SET NULL`：即便将来清理条件被改错，也只能留下 NULL，
/// 不能留下指向已删除行的悬空 id。
#[tokio::test]
async fn latest_report_reference_can_never_dangle() {
    let url = common::test_database_url("latest_report_reference_can_never_dangle");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    let host_id = Uuid::new_v4();
    common::insert_active_monitoring_host(
        &pool,
        &HostIdentity {
            id: host_id.to_string(),
            os: "linux".into(),
            os_version: None,
            kernel_version: None,
            arch: "x86_64".into(),
            agent_version: "0.3.5".into(),
        },
        &hex_hash(&format!("{host_id}-token")),
    )
    .await
    .expect("register host");

    let parsed = serde_json::from_value(report(host_id, Utc::now())).expect("valid report");
    unionc::monitoring::store::store_monitoring_report(&pool, &parsed)
        .await
        .expect("store report");

    // 绕过保留期逻辑，直接强删被引用的那一行——模拟"清理条件写错了"。
    query("DELETE FROM agent_metric_reports WHERE host_id=?1")
        .bind(host_id.to_string())
        .execute(&pool)
        .await
        .expect("force delete referenced report");

    let latest: Option<String> =
        query("SELECT latest_report_id FROM monitored_hosts WHERE host_id=?1")
            .bind(host_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("read latest id")
            .try_get("latest_report_id")
            .expect("column present");
    assert!(
        latest.is_none(),
        "被引用的报告被删除后，latest_report_id 应由外键置为 NULL，实际为 {latest:?}"
    );

    // 详情接口此时应正常返回主机（只是没有报文体），而不是报错。
    let stored = unionc::monitoring::store::get_monitored_host(&pool, &host_id.to_string())
        .await
        .expect("detail query should not fail on a host without reports")
        .expect("host still exists");
    assert!(stored.latest.is_none());

    query("DELETE FROM monitored_hosts WHERE host_id=?1")
        .bind(host_id.to_string())
        .execute(&pool)
        .await
        .expect("cleanup");
}

/// 分批删除必须删干净——**跨批次**也要终止正确。
///
/// 清理从"一条大 DELETE"改成"每批 10 000 行的循环"后，多了两种新的失效模式：
/// 循环提前退出（残留旧数据）与循环不终止（后台任务空转）。行数少于一批时两者
/// 都暴露不出来，因此这里特意造出**超过一个批次**的数据量。
///
/// 直接用 SQL 灌数据而不走上报路径：本用例要验的是删除侧的批次逻辑，
/// 一万多次 HTTP 语义的写入既慢又与被测行为无关。
#[tokio::test]
async fn batched_pruning_removes_everything_across_multiple_batches() {
    let url =
        common::test_database_url("batched_pruning_removes_everything_across_multiple_batches");
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
        &HostIdentity {
            id: host_id.to_string(),
            os: "linux".into(),
            os_version: None,
            kernel_version: None,
            arch: "x86_64".into(),
            agent_version: "0.3.5".into(),
        },
        &hex_hash(&format!("{marker}-token")),
    )
    .await
    .expect("register");

    // 一批是 10 000 行，这里多造一行，强制进入第二批。
    // SQLite 没有内建的序列生成表，用 QueryBuilder 分块灵活地灌数，同时避免超过
    // SQLite 单条语句的绑定变量上限。
    const ROWS: i64 = 10_001;
    const INSERT_BATCH: i64 = 500;
    let old_time = database::to_epoch_micros(Utc::now() - Duration::days(400));
    let mut seeded = 0_i64;
    let mut tx = database::begin_write(&pool).await.expect("begin seed");
    while seeded < ROWS {
        let batch = (ROWS - seeded).min(INSERT_BATCH);
        let mut builder = QueryBuilder::<Sqlite>::new(
            "INSERT INTO agent_metric_reports(\
             report_id, host_id, schema_version, collected_at, received_at, interval_seconds\
             ) ",
        );
        builder.push_values(0..batch, |mut row, _| {
            row.push_bind(Uuid::new_v4().to_string())
                .push_bind(host_id.to_string())
                .push_bind(1_i64)
                .push_bind(old_time)
                .push_bind(old_time)
                .push_bind(10.0_f64);
        });
        builder
            .build()
            .execute(tx.connection())
            .await
            .expect("seed old report batch");
        seeded += batch;
    }
    tx.commit().await.expect("commit seed");

    let before: i64 = query("SELECT COUNT(*) AS n FROM agent_metric_reports WHERE host_id=?1")
        .bind(host_id.to_string())
        .fetch_one(&pool)
        .await
        .expect("count before")
        .try_get("n")
        .expect("n");
    assert!(before >= ROWS, "灌数应至少 {ROWS} 行，实际 {before}");

    // 保留 30 天，这批 400 天前的数据全部超期。
    let removed = unionc::monitoring::store::prune_monitoring_history(&pool, 30)
        .await
        .expect("prune");
    eprintln!("分批清理：灌入 {before} 行，本次删除 {removed} 行");

    let after: i64 = query("SELECT COUNT(*) AS n FROM agent_metric_reports WHERE host_id=?1")
        .bind(host_id.to_string())
        .fetch_one(&pool)
        .await
        .expect("count after")
        .try_get("n")
        .expect("n");

    assert_eq!(
        after, 0,
        "超期报文必须被删光。残留 {after} 行说明批次循环提前退出了——\
         这正是「每批不足上限即 break」写错时的表现。"
    );
    assert!(
        removed >= ROWS as u64,
        "返回的删除计数应覆盖全部灌入行；实测 {removed} < {ROWS}，说明计数只累加了最后一批"
    );

    query("DELETE FROM monitored_hosts WHERE host_id=?1")
        .bind(host_id.to_string())
        .execute(&pool)
        .await
        .expect("cleanup");
}
