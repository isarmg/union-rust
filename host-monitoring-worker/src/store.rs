use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool, Row, postgres::PgPoolOptions, types::Json};
use unionc_protocol::{AgentPairingRequest, AgentReport, Capability, PairingStatus};
use uuid::Uuid;

use crate::model::{
    AgentInstanceSummary, AgentPairingPublicSummary, HistoryPoint, HostSummary, MetricSummary,
    host_status,
};

pub async fn connect(database_url: &str) -> anyhow::Result<PgPool> {
    Ok(PgPoolOptions::new()
        .max_connections(16)
        .min_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(database_url)
        .await?)
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

pub async fn ready(pool: &PgPool) -> bool {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(pool)
        .await
        .is_ok()
}

async fn audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    action: &str,
    target: &str,
    detail: Option<&str>,
    actor: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO host_monitoring.audit_events(action,target,detail,actor) VALUES($1,$2,$3,$4)",
    )
    .bind(action)
    .bind(target)
    .bind(detail)
    .bind(actor)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub enum CreateInviteResult {
    Created(AgentInstanceSummary),
    Conflict,
}

pub async fn create_invite(
    pool: &PgPool,
    display_name: &str,
    expires_in_minutes: i64,
    actor: &str,
) -> anyhow::Result<(CreateInviteResult, Option<String>)> {
    let invite_id = Uuid::new_v4();
    let instance_id = Uuid::new_v4();
    let activation_code = format!("uci_{}", Uuid::new_v4().simple());
    let activation_hash = crate::token_hash(&activation_code);
    let created_at = Utc::now();
    let expires_at = created_at + chrono::Duration::minutes(expires_in_minutes);
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        r#"INSERT INTO host_monitoring.agent_instance_invites(
               invite_id,instance_id,activation_code_hash,display_name,expires_at,created_at
           ) VALUES($1,$2,$3,$4,$5,$6)
           ON CONFLICT (instance_id) WHERE status='pending' DO NOTHING
           RETURNING invite_id,instance_id,display_name,status,expires_at,created_at"#,
    )
    .bind(invite_id)
    .bind(instance_id)
    .bind(&activation_hash)
    .bind(display_name)
    .bind(expires_at)
    .bind(created_at)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.rollback().await?;
        return Ok((CreateInviteResult::Conflict, None));
    };
    audit(
        &mut tx,
        "monitoring.agent_instance.invite.create",
        &instance_id.to_string(),
        Some(&format!("invite_id={invite_id}; expires_at={expires_at}")),
        actor,
    )
    .await?;
    tx.commit().await?;
    Ok((
        CreateInviteResult::Created(agent_instance(&row)?),
        Some(activation_code),
    ))
}

pub async fn list_invites(pool: &PgPool) -> anyhow::Result<Vec<AgentInstanceSummary>> {
    let rows = sqlx::query(
        r#"SELECT invite_id,instance_id,display_name,expires_at,created_at,
                  CASE WHEN status='pending' AND expires_at <= now() THEN 'expired' ELSE status END AS status
           FROM host_monitoring.agent_instance_invites
           ORDER BY created_at DESC LIMIT 200"#,
    )
    .fetch_all(pool)
    .await?;
    rows.iter().map(agent_instance).collect()
}

pub enum CancelInviteResult {
    Cancelled,
    NotFound,
    NotPending,
}

pub async fn cancel_invite(
    pool: &PgPool,
    invite_id: Uuid,
    actor: &str,
) -> anyhow::Result<CancelInviteResult> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query("SELECT status,instance_id FROM host_monitoring.agent_instance_invites WHERE invite_id=$1 FOR UPDATE")
        .bind(invite_id).fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        tx.rollback().await?;
        return Ok(CancelInviteResult::NotFound);
    };
    if row.try_get::<String, _>("status")? != "pending" {
        tx.rollback().await?;
        return Ok(CancelInviteResult::NotPending);
    }
    let instance_id: Uuid = row.try_get("instance_id")?;
    sqlx::query("UPDATE host_monitoring.agent_instance_invites SET status='cancelled',cancelled_at=now() WHERE invite_id=$1")
        .bind(invite_id).execute(&mut *tx).await?;
    audit(
        &mut tx,
        "monitoring.agent_instance.invite.cancel",
        &instance_id.to_string(),
        Some(&format!("invite_id={invite_id}")),
        actor,
    )
    .await?;
    tx.commit().await?;
    Ok(CancelInviteResult::Cancelled)
}

fn agent_instance(row: &sqlx::postgres::PgRow) -> anyhow::Result<AgentInstanceSummary> {
    Ok(AgentInstanceSummary {
        request_id: row.try_get::<Uuid, _>("invite_id")?.to_string(),
        instance_id: row.try_get::<Uuid, _>("instance_id")?.to_string(),
        display_name: row.try_get("display_name")?,
        status: row.try_get("status")?,
        expires_at: row.try_get("expires_at")?,
        created_at: row.try_get("created_at")?,
    })
}

pub enum CreatePairingResult {
    Ready {
        request_id: Uuid,
        expires_at: DateTime<Utc>,
        created: bool,
    },
    Expired,
    Conflict,
    AtCapacity,
}

pub async fn create_pairing(
    pool: &PgPool,
    request: &AgentPairingRequest,
) -> anyhow::Result<CreatePairingResult> {
    const MAX_PENDING: i64 = 4096;
    let mut tx = pool.begin().await?;
    // Serialize identical polling secrets without locking the whole table.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(&request.polling_secret_hash)
        .execute(&mut *tx)
        .await?;
    let existing = sqlx::query(
        "SELECT request_id,requested_host_id,os,os_version,kernel_version,arch,agent_version,token_hash,status,expires_at \
         FROM host_monitoring.agent_pairing_requests WHERE polling_secret_hash=$1",
    ).bind(&request.polling_secret_hash).fetch_optional(&mut *tx).await?;
    if let Some(row) = existing {
        let matches = row.try_get::<Uuid, _>("requested_host_id")?.to_string() == request.host.id
            && row.try_get::<String, _>("os")? == request.host.os.trim()
            && row.try_get::<Option<String>, _>("os_version")? == request.host.os_version
            && row.try_get::<Option<String>, _>("kernel_version")? == request.host.kernel_version
            && row.try_get::<String, _>("arch")? == request.host.arch.trim()
            && row.try_get::<String, _>("agent_version")? == request.host.agent_version.trim()
            && row.try_get::<String, _>("token_hash")? == request.token_hash;
        let expires_at: DateTime<Utc> = row.try_get("expires_at")?;
        let status: String = row.try_get("status")?;
        let request_id: Uuid = row.try_get("request_id")?;
        tx.rollback().await?;
        if !matches || status == "denied" {
            return Ok(CreatePairingResult::Conflict);
        }
        if expires_at <= Utc::now() {
            return Ok(CreatePairingResult::Expired);
        }
        return Ok(CreatePairingResult::Ready {
            request_id,
            expires_at,
            created: false,
        });
    }
    sqlx::query(
        "DELETE FROM host_monitoring.agent_pairing_requests WHERE request_id IN (\
           SELECT request_id FROM host_monitoring.agent_pairing_requests \
           WHERE (status='pending' AND expires_at <= now()) OR (status='denied' AND created_at < now()-interval '30 days') \
           ORDER BY created_at LIMIT 512)",
    ).execute(&mut *tx).await?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM host_monitoring.agent_pairing_requests WHERE status='pending' AND expires_at>now()",
    ).fetch_one(&mut *tx).await?;
    if pending >= MAX_PENDING {
        tx.commit().await?;
        return Ok(CreatePairingResult::AtCapacity);
    }
    let request_id = Uuid::new_v4();
    let expires_at = Utc::now() + chrono::Duration::minutes(15);
    let result = sqlx::query(
        r#"INSERT INTO host_monitoring.agent_pairing_requests(
               request_id,requested_host_id,os,os_version,kernel_version,arch,agent_version,
               token_hash,polling_secret_hash,expires_at)
           VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT DO NOTHING"#,
    )
    .bind(request_id)
    .bind(Uuid::parse_str(&request.host.id)?)
    .bind(request.host.os.trim())
    .bind(&request.host.os_version)
    .bind(&request.host.kernel_version)
    .bind(request.host.arch.trim())
    .bind(request.host.agent_version.trim())
    .bind(&request.token_hash)
    .bind(&request.polling_secret_hash)
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() != 1 {
        tx.rollback().await?;
        return Ok(CreatePairingResult::Conflict);
    }
    tx.commit().await?;
    Ok(CreatePairingResult::Ready {
        request_id,
        expires_at,
        created: true,
    })
}

pub async fn pairing_public(
    pool: &PgPool,
    request_id: Uuid,
) -> anyhow::Result<Option<AgentPairingPublicSummary>> {
    let row = sqlx::query(
        "SELECT request_id,os,arch,agent_version,expires_at,CASE WHEN status='pending' AND expires_at<=now() THEN 'expired' ELSE CASE WHEN status='pending' THEN 'waiting' ELSE status END END AS status \
         FROM host_monitoring.agent_pairing_requests WHERE request_id=$1",
    ).bind(request_id).fetch_optional(pool).await?;
    row.map(|row| {
        Ok(AgentPairingPublicSummary {
            request_id: row.try_get::<Uuid, _>("request_id")?.to_string(),
            os: row.try_get("os")?,
            arch: row.try_get("arch")?,
            agent_version: row.try_get("agent_version")?,
            status: row.try_get("status")?,
            expires_at: row.try_get("expires_at")?,
        })
    })
    .transpose()
}

pub async fn pairing_status(
    pool: &PgPool,
    request_id: Uuid,
    secret_hash: &str,
) -> anyhow::Result<Option<(PairingStatus, Option<String>)>> {
    let row = sqlx::query(
        "SELECT instance_id,CASE WHEN status='pending' AND expires_at<=now() THEN 'expired' WHEN status='pending' THEN 'waiting' ELSE status END AS status \
         FROM host_monitoring.agent_pairing_requests WHERE request_id=$1 AND polling_secret_hash=$2",
    ).bind(request_id).bind(secret_hash).fetch_optional(pool).await?;
    row.map(|row| {
        let raw: String = row.try_get("status")?;
        let status = PairingStatus::try_from(raw.as_str())
            .map_err(|_| anyhow::anyhow!("invalid pairing status in database"))?;
        let instance = if status == PairingStatus::Active {
            row.try_get::<Option<Uuid>, _>("instance_id")?
                .map(|v| v.to_string())
        } else {
            None
        };
        Ok((status, instance))
    })
    .transpose()
}

pub enum ActivateResult {
    Active(Uuid),
    NotFound,
    InvalidCode,
    Expired,
    Conflict,
}

pub async fn activate(
    pool: &PgPool,
    request_id: Uuid,
    activation_hash: &str,
    actor: &str,
) -> anyhow::Result<ActivateResult> {
    let mut tx = pool.begin().await?;
    let pairing = sqlx::query(
        "SELECT request_id,os,os_version,kernel_version,arch,agent_version,token_hash,status,invite_id,instance_id,expires_at \
         FROM host_monitoring.agent_pairing_requests WHERE request_id=$1 FOR UPDATE",
    ).bind(request_id).fetch_optional(&mut *tx).await?;
    let Some(pairing) = pairing else {
        tx.rollback().await?;
        return Ok(ActivateResult::NotFound);
    };
    let invite = sqlx::query(
        "SELECT invite_id,instance_id,display_name,status,expires_at FROM host_monitoring.agent_instance_invites \
         WHERE activation_code_hash=$1 FOR UPDATE",
    ).bind(activation_hash).fetch_optional(&mut *tx).await?;
    let Some(invite) = invite else {
        tx.rollback().await?;
        return Ok(ActivateResult::InvalidCode);
    };
    let invite_id: Uuid = invite.try_get("invite_id")?;
    let instance_id: Uuid = invite.try_get("instance_id")?;
    let pairing_status: String = pairing.try_get("status")?;
    if pairing_status == "active" {
        let same = pairing.try_get::<Option<Uuid>, _>("invite_id")? == Some(invite_id)
            && pairing.try_get::<Option<Uuid>, _>("instance_id")? == Some(instance_id);
        let token_hash: String = pairing.try_get("token_hash")?;
        let active: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM host_monitoring.agent_credentials WHERE host_id=$1 AND token_hash=$2 AND revoked_at IS NULL)",
        ).bind(instance_id).bind(token_hash).fetch_one(&mut *tx).await?;
        tx.rollback().await?;
        return Ok(if same && active {
            ActivateResult::Active(instance_id)
        } else {
            ActivateResult::Conflict
        });
    }
    if pairing_status != "pending" || invite.try_get::<String, _>("status")? != "pending" {
        tx.rollback().await?;
        return Ok(ActivateResult::Conflict);
    }
    let now = Utc::now();
    if pairing.try_get::<DateTime<Utc>, _>("expires_at")? <= now
        || invite.try_get::<DateTime<Utc>, _>("expires_at")? <= now
    {
        tx.rollback().await?;
        return Ok(ActivateResult::Expired);
    }
    let token_hash: String = pairing.try_get("token_hash")?;
    sqlx::query(
        "INSERT INTO host_monitoring.monitored_hosts(host_id,name,os,os_version,kernel_version,arch,agent_version,registered_at,last_seen_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$8)",
    ).bind(instance_id).bind(invite.try_get::<String,_>("display_name")?)
      .bind(pairing.try_get::<String,_>("os")?).bind(pairing.try_get::<Option<String>,_>("os_version")?)
      .bind(pairing.try_get::<Option<String>,_>("kernel_version")?).bind(pairing.try_get::<String,_>("arch")?)
      .bind(pairing.try_get::<String,_>("agent_version")?).bind(now).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO host_monitoring.agent_credentials(credential_id,host_id,token_hash,issued_at) VALUES($1,$2,$3,$4)")
        .bind(Uuid::new_v4()).bind(instance_id).bind(&token_hash).bind(now).execute(&mut *tx).await?;
    sqlx::query("UPDATE host_monitoring.agent_pairing_requests SET status='active',invite_id=$2,instance_id=$3,activated_at=$4 WHERE request_id=$1")
        .bind(request_id).bind(invite_id).bind(instance_id).bind(now).execute(&mut *tx).await?;
    sqlx::query("UPDATE host_monitoring.agent_instance_invites SET status='active',activated_at=$2 WHERE invite_id=$1")
        .bind(invite_id).bind(now).execute(&mut *tx).await?;
    audit(
        &mut tx,
        "monitoring.agent_instance.activate",
        &instance_id.to_string(),
        Some(&format!("request_id={request_id}; invite_id={invite_id}")),
        actor,
    )
    .await?;
    tx.commit().await?;
    Ok(ActivateResult::Active(instance_id))
}

pub async fn host_for_token(pool: &PgPool, token_hash: &str) -> anyhow::Result<Option<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT c.host_id FROM host_monitoring.agent_credentials c JOIN host_monitoring.monitored_hosts h ON h.host_id=c.host_id WHERE c.token_hash=$1 AND c.revoked_at IS NULL AND h.lifecycle_status='active'",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?)
}

#[derive(Debug, thiserror::Error)]
pub enum ReportStoreError {
    #[error("monitoring host or credential no longer exists")]
    Unauthorized,
    #[error("report_id already belongs to another host")]
    ReportIdConflict,
}

pub async fn store_report(
    pool: &PgPool,
    report: &AgentReport,
    token_hash: &str,
    metrics: &MetricSummary,
) -> anyhow::Result<(bool, DateTime<Utc>)> {
    let host_id = Uuid::parse_str(&report.host.id)?;
    let report_id = Uuid::parse_str(&report.report_id)?;
    let mut tx = pool.begin().await?;
    let current = sqlx::query(
        "SELECT latest_report_id,latest_collected_at FROM host_monitoring.monitored_hosts h \
         WHERE host_id=$1 AND h.lifecycle_status='active' AND EXISTS(SELECT 1 FROM host_monitoring.agent_credentials c WHERE c.host_id=h.host_id AND c.token_hash=$2 AND c.revoked_at IS NULL) FOR UPDATE",
    ).bind(host_id).bind(token_hash).fetch_optional(&mut *tx).await?;
    let Some(current) = current else {
        tx.rollback().await?;
        return Err(ReportStoreError::Unauthorized.into());
    };
    let previous_report: Option<Uuid> = current.try_get("latest_report_id")?;
    let previous_collected: Option<DateTime<Utc>> = current.try_get("latest_collected_at")?;
    let becomes_latest = previous_collected.is_none_or(|previous| {
        report.collected_at > previous
            || (report.collected_at == previous
                && previous_report.is_none_or(|previous| report_id > previous))
    });
    let payload = becomes_latest.then(|| Json(report.clone()));
    let received_at = Utc::now();
    let inserted = sqlx::query(
        r#"INSERT INTO host_monitoring.agent_metric_reports(
             report_id,host_id,schema_version,collected_at,received_at,interval_seconds,payload,
             cpu_usage_percent,memory_usage_percent,network_received_bytes_per_second,
             network_transmitted_bytes_per_second,disk_read_bytes_per_second,disk_written_bytes_per_second,
             max_temperature_celsius,gpu_utilization_percent,gpu_memory_usage_percent)
           VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
           ON CONFLICT(report_id) DO NOTHING RETURNING received_at"#,
    ).bind(report_id).bind(host_id).bind(i32::from(report.schema_version)).bind(report.collected_at)
      .bind(received_at).bind(report.interval_seconds).bind(payload)
      .bind(metrics.cpu_usage_percent).bind(metrics.memory_usage_percent)
      .bind(metrics.network_received_bytes_per_second).bind(metrics.network_transmitted_bytes_per_second)
      .bind(metrics.disk_read_bytes_per_second).bind(metrics.disk_written_bytes_per_second)
      .bind(metrics.max_temperature_celsius).bind(metrics.gpu_utilization_percent).bind(metrics.gpu_memory_usage_percent)
      .fetch_optional(&mut *tx).await?;
    let Some(row) = inserted else {
        let existing: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(
            "SELECT host_id,received_at FROM host_monitoring.agent_metric_reports WHERE report_id=$1",
        ).bind(report_id).fetch_optional(&mut *tx).await?;
        tx.rollback().await?;
        return match existing {
            Some((owner, timestamp)) if owner == host_id => Ok((false, timestamp)),
            _ => Err(ReportStoreError::ReportIdConflict.into()),
        };
    };
    let stored_received: DateTime<Utc> = row.try_get("received_at")?;
    if becomes_latest {
        sqlx::query(
            r#"UPDATE host_monitoring.monitored_hosts SET
                 os=$2,os_version=$3,kernel_version=$4,arch=$5,agent_version=$6,capabilities=$7,
                 last_seen_at=GREATEST(last_seen_at,$8),latest_report_id=$9,
                 latest_collected_at=$10,latest_interval_seconds=$11 WHERE host_id=$1"#,
        )
        .bind(host_id)
        .bind(report.host.os.trim())
        .bind(&report.host.os_version)
        .bind(&report.host.kernel_version)
        .bind(report.host.arch.trim())
        .bind(report.host.agent_version.trim())
        .bind(Json(&report.capabilities))
        .bind(stored_received)
        .bind(report_id)
        .bind(report.collected_at)
        .bind(report.interval_seconds)
        .execute(&mut *tx)
        .await?;
        if let Some(previous) = previous_report.filter(|previous| *previous != report_id) {
            sqlx::query(
                "UPDATE host_monitoring.agent_metric_reports SET payload=NULL WHERE report_id=$1",
            )
            .bind(previous)
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query("UPDATE host_monitoring.agent_credentials SET last_used_at=$2 WHERE token_hash=$1")
        .bind(token_hash)
        .bind(stored_received)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((true, stored_received))
}

#[derive(FromRow)]
struct HostRow {
    host_id: Uuid,
    name: String,
    os: String,
    os_version: Option<String>,
    kernel_version: Option<String>,
    arch: String,
    agent_version: String,
    capabilities: Json<Vec<Capability>>,
    registered_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    latest_collected_at: Option<DateTime<Utc>>,
    latest_interval_seconds: Option<f64>,
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

const HOST_SELECT: &str = r#"SELECT h.host_id,h.name,h.os,h.os_version,h.kernel_version,h.arch,h.agent_version,
 h.capabilities,h.registered_at,h.last_seen_at,h.latest_collected_at,h.latest_interval_seconds,
 r.cpu_usage_percent,r.memory_usage_percent,r.network_received_bytes_per_second,
 r.network_transmitted_bytes_per_second,r.disk_read_bytes_per_second,r.disk_written_bytes_per_second,
 r.max_temperature_celsius,r.gpu_utilization_percent,r.gpu_memory_usage_percent
 FROM host_monitoring.monitored_hosts h LEFT JOIN host_monitoring.agent_metric_reports r ON r.report_id=h.latest_report_id"#;

fn summarize(row: HostRow) -> HostSummary {
    HostSummary {
        id: row.host_id.to_string(),
        name: row.name,
        os: row.os,
        os_version: row.os_version,
        kernel_version: row.kernel_version,
        arch: row.arch,
        agent_version: row.agent_version,
        registered_at: row.registered_at,
        last_seen_at: row.last_seen_at,
        latest_collected_at: row.latest_collected_at,
        status: host_status(row.last_seen_at, row.latest_interval_seconds),
        capabilities: row.capabilities.0,
        metrics: MetricSummary {
            cpu_usage_percent: row.cpu_usage_percent,
            memory_usage_percent: row.memory_usage_percent,
            network_received_bytes_per_second: row.network_received_bytes_per_second,
            network_transmitted_bytes_per_second: row.network_transmitted_bytes_per_second,
            disk_read_bytes_per_second: row.disk_read_bytes_per_second,
            disk_written_bytes_per_second: row.disk_written_bytes_per_second,
            max_temperature_celsius: row.max_temperature_celsius,
            gpu_utilization_percent: row.gpu_utilization_percent,
            gpu_memory_usage_percent: row.gpu_memory_usage_percent,
        },
    }
}

pub async fn list_hosts(
    pool: &PgPool,
    limit: i64,
    offset: i64,
) -> anyhow::Result<(Vec<HostSummary>, i64)> {
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM host_monitoring.monitored_hosts WHERE lifecycle_status='active'",
    )
    .fetch_one(pool)
    .await?;
    let sql = format!(
        "{HOST_SELECT} WHERE h.lifecycle_status='active' ORDER BY h.registered_at,h.host_id LIMIT $1 OFFSET $2"
    );
    let rows: Vec<HostRow> = sqlx::query_as(&sql)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    Ok((rows.into_iter().map(summarize).collect(), total))
}

pub async fn get_host(
    pool: &PgPool,
    host_id: Uuid,
) -> anyhow::Result<Option<(HostSummary, Option<AgentReport>)>> {
    let sql = format!("{HOST_SELECT} WHERE h.host_id=$1 AND h.lifecycle_status='active'");
    let row: Option<HostRow> = sqlx::query_as(&sql)
        .bind(host_id)
        .fetch_optional(pool)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let payload: Option<Json<AgentReport>> = sqlx::query_scalar(
        "SELECT r.payload FROM host_monitoring.monitored_hosts h LEFT JOIN host_monitoring.agent_metric_reports r ON r.report_id=h.latest_report_id WHERE h.host_id=$1",
    ).bind(host_id).fetch_one(pool).await?;
    Ok(Some((summarize(row), payload.map(|json| json.0))))
}

#[derive(FromRow)]
struct HistoryRow {
    report_id: Uuid,
    collected_at: DateTime<Utc>,
    received_at: DateTime<Utc>,
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

pub async fn history(
    pool: &PgPool,
    host_id: Uuid,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    limit: i64,
) -> anyhow::Result<Option<Vec<HistoryPoint>>> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM host_monitoring.monitored_hosts WHERE host_id=$1 AND lifecycle_status='active')",
    )
    .bind(host_id)
    .fetch_one(pool)
    .await?;
    if !exists {
        return Ok(None);
    }
    let rows: Vec<HistoryRow> = sqlx::query_as(
        r#"SELECT report_id,collected_at,received_at,cpu_usage_percent,memory_usage_percent,
         network_received_bytes_per_second,network_transmitted_bytes_per_second,disk_read_bytes_per_second,
         disk_written_bytes_per_second,max_temperature_celsius,gpu_utilization_percent,gpu_memory_usage_percent
         FROM host_monitoring.agent_metric_reports WHERE host_id=$1
           AND ($2::timestamptz IS NULL OR collected_at >= $2)
           AND ($3::timestamptz IS NULL OR collected_at <= $3)
         ORDER BY collected_at DESC,report_id DESC LIMIT $4"#,
    ).bind(host_id).bind(from).bind(to).bind(limit).fetch_all(pool).await?;
    let mut points: Vec<_> = rows
        .into_iter()
        .map(|row| HistoryPoint {
            report_id: row.report_id.to_string(),
            collected_at: row.collected_at,
            received_at: row.received_at,
            metrics: MetricSummary {
                cpu_usage_percent: row.cpu_usage_percent,
                memory_usage_percent: row.memory_usage_percent,
                network_received_bytes_per_second: row.network_received_bytes_per_second,
                network_transmitted_bytes_per_second: row.network_transmitted_bytes_per_second,
                disk_read_bytes_per_second: row.disk_read_bytes_per_second,
                disk_written_bytes_per_second: row.disk_written_bytes_per_second,
                max_temperature_celsius: row.max_temperature_celsius,
                gpu_utilization_percent: row.gpu_utilization_percent,
                gpu_memory_usage_percent: row.gpu_memory_usage_percent,
            },
        })
        .collect();
    points.reverse();
    Ok(Some(points))
}

pub async fn update_remark(
    pool: &PgPool,
    host_id: Uuid,
    remark: &str,
    actor: &str,
) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await?;
    let changed =
        sqlx::query("UPDATE host_monitoring.monitored_hosts SET name=$2 WHERE host_id=$1")
            .bind(host_id)
            .bind(remark)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            == 1;
    if changed {
        audit(
            &mut tx,
            "monitoring.instance.remark.update",
            &host_id.to_string(),
            None,
            actor,
        )
        .await?;
        tx.commit().await?;
    } else {
        tx.rollback().await?;
    }
    Ok(changed)
}

pub async fn delete_host(pool: &PgPool, host_id: Uuid, actor: &str) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM host_monitoring.monitored_hosts WHERE host_id=$1 FOR UPDATE)",
    )
    .bind(host_id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        tx.rollback().await?;
        return Ok(false);
    }
    audit(
        &mut tx,
        "monitoring.instance.delete",
        &host_id.to_string(),
        Some("host, reports, credentials, pairings and invites permanently deleted"),
        actor,
    )
    .await?;
    sqlx::query("DELETE FROM host_monitoring.agent_pairing_requests WHERE instance_id=$1 OR (requested_host_id=$1 AND status IN ('pending','denied'))")
        .bind(host_id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM host_monitoring.agent_instance_invites WHERE instance_id=$1")
        .bind(host_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM host_monitoring.monitored_hosts WHERE host_id=$1")
        .bind(host_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn schema_counts(pool: &PgPool, import_id: Uuid) -> anyhow::Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for table in [
        "monitored_hosts",
        "agent_metric_reports",
        "agent_credentials",
        "agent_instance_invites",
        "agent_pairing_requests",
    ] {
        let query =
            format!("SELECT count(*) FROM host_monitoring.{table} WHERE source_import_id=$1");
        let count: i64 = sqlx::query_scalar(&query)
            .bind(import_id)
            .fetch_one(pool)
            .await?;
        map.insert(table.into(), Value::from(count));
    }
    Ok(Value::Object(map))
}
