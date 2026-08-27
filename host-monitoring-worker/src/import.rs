use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableEvidence {
    pub source_count: usize,
    pub source_logical_sha256: String,
    pub target_count: i64,
    pub target_logical_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportEvidence {
    pub format: String,
    pub import_id: Uuid,
    pub source_path: String,
    pub source_file_sha256: String,
    pub imported_at: DateTime<Utc>,
    pub tables: BTreeMap<String, TableEvidence>,
}

#[derive(Debug, Serialize)]
pub struct VerificationEvidence {
    pub import_id: Uuid,
    pub verified_at: DateTime<Utc>,
    pub valid: bool,
    pub expected_tables: BTreeMap<String, TableEvidence>,
    pub actual_target: BTreeMap<String, TargetEvidence>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetEvidence {
    pub count: i64,
    pub logical_sha256: String,
}

#[derive(Debug, Serialize)]
pub struct RollbackEvidence {
    pub format: String,
    pub import_id: Uuid,
    pub rolled_back_at: DateTime<Utc>,
    pub rows_before: Value,
    pub rows_after: Value,
}

#[derive(Debug, Clone, Serialize)]
struct HostRow {
    host_id: String,
    name: String,
    os: String,
    os_version: Option<String>,
    kernel_version: Option<String>,
    arch: String,
    agent_version: String,
    capabilities: Value,
    registered_at: i64,
    last_seen_at: i64,
    latest_report_id: Option<String>,
    latest_collected_at: Option<i64>,
    latest_interval_seconds: Option<f64>,
    lifecycle_status: String,
    revoked_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct ReportRow {
    report_id: String,
    host_id: String,
    schema_version: i64,
    collected_at: i64,
    received_at: i64,
    interval_seconds: f64,
    payload: Option<Value>,
    cpu_usage_percent: Option<f64>,
    memory_usage_percent: Option<f64>,
    network_received_bytes_per_second: Option<f64>,
    network_transmitted_bytes_per_second: Option<f64>,
    disk_read_bytes_per_second: Option<f64>,
    disk_written_bytes_per_second: Option<f64>,
    max_temperature_celsius: Option<f64>,
    gpu_utilization_percent: Option<f64>,
    gpu_memory_usage_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct CredentialRow {
    credential_id: String,
    host_id: String,
    token_hash: String,
    issued_at: i64,
    last_used_at: Option<i64>,
    kind: String,
    revoked_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct InviteRow {
    invite_id: String,
    instance_id: String,
    activation_code_hash: String,
    display_name: String,
    status: String,
    expires_at: i64,
    created_at: i64,
    activated_at: Option<i64>,
    cancelled_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct PairingRow {
    request_id: String,
    requested_host_id: String,
    os: String,
    os_version: Option<String>,
    kernel_version: Option<String>,
    arch: String,
    agent_version: String,
    token_hash: String,
    polling_secret_hash: String,
    status: String,
    invite_id: Option<String>,
    instance_id: Option<String>,
    expires_at: i64,
    created_at: i64,
    activated_at: Option<i64>,
}

struct Snapshot {
    hosts: Vec<HostRow>,
    reports: Vec<ReportRow>,
    credentials: Vec<CredentialRow>,
    invites: Vec<InviteRow>,
    pairings: Vec<PairingRow>,
}

pub async fn import_sqlite(
    pool: &PgPool,
    sqlite_path: &Path,
    evidence_path: &Path,
) -> anyhow::Result<ImportEvidence> {
    crate::store::migrate(pool).await?;
    ensure_target_empty(pool).await?;
    let snapshot = load_snapshot(sqlite_path)?;
    let source_file_sha256 = hash_bytes(&tokio::fs::read(sqlite_path).await?);
    let import_id = Uuid::new_v4();
    let mut tx = pool.begin().await?;
    insert_snapshot(&mut tx, import_id, &snapshot).await?;

    let mut tables = BTreeMap::new();
    for (table, count, digest) in [
        (
            "monitored_hosts",
            snapshot.hosts.len(),
            hash_serialized(&snapshot.hosts)?,
        ),
        (
            "agent_metric_reports",
            snapshot.reports.len(),
            hash_serialized(&snapshot.reports)?,
        ),
        (
            "agent_credentials",
            snapshot.credentials.len(),
            hash_serialized(&snapshot.credentials)?,
        ),
        (
            "agent_instance_invites",
            snapshot.invites.len(),
            hash_serialized(&snapshot.invites)?,
        ),
        (
            "agent_pairing_requests",
            snapshot.pairings.len(),
            hash_serialized(&snapshot.pairings)?,
        ),
    ] {
        let target = target_evidence_tx(&mut tx, table, import_id).await?;
        if target.count != count as i64 {
            anyhow::bail!(
                "import validation failed for {table}: source={count}, target={}",
                target.count
            );
        }
        tables.insert(
            table.into(),
            TableEvidence {
                source_count: count,
                source_logical_sha256: digest,
                target_count: target.count,
                target_logical_sha256: target.logical_sha256,
            },
        );
    }
    let imported_at = Utc::now();
    let preliminary = ImportEvidence {
        format: "union-host-monitoring-import-v1".into(),
        import_id,
        source_path: absolute_display(sqlite_path)?,
        source_file_sha256,
        imported_at,
        tables,
    };
    sqlx::query(
        "INSERT INTO host_monitoring.import_batches(import_id,source_path,source_sha256,manifest,status,imported_at) VALUES($1,$2,$3,$4,'complete',$5)",
    ).bind(import_id).bind(&preliminary.source_path).bind(&preliminary.source_file_sha256)
      .bind(sqlx::types::Json(&preliminary)).bind(imported_at).execute(&mut *tx).await?;
    tx.commit().await?;
    write_new_json(evidence_path, &preliminary).await?;
    Ok(preliminary)
}

pub async fn verify(pool: &PgPool, import_id: Uuid) -> anyhow::Result<VerificationEvidence> {
    let row = sqlx::query(
        "SELECT manifest,status FROM host_monitoring.import_batches WHERE import_id=$1",
    )
    .bind(import_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("import batch not found"))?;
    if row.try_get::<String, _>("status")? != "complete" {
        anyhow::bail!("import batch has already been rolled back");
    }
    let expected: sqlx::types::Json<ImportEvidence> = row.try_get("manifest")?;
    let mut actual_target = BTreeMap::new();
    let mut valid = true;
    for (table, evidence) in &expected.tables {
        let actual = target_evidence(pool, table, import_id).await?;
        valid &= actual.count == evidence.target_count
            && actual.logical_sha256 == evidence.target_logical_sha256;
        actual_target.insert(table.clone(), actual);
    }
    Ok(VerificationEvidence {
        import_id,
        verified_at: Utc::now(),
        valid,
        expected_tables: expected.tables.clone(),
        actual_target,
    })
}

pub async fn rollback(
    pool: &PgPool,
    import_id: Uuid,
    evidence_path: &Path,
) -> anyhow::Result<RollbackEvidence> {
    let verification = verify(pool, import_id).await?;
    if !verification.valid {
        anyhow::bail!(
            "refusing rollback because the imported target rows no longer match their evidence; investigate first"
        );
    }
    let rows_before = crate::store::schema_counts(pool, import_id).await?;
    let mut tx = pool.begin().await?;
    for table in [
        "agent_pairing_requests",
        "agent_instance_invites",
        "agent_credentials",
        "agent_metric_reports",
        "monitored_hosts",
    ] {
        let sql = format!("DELETE FROM host_monitoring.{table} WHERE source_import_id=$1");
        sqlx::query(&sql).bind(import_id).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE host_monitoring.import_batches SET status='rolled_back',rolled_back_at=now() WHERE import_id=$1 AND status='complete'")
        .bind(import_id).execute(&mut *tx).await?;
    tx.commit().await?;
    let rows_after = crate::store::schema_counts(pool, import_id).await?;
    let result = RollbackEvidence {
        format: "union-host-monitoring-rollback-v1".into(),
        import_id,
        rolled_back_at: Utc::now(),
        rows_before,
        rows_after,
    };
    write_new_json(evidence_path, &result).await?;
    Ok(result)
}

async fn ensure_target_empty(pool: &PgPool) -> anyhow::Result<()> {
    for table in [
        "monitored_hosts",
        "agent_metric_reports",
        "agent_credentials",
        "agent_instance_invites",
        "agent_pairing_requests",
    ] {
        let sql = format!("SELECT EXISTS(SELECT 1 FROM host_monitoring.{table})");
        if sqlx::query_scalar::<_, bool>(&sql).fetch_one(pool).await? {
            anyhow::bail!(
                "target host_monitoring.{table} is not empty; imports are intentionally one-shot into an empty module domain"
            );
        }
    }
    Ok(())
}

fn load_snapshot(path: &Path) -> anyhow::Result<Snapshot> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.execute_batch("PRAGMA query_only=ON; BEGIN DEFERRED;")?;
    let host_lifecycle = if has_column(&connection, "monitored_hosts", "lifecycle_status")? {
        "lifecycle_status,revoked_at"
    } else {
        "'active' AS lifecycle_status,NULL AS revoked_at"
    };
    let hosts_sql = format!(
        r#"SELECT host_id,name,os,os_version,kernel_version,arch,agent_version,capabilities,
      registered_at,last_seen_at,latest_report_id,latest_collected_at,latest_interval_seconds,{host_lifecycle} FROM monitored_hosts ORDER BY host_id"#,
    );
    let hosts = collect(&connection, &hosts_sql, |row| {
        Ok(HostRow {
            host_id: row.get(0)?,
            name: row.get(1)?,
            os: row.get(2)?,
            os_version: row.get(3)?,
            kernel_version: row.get(4)?,
            arch: row.get(5)?,
            agent_version: row.get(6)?,
            capabilities: parse_json(row.get::<_, String>(7)?)?,
            registered_at: row.get(8)?,
            last_seen_at: row.get(9)?,
            latest_report_id: row.get(10)?,
            latest_collected_at: row.get(11)?,
            latest_interval_seconds: row.get(12)?,
            lifecycle_status: row.get(13)?,
            revoked_at: row.get(14)?,
        })
    })?;
    let reports = collect(
        &connection,
        r#"SELECT report_id,host_id,schema_version,collected_at,received_at,interval_seconds,payload,
      cpu_usage_percent,memory_usage_percent,network_received_bytes_per_second,network_transmitted_bytes_per_second,
      disk_read_bytes_per_second,disk_written_bytes_per_second,max_temperature_celsius,gpu_utilization_percent,gpu_memory_usage_percent
      FROM agent_metric_reports ORDER BY report_id"#,
        |row| {
            Ok(ReportRow {
                report_id: row.get(0)?,
                host_id: row.get(1)?,
                schema_version: row.get(2)?,
                collected_at: row.get(3)?,
                received_at: row.get(4)?,
                interval_seconds: row.get(5)?,
                payload: row
                    .get::<_, Option<String>>(6)?
                    .map(parse_json)
                    .transpose()?,
                cpu_usage_percent: row.get(7)?,
                memory_usage_percent: row.get(8)?,
                network_received_bytes_per_second: row.get(9)?,
                network_transmitted_bytes_per_second: row.get(10)?,
                disk_read_bytes_per_second: row.get(11)?,
                disk_written_bytes_per_second: row.get(12)?,
                max_temperature_celsius: row.get(13)?,
                gpu_utilization_percent: row.get(14)?,
                gpu_memory_usage_percent: row.get(15)?,
            })
        },
    )?;
    let credential_tail = if has_column(&connection, "agent_credentials", "revoked_at")? {
        "kind,revoked_at"
    } else {
        "'pairing' AS kind,NULL AS revoked_at"
    };
    let credentials_sql = format!(
        "SELECT credential_id,host_id,token_hash,issued_at,last_used_at,{credential_tail} FROM agent_credentials ORDER BY credential_id"
    );
    let credentials = collect(&connection, &credentials_sql, |row| {
        Ok(CredentialRow {
            credential_id: row.get(0)?,
            host_id: row.get(1)?,
            token_hash: row.get(2)?,
            issued_at: row.get(3)?,
            last_used_at: row.get(4)?,
            kind: row.get(5)?,
            revoked_at: row.get(6)?,
        })
    })?;
    let invite_tail = if has_column(&connection, "agent_instance_invites", "cancelled_at")? {
        "status,cancelled_at"
    } else {
        "CASE status WHEN 'revoked' THEN 'cancelled' ELSE status END AS status,revoked_at AS cancelled_at"
    };
    let invites_sql = format!(
        "SELECT invite_id,instance_id,activation_code_hash,display_name,{invite_tail},expires_at,created_at,activated_at FROM agent_instance_invites ORDER BY invite_id"
    );
    let invites = collect(&connection, &invites_sql, |row| {
        Ok(InviteRow {
            invite_id: row.get(0)?,
            instance_id: row.get(1)?,
            activation_code_hash: row.get(2)?,
            display_name: row.get(3)?,
            status: row.get(4)?,
            cancelled_at: row.get(5)?,
            expires_at: row.get(6)?,
            created_at: row.get(7)?,
            activated_at: row.get(8)?,
        })
    })?;
    let pairings = collect(
        &connection,
        r#"SELECT request_id,requested_host_id,os,os_version,kernel_version,arch,agent_version,token_hash,
      polling_secret_hash,status,invite_id,instance_id,expires_at,created_at,activated_at FROM agent_pairing_requests ORDER BY request_id"#,
        |row| {
            Ok(PairingRow {
                request_id: row.get(0)?,
                requested_host_id: row.get(1)?,
                os: row.get(2)?,
                os_version: row.get(3)?,
                kernel_version: row.get(4)?,
                arch: row.get(5)?,
                agent_version: row.get(6)?,
                token_hash: row.get(7)?,
                polling_secret_hash: row.get(8)?,
                status: row.get(9)?,
                invite_id: row.get(10)?,
                instance_id: row.get(11)?,
                expires_at: row.get(12)?,
                created_at: row.get(13)?,
                activated_at: row.get(14)?,
            })
        },
    )?;
    connection.execute_batch("ROLLBACK")?;
    Ok(Snapshot {
        hosts,
        reports,
        credentials,
        invites,
        pairings,
    })
}

fn collect<T, F>(connection: &Connection, sql: &str, map: F) -> anyhow::Result<Vec<T>>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
{
    Ok(connection
        .prepare(sql)?
        .query_map([], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn has_column(connection: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    let sql = format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name=?1)");
    Ok(connection.query_row(&sql, [column], |row| row.get(0))?)
}

fn parse_json(value: String) -> rusqlite::Result<Value> {
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

async fn insert_snapshot(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    import_id: Uuid,
    data: &Snapshot,
) -> anyhow::Result<()> {
    for row in &data.hosts {
        sqlx::query(r#"INSERT INTO host_monitoring.monitored_hosts(host_id,name,os,os_version,kernel_version,arch,agent_version,capabilities,
          registered_at,last_seen_at,latest_report_id,latest_collected_at,latest_interval_seconds,lifecycle_status,revoked_at,source_import_id)
          VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)"#)
          .bind(uuid(&row.host_id)?).bind(&row.name).bind(&row.os).bind(&row.os_version).bind(&row.kernel_version).bind(&row.arch)
          .bind(&row.agent_version).bind(sqlx::types::Json(&row.capabilities)).bind(timestamp(row.registered_at)?).bind(timestamp(row.last_seen_at)?)
          .bind(optional_uuid(&row.latest_report_id)?).bind(optional_timestamp(row.latest_collected_at)?).bind(row.latest_interval_seconds)
          .bind(&row.lifecycle_status).bind(optional_timestamp(row.revoked_at)?).bind(import_id)
          .execute(&mut **tx).await?;
    }
    for row in &data.reports {
        sqlx::query(r#"INSERT INTO host_monitoring.agent_metric_reports(report_id,host_id,schema_version,collected_at,received_at,interval_seconds,payload,
          cpu_usage_percent,memory_usage_percent,network_received_bytes_per_second,network_transmitted_bytes_per_second,disk_read_bytes_per_second,
          disk_written_bytes_per_second,max_temperature_celsius,gpu_utilization_percent,gpu_memory_usage_percent,source_import_id)
          VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)"#)
          .bind(uuid(&row.report_id)?).bind(uuid(&row.host_id)?).bind(row.schema_version as i32).bind(timestamp(row.collected_at)?)
          .bind(timestamp(row.received_at)?).bind(row.interval_seconds).bind(row.payload.as_ref().map(sqlx::types::Json))
          .bind(row.cpu_usage_percent).bind(row.memory_usage_percent).bind(row.network_received_bytes_per_second)
          .bind(row.network_transmitted_bytes_per_second).bind(row.disk_read_bytes_per_second).bind(row.disk_written_bytes_per_second)
          .bind(row.max_temperature_celsius).bind(row.gpu_utilization_percent).bind(row.gpu_memory_usage_percent).bind(import_id)
          .execute(&mut **tx).await?;
    }
    for row in &data.credentials {
        sqlx::query("INSERT INTO host_monitoring.agent_credentials(credential_id,host_id,token_hash,issued_at,last_used_at,kind,revoked_at,source_import_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
          .bind(uuid(&row.credential_id)?).bind(uuid(&row.host_id)?).bind(&row.token_hash).bind(timestamp(row.issued_at)?)
          .bind(optional_timestamp(row.last_used_at)?).bind(&row.kind).bind(optional_timestamp(row.revoked_at)?).bind(import_id).execute(&mut **tx).await?;
    }
    for row in &data.invites {
        sqlx::query(r#"INSERT INTO host_monitoring.agent_instance_invites(invite_id,instance_id,activation_code_hash,display_name,status,
          expires_at,created_at,activated_at,cancelled_at,source_import_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)"#)
          .bind(uuid(&row.invite_id)?).bind(uuid(&row.instance_id)?).bind(&row.activation_code_hash).bind(&row.display_name).bind(&row.status)
          .bind(timestamp(row.expires_at)?).bind(timestamp(row.created_at)?).bind(optional_timestamp(row.activated_at)?)
          .bind(optional_timestamp(row.cancelled_at)?).bind(import_id).execute(&mut **tx).await?;
    }
    for row in &data.pairings {
        sqlx::query(r#"INSERT INTO host_monitoring.agent_pairing_requests(request_id,requested_host_id,os,os_version,kernel_version,arch,agent_version,
          token_hash,polling_secret_hash,status,invite_id,instance_id,expires_at,created_at,activated_at,source_import_id)
          VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)"#)
          .bind(uuid(&row.request_id)?).bind(uuid(&row.requested_host_id)?).bind(&row.os).bind(&row.os_version).bind(&row.kernel_version)
          .bind(&row.arch).bind(&row.agent_version).bind(&row.token_hash).bind(&row.polling_secret_hash).bind(&row.status)
          .bind(optional_uuid(&row.invite_id)?).bind(optional_uuid(&row.instance_id)?).bind(timestamp(row.expires_at)?)
          .bind(timestamp(row.created_at)?).bind(optional_timestamp(row.activated_at)?).bind(import_id).execute(&mut **tx).await?;
    }
    Ok(())
}

async fn target_evidence(
    pool: &PgPool,
    table: &str,
    import_id: Uuid,
) -> anyhow::Result<TargetEvidence> {
    let mut tx = pool.begin().await?;
    let evidence = target_evidence_tx(&mut tx, table, import_id).await?;
    tx.rollback().await?;
    Ok(evidence)
}

async fn target_evidence_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    import_id: Uuid,
) -> anyhow::Result<TargetEvidence> {
    let primary = match table {
        "monitored_hosts" => "host_id",
        "agent_metric_reports" => "report_id",
        "agent_credentials" => "credential_id",
        "agent_instance_invites" => "invite_id",
        "agent_pairing_requests" => "request_id",
        _ => anyhow::bail!("unknown evidence table"),
    };
    let sql = format!(
        r#"SELECT count(*) AS count,
      COALESCE(jsonb_agg(to_jsonb(t) - 'source_import_id' ORDER BY {primary})::text, '[]') AS logical
      FROM host_monitoring.{table} t WHERE source_import_id=$1"#
    );
    let row = sqlx::query(&sql)
        .bind(import_id)
        .fetch_one(&mut **tx)
        .await?;
    let logical: String = row.try_get("logical")?;
    Ok(TargetEvidence {
        count: row.try_get("count")?,
        logical_sha256: hash_bytes(logical.as_bytes()),
    })
}

fn timestamp(micros: i64) -> anyhow::Result<DateTime<Utc>> {
    DateTime::from_timestamp_micros(micros)
        .ok_or_else(|| anyhow::anyhow!("invalid SQLite timestamp {micros}"))
}
fn optional_timestamp(value: Option<i64>) -> anyhow::Result<Option<DateTime<Utc>>> {
    value.map(timestamp).transpose()
}
fn uuid(value: &str) -> anyhow::Result<Uuid> {
    Ok(Uuid::parse_str(value)?)
}
fn optional_uuid(value: &Option<String>) -> anyhow::Result<Option<Uuid>> {
    value.as_deref().map(uuid).transpose()
}

fn hash_serialized(value: &impl Serialize) -> anyhow::Result<String> {
    Ok(hash_bytes(&serde_json::to_vec(value)?))
}
fn hash_bytes(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn absolute_display(path: &Path) -> anyhow::Result<String> {
    let path: PathBuf = if path.is_absolute() {
        path.into()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(path.display().to_string())
}

async fn write_new_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .await?;
    file.write_all(&serde_json::to_vec_pretty(value)?).await?;
    file.write_all(b"\n").await?;
    file.sync_all().await?;
    Ok(())
}
