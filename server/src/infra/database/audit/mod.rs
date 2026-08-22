//! 审计日志持久化与请求上下文。

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::DbPool;

#[derive(Clone)]
pub struct AuditContext {
    pub actor: String,
    pub request_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuditLogEntry {
    pub id: i64,
    pub action: String,
    pub target: String,
    pub detail: Option<String>,
    pub actor: String,
    pub request_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct AuditLogPage {
    pub entries: Vec<AuditLogEntry>,
    /// Pass this value as `before_id` to read the next older page.
    pub next_before_id: Option<i64>,
}

tokio::task_local! { static AUDIT_CONTEXT: AuditContext; }

pub async fn with_audit_context<F>(context: AuditContext, future: F) -> F::Output
where
    F: std::future::Future,
{
    AUDIT_CONTEXT.scope(context, future).await
}

mod operations;
pub use operations::{
    insert_audit, insert_audit_in_transaction, list_audit_logs, prune_audit_history,
};
