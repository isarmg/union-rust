use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    pub id: String,
    pub source: String,
    pub severity: NotificationSeverity,
    pub title: String,
    pub message: String,
    pub created_at: String,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<String>,
}

#[derive(Clone, Default)]
pub struct NotificationCenter {
    entries: Arc<RwLock<BTreeMap<String, Notification>>>,
}

impl NotificationCenter {
    pub async fn publish(
        &self,
        source: &str,
        severity: NotificationSeverity,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> anyhow::Result<Notification> {
        let title = title.into();
        let message = message.into();
        if source.trim().is_empty()
            || title.trim().is_empty()
            || title.len() > 160
            || message.len() > 4096
        {
            anyhow::bail!("notification source/title/message is invalid")
        }
        let notification = Notification {
            id: uuid::Uuid::new_v4().to_string(),
            source: source.into(),
            severity,
            title,
            message,
            created_at: chrono::Utc::now().to_rfc3339(),
            acknowledged_by: None,
            acknowledged_at: None,
        };
        self.entries
            .write()
            .await
            .insert(notification.id.clone(), notification.clone());
        Ok(notification)
    }

    pub async fn list(&self) -> Vec<Notification> {
        self.entries.read().await.values().rev().cloned().collect()
    }

    pub async fn acknowledge(&self, id: &str, principal: &str) -> Option<Notification> {
        let mut entries = self.entries.write().await;
        let notification = entries.get_mut(id)?;
        if notification.acknowledged_at.is_none() {
            notification.acknowledged_by = Some(principal.into());
            notification.acknowledged_at = Some(chrono::Utc::now().to_rfc3339());
        }
        Some(notification.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn notifications_can_be_listed_and_acknowledged_idempotently() {
        let center = NotificationCenter::default();
        let created = center
            .publish(
                "platform",
                NotificationSeverity::Warning,
                "Module degraded",
                "health probe failed",
            )
            .await
            .unwrap();
        assert_eq!(center.list().await.len(), 1);
        let first = center.acknowledge(&created.id, "admin").await.unwrap();
        let second = center.acknowledge(&created.id, "other").await.unwrap();
        assert_eq!(first.acknowledged_by.as_deref(), Some("admin"));
        assert_eq!(second.acknowledged_by.as_deref(), Some("admin"));
    }
}
