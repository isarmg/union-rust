async fn insert_agent_credential(
    connection: &mut SqliteConnection,
    host_id: &str,
    token_hash: &str,
) -> anyhow::Result<()> {
    let now_micros = database::to_epoch_micros(Utc::now());
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
) -> anyhow::Result<CreateInviteResult> {
    let invite_id = canonical_uuid(invite_id)?;
    let instance_id = canonical_uuid(instance_id)?;
    let now_micros = database::to_epoch_micros(Utc::now());
    let mut tx = database::begin_write(pool).await?;


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
                 WHEN i.status='cancelled' THEN 'cancelled'
                 WHEN i.status='pending' AND i.expires_at <= ?1 THEN 'expired'
                 ELSE i.status
               END AS status
        FROM agent_instance_invites i
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

pub async fn cancel_agent_instance_invite(
    pool: &DbPool,
    invite_id: &str,
) -> anyhow::Result<CancelInviteResult> {
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
        return Ok(CancelInviteResult::NotFound);
    };
    let status: String = row.try_get("status")?;
    if status != "pending" {
        tx.rollback().await?;
        return Ok(CancelInviteResult::NotPending);
    }
    let instance_id: String = row.try_get("instance_id")?;
    query(
        "UPDATE agent_instance_invites SET status='cancelled',cancelled_at=?2 \
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
    Ok(CancelInviteResult::Cancelled)
}

pub async fn create_agent_pairing_request(
    pool: &DbPool,
    request_id: &str,
    request: &AgentPairingRequest,
    expires_at: DateTime<Utc>,
) -> anyhow::Result<CreatePairingResult> {
    // Bound each cleanup transaction as well as the retained live set. This is
    // important if an unexpected oversized backlog exists: one new pairing
    // request must not monopolize SQLite's single writer while deleting every
    // expired row at once.
    const CLEANUP_BATCH_SIZE: i64 = 512;
    const EXISTING_BY_POLLING_SECRET: &str = r#"
        SELECT request_id,requested_host_id,
               os,os_version,kernel_version,arch,agent_version,token_hash,status,expires_at
        FROM agent_pairing_requests
        WHERE polling_secret_hash=?1
    "#;
    let request_id = canonical_uuid(request_id)?;
    let requested_host_id = canonical_uuid(&request.host.id)?;
    let now = Utc::now();
    let now_micros = database::to_epoch_micros(now);
    let stale_denied_before =
        database::to_epoch_micros(now - chrono::Duration::days(30));
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
        WHERE request_id IN (
            SELECT request_id
            FROM agent_pairing_requests
            WHERE (status='pending' AND expires_at <= ?1)
               OR (status='denied' AND created_at < ?2)
            ORDER BY CASE
                       WHEN status='pending' THEN expires_at
                       ELSE created_at
                     END,
                     request_id
            LIMIT ?3
        )
        "#,
    )
    .bind(now_micros)
    .bind(stale_denied_before)
    .bind(CLEANUP_BATCH_SIZE)
    .execute(tx.connection())
    .await?;
    let pending: i64 = query(
        "SELECT COUNT(*) AS count FROM ( \
             SELECT 1 FROM agent_pairing_requests \
             WHERE status='pending' AND expires_at > ?1 LIMIT ?2 \
         )",
    )
    .bind(now_micros)
    .bind(MAX_PENDING_PAIRING_REQUESTS)
    .fetch_one(tx.connection())
    .await?
    .try_get("count")?;
    if pending >= MAX_PENDING_PAIRING_REQUESTS {
        // Preserve any bounded cleanup performed above even though this
        // request cannot allocate another live slot.
        tx.commit().await?;
        return Ok(CreatePairingResult::AtCapacity);
    }
    let inserted = query(
        r#"
        INSERT INTO agent_pairing_requests(
            request_id,requested_host_id,os,os_version,kernel_version,arch,
            agent_version,token_hash,polling_secret_hash,expires_at,created_at
        ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
        ON CONFLICT DO NOTHING
        RETURNING request_id
        "#,
    )
    .bind(&request_id)
    .bind(&requested_host_id)
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
                 ELSE p.status
               END AS status
        FROM agent_pairing_requests p
        WHERE p.request_id=?1 AND p.polling_secret_hash=?2
        "#,
    )
    .bind(&request_id)
    .bind(polling_secret_hash)
    .bind(now_micros)
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        let raw_status: String = row.try_get("status")?;
        let status = PairingStatus::try_from(raw_status.as_str()).map_err(|()| {
            anyhow::anyhow!("database returned invalid pairing status {raw_status}")
        })?;
        let instance_id = if status == PairingStatus::Active {
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
        SELECT p.request_id,p.os,p.arch,p.agent_version,p.expires_at,
               CASE
                 WHEN p.status='pending' AND p.expires_at <= ?2 THEN 'expired'
                 WHEN p.status='pending' THEN 'waiting'
                 ELSE p.status
               END AS status
        FROM agent_pairing_requests p
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
    activate_agent_pairing_with_clock(pool, request_id, activation_code_hash, Utc::now).await
}

async fn activate_agent_pairing_with_clock<F>(
    pool: &DbPool,
    request_id: &str,
    activation_code_hash: &str,
    clock: F,
) -> anyhow::Result<ActivatePairingResult>
where
    F: FnOnce() -> DateTime<Utc> + Send,
{
    let request_id = canonical_uuid(request_id)?;
    let mut tx = database::begin_write(pool).await?;
    // Expiration belongs to the serialized read/check/write decision. Taking
    // the timestamp before waiting for the writer gate would let a request
    // queued before expiry activate after the code has already expired.
    let now = clock();
    let now_micros = database::to_epoch_micros(now);
    let pairing = query(
        r#"
        SELECT request_id,os,os_version,kernel_version,arch,
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
                FROM agent_credentials c
                WHERE c.host_id=?1 AND c.token_hash=?2
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
        INSERT INTO monitored_hosts(host_id,name,os,os_version,kernel_version,arch,agent_version,registered_at,last_seen_at)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8)
        "#,
    )
    .bind(&instance_id)
    .bind(invite.try_get::<String, _>("display_name")?)
    .bind(pairing.try_get::<String, _>("os")?)
    .bind(pairing.try_get::<Option<String>, _>("os_version")?)
    .bind(pairing.try_get::<Option<String>, _>("kernel_version")?)
    .bind(pairing.try_get::<String, _>("arch")?)
    .bind(pairing.try_get::<String, _>("agent_version")?)
    .bind(now_micros)
    .execute(tx.connection())
    .await?;
    insert_agent_credential(tx.connection(), &instance_id, &token_hash).await?;

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
