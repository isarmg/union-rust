use sqlx_core::{query::query, row::Row};
use sqlx_sqlite::SqliteConnection;

use super::super::{begin_write, to_epoch_micros};
use super::*;

/// Audit retention must not monopolize SQLite's sole writer while deleting a
/// large backlog. One thousand narrow rows keeps transaction duration short
/// without turning normal retention into hundreds of round trips.
const AUDIT_RETENTION_BATCH: i64 = 1_000;

fn context() -> AuditContext {
    AUDIT_CONTEXT
        .try_with(Clone::clone)
        .unwrap_or(AuditContext {
            actor: "system".to_string(),
            request_id: None,
        })
}

pub async fn insert_audit(
    pool: &DbPool,
    action: &str,
    target: &str,
    detail: Option<&str>,
) -> anyhow::Result<()> {
    let context = context();
    let mut tx = begin_write(pool).await?;
    insert_audit_with_context(tx.connection(), &context, action, target, detail).await?;
    tx.commit().await?;
    Ok(())
}

async fn insert_audit_with_context(
    connection: &mut SqliteConnection,
    context: &AuditContext,
    action: &str,
    target: &str,
    detail: Option<&str>,
) -> anyhow::Result<()> {
    query("INSERT INTO audit_logs(action,target,detail,actor,request_id) VALUES(?,?,?,?,?)")
        .bind(action)
        .bind(target)
        .bind(detail)
        .bind(&context.actor)
        .bind(&context.request_id)
        .execute(connection)
        .await?;
    Ok(())
}

/// 在调用方已有的事务中写入审计记录。
///
/// 凭据轮换、主机注销等数据库内变更必须与审计记录原子提交：否则业务变更已经生效，
/// 客户端却会因为后续审计失败收到 500，甚至拿不到只返回一次的新凭据。
pub async fn insert_audit_in_transaction(
    connection: &mut SqliteConnection,
    action: &str,
    target: &str,
    detail: Option<&str>,
) -> anyhow::Result<()> {
    let context = context();
    insert_audit_with_context(connection, &context, action, target, detail).await
}

/// Read an immutable page of audit records using an id cursor.
///
/// Offset pagination is unstable while new events are inserted at the head;
/// `before_id` gives callers a repeatable export without duplicates or gaps.
pub async fn list_audit_logs(
    pool: &DbPool,
    before_id: Option<i64>,
    limit: i64,
) -> anyhow::Result<AuditLogPage> {
    let limit = limit.clamp(1, 500);
    // Keep the optional cursor out of an `? IS NULL OR id < ?` predicate.
    // SQLite cannot turn that shape into a rowid range and scans the complete
    // audit table for every later page, defeating the purpose of keyset
    // pagination. The cursor form below uses the INTEGER PRIMARY KEY directly.
    let rows = if let Some(before_id) = before_id {
        query(
            r#"
            SELECT id,action,target,detail,actor,request_id,created_at
            FROM audit_logs
            WHERE id < ?1
            ORDER BY id DESC
            LIMIT ?2
            "#,
        )
        .bind(before_id)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?
    } else {
        query(
            r#"
            SELECT id,action,target,detail,actor,request_id,created_at
            FROM audit_logs
            ORDER BY id DESC
            LIMIT ?1
            "#,
        )
        .bind(limit + 1)
        .fetch_all(pool)
        .await?
    };
    let has_more = rows.len() > limit as usize;
    let entries = rows
        .into_iter()
        .take(limit as usize)
        .map(|row| {
            Ok(AuditLogEntry {
                id: row.try_get("id")?,
                action: row.try_get("action")?,
                target: row.try_get("target")?,
                detail: row.try_get("detail")?,
                actor: row.try_get("actor")?,
                request_id: row.try_get("request_id")?,
                created_at: super::super::from_epoch_micros(row.try_get("created_at")?)?,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let next_before_id = if has_more {
        entries.last().map(|entry| entry.id)
    } else {
        None
    };
    Ok(AuditLogPage {
        entries,
        next_before_id,
    })
}

pub async fn prune_audit_history(pool: &DbPool, retention_days: i64) -> anyhow::Result<u64> {
    let days = retention_days.clamp(7, 3650);
    let cutoff = to_epoch_micros(chrono::Utc::now() - chrono::Duration::days(days));
    let mut total = 0_u64;

    loop {
        let mut tx = begin_write(pool).await?;
        let removed = query(
            r#"
            DELETE FROM audit_logs
            WHERE id IN (
                SELECT id
                FROM audit_logs
                WHERE created_at < ?
                ORDER BY created_at, id
                LIMIT ?
            )
            "#,
        )
        .bind(cutoff)
        .bind(AUDIT_RETENTION_BATCH)
        .execute(tx.connection())
        .await?
        .rows_affected();
        tx.commit().await?;
        total = total
            .checked_add(removed)
            .ok_or_else(|| anyhow::anyhow!("audit retention removed-row count overflow"))?;

        if removed < AUDIT_RETENTION_BATCH as u64 {
            return Ok(total);
        }
        // Committing releases SQLite's cross-process writer lock. Yield once
        // before reacquiring our in-process gate so report/config writes that
        // became ready during this batch can make progress.
        tokio::task::yield_now().await;
    }
}
