use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use super::rbac::PermissionRegistry;

pub type TaskFuture = Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>>;
pub type TaskHandler = Arc<dyn Fn(Value) -> TaskFuture + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDefinition {
    pub id: String,
    pub description: String,
    pub permission: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunState {
    Queued,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskRun {
    pub id: String,
    pub task: String,
    pub requested_by: String,
    pub state: TaskRunState,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub result: Option<Value>,
    pub error: Option<String>,
}

#[derive(Clone)]
struct RegisteredTask {
    definition: TaskDefinition,
    handler: TaskHandler,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TaskError {
    #[error("invalid task id: {0}")]
    InvalidId(String),
    #[error("task is already registered: {0}")]
    Duplicate(String),
    #[error("task does not exist: {0}")]
    NotFound(String),
    #[error("principal lacks permission {0}")]
    Forbidden(String),
}

#[derive(Clone, Default)]
pub struct TaskScheduler {
    tasks: Arc<RwLock<BTreeMap<String, RegisteredTask>>>,
    runs: Arc<RwLock<BTreeMap<String, TaskRun>>>,
}

impl TaskScheduler {
    pub async fn register(
        &self,
        owner: &str,
        definition: TaskDefinition,
        handler: TaskHandler,
    ) -> Result<(), TaskError> {
        let expected = format!("{owner}.");
        if !definition.id.starts_with(&expected)
            || definition.id.len() > 160
            || definition.description.trim().is_empty()
        {
            return Err(TaskError::InvalidId(definition.id));
        }
        let mut tasks = self.tasks.write().await;
        if tasks.contains_key(&definition.id) {
            return Err(TaskError::Duplicate(definition.id));
        }
        tasks.insert(
            definition.id.clone(),
            RegisteredTask {
                definition,
                handler,
            },
        );
        Ok(())
    }

    pub async fn unregister_owner(&self, owner: &str) {
        let prefix = format!("{owner}.");
        self.tasks
            .write()
            .await
            .retain(|id, _| !id.starts_with(&prefix));
    }

    pub async fn definitions(&self) -> Vec<TaskDefinition> {
        self.tasks
            .read()
            .await
            .values()
            .map(|task| task.definition.clone())
            .collect()
    }

    pub async fn runs(&self) -> Vec<TaskRun> {
        self.runs.read().await.values().rev().cloned().collect()
    }

    pub async fn run(&self, id: &str) -> Option<TaskRun> {
        self.runs.read().await.get(id).cloned()
    }

    pub async fn trigger(
        &self,
        principal: &str,
        task_id: &str,
        input: Value,
        permissions: &PermissionRegistry,
    ) -> Result<TaskRun, TaskError> {
        let task = self
            .tasks
            .read()
            .await
            .get(task_id)
            .cloned()
            .ok_or_else(|| TaskError::NotFound(task_id.into()))?;
        if !permissions
            .allows(principal, &task.definition.permission)
            .await
        {
            return Err(TaskError::Forbidden(task.definition.permission));
        }
        self.enqueue(principal, task, task_id, input).await
    }

    pub async fn trigger_trusted(
        &self,
        principal: &str,
        task_id: &str,
        input: Value,
    ) -> Result<TaskRun, TaskError> {
        let task = self
            .tasks
            .read()
            .await
            .get(task_id)
            .cloned()
            .ok_or_else(|| TaskError::NotFound(task_id.into()))?;
        self.enqueue(principal, task, task_id, input).await
    }

    async fn enqueue(
        &self,
        principal: &str,
        task: RegisteredTask,
        task_id: &str,
        input: Value,
    ) -> Result<TaskRun, TaskError> {
        let run = TaskRun {
            id: uuid::Uuid::new_v4().to_string(),
            task: task_id.into(),
            requested_by: principal.into(),
            state: TaskRunState::Queued,
            created_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
            result: None,
            error: None,
        };
        self.runs.write().await.insert(run.id.clone(), run.clone());
        let runs = self.runs.clone();
        let run_id = run.id.clone();
        tokio::spawn(async move {
            if let Some(run) = runs.write().await.get_mut(&run_id) {
                run.state = TaskRunState::Running;
            }
            let outcome = (task.handler)(input).await;
            if let Some(run) = runs.write().await.get_mut(&run_id) {
                run.finished_at = Some(chrono::Utc::now().to_rfc3339());
                match outcome {
                    Ok(result) => {
                        run.state = TaskRunState::Succeeded;
                        run.result = Some(result);
                    }
                    Err(error) => {
                        run.state = TaskRunState::Failed;
                        run.error = Some(error.to_string());
                    }
                }
            }
        });
        Ok(run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registered_task_is_authorized_executed_and_observable() {
        let permissions = PermissionRegistry::default();
        permissions.initialize("admin").await.unwrap();
        let scheduler = TaskScheduler::default();
        scheduler
            .register(
                "platform",
                TaskDefinition {
                    id: "platform.echo".into(),
                    description: "Echo a JSON value".into(),
                    permission: "platform.tasks.trigger".into(),
                },
                Arc::new(|value| Box::pin(async move { Ok(value) })),
            )
            .await
            .unwrap();
        assert!(
            scheduler
                .trigger("unknown", "platform.echo", Value::Null, &permissions)
                .await
                .is_err()
        );
        let run = scheduler
            .trigger(
                "admin",
                "platform.echo",
                serde_json::json!({"ok":true}),
                &permissions,
            )
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let current = scheduler.run(&run.id).await.unwrap();
                if current.state == TaskRunState::Succeeded {
                    assert_eq!(current.result, Some(serde_json::json!({"ok":true})));
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
