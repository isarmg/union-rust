use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{RwLock, broadcast};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventContract {
    #[serde(default)]
    pub publishes: Vec<String>,
    #[serde(default)]
    pub subscribes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlatformEvent {
    pub sequence: u64,
    pub topic: String,
    pub source: String,
    pub emitted_at: String,
    pub payload: Value,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EventBusError {
    #[error("invalid event topic: {0}")]
    InvalidTopic(String),
    #[error("module {module} did not declare permission to publish {topic}")]
    PublishNotDeclared { module: String, topic: String },
    #[error("module {module} did not declare a subscription to {topic}")]
    SubscriptionNotDeclared { module: String, topic: String },
}

struct EventBusState {
    contracts: RwLock<BTreeMap<String, EventContract>>,
    sequence: std::sync::atomic::AtomicU64,
    sender: broadcast::Sender<PlatformEvent>,
}

#[derive(Clone)]
pub struct EventBus {
    state: Arc<EventBusState>,
}

impl Default for EventBus {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self {
            state: Arc::new(EventBusState {
                contracts: RwLock::new(BTreeMap::new()),
                sequence: std::sync::atomic::AtomicU64::new(0),
                sender,
            }),
        }
    }
}

impl EventBus {
    pub async fn register_module(
        &self,
        module: &str,
        contract: EventContract,
    ) -> Result<(), EventBusError> {
        for topic in contract.publishes.iter().chain(&contract.subscribes) {
            validate_topic(topic)?;
        }
        self.state
            .contracts
            .write()
            .await
            .insert(module.into(), contract);
        Ok(())
    }

    pub async fn unregister_module(&self, module: &str) {
        self.state.contracts.write().await.remove(module);
    }

    pub async fn publish(
        &self,
        module: &str,
        topic: &str,
        payload: Value,
    ) -> Result<PlatformEvent, EventBusError> {
        validate_topic(topic)?;
        let contracts = self.state.contracts.read().await;
        let allowed = module == "platform"
            || contracts
                .get(module)
                .is_some_and(|contract| contract.publishes.iter().any(|entry| entry == topic));
        if !allowed {
            return Err(EventBusError::PublishNotDeclared {
                module: module.into(),
                topic: topic.into(),
            });
        }
        drop(contracts);
        let event = PlatformEvent {
            sequence: self
                .state
                .sequence
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1,
            topic: topic.into(),
            source: module.into(),
            emitted_at: chrono::Utc::now().to_rfc3339(),
            payload,
        };
        let _ = self.state.sender.send(event.clone());
        Ok(event)
    }

    pub async fn subscribe(
        &self,
        module: &str,
        topic: &str,
    ) -> Result<EventSubscription, EventBusError> {
        validate_topic(topic)?;
        let allowed = module == "platform"
            || self
                .state
                .contracts
                .read()
                .await
                .get(module)
                .is_some_and(|contract| contract.subscribes.iter().any(|entry| entry == topic));
        if !allowed {
            return Err(EventBusError::SubscriptionNotDeclared {
                module: module.into(),
                topic: topic.into(),
            });
        }
        Ok(EventSubscription {
            topic: topic.into(),
            receiver: self.state.sender.subscribe(),
        })
    }
}

pub struct EventSubscription {
    topic: String,
    receiver: broadcast::Receiver<PlatformEvent>,
}

impl EventSubscription {
    pub async fn recv(&mut self) -> Result<PlatformEvent, broadcast::error::RecvError> {
        loop {
            let event = self.receiver.recv().await?;
            if event.topic == self.topic {
                return Ok(event);
            }
        }
    }
}

fn validate_topic(topic: &str) -> Result<(), EventBusError> {
    let valid = !topic.is_empty()
        && topic.len() <= 160
        && topic.split('.').count() >= 3
        && topic.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(EventBusError::InvalidTopic(topic.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn declared_topics_are_scoped_and_delivered() {
        let bus = EventBus::default();
        bus.register_module(
            "photo-backup",
            EventContract {
                publishes: vec!["photo-backup.asset.created.v1".into()],
                subscribes: vec!["platform.module.enabled.v1".into()],
            },
        )
        .await
        .unwrap();
        let mut receiver = bus
            .subscribe("photo-backup", "platform.module.enabled.v1")
            .await
            .unwrap();
        bus.publish(
            "platform",
            "platform.module.enabled.v1",
            serde_json::json!({"id":"photo-backup"}),
        )
        .await
        .unwrap();
        assert_eq!(receiver.recv().await.unwrap().sequence, 1);
        assert!(
            bus.publish(
                "photo-backup",
                "dufs.file.deleted.v1",
                serde_json::Value::Null
            )
            .await
            .is_err()
        );
    }
}
