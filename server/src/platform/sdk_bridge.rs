use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use sarmg_platform_events::{EventEnvelope, EventError, EventFuture, EventPublisher};
use sarmg_platform_sdk::{
    Actor, AuditApi, AuthorizationApi, ConfigurationApi, LogApi, LogLevel, NotificationApi,
    PlatformContext, PlatformError, PlatformFuture, ServiceDiscoveryApi, TaskApi,
};
use tokio::sync::RwLock;

use super::{
    configuration::ConfigurationRegistry,
    event_bus::EventBus,
    notifications::{NotificationCenter, NotificationSeverity},
    tasks::TaskScheduler,
};

#[derive(Clone)]
pub(super) struct SdkPlatformContext {
    module: String,
    configuration: ConfigurationRegistry,
    tasks: TaskScheduler,
    notifications: NotificationCenter,
    events: EventBus,
    services: Arc<RwLock<BTreeMap<String, String>>>,
}

impl SdkPlatformContext {
    pub fn new(
        module: String,
        configuration: ConfigurationRegistry,
        tasks: TaskScheduler,
        notifications: NotificationCenter,
        events: EventBus,
        services: Arc<RwLock<BTreeMap<String, String>>>,
    ) -> Self {
        Self {
            module,
            configuration,
            tasks,
            notifications,
            events,
            services,
        }
    }
}

impl PlatformContext for SdkPlatformContext {
    fn plugin_id(&self) -> &str {
        &self.module
    }

    fn configuration(&self) -> &dyn ConfigurationApi {
        self
    }

    fn authorization(&self) -> &dyn AuthorizationApi {
        self
    }

    fn audit(&self) -> &dyn AuditApi {
        self
    }

    fn logs(&self) -> &dyn LogApi {
        self
    }

    fn tasks(&self) -> &dyn TaskApi {
        self
    }

    fn notifications(&self) -> &dyn NotificationApi {
        self
    }

    fn services(&self) -> &dyn ServiceDiscoveryApi {
        self
    }

    fn events(&self) -> &dyn EventPublisher {
        self
    }
}

impl ConfigurationApi for SdkPlatformContext {
    fn get<'a>(
        &'a self,
        key: &'a str,
    ) -> PlatformFuture<'a, Result<Option<serde_json::Value>, PlatformError>> {
        Box::pin(async move {
            let Some(configuration) = self.configuration.raw_value(&self.module).await else {
                return Err(PlatformError::Unavailable(
                    "configuration is not registered".into(),
                ));
            };
            let value = if key.starts_with('/') {
                configuration.pointer(key).cloned()
            } else {
                configuration.get(key).cloned()
            };
            Ok(value)
        })
    }
}

impl AuthorizationApi for SdkPlatformContext {
    fn authorize(&self, actor: &Actor, permission: &str) -> Result<(), PlatformError> {
        if actor.permissions.contains("*") || actor.permissions.contains(permission) {
            Ok(())
        } else {
            Err(PlatformError::PermissionDenied(permission.into()))
        }
    }
}

impl AuditApi for SdkPlatformContext {
    fn record<'a>(
        &'a self,
        action: &'a str,
        actor: &'a Actor,
        fields: serde_json::Value,
    ) -> PlatformFuture<'a, Result<(), PlatformError>> {
        Box::pin(async move {
            if action.trim().is_empty() || action.len() > 160 {
                return Err(PlatformError::Invalid("audit action".into()));
            }
            // HTTP-triggered calls already have Union's durable gateway audit record. Keep a
            // structured diagnostic for internal in-process actions without implying that this
            // helper itself writes another durable audit entry.
            tracing::info!(
                module = self.module,
                action,
                actor = actor.subject,
                correlation_id = actor.correlation_id,
                fields = %fields,
                "plugin audit"
            );
            Ok(())
        })
    }
}

impl LogApi for SdkPlatformContext {
    fn write(
        &self,
        level: LogLevel,
        message: &str,
        fields: serde_json::Value,
    ) -> Result<(), PlatformError> {
        if message.len() > 4096 || message.chars().any(|character| character == '\0') {
            return Err(PlatformError::Invalid("log message".into()));
        }
        match level {
            LogLevel::Debug => tracing::debug!(module = self.module, fields = %fields, "{message}"),
            LogLevel::Info => tracing::info!(module = self.module, fields = %fields, "{message}"),
            LogLevel::Warn => tracing::warn!(module = self.module, fields = %fields, "{message}"),
            LogLevel::Error => tracing::error!(module = self.module, fields = %fields, "{message}"),
        }
        Ok(())
    }
}

impl TaskApi for SdkPlatformContext {
    fn enqueue<'a>(
        &'a self,
        task: &'a str,
        payload: serde_json::Value,
    ) -> PlatformFuture<'a, Result<String, PlatformError>> {
        Box::pin(async move {
            let task = if task.starts_with(&format!("{}.", self.module)) {
                task.to_string()
            } else {
                format!("{}.{}", self.module, task)
            };
            self.tasks
                .trigger_trusted(&format!("plugin:{}", self.module), &task, payload)
                .await
                .map(|run| run.id)
                .map_err(|error| PlatformError::Operation(error.to_string()))
        })
    }
}

impl NotificationApi for SdkPlatformContext {
    fn notify<'a>(
        &'a self,
        channel: &'a str,
        message: &'a str,
    ) -> PlatformFuture<'a, Result<(), PlatformError>> {
        Box::pin(async move {
            self.notifications
                .publish(&self.module, NotificationSeverity::Info, channel, message)
                .await
                .map(|_| ())
                .map_err(|error| PlatformError::Operation(error.to_string()))
        })
    }
}

impl ServiceDiscoveryApi for SdkPlatformContext {
    fn resolve<'a>(
        &'a self,
        service: &'a str,
    ) -> PlatformFuture<'a, Result<String, PlatformError>> {
        Box::pin(async move {
            self.services
                .read()
                .await
                .get(service)
                .cloned()
                .ok_or_else(|| {
                    PlatformError::Unavailable(format!("service not available: {service}"))
                })
        })
    }
}

impl EventPublisher for SdkPlatformContext {
    fn publish<'a>(&'a self, event: &'a EventEnvelope) -> EventFuture<'a, Result<(), EventError>> {
        Box::pin(async move {
            event.validate()?;
            if event.producer != self.module {
                return Err(EventError::Invalid(
                    "producer does not match plugin context".into(),
                ));
            }
            self.events
                .publish(
                    &self.module,
                    &format!("{}.v{}", event.topic, event.version),
                    event.payload.clone(),
                )
                .await
                .map(|_| ())
                .map_err(|error| EventError::Transport(error.to_string()))
        })
    }
}

pub(super) fn actor(subject: String, permissions: Vec<String>, correlation_id: String) -> Actor {
    Actor {
        subject,
        permissions: permissions.into_iter().collect::<BTreeSet<_>>(),
        correlation_id,
    }
}
