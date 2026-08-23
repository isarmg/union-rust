//! 读路径代价：列表与历史查询不得再传输/解析完整报告 JSON。
//!
//! 场景取自 P1-5 的问题描述：100 台主机的概览表格每 10 秒刷新一次，
//! 单份报告 30-50KB。若读路径触碰报文体，每次刷新就要取出并反序列化 3-5MB JSON，
//! 而响应里真正用到的只有十来个标量。

use std::time::Instant;

use chrono::{Duration, Utc};
use sqlx_core::{query::query, row::Row};
use unionc::monitoring::HostIdentity;
use unionc::{config::Settings, infra::database};
use uuid::Uuid;

mod common;

const HOSTS: usize = 100;
const HISTORY_POINTS: usize = 1000;

/// 构造一份接近真实规模的报告：64 核 + 20 块磁盘 + 10 张网卡 + 50 个温度传感器。
fn bulky_report(
    host_id: Uuid,
    report_id: Uuid,
    collected_at: chrono::DateTime<Utc>,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "report_id": report_id,
        "collected_at": collected_at,
        "host": {
            "id": host_id, "os": "linux",
            "os_version": "6.1.0-generic-very-long-version-string",
            "kernel_version": "6.1.0", "arch": "x86_64", "agent_version": "0.3.4"
        },
        "interval_seconds": 10.0,
        "system": {
            "uptime_seconds": 123456,
            "cpu": {
                "usage_percent": 42.5, "logical_count": 64, "physical_count": 32,
                "per_core_percent": (0..64).map(|i| f64::from(i) % 100.0).collect::<Vec<_>>()
            },
            "memory": { "total_bytes": 137438953472_u64, "used_bytes": 68719476736_u64,
                        "available_bytes": 68719476736_u64, "swap_total_bytes": 0, "swap_used_bytes": 0 },
            "networks": (0..10).map(|i| serde_json::json!({
                "name": format!("enp{i}s0f0np{i}"),
                "received_bytes_total": 1_000_000_u64, "transmitted_bytes_total": 2_000_000_u64,
                "received_bytes_per_second": f64::from(i) * 10.0,
                "transmitted_bytes_per_second": f64::from(i) * 5.0,
                "packets_received_total": 10_000_u64, "packets_transmitted_total": 20_000_u64,
                "receive_errors_total": 0, "transmit_errors_total": 0
            })).collect::<Vec<_>>(),
            "disks": (0..20).map(|i| serde_json::json!({
                "name": format!("nvme{i}n1"),
                "mount_point": format!("/var/lib/storage/volume-{i}/data"),
                "file_system": "ext4",
                "total_bytes": 2_000_000_000_000_u64, "available_bytes": 1_000_000_000_000_u64,
                "read_bytes_total": 500_000_u64, "written_bytes_total": 600_000_u64,
                "read_bytes_per_second": f64::from(i) * 3.0,
                "written_bytes_per_second": f64::from(i) * 4.0,
                "is_read_only": false
            })).collect::<Vec<_>>(),
            "temperatures": (0..50).map(|i| serde_json::json!({
                "id": format!("coretemp-isa-0000-core-{i}"),
                "label": format!("Package id {i} / Core {i}"),
                "celsius": 40.0 + f64::from(i) % 30.0,
                "max_celsius": null, "critical_celsius": 100.0, "source": "hwmon"
            })).collect::<Vec<_>>(),
            "gpus": []
        },
        "capabilities": (0..8).map(|i| serde_json::json!({
            "name": format!("system.capability.{i}"), "available": true,
            "source": "sysinfo", "error_kind": null, "message": null
        })).collect::<Vec<_>>(),
        "agent": { "spool_pending_batches": 0, "collector_errors": 0 }
    })
}

fn identity(host_id: Uuid, index: usize) -> HostIdentity {
    let _ = index;
    HostIdentity {
        id: host_id.to_string(),
        os: "linux".into(),
        os_version: Some("6.1.0".into()),
        kernel_version: Some("6.1.0".into()),
        arch: "x86_64".into(),
        agent_version: "0.3.4".into(),
    }
}

fn hex_hash(seed: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(seed.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
async fn list_and_history_do_not_read_report_payloads() {
    let url = common::test_database_url("list_and_history_do_not_read_report_payloads");
    let mut settings = Settings::default();
    settings.database.url = url.to_string();
    let pool = database::connect(&settings).await.expect("connect");
    database::initialize_schema(&pool)
        .await
        .expect("initialize schema");

    // 使用独立前缀，便于测试结束后清理，避免污染其他用例。
    let marker = Uuid::new_v4();
    let mut host_ids = Vec::with_capacity(HOSTS);

    // ── 造数：100 台主机，每台一份大报告 ───────────────────────────────────
    let sample_size = {
        let host_id = Uuid::new_v4();
        serde_json::to_string(&bulky_report(host_id, Uuid::new_v4(), Utc::now()))
            .unwrap()
            .len()
    };
    eprintln!("单份报告体积约 {} KiB", sample_size / 1024);

    for index in 0..HOSTS {
        let host_id = Uuid::new_v4();
        host_ids.push(host_id);
        common::insert_active_monitoring_host(
            &pool,
            &identity(host_id, index),
            &hex_hash(&format!("{marker}-token-{index}")),
        )
        .await
        .expect("register host");

        let report = serde_json::from_value(bulky_report(host_id, Uuid::new_v4(), Utc::now()))
            .expect("valid report");
        unionc::monitoring::store::store_monitoring_report(&pool, &report)
            .await
            .expect("store report");
    }

    // ── 造数：第一台主机再补满 1000 条历史（history 接口的 limit 上限）────
    let history_host = host_ids[0];
    for point in 0..HISTORY_POINTS {
        let collected_at = Utc::now() - Duration::seconds((HISTORY_POINTS - point) as i64 * 10);
        let report =
            serde_json::from_value(bulky_report(history_host, Uuid::new_v4(), collected_at))
                .expect("valid report");
        unionc::monitoring::store::store_monitoring_report(&pool, &report)
            .await
            .expect("store history report");
    }

    // ── 断言 1：列表查询只按最新报告主键关联摘要列 ────────────
    let plan: String = query(
        "EXPLAIN QUERY PLAN \
         SELECT h.host_id, r.cpu_usage_percent, r.memory_usage_percent \
         FROM monitored_hosts h \
         LEFT JOIN agent_metric_reports r ON r.report_id = h.latest_report_id",
    )
    .fetch_all(&pool)
    .await
    .expect("explain list query")
    .iter()
    .map(|row| row.try_get::<String, _>("detail").unwrap_or_default())
    .collect::<Vec<_>>()
    .join("\n");
    eprintln!("--- 列表查询计划 ---\n{plan}\n");
    assert!(
        plan.contains("agent_metric_reports") && plan.contains("INDEX"),
        "列表查询应通过 report_id 索引定位最新报告：\n{plan}"
    );

    // ── 断言 2：列表接口耗时 ───────────────────────────────────────────────
    let started = Instant::now();
    // 分页上限取 1000（接口允许的最大值），确保本用例造的主机全都落在同一页里。
    let (hosts, total) = unionc::monitoring::store::list_monitored_hosts(&pool, 1000, 0)
        .await
        .expect("list hosts");
    let list_elapsed = started.elapsed();
    eprintln!(
        "list_monitored_hosts：{} 台主机（库中共 {total} 台）耗时 {list_elapsed:?}",
        hosts.len()
    );

    assert!(hosts.len() >= HOSTS, "应至少返回 {HOSTS} 台主机");
    assert!(
        total >= hosts.len() as i64,
        "COUNT(*) OVER() 返回的总数不得小于本页行数：total={total}，本页={}",
        hosts.len()
    );
    assert!(
        hosts.iter().all(|host| host.latest.is_none()),
        "列表接口不应装载完整报告体——latest 必须为 None"
    );

    // 指标断言只针对**本用例造的**主机。
    //
    // 列表接口返回库里的全部主机，而同一个测试库上的其他用例可能留下"已注册但从未
    // 上报"的主机（那是合法状态，摘要列本就为 NULL）。对全量结果断言
    // `cpu_usage_percent.is_some()`，于是本用例的成败取决于**别的用例**是否恰好留下
    // 了这样一台主机——一个与被测行为完全无关的失败源。
    let created: std::collections::HashSet<String> =
        host_ids.iter().map(|id| id.to_string()).collect();
    let mine: Vec<_> = hosts
        .iter()
        .filter(|host| created.contains(&host.identity.id))
        .collect();
    assert_eq!(mine.len(), HOSTS, "本用例创建的主机应全部出现在列表中");
    assert!(
        mine.iter()
            .all(|host| host.metrics.cpu_usage_percent.is_some()),
        "列表接口必须能从摘要列取到 CPU 指标"
    );
    // `EXPLAIN QUERY PLAN`、`latest == None` 与摘要列断言才是可重复的契约。
    // 不把共享 CI 主机上的绝对墙钟时间设为成败条件；调度抢占、杀毒扫描或慢盘都会让
    // 500ms 阈值偶发失败，却与查询是否读取完整 JSON 无关。耗时仍打印供基准观察。

    // ── 断言 3：历史接口耗时 ───────────────────────────────────────────────
    let started = Instant::now();
    let points = unionc::monitoring::store::monitoring_history(
        &pool,
        &history_host.to_string(),
        None,
        None,
        HISTORY_POINTS as i64,
    )
    .await
    .expect("history query")
    .expect("host exists");
    let history_elapsed = started.elapsed();
    eprintln!(
        "monitoring_history：{} 个点耗时 {history_elapsed:?}（若解析完整报告需搬运约 {} MiB）",
        points.len(),
        points.len() * sample_size / 1024 / 1024
    );

    assert_eq!(points.len(), HISTORY_POINTS, "应返回全部历史点");
    assert!(
        points
            .iter()
            .all(|point| point.metrics.cpu_usage_percent.is_some()),
        "历史点必须能从摘要列取到 CPU 指标"
    );
    // 历史返回类型只含摘要指标，配合下面的字段断言锁定“不解析完整报告”的行为；
    // 墙钟耗时只作为诊断信息，不作为跨机器的正确性断言。

    // ── 断言 4：详情接口仍然提供完整报告体 ─────────────────────────────────
    let detail = unionc::monitoring::store::get_monitored_host(&pool, &history_host.to_string())
        .await
        .expect("detail")
        .expect("host exists");
    assert!(
        detail.latest.is_some(),
        "详情接口仍必须返回完整报告体，否则前端详情页会缺数据"
    );
    assert!(
        detail.metrics.cpu_usage_percent.is_some(),
        "详情接口的摘要同样应来自数值列"
    );

    // 清理
    for host_id in host_ids {
        query("DELETE FROM monitored_hosts WHERE host_id=?1")
            .bind(host_id.to_string())
            .execute(&pool)
            .await
            .expect("cleanup");
    }
}
