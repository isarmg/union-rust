//! Persistence for read-only host metric reports.

use chrono::{DateTime, Utc};
use sqlx_core::{query::query, row::Row};
use sqlx_sqlite::{SqliteConnection, SqliteRow};

use crate::monitoring::{
    AgentInstanceSummary, AgentPairingPublicSummary, AgentPairingRequest, AgentReport,
    AgentReportExt, Capability, HostIdentity, MetricSummary,
};

use crate::infra::database::{self, DbPool};

#[derive(Debug)]
pub struct StoredHost {
    pub identity: HostIdentity,
    pub lifecycle_status: String,
    pub capabilities: Vec<Capability>,
    pub registered_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub latest_collected_at: Option<DateTime<Utc>>,
    pub latest_interval_seconds: Option<f64>,
    /// 最新一份报告的指标摘要，直接来自数值列，不解析 JSON。
    pub metrics: MetricSummary,
    /// 完整报告体，**仅详情接口**装载；列表接口为 `None`。
    pub latest: Option<AgentReport>,
}

/// 写入报文时可被上层**区分对待**的失败。
///
/// `report_id` 由 Agent 自行生成并提交，因此"这个 id 已经属于另一台主机"是一个
/// 客户端输入冲突，不是服务端故障。若把它汇入 `AppError::Anyhow` 返回 500，
/// 就把一个 4xx 语义的情况报成了内部错误——既误导调用方（重试无用），
/// 也污染了"500 必须有人看"的告警口径。
#[derive(Debug, thiserror::Error)]
pub enum StoreReportError {
    #[error("report_id already belongs to another host")]
    ReportIdBelongsToAnotherHost,
    #[error("monitoring host is not active")]
    HostNotActive,
}

#[derive(Debug)]
pub struct StoredHistoryPoint {
    pub report_id: String,
    pub collected_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub metrics: MetricSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeInviteResult {
    Revoked,
    NotFound,
    NotPending,
}

#[derive(Debug)]
pub enum CreateInviteResult {
    Created(AgentInstanceSummary),
    InstanceNotFound,
    Conflict,
}

#[derive(Debug)]
pub struct StoredPairingStatus {
    pub status: String,
    pub instance_id: Option<String>,
}

#[derive(Debug)]
pub struct StoredPairingCreation {
    pub request_id: String,
    pub expires_at: DateTime<Utc>,
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitoringTokenAuthentication {
    Active(String),
    Revoked,
    Unknown,
}

#[derive(Debug)]
pub enum CreatePairingResult {
    Ready(StoredPairingCreation),
    Expired,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivatePairingResult {
    Active(String),
    RequestNotFound,
    InvalidCode,
    Expired,
    Conflict,
}

async fn replace_agent_credential(
    connection: &mut SqliteConnection,
    host_id: &str,
    token_hash: &str,
) -> anyhow::Result<()> {
    let now_micros = database::to_epoch_micros(Utc::now());
    query(
        "UPDATE agent_credentials SET revoked_at=COALESCE(revoked_at,?2) \
         WHERE host_id=?1 AND revoked_at IS NULL",
    )
    .bind(host_id)
    .bind(now_micros)
    .execute(&mut *connection)
    .await?;
    query(
        r#"
        INSERT INTO agent_credentials(credential_id,host_id,token_hash,issued_at)
        VALUES(?1,?2,?3,?4)
        "#,
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(host_id)
    .bind(token_hash)
    .bind(now_micros)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

pub async fn create_agent_instance_invite(
    pool: &DbPool,
    invite_id: &str,
    instance_id: &str,
    activation_code_hash: &str,
    display_name: &str,
    expires_at: DateTime<Utc>,
    existing_instance: bool,
) -> anyhow::Result<CreateInviteResult> {
    let invite_id = canonical_uuid(invite_id)?;
    let instance_id = canonical_uuid(instance_id)?;
    let now_micros = database::to_epoch_micros(Utc::now());
    let mut tx = database::begin_write(pool).await?;

    if existing_instance {
        let found = query("SELECT 1 FROM monitored_hosts WHERE host_id=?1")
            .bind(&instance_id)
            .fetch_optional(tx.connection())
            .await?
            .is_some();
        if !found {
            tx.rollback().await?;
            return Ok(CreateInviteResult::InstanceNotFound);
        }
        // Reissuing an invite intentionally cancels the previous unconsumed
        // code for this instance. The write transaction serializes concurrent
        // reissue requests before this read/retire/insert sequence begins.
        query(
            r#"
            UPDATE agent_instance_invites
            SET status='revoked',revoked_at=?2
            WHERE instance_id=?1 AND status='pending'
            "#,
        )
        .bind(&instance_id)
        .bind(now_micros)
        .execute(tx.connection())
        .await?;
    }

    let row = query(
        r#"
        INSERT INTO agent_instance_invites(
            invite_id,instance_id,activation_code_hash,display_name,expires_at,created_at
        ) VALUES(?1,?2,?3,?4,?5,?6)
        ON CONFLICT (instance_id) WHERE status='pending' DO NOTHING
        RETURNING invite_id AS request_id,instance_id,
                  display_name,status,expires_at,created_at
        "#,
    )
    .bind(&invite_id)
    .bind(&instance_id)
    .bind(activation_code_hash)
    .bind(display_name)
    .bind(database::to_epoch_micros(expires_at))
    .bind(now_micros)
    .fetch_optional(tx.connection())
    .await?;
    let Some(row) = row else {
        tx.rollback().await?;
        return Ok(CreateInviteResult::Conflict);
    };
    let summary = agent_instance_from_row(row)?;
    crate::infra::database::insert_audit_in_transaction(
        tx.connection(),
        "monitoring.agent_instance.invite.create",
        &instance_id,
        Some(&format!("invite_id={invite_id}; expires_at={expires_at}")),
    )
    .await?;
    tx.commit().await?;
    Ok(CreateInviteResult::Created(summary))
}

pub async fn list_agent_instance_invites(
    pool: &DbPool,
) -> anyhow::Result<Vec<AgentInstanceSummary>> {
    let now_micros = database::to_epoch_micros(Utc::now());
    query(
        r#"
        SELECT i.invite_id AS request_id,i.instance_id,
               i.display_name,i.expires_at,i.created_at,
               CASE
                 WHEN i.status='revoked' THEN 'cancelled'
                 WHEN i.status='pending' AND i.expires_at <= ?1 THEN 'expired'
                 WHEN i.status='active' AND (
                      p.request_id IS NULL OR p.status <> 'active'
                      OR h.lifecycle_status <> 'active'
                      OR c.credential_id IS NULL OR c.revoked_at IS NOT NULL
                 ) THEN 'revoked'
                 ELSE i.status
               END AS status
        FROM agent_instance_invites i
        LEFT JOIN agent_pairing_requests p ON p.invite_id=i.invite_id
        LEFT JOIN monitored_hosts h ON h.host_id=i.instance_id
        LEFT JOIN agent_credentials c ON c.token_hash=p.token_hash
        ORDER BY i.created_at DESC
        LIMIT 200
        "#,
    )
    .bind(now_micros)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(agent_instance_from_row)
    .collect()
}

pub async fn revoke_agent_instance_invite(
    pool: &DbPool,
    invite_id: &str,
) -> anyhow::Result<RevokeInviteResult> {
    let invite_id = canonical_uuid(invite_id)?;
    let now_micros = database::to_epoch_micros(Utc::now());
    let mut tx = database::begin_write(pool).await?;
    let row = query(
        r#"
        SELECT status,instance_id
        FROM agent_instance_invites
        WHERE invite_id=?1
        "#,
    )
    .bind(&invite_id)
    .fetch_optional(tx.connection())
    .await?;
    let Some(row) = row else {
        tx.rollback().await?;
        return Ok(RevokeInviteResult::NotFound);
    };
    let status: String = row.try_get("status")?;
    if status != "pending" {
        tx.rollback().await?;
        return Ok(RevokeInviteResult::NotPending);
    }
    let instance_id: String = row.try_get("instance_id")?;
    query(
        "UPDATE agent_instance_invites SET status='revoked',revoked_at=?2 \
         WHERE invite_id=?1",
    )
    .bind(&invite_id)
    .bind(now_micros)
    .execute(tx.connection())
    .await?;
    crate::infra::database::insert_audit_in_transaction(
        tx.connection(),
        "monitoring.agent_instance.invite.cancel",
        &instance_id,
        Some(&format!("invite_id={invite_id}")),
    )
    .await?;
    tx.commit().await?;
    Ok(RevokeInviteResult::Revoked)
}

pub async fn create_agent_pairing_request(
    pool: &DbPool,
    request_id: &str,
    request: &AgentPairingRequest,
    expires_at: DateTime<Utc>,
) -> anyhow::Result<CreatePairingResult> {
    const EXISTING_BY_POLLING_SECRET: &str = r#"
        SELECT request_id,requested_host_id,
               name,os,os_version,kernel_version,arch,agent_version,token_hash,status,expires_at
        FROM agent_pairing_requests
        WHERE polling_secret_hash=?1
    "#;
    let request_id = canonical_uuid(request_id)?;
    let requested_host_id = canonical_uuid(&request.host.id)?;
    let now = Utc::now();
    let now_micros = database::to_epoch_micros(now);
    let stale_before = database::to_epoch_micros(now - chrono::Duration::days(30));
    let mut tx = database::begin_write(pool).await?;
    let existing = query(EXISTING_BY_POLLING_SECRET)
        .bind(&request.polling_secret_hash)
        .fetch_optional(tx.connection())
        .await?;
    if let Some(existing) = existing {
        let matches = pairing_creation_matches(&existing, &requested_host_id, request)?;
        if !matches || existing.try_get::<String, _>("status")? == "denied" {
            tx.rollback().await?;
            return Ok(CreatePairingResult::Conflict);
        }
        let stored_expires_at = timestamp(&existing, "expires_at")?;
        if stored_expires_at <= now {
            tx.rollback().await?;
            return Ok(CreatePairingResult::Expired);
        }
        let stored_request_id: String = existing.try_get("request_id")?;
        tx.rollback().await?;
        return Ok(CreatePairingResult::Ready(StoredPairingCreation {
            request_id: stored_request_id,
            expires_at: stored_expires_at,
            created: false,
        }));
    }
    query(
        r#"
        DELETE FROM agent_pairing_requests
        WHERE created_at < ?1
          AND (status='denied' OR (status='pending' AND expires_at <= ?2))
        "#,
    )
    .bind(stale_before)
    .bind(now_micros)
    .execute(tx.connection())
    .await?;
    let inserted = query(
        r#"
        INSERT INTO agent_pairing_requests(
            request_id,requested_host_id,name,os,os_version,kernel_version,arch,
            agent_version,token_hash,polling_secret_hash,expires_at,created_at
        ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
        ON CONFLICT DO NOTHING
        RETURNING request_id
        "#,
    )
    .bind(&request_id)
    .bind(&requested_host_id)
    .bind(request.host.name.trim())
    .bind(request.host.os.trim())
    .bind(request.host.os_version.as_deref())
    .bind(request.host.kernel_version.as_deref())
    .bind(request.host.arch.trim())
    .bind(request.host.agent_version.trim())
    .bind(&request.token_hash)
    .bind(&request.polling_secret_hash)
    .bind(database::to_epoch_micros(expires_at))
    .bind(now_micros)
    .fetch_optional(tx.connection())
    .await?
    .is_some();
    if inserted {
        tx.commit().await?;
        Ok(CreatePairingResult::Ready(StoredPairingCreation {
            request_id,
            expires_at,
            created: true,
        }))
    } else {
        // `begin_write` makes byte-identical concurrent creates observe the
        // committed winner in the initial SELECT. This fallback distinguishes
        // a same-secret row from a collision on another unique key.
        let raced = query(EXISTING_BY_POLLING_SECRET)
            .bind(&request.polling_secret_hash)
            .fetch_optional(tx.connection())
            .await?;
        if let Some(raced) = raced {
            let matches = pairing_creation_matches(&raced, &requested_host_id, request)?;
            let status: String = raced.try_get("status")?;
            let stored_expires_at = timestamp(&raced, "expires_at")?;
            let stored_request_id: String = raced.try_get("request_id")?;
            tx.rollback().await?;
            if matches && status != "denied" {
                return Ok(if stored_expires_at <= now {
                    CreatePairingResult::Expired
                } else {
                    CreatePairingResult::Ready(StoredPairingCreation {
                        request_id: stored_request_id,
                        expires_at: stored_expires_at,
                        created: false,
                    })
                });
            }
            return Ok(CreatePairingResult::Conflict);
        }
        tx.rollback().await?;
        Ok(CreatePairingResult::Conflict)
    }
}

fn pairing_creation_matches(
    row: &SqliteRow,
    requested_host_id: &str,
    request: &AgentPairingRequest,
) -> anyhow::Result<bool> {
    Ok(
        row.try_get::<String, _>("requested_host_id")? == requested_host_id
            && row.try_get::<String, _>("name")? == request.host.name.trim()
            && row.try_get::<String, _>("os")? == request.host.os.trim()
            && row.try_get::<Option<String>, _>("os_version")?.as_deref()
                == request.host.os_version.as_deref()
            && row
                .try_get::<Option<String>, _>("kernel_version")?
                .as_deref()
                == request.host.kernel_version.as_deref()
            && row.try_get::<String, _>("arch")? == request.host.arch.trim()
            && row.try_get::<String, _>("agent_version")? == request.host.agent_version.trim()
            && row.try_get::<String, _>("token_hash")? == request.token_hash,
    )
}

pub async fn agent_pairing_status(
    pool: &DbPool,
    request_id: &str,
    polling_secret_hash: &str,
) -> anyhow::Result<Option<StoredPairingStatus>> {
    let request_id = canonical_uuid(request_id)?;
    let now_micros = database::to_epoch_micros(Utc::now());
    let row = query(
        r#"
        SELECT p.instance_id,
               CASE
                 WHEN p.status='pending' AND p.expires_at <= ?3 THEN 'expired'
                 WHEN p.status='pending' THEN 'waiting'
                 WHEN p.status='active' AND (
                      h.lifecycle_status <> 'active'
                      OR c.credential_id IS NULL OR c.revoked_at IS NOT NULL
                 ) THEN 'denied'
                 ELSE p.status
               END AS status
        FROM agent_pairing_requests p
        LEFT JOIN monitored_hosts h ON h.host_id=p.instance_id
        LEFT JOIN agent_credentials c ON c.token_hash=p.token_hash
        WHERE p.request_id=?1 AND p.polling_secret_hash=?2
        "#,
    )
    .bind(&request_id)
    .bind(polling_secret_hash)
    .bind(now_micros)
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        let status: String = row.try_get("status")?;
        let instance_id = if status == "active" {
            row.try_get("instance_id")?
        } else {
            None
        };
        Ok(StoredPairingStatus {
            status,
            instance_id,
        })
    })
    .transpose()
}

pub async fn public_agent_pairing_request(
    pool: &DbPool,
    request_id: &str,
) -> anyhow::Result<Option<AgentPairingPublicSummary>> {
    let request_id = canonical_uuid(request_id)?;
    let now_micros = database::to_epoch_micros(Utc::now());
    let row = query(
        r#"
        SELECT p.request_id,p.name,p.os,p.arch,p.agent_version,p.expires_at,
               CASE
                 WHEN p.status='pending' AND p.expires_at <= ?2 THEN 'expired'
                 WHEN p.status='pending' THEN 'waiting'
                 WHEN p.status='active' AND (
                      h.lifecycle_status <> 'active'
                      OR c.credential_id IS NULL OR c.revoked_at IS NOT NULL
                 ) THEN 'denied'
                 ELSE p.status
               END AS status
        FROM agent_pairing_requests p
        LEFT JOIN monitored_hosts h ON h.host_id=p.instance_id
        LEFT JOIN agent_credentials c ON c.token_hash=p.token_hash
        WHERE p.request_id=?1
        "#,
    )
    .bind(&request_id)
    .bind(now_micros)
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        Ok(AgentPairingPublicSummary {
            request_id: row.try_get("request_id")?,
            name: row.try_get("name")?,
            os: row.try_get("os")?,
            arch: row.try_get("arch")?,
            agent_version: row.try_get("agent_version")?,
            status: row.try_get("status")?,
            expires_at: timestamp(&row, "expires_at")?,
        })
    })
    .transpose()
}

pub async fn activate_agent_pairing(
    pool: &DbPool,
    request_id: &str,
    activation_code_hash: &str,
) -> anyhow::Result<ActivatePairingResult> {
    let request_id = canonical_uuid(request_id)?;
    let now = Utc::now();
    let now_micros = database::to_epoch_micros(now);
    let mut tx = database::begin_write(pool).await?;
    let pairing = query(
        r#"
        SELECT request_id,name,os,os_version,kernel_version,arch,
               agent_version,token_hash,status,
               invite_id,instance_id,expires_at
        FROM agent_pairing_requests
        WHERE request_id=?1
        "#,
    )
    .bind(&request_id)
    .fetch_optional(tx.connection())
    .await?;
    let Some(pairing) = pairing else {
        tx.rollback().await?;
        return Ok(ActivatePairingResult::RequestNotFound);
    };
    let invite = query(
        r#"
        SELECT invite_id,instance_id,
               display_name,status,expires_at
        FROM agent_instance_invites
        WHERE activation_code_hash=?1
        "#,
    )
    .bind(activation_code_hash)
    .fetch_optional(tx.connection())
    .await?;
    let Some(invite) = invite else {
        tx.rollback().await?;
        return Ok(ActivatePairingResult::InvalidCode);
    };

    let pairing_status: String = pairing.try_get("status")?;
    let invite_id: String = invite.try_get("invite_id")?;
    let instance_id: String = invite.try_get("instance_id")?;
    if pairing_status == "active" {
        let bound_invite: Option<String> = pairing.try_get("invite_id")?;
        let bound_instance: Option<String> = pairing.try_get("instance_id")?;
        if bound_invite.as_deref() != Some(invite_id.as_str())
            || bound_instance.as_deref() != Some(instance_id.as_str())
        {
            tx.rollback().await?;
            return Ok(ActivatePairingResult::Conflict);
        }
        let token_hash: String = pairing.try_get("token_hash")?;
        let still_active: bool = query(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM monitored_hosts h
                JOIN agent_credentials c ON c.host_id=h.host_id
                WHERE h.host_id=?1 AND h.lifecycle_status='active'
                  AND c.token_hash=?2 AND c.revoked_at IS NULL
            ) AS active
            "#,
        )
        .bind(&instance_id)
        .bind(&token_hash)
        .fetch_one(tx.connection())
        .await?
        .try_get("active")?;
        tx.rollback().await?;
        return Ok(if still_active {
            ActivatePairingResult::Active(instance_id)
        } else {
            ActivatePairingResult::Conflict
        });
    }
    if pairing_status != "pending" {
        tx.rollback().await?;
        return Ok(ActivatePairingResult::Conflict);
    }
    let pairing_expires_at = timestamp(&pairing, "expires_at")?;
    let invite_expires_at = timestamp(&invite, "expires_at")?;
    if pairing_expires_at <= now || invite_expires_at <= now {
        tx.rollback().await?;
        return Ok(ActivatePairingResult::Expired);
    }
    let invite_status: String = invite.try_get("status")?;
    if invite_status != "pending" {
        tx.rollback().await?;
        return Ok(ActivatePairingResult::Conflict);
    }

    let token_hash: String = pairing.try_get("token_hash")?;
    query(
        r#"
        INSERT INTO monitored_hosts(
            host_id,name,os,os_version,kernel_version,arch,agent_version,
            lifecycle_status,revoked_at,registered_at,last_seen_at
        ) VALUES(?1,?2,?3,?4,?5,?6,?7,'active',NULL,?8,?8)
        ON CONFLICT(host_id) DO UPDATE SET
            name=EXCLUDED.name,
            os=EXCLUDED.os,
            os_version=EXCLUDED.os_version,
            kernel_version=EXCLUDED.kernel_version,
            arch=EXCLUDED.arch,
            agent_version=EXCLUDED.agent_version,
            lifecycle_status='active',
            revoked_at=NULL,
            last_seen_at=?8
        "#,
    )
    .bind(&instance_id)
    .bind(pairing.try_get::<String, _>("name")?)
    .bind(pairing.try_get::<String, _>("os")?)
    .bind(pairing.try_get::<Option<String>, _>("os_version")?)
    .bind(pairing.try_get::<Option<String>, _>("kernel_version")?)
    .bind(pairing.try_get::<String, _>("arch")?)
    .bind(pairing.try_get::<String, _>("agent_version")?)
    .bind(now_micros)
    .execute(tx.connection())
    .await?;
    replace_agent_credential(tx.connection(), &instance_id, &token_hash).await?;

    // A new administrator-approved pairing supersedes every previous active
    // request for the same instance, without deleting host report history.
    query(
        r#"
        UPDATE agent_pairing_requests
        SET status='denied'
        WHERE instance_id=?1 AND status='active' AND request_id<>?2
        "#,
    )
    .bind(&instance_id)
    .bind(&request_id)
    .execute(tx.connection())
    .await?;
    query(
        r#"
        UPDATE agent_pairing_requests
        SET status='active',invite_id=?2,instance_id=?3,activated_at=?4
        WHERE request_id=?1
        "#,
    )
    .bind(&request_id)
    .bind(&invite_id)
    .bind(&instance_id)
    .bind(now_micros)
    .execute(tx.connection())
    .await?;
    query(
        r#"
        UPDATE agent_instance_invites
        SET status='active',activated_at=?2
        WHERE invite_id=?1
        "#,
    )
    .bind(&invite_id)
    .bind(now_micros)
    .execute(tx.connection())
    .await?;
    crate::infra::database::insert_audit_in_transaction(
        tx.connection(),
        "monitoring.agent_instance.activate",
        &instance_id,
        Some(&format!("request_id={request_id}; invite_id={invite_id}")),
    )
    .await?;
    tx.commit().await?;
    Ok(ActivatePairingResult::Active(instance_id))
}

fn agent_instance_from_row(row: SqliteRow) -> anyhow::Result<AgentInstanceSummary> {
    Ok(AgentInstanceSummary {
        request_id: row.try_get("request_id")?,
        instance_id: row.try_get("instance_id")?,
        display_name: row.try_get("display_name")?,
        status: row.try_get("status")?,
        expires_at: timestamp(&row, "expires_at")?,
        created_at: timestamp(&row, "created_at")?,
    })
}

pub async fn monitoring_host_for_token(
    pool: &DbPool,
    token_hash: &str,
) -> anyhow::Result<MonitoringTokenAuthentication> {
    let row = query(
        r#"
        SELECT h.host_id,c.revoked_at IS NOT NULL AS credential_revoked,h.lifecycle_status
        FROM agent_credentials c
        JOIN monitored_hosts h ON h.host_id=c.host_id
        WHERE c.token_hash=?1
        "#,
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(MonitoringTokenAuthentication::Unknown);
    };
    let credential_revoked: bool = row.try_get("credential_revoked")?;
    let lifecycle_status: String = row.try_get("lifecycle_status")?;
    // 403 is reserved for persistent host decommissioning. A credential
    // superseded by a current re-pair is no longer valid and behaves like an
    // unknown token (401), while a revoked host remains a terminal 403.
    if lifecycle_status != "active" {
        return Ok(MonitoringTokenAuthentication::Revoked);
    }
    if credential_revoked {
        return Ok(MonitoringTokenAuthentication::Unknown);
    }
    Ok(MonitoringTokenAuthentication::Active(
        row.try_get("host_id")?,
    ))
}

/// 写入一份报告。
///
/// # 报文体只为"最新一份"保留
///
/// `payload` 这一 JSON 文本列在整个代码库里**只有一个读取点**：详情接口经
/// `latest_report_id` 主键 JOIN 取当前最新的那一份（见 `host_select`）。列表接口和
/// 历史曲线都只读 9 个摘要数值列，从不触碰它。
///
/// 若为**每一份**历史报文都保留完整 JSON，其中只有每台主机的最新一份会被读到，
/// 其余会持续增加 SQLite 主文件、WAL 与备份的体积。按 100 台、10 秒周期、
/// 保留 30 天计算已是 2590 万行，因此这里只为每台主机保留最新一份报文体。
///
/// 因此这里在**写入前**就判断这份报文会不会成为最新：
/// * 会 → 带 payload 插入，并把上一份 latest 的 payload 置空；
/// * 不会（补传的历史报文）→ 直接以 NULL payload 插入。
///
/// 在写入前决策而不是"先写后清"，是为了避免白写 17 KiB、再立刻清空
/// 所带来的额外页面与 WAL 写入。
///
/// 代价是失去"回溯任意历史时刻的完整硬件快照"这一能力——但它当前没有任何接口暴露，
/// 若将来需要，正确的做法是按更长的间隔留样，而不是每个采集周期都留一份。
pub async fn store_monitoring_report(
    pool: &DbPool,
    report: &AgentReport,
) -> anyhow::Result<(bool, DateTime<Utc>)> {
    let host_id = canonical_uuid(&report.host.id)?;
    let report_id = canonical_uuid(&report.report_id)?;
    let capabilities = serde_json::to_string(&report.capabilities)?;
    let received_at = Utc::now();
    let received_at_micros = database::to_epoch_micros(received_at);
    let mut tx = database::begin_write(pool).await?;

    // 一条语句同时完成三件事：确认主机存在、取出当前的 latest 指针与时间戳，
    // 取出当前的 latest 指针与时间戳，以及**判断身份列是否真的变了**。
    //
    // 最后一项是为了避免写放大：identity 与 capabilities 几乎从不变化，若每份上报都把
    // 它们原样重写一遍，代价有二——
    //   1. SQLite 同一时刻只有一个写者，多一次 UPDATE 会直接延长事务持锁时间；
    //   2. 无条件重写未变的 `capabilities` JSON 会制造多余的 WAL 与页面写入。
    // 把比较放进这条已经存在的语句里，不增加任何往返。
    let current = query(
        r#"
        SELECT latest_report_id,
               latest_collected_at,
               (name           IS NOT ?2
             OR os             IS NOT ?3
             OR os_version     IS NOT ?4
             OR kernel_version IS NOT ?5
             OR arch           IS NOT ?6
             OR agent_version  IS NOT ?7
             OR capabilities   IS NOT ?8) AS identity_changed
        FROM monitored_hosts
        WHERE host_id=?1 AND lifecycle_status='active'
        "#,
    )
    .bind(&host_id)
    .bind(report.host.name.trim())
    .bind(report.host.os.trim())
    .bind(report.host.os_version.as_deref())
    .bind(report.host.kernel_version.as_deref())
    .bind(report.host.arch.trim())
    .bind(report.host.agent_version.trim())
    .bind(&capabilities)
    .fetch_optional(tx.connection())
    .await?
    .ok_or_else(|| anyhow::Error::new(StoreReportError::HostNotActive))?;
    let previous_latest: Option<String> = current.try_get("latest_report_id")?;
    let previous_collected: Option<i64> = current.try_get("latest_collected_at")?;
    let identity_changed: bool = current.try_get("identity_changed")?;

    // 这份报文是否代表主机的"当前状态"。
    //
    // 断线恢复时 spool 会按时间升序补传一批**历史**报文，重放也可能把同一份再送一次。
    // 旧报文不该回写主机状态：否则一份小时前的报文能把刚更新的能力清单覆盖回去，
    // 而重写 `last_seen_at` 会让任何一次重放都把离线主机刷成 online。
    // SQLite persists timestamps at microsecond precision, so compare the same
    // normalized value used by history queries. For equal timestamps, history
    // orders `report_id DESC`; using the same secondary key here makes the
    // detail pointer independent of arrival order and guarantees it matches the
    // final history point.
    let collected_at_micros = database::to_epoch_micros(report.collected_at);
    let becomes_latest = previous_collected.is_none_or(|previous| {
        collected_at_micros > previous
            || (collected_at_micros == previous
                && previous_latest
                    .as_deref()
                    .is_none_or(|latest_id| report_id.as_str() > latest_id))
    });

    // 摘要列由 Rust 侧统一计算，SQL 只负责存放——聚合逻辑只有这一份实现。
    let metrics = report.metric_summary();
    // 只有会成为 latest 的报文才需要保留报文体。
    let payload = becomes_latest
        .then(|| serde_json::to_string(report))
        .transpose()?;
    let inserted = query(
        r#"
        INSERT INTO agent_metric_reports(
            report_id,host_id,schema_version,collected_at,interval_seconds,payload,
            cpu_usage_percent,memory_usage_percent,
            network_received_bytes_per_second,network_transmitted_bytes_per_second,
            disk_read_bytes_per_second,disk_written_bytes_per_second,
            max_temperature_celsius,gpu_utilization_percent,gpu_memory_usage_percent,
            received_at
        ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
        ON CONFLICT(report_id) DO NOTHING
        RETURNING received_at
        "#,
    )
    .bind(&report_id)
    .bind(&host_id)
    .bind(i32::from(report.schema_version))
    .bind(collected_at_micros)
    .bind(report.interval_seconds)
    .bind(payload.as_deref())
    .bind(metrics.cpu_usage_percent)
    .bind(metrics.memory_usage_percent)
    .bind(metrics.network_received_bytes_per_second)
    .bind(metrics.network_transmitted_bytes_per_second)
    .bind(metrics.disk_read_bytes_per_second)
    .bind(metrics.disk_written_bytes_per_second)
    .bind(metrics.max_temperature_celsius)
    .bind(metrics.gpu_utilization_percent)
    .bind(metrics.gpu_memory_usage_percent)
    .bind(received_at_micros)
    .fetch_optional(tx.connection())
    .await?;

    let (accepted, received_at) = if let Some(row) = inserted {
        (true, timestamp(&row, "received_at")?)
    } else {
        let row =
            query("SELECT received_at FROM agent_metric_reports WHERE report_id=?1 AND host_id=?2")
                .bind(&report_id)
                .bind(&host_id)
                .fetch_optional(tx.connection())
                .await?
                .ok_or_else(|| {
                    anyhow::Error::new(StoreReportError::ReportIdBelongsToAnotherHost)
                })?;
        (false, timestamp(&row, "received_at")?)
    };

    // A same-host duplicate is an idempotent replay, not a fresh heartbeat.
    // In particular it must not refresh last_seen_at or rewrite identity from
    // a body that happens to reuse an old report_id.
    if !accepted {
        tx.commit().await?;
        return Ok((false, received_at));
    }

    // 主机行的**唯一**一次写入。
    //
    // 拆成两条 UPDATE（INSERT 前写 identity + last_seen_at、INSERT 后写 latest_* 指针）
    // 更直观，但代价不只是多一次往返——两条 UPDATE 会延长 SQLite 单写者
    // 事务的持锁时间，并制造额外 WAL 与页面写入。
    //
    // 三组列各有各的写入条件，用 CASE 表达，避免为组合情况拼接 SQL：
    //   * identity/capabilities —— 仅在**确实变化**时写（见上面 SELECT 里的比较）。
    //     `ELSE <列自身>` 避免给未变的 JSON 文本重新赋值。
    //   * last_seen_at —— 只要这份**新插入**报文代表当前状态就刷新；同 report_id
    //     的重放已在上方提前返回，不能伪造一份新心跳。
    //   * latest_* 指针 —— 仅在报文确实新插入时推进。
    if becomes_latest {
        query(
            r#"
            UPDATE monitored_hosts SET
                name                    = CASE WHEN ?9 THEN ?2 ELSE name END,
                os                      = CASE WHEN ?9 THEN ?3 ELSE os END,
                os_version              = CASE WHEN ?9 THEN ?4 ELSE os_version END,
                kernel_version          = CASE WHEN ?9 THEN ?5 ELSE kernel_version END,
                arch                    = CASE WHEN ?9 THEN ?6 ELSE arch END,
                agent_version           = CASE WHEN ?9 THEN ?7 ELSE agent_version END,
                capabilities            = CASE WHEN ?9 THEN ?8 ELSE capabilities END,
                last_seen_at            = ?14,
                latest_report_id        = CASE WHEN ?10 THEN ?11 ELSE latest_report_id END,
                latest_collected_at     = CASE WHEN ?10 THEN ?12 ELSE latest_collected_at END,
                latest_interval_seconds = CASE WHEN ?10 THEN ?13 ELSE latest_interval_seconds END
            WHERE host_id=?1
            "#,
        )
        .bind(&host_id)
        .bind(report.host.name.trim())
        .bind(report.host.os.trim())
        .bind(report.host.os_version.as_deref())
        .bind(report.host.kernel_version.as_deref())
        .bind(report.host.arch.trim())
        .bind(report.host.agent_version.trim())
        .bind(&capabilities)
        .bind(identity_changed)
        .bind(accepted)
        .bind(&report_id)
        .bind(collected_at_micros)
        .bind(report.interval_seconds)
        .bind(received_at_micros)
        .execute(tx.connection())
        .await?;

        // 上一份报文的 payload 从此不再有任何读取路径，立即释放。
        // 走主键定位，代价与表规模无关。
        if accepted
            && let Some(previous) = previous_latest.filter(|previous| previous != &report_id)
        {
            query("UPDATE agent_metric_reports SET payload=NULL WHERE report_id=?1")
                .bind(previous)
                .execute(tx.connection())
                .await?;
        }
    }
    tx.commit().await?;
    Ok((accepted, received_at))
}

/// 主机列表。**不读取任何完整报告 JSON**——指标来自 JOIN 到的摘要数值列。
///
/// # 为什么必须有上限
///
/// 不带 `LIMIT` 一次返回全部主机在几百台时无感，但它是一条**随部署规模线性增长**
/// 的响应：每台主机都带着 capabilities 数组，几千台时单次响应就到数 MB，
/// 而控制台每 10 秒轮询一次，每次都要付这个代价。
///
/// `COUNT(*) OVER()` 在同一次扫描里带出总数，因此仍是一次往返——调用方据此可以
/// 告诉用户"还有多少没显示"，而不是**静默截断**。截断而不告知比不分页更糟。
pub async fn list_monitored_hosts(
    pool: &DbPool,
    limit: i64,
    offset: i64,
) -> anyhow::Result<(Vec<StoredHost>, i64)> {
    // `host_id` 作为最终决胜键：`last_seen_at` 会随上报持续变化、`name` 可能重复，
    // 只有再补一个唯一列才能保证同一查询的分页顺序是确定的（相邻两页不重不漏）。
    let rows = query(&host_select(
        false,
        "ORDER BY (h.lifecycle_status='active') DESC, h.last_seen_at DESC, h.name, h.host_id LIMIT ?1 OFFSET ?2",
    ))
    .bind(limit)
    .bind(offset.max(0))
    .fetch_all(pool)
    .await?;
    // 0 行时无从取窗口函数值，此时总数只能另算——但这只发生在空库或翻过头的页上，
    // 不是热路径。
    let total = match rows.first() {
        Some(row) => row.try_get::<i64, _>("total")?,
        None => query("SELECT COUNT(*) AS total FROM monitored_hosts")
            .fetch_one(pool)
            .await?
            .try_get("total")?,
    };
    let hosts = rows
        .into_iter()
        .map(|row| stored_host_from_row(row, false))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((hosts, total))
}

/// 主机详情。这是唯一需要完整报告体的读路径，报文从 JOIN 到的
/// `agent_metric_reports.payload` 取得——报文体只此一份，且只为最新报告保留。
pub async fn get_monitored_host(
    pool: &DbPool,
    host_id: &str,
) -> anyhow::Result<Option<StoredHost>> {
    let host_id = canonical_uuid(host_id)?;
    let row = query(&host_select(true, "WHERE h.host_id=?1"))
        .bind(host_id)
        .fetch_optional(pool)
        .await?;
    row.map(|row| stored_host_from_row(row, true)).transpose()
}

/// 查询历史曲线。返回 `None` 表示主机不存在（供上层返回 404）。
///
/// 存在性判断与数据查询合并为**一次往返**：限量 CTE 与目标主机做 LEFT JOIN，使得
///   * 主机不存在 → 0 行；
///   * 主机存在但无历史 → 1 行，且 `report_id` 为 NULL；
///   * 主机存在且有历史 → N 行。
///
/// 先调 `get_monitored_host` 判存在、再查历史是更直白的写法，但那不仅多一次往返，
/// 更糟的是存在性检查会连带把完整的 `latest_report` JSON 读出来——仅仅为了判断
/// 一行是否存在。
pub async fn monitoring_history(
    pool: &DbPool,
    host_id: &str,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    limit: i64,
) -> anyhow::Result<Option<Vec<StoredHistoryPoint>>> {
    let host_id = canonical_uuid(host_id)?;
    let from = from.map(database::to_epoch_micros);
    let to = to.map(database::to_epoch_micros);
    // 只读摘要数值列。取出并反序列化整份 payload 的代价是 limit 上限 1000 × 单份
    // 30-50KB，而响应里真正需要的只有下面这几个标量。
    //
    // 走 idx_agent_metric_reports_host_collected_at
    // (host_id, collected_at DESC, report_id)，它完全覆盖了 WHERE 与 ORDER BY。
    // 若把 9 个指标列都放进索引可以减少回表，但会明显扩大每次上报必须
    // 更新的索引；实测读取无收益而写入慢 37%，因此不建立覆盖索引。
    let rows = query(&format!(
        r#"
        WITH recent AS (
            SELECT host_id, report_id, collected_at, received_at, {plain_metrics}
            FROM agent_metric_reports
            WHERE host_id = ?1
              AND (?2 IS NULL OR collected_at >= ?2)
              AND (?3 IS NULL OR collected_at <= ?3)
            ORDER BY collected_at DESC, report_id DESC
            LIMIT ?4
        )
        SELECT r.report_id, r.collected_at, r.received_at, {metrics}
        FROM (SELECT host_id FROM monitored_hosts WHERE host_id = ?1) h
        LEFT JOIN recent r ON r.host_id = h.host_id
        ORDER BY r.collected_at DESC, r.report_id DESC
        "#,
        metrics = METRIC_COLUMNS
            .iter()
            .map(|column| format!("r.{column}"))
            .collect::<Vec<_>>()
            .join(","),
        plain_metrics = METRIC_COLUMNS.join(","),
    ))
    .bind(host_id)
    .bind(from)
    .bind(to)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    // 0 行 = 主机不存在。
    if rows.is_empty() {
        return Ok(None);
    }
    let mut points = rows
        .into_iter()
        // 主机存在但无历史时，LEFT JOIN 会补出一行全 NULL 的占位，跳过即可。
        .filter_map(|row| {
            let report_id: Option<String> = match row.try_get("report_id") {
                Ok(value) => value,
                Err(error) => return Some(Err(error.into())),
            };
            let report_id = report_id?;
            Some((|| {
                Ok(StoredHistoryPoint {
                    report_id,
                    collected_at: timestamp(&row, "collected_at")?,
                    received_at: timestamp(&row, "received_at")?,
                    metrics: metrics_from_row(&row)?,
                })
            })())
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    points.reverse();
    Ok(Some(points))
}

pub async fn revoke_monitored_host(pool: &DbPool, host_id: &str) -> anyhow::Result<bool> {
    let host_id = canonical_uuid(host_id)?;
    let now_micros = database::to_epoch_micros(Utc::now());
    let mut tx = database::begin_write(pool).await?;
    let host = query("SELECT lifecycle_status FROM monitored_hosts WHERE host_id=?1")
        .bind(&host_id)
        .fetch_optional(tx.connection())
        .await?;
    let Some(host) = host else {
        tx.rollback().await?;
        return Ok(false);
    };
    let status: String = host.try_get("lifecycle_status")?;
    let host_was_active = status == "active";
    if host_was_active {
        query(
            "UPDATE monitored_hosts SET lifecycle_status='revoked',revoked_at=?2 \
             WHERE host_id=?1",
        )
        .bind(&host_id)
        .bind(now_micros)
        .execute(tx.connection())
        .await?;
    }

    // Reassert the terminal state even when the host was already revoked. An
    // administrator may have issued a replacement invite and then changed
    // their mind; a second revoke must cancel that newer authorization rather
    // than returning 204 while leaving a path that can reactivate the host.
    let credentials = query(
        "UPDATE agent_credentials SET revoked_at=?2 \
         WHERE host_id=?1 AND revoked_at IS NULL",
    )
    .bind(&host_id)
    .bind(now_micros)
    .execute(tx.connection())
    .await?
    .rows_affected();
    let pairing_requests = query(
        r#"
        UPDATE agent_pairing_requests
        SET status='denied'
        WHERE (instance_id=?1 AND status='active')
           OR (requested_host_id=?1 AND status='pending')
        "#,
    )
    .bind(&host_id)
    .execute(tx.connection())
    .await?
    .rows_affected();
    let invites = query(
        r#"
        UPDATE agent_instance_invites
        SET status='revoked',revoked_at=?2
        WHERE instance_id=?1 AND status='pending'
        "#,
    )
    .bind(&host_id)
    .bind(now_micros)
    .execute(tx.connection())
    .await?
    .rows_affected();
    if host_was_active || credentials > 0 || pairing_requests > 0 || invites > 0 {
        crate::infra::database::insert_audit_in_transaction(
            tx.connection(),
            "monitoring.host.revoke",
            &host_id,
            Some(
                "host, issued credentials, pending invites and pairing requests persistently revoked",
            ),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// 单批删除的行数。
///
/// 取 10_000 是在两种成本之间取平衡：批次太小则往返次数与事务开销占比过高，
/// 太大则又回到"一个长事务"的老问题。
const RETENTION_BATCH: i64 = 10_000;

/// 单次清理的批次上限，防止异常情况下无限循环。
/// 10_000 × 1_000 = 一千万行，远超任何一天的正常增量。
const MAX_RETENTION_BATCHES: usize = 1_000;

/// 按保留期清理历史报告。
///
/// **保留每台主机当前的最新一份报告**，即使它已超出保留期。报文体只存在于本表，
/// 且只有最新一份非空，详情接口通过 `latest_report_id` 关联到它；若把这一份也删掉，
/// 长期离线主机的详情页就会变成空白。这类主机每台只多留一行，代价可以忽略。
///
/// 外键的 `ON DELETE SET NULL` 是第二道保险：即便这里的条件将来被改错，也只会让
/// 引用变成 NULL，而不会留下悬空 id。
pub async fn prune_monitoring_history(pool: &DbPool, retention_days: i64) -> anyhow::Result<u64> {
    let retention_days = retention_days.clamp(1, 3650);
    let cutoff = database::to_epoch_micros(Utc::now() - chrono::Duration::days(retention_days));
    let mut removed = 0_u64;

    // 分批删除，而不是一条 DELETE 扫完整张表。
    //
    // 按 100 台 / 10 秒 / 保留 30 天计算，每天约清掉 86 万行。SQLite
    // 只允许一个写者：一条大 DELETE 会长时间占用写锁、制造巨大 WAL，并让
    // Agent 上报在 busy timeout 内排队。每批使用独立的短 `BEGIN IMMEDIATE`
    // 事务，批次间释放写锁，WAL 模式下读路径仍可继续。
    //
    // DELETE 后的页面进入 freelist 并供后续写入复用，不会立即缩小数据库
    // 文件；若运维上确实需要把大量空间归还给文件系统，应在低峰窗口单独执行
    // VACUUM，而不应把它放进日常保留期任务。
    for _ in 0..MAX_RETENTION_BATCHES {
        let mut tx = database::begin_write(pool).await?;
        let affected = query(
            r#"
            DELETE FROM agent_metric_reports
            WHERE report_id IN (
                SELECT r.report_id
                FROM agent_metric_reports AS r
                WHERE r.received_at < ?1
                  AND NOT EXISTS (
                      SELECT 1 FROM monitored_hosts AS h WHERE h.latest_report_id = r.report_id
                  )
                LIMIT ?2
            )
            "#,
        )
        .bind(cutoff)
        .bind(RETENTION_BATCH)
        .execute(tx.connection())
        .await?
        .rows_affected();
        tx.commit().await?;

        removed += affected;
        // 不足一批说明已经删干净了。
        if affected < RETENTION_BATCH as u64 {
            break;
        }
        // 批次之间让出一小段时间，让排队的上报获得写锁。
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Ok(removed)
}

/// 摘要数值列。摘要**只存在于** `agent_metric_reports`，主机侧通过
/// `latest_report_id` 主键 JOIN 取用——不在 `monitored_hosts` 再存一份副本，
/// 从根本上消除两份摘要之间漂移的可能。
const METRIC_COLUMNS: [&str; 9] = [
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

/// 构造主机查询。
///
/// `with_payload` 为 false 时**完全不触碰** payload 这一 JSON 列，这正是列表接口
/// 廉价的原因：否则每行都要传输并反序列化 30-50KB 的报告，仅为得到下面这几个标量。
fn host_select(with_payload: bool, suffix: &str) -> String {
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
               h.arch,h.agent_version,h.lifecycle_status,
               h.capabilities,
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

/// 从结果行读取摘要。列为 NULL 时对应 `None`，与 Rust 侧 `metric_summary()`
/// 在空集合上 `reduce()` 得到 `None` 的语义一致。
fn metrics_from_row(row: &SqliteRow) -> anyhow::Result<MetricSummary> {
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

fn stored_host_from_row(row: SqliteRow, with_payload: bool) -> anyhow::Result<StoredHost> {
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
        identity: HostIdentity {
            id: row.try_get("host_id")?,
            name: row.try_get("name")?,
            os: row.try_get("os")?,
            os_version: row.try_get("os_version")?,
            kernel_version: row.try_get("kernel_version")?,
            arch: row.try_get("arch")?,
            agent_version: row.try_get("agent_version")?,
        },
        lifecycle_status: row.try_get("lifecycle_status")?,
        capabilities,
        registered_at: timestamp(&row, "registered_at")?,
        last_seen_at: timestamp(&row, "last_seen_at")?,
        latest_collected_at: optional_timestamp(&row, "latest_collected_at")?,
        latest_interval_seconds: row.try_get("latest_interval_seconds")?,
        metrics,
        latest,
    })
}

fn timestamp(row: &SqliteRow, column: &str) -> anyhow::Result<DateTime<Utc>> {
    database::from_epoch_micros(row.try_get(column)?)
}

fn optional_timestamp(row: &SqliteRow, column: &str) -> anyhow::Result<Option<DateTime<Utc>>> {
    row.try_get::<Option<i64>, _>(column)?
        .map(database::from_epoch_micros)
        .transpose()
}

fn canonical_uuid(value: &str) -> anyhow::Result<String> {
    let parsed = uuid::Uuid::parse_str(value)?;
    let canonical = parsed.to_string();
    anyhow::ensure!(
        canonical == value,
        "UUID must use canonical lowercase, hyphenated text"
    );
    Ok(canonical)
}
