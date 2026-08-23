use chrono::{DateTime, Utc};
use sqlx_core::row::Row;
use sqlx_sqlite::SqliteRow;

use crate::{
    infra::database,
    monitoring::{HostIdentity, MetricSummary},
};

use super::StoredHost;

/// Summary columns live only on `agent_metric_reports`; host reads join the
/// latest report rather than maintaining a second, drift-prone copy.
pub(super) const METRIC_COLUMNS: [&str; 9] = [
    "cpu_usage_percent",
    "memory_usage_percent",
    "network_received_bytes_per_second",
    "network_transmitted_bytes_per_second",
    "disk_read_bytes_per_second",
    "disk_written_bytes_per_second",
    "max_temperature_celsius",
    "gpu_utilization_percent",
    "gpu_memory_usage_percent",
];

/// Build the common host query. List reads deliberately avoid selecting either
/// potentially large JSON field; detail reads request them explicitly.
pub(super) fn host_select(with_payload: bool, suffix: &str) -> String {
    let capabilities = if with_payload {
        "h.capabilities"
    } else {
        "CAST('[]' AS TEXT) AS capabilities"
    };
    let payload = if with_payload {
        "r.payload AS latest_report"
    } else {
        "CAST(NULL AS TEXT) AS latest_report"
    };
    let metrics = METRIC_COLUMNS
        .iter()
        .map(|column| format!("r.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"
        SELECT h.host_id,h.name,h.os,h.os_version,h.kernel_version,
               h.arch,h.agent_version,
               {capabilities},
               h.registered_at,h.last_seen_at,
               h.latest_collected_at,h.latest_interval_seconds,
               COUNT(*) OVER() AS total,
               {metrics},{payload}
        FROM monitored_hosts h
        LEFT JOIN agent_metric_reports r ON r.report_id = h.latest_report_id
        {suffix}
        "#
    )
}

pub(super) fn metrics_from_row(row: &SqliteRow) -> anyhow::Result<MetricSummary> {
    Ok(MetricSummary {
        cpu_usage_percent: row.try_get("cpu_usage_percent")?,
        memory_usage_percent: row.try_get("memory_usage_percent")?,
        network_received_bytes_per_second: row.try_get("network_received_bytes_per_second")?,
        network_transmitted_bytes_per_second: row
            .try_get("network_transmitted_bytes_per_second")?,
        disk_read_bytes_per_second: row.try_get("disk_read_bytes_per_second")?,
        disk_written_bytes_per_second: row.try_get("disk_written_bytes_per_second")?,
        max_temperature_celsius: row.try_get("max_temperature_celsius")?,
        gpu_utilization_percent: row.try_get("gpu_utilization_percent")?,
        gpu_memory_usage_percent: row.try_get("gpu_memory_usage_percent")?,
    })
}

pub(super) fn stored_host_from_row(
    row: SqliteRow,
    with_payload: bool,
) -> anyhow::Result<StoredHost> {
    let capabilities = serde_json::from_str(&row.try_get::<String, _>("capabilities")?)?;
    let latest = if with_payload {
        row.try_get::<Option<String>, _>("latest_report")?
            .map(|payload| serde_json::from_str(&payload))
            .transpose()?
    } else {
        None
    };
    let metrics = metrics_from_row(&row)?;
    Ok(StoredHost {
        name: row.try_get("name")?,
        identity: HostIdentity {
            id: row.try_get("host_id")?,
            os: row.try_get("os")?,
            os_version: row.try_get("os_version")?,
            kernel_version: row.try_get("kernel_version")?,
            arch: row.try_get("arch")?,
            agent_version: row.try_get("agent_version")?,
        },
        capabilities,
        registered_at: timestamp(&row, "registered_at")?,
        last_seen_at: timestamp(&row, "last_seen_at")?,
        latest_collected_at: optional_timestamp(&row, "latest_collected_at")?,
        latest_interval_seconds: row.try_get("latest_interval_seconds")?,
        metrics,
        latest,
    })
}

pub(super) fn timestamp(row: &SqliteRow, column: &str) -> anyhow::Result<DateTime<Utc>> {
    database::from_epoch_micros(row.try_get(column)?)
}

pub(super) fn optional_timestamp(
    row: &SqliteRow,
    column: &str,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    row.try_get::<Option<i64>, _>(column)?
        .map(database::from_epoch_micros)
        .transpose()
}

pub(super) fn canonical_uuid(value: &str) -> anyhow::Result<String> {
    let parsed = uuid::Uuid::parse_str(value)?;
    let canonical = parsed.to_string();
    anyhow::ensure!(
        canonical == value,
        "UUID must use canonical lowercase, hyphenated text"
    );
    Ok(canonical)
}
