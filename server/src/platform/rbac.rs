use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// A permission is data owned by the platform or by exactly one module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionDefinition {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub default_roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisteredPermission {
    #[serde(flatten)]
    pub definition: PermissionDefinition,
    pub owner: String,
}

#[derive(Debug, Clone)]
pub struct AuthenticatedPrincipal {
    pub username: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PermissionError {
    #[error("invalid permission id: {0}")]
    InvalidId(String),
    #[error("permission description must not be empty: {0}")]
    EmptyDescription(String),
    #[error("module {module} may only register permissions below {module}.*: {permission}")]
    WrongNamespace { module: String, permission: String },
    #[error("permission is already owned by another component: {0}")]
    Duplicate(String),
}

#[derive(Default)]
struct PermissionState {
    definitions: BTreeMap<String, RegisteredPermission>,
    roles: BTreeMap<String, Vec<String>>,
    assignments: BTreeMap<String, Vec<String>>,
}

/// Product-neutral RBAC registry. The current single local administrator receives the built-in
/// `admin` role, but the model already supports multiple roles and principals.
#[derive(Clone, Default)]
pub struct PermissionRegistry {
    state: Arc<RwLock<PermissionState>>,
}

impl PermissionRegistry {
    pub fn for_administrator(administrator: &str) -> Result<Self, PermissionError> {
        let definitions = [
            definition("platform.modules.read", "Read bundled module metadata"),
            definition(
                "platform.modules.manage",
                "Rescan, enable or disable bundled modules",
            ),
            definition("platform.permissions.read", "Read the permission catalog"),
            definition("platform.tasks.read", "Read registered tasks and task runs"),
            definition(
                "platform.tasks.trigger",
                "Trigger registered platform tasks",
            ),
            definition("platform.notifications.read", "Read platform notifications"),
            definition(
                "platform.notifications.ack",
                "Acknowledge platform notifications",
            ),
        ];
        for definition in &definitions {
            validate_definition(definition)?;
        }
        let mut state = PermissionState::default();
        for definition in definitions {
            state.definitions.insert(
                definition.id.clone(),
                RegisteredPermission {
                    definition,
                    owner: "platform".into(),
                },
            );
        }
        state.roles.insert("admin".into(), vec!["*".into()]);
        state
            .assignments
            .insert(administrator.to_string(), vec!["admin".into()]);
        Ok(Self {
            state: Arc::new(RwLock::new(state)),
        })
    }

    pub async fn initialize(&self, administrator: &str) -> Result<(), PermissionError> {
        self.register_owned(
            "platform",
            &[
                definition("platform.modules.read", "Read bundled module metadata"),
                definition(
                    "platform.modules.manage",
                    "Rescan, enable or disable bundled modules",
                ),
                definition("platform.permissions.read", "Read the permission catalog"),
                definition("platform.tasks.read", "Read registered tasks and task runs"),
                definition(
                    "platform.tasks.trigger",
                    "Trigger registered platform tasks",
                ),
                definition("platform.notifications.read", "Read platform notifications"),
                definition(
                    "platform.notifications.ack",
                    "Acknowledge platform notifications",
                ),
            ],
        )
        .await?;
        let mut state = self.state.write().await;
        state.roles.insert("admin".into(), vec!["*".into()]);
        state
            .assignments
            .insert(administrator.to_string(), vec!["admin".into()]);
        Ok(())
    }

    pub async fn register_module(
        &self,
        module: &str,
        definitions: &[PermissionDefinition],
    ) -> Result<(), PermissionError> {
        for definition in definitions {
            if !definition.id.starts_with(&format!("{module}.")) {
                return Err(PermissionError::WrongNamespace {
                    module: module.into(),
                    permission: definition.id.clone(),
                });
            }
        }
        self.register_owned(module, definitions).await
    }

    async fn register_owned(
        &self,
        owner: &str,
        definitions: &[PermissionDefinition],
    ) -> Result<(), PermissionError> {
        for definition in definitions {
            validate_definition(definition)?;
        }
        let mut state = self.state.write().await;
        for definition in definitions {
            if state
                .definitions
                .get(&definition.id)
                .is_some_and(|existing| existing.owner != owner)
            {
                return Err(PermissionError::Duplicate(definition.id.clone()));
            }
        }
        for definition in definitions {
            state.definitions.insert(
                definition.id.clone(),
                RegisteredPermission {
                    definition: definition.clone(),
                    owner: owner.to_string(),
                },
            );
            for role in &definition.default_roles {
                let permissions = state.roles.entry(role.clone()).or_default();
                if !permissions.contains(&definition.id) {
                    permissions.push(definition.id.clone());
                }
            }
        }
        Ok(())
    }

    pub async fn unregister_module(&self, module: &str) {
        let mut state = self.state.write().await;
        state
            .definitions
            .retain(|_, definition| definition.owner != module);
        let active = state
            .definitions
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        for permissions in state.roles.values_mut() {
            permissions.retain(|permission| permission == "*" || active.contains(permission));
        }
    }

    pub async fn list(&self) -> Vec<RegisteredPermission> {
        self.state
            .read()
            .await
            .definitions
            .values()
            .cloned()
            .collect()
    }

    pub async fn permissions_for(&self, principal: &str) -> Vec<String> {
        let state = self.state.read().await;
        let mut permissions = state
            .assignments
            .get(principal)
            .into_iter()
            .flatten()
            .filter_map(|role| state.roles.get(role))
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        permissions.sort();
        permissions.dedup();
        permissions
    }

    pub async fn allows(&self, principal: &str, permission: &str) -> bool {
        self.permissions_for(principal)
            .await
            .iter()
            .any(|granted| granted == "*" || granted == permission)
    }

    pub async fn require(
        &self,
        principal: &str,
        permission: &str,
    ) -> Result<(), crate::error::AppError> {
        if self.allows(principal, permission).await {
            Ok(())
        } else {
            Err(crate::error::AppError::Forbidden(format!(
                "missing required permission: {permission}"
            )))
        }
    }
}

fn definition(id: &str, description: &str) -> PermissionDefinition {
    PermissionDefinition {
        id: id.into(),
        description: description.into(),
        default_roles: Vec::new(),
    }
}

fn validate_definition(definition: &PermissionDefinition) -> Result<(), PermissionError> {
    let valid = !definition.id.is_empty()
        && definition.id.len() <= 128
        && definition.id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
        && !definition.id.starts_with('.')
        && !definition.id.ends_with('.')
        && definition.id.contains('.');
    if !valid {
        return Err(PermissionError::InvalidId(definition.id.clone()));
    }
    if definition.description.trim().is_empty() || definition.description.len() > 256 {
        return Err(PermissionError::EmptyDescription(definition.id.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn administrator_and_module_namespaces_are_enforced() {
        let registry = PermissionRegistry::default();
        registry.initialize("admin").await.unwrap();
        assert!(registry.allows("admin", "photo-backup.assets.read").await);
        assert!(!registry.allows("unknown", "platform.modules.read").await);
        registry
            .register_module(
                "photo-backup",
                &[definition("photo-backup.assets.read", "Read photo assets")],
            )
            .await
            .unwrap();
        let error = registry
            .register_module(
                "photo-backup",
                &[definition("dufs.files.write", "Wrong owner")],
            )
            .await
            .unwrap_err();
        assert!(matches!(error, PermissionError::WrongNamespace { .. }));
        registry.unregister_module("photo-backup").await;
        assert!(
            registry
                .list()
                .await
                .iter()
                .all(|permission| permission.owner != "photo-backup")
        );
    }
}
