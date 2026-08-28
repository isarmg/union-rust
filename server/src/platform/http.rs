use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::Value;

use super::{AuthenticatedPrincipal, ModuleRuntimeView};
use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

pub(crate) fn console_router() -> Router<AppState> {
    Router::new()
        .route("/api/platform/modules", get(list_modules))
        .route("/api/platform/modules/rescan", post(rescan))
        .route("/api/platform/modules/{module}/enable", post(enable))
        .route("/api/platform/modules/{module}/disable", post(disable))
        .route("/api/platform/permissions", get(list_permissions))
        .route(
            "/api/platform/modules/{module}/configuration",
            get(get_configuration).put(set_configuration),
        )
        .route("/api/platform/tasks", get(list_tasks))
        .route("/api/platform/task-runs", get(list_task_runs))
        .route("/api/platform/task-runs/{run}", get(get_task_run))
        .route("/api/platform/tasks/{task}/trigger", post(trigger_task))
        .route("/api/platform/notifications", get(list_notifications))
        .route(
            "/api/platform/notifications/{notification}/ack",
            post(acknowledge_notification),
        )
}

async fn list_modules(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
) -> AppResult<Json<Vec<ModuleRuntimeView>>> {
    require(&state, &principal, "platform.modules.read").await?;
    Ok(Json(state.platform.modules().await))
}

async fn rescan(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
) -> AppResult<Json<Vec<ModuleRuntimeView>>> {
    require(&state, &principal, "platform.modules.manage").await?;
    audit(
        &state,
        "platform.modules.rescan",
        "bundled-modules",
        Some("runtime discovery requested"),
    )
    .await?;
    match state.platform.rescan().await {
        Ok(modules) => {
            audit(
                &state,
                "platform.modules.rescan.succeeded",
                "bundled-modules",
                Some("runtime discovery completed without replacing running modules"),
            )
            .await?;
            Ok(Json(modules))
        }
        Err(error) => {
            audit(
                &state,
                "platform.modules.rescan.failed",
                "bundled-modules",
                Some(&error.to_string()),
            )
            .await?;
            Err(lifecycle_error(error))
        }
    }
}

async fn enable(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
    Path(module): Path<String>,
) -> AppResult<Json<Vec<ModuleRuntimeView>>> {
    require(&state, &principal, "platform.modules.manage").await?;
    audit(
        &state,
        "platform.module.enable",
        &module,
        Some("enable requested"),
    )
    .await?;
    match state.platform.enable(&module).await {
        Ok(modules) => {
            audit(
                &state,
                "platform.module.enable.succeeded",
                &module,
                Some("target module reached its startup health gate"),
            )
            .await?;
            Ok(Json(modules))
        }
        Err(error) => {
            audit(
                &state,
                "platform.module.enable.failed",
                &module,
                Some(&error.to_string()),
            )
            .await?;
            Err(lifecycle_error(error))
        }
    }
}

async fn disable(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
    Path(module): Path<String>,
) -> AppResult<Json<Vec<ModuleRuntimeView>>> {
    require(&state, &principal, "platform.modules.manage").await?;
    audit(
        &state,
        "platform.module.disable",
        &module,
        Some("disable requested"),
    )
    .await?;
    match state.platform.disable(&module).await {
        Ok(modules) => {
            audit(
                &state,
                "platform.module.disable.succeeded",
                &module,
                Some("target module stopped; unrelated modules were left running"),
            )
            .await?;
            Ok(Json(modules))
        }
        Err(error) => {
            audit(
                &state,
                "platform.module.disable.failed",
                &module,
                Some(&error.to_string()),
            )
            .await?;
            Err(lifecycle_error(error))
        }
    }
}

async fn list_permissions(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
) -> AppResult<Json<Vec<super::RegisteredPermission>>> {
    require(&state, &principal, "platform.permissions.read").await?;
    Ok(Json(state.platform.permissions.list().await))
}

async fn get_configuration(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
    Path(module): Path<String>,
) -> AppResult<Json<super::configuration::ModuleConfiguration>> {
    require(&state, &principal, "platform.modules.read").await?;
    let permission = format!("{module}.configuration.read");
    if state
        .platform
        .permissions
        .list()
        .await
        .iter()
        .any(|definition| definition.definition.id == permission)
    {
        require(&state, &principal, &permission).await?;
    }
    state
        .platform
        .configuration
        .get(&module)
        .await
        .map(Json)
        .ok_or_else(|| AppError::NotFound("module configuration is not registered".into()))
}

async fn set_configuration(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
    Path(module): Path<String>,
    Json(value): Json<Value>,
) -> AppResult<Json<super::configuration::ModuleConfiguration>> {
    require(&state, &principal, "platform.modules.manage").await?;
    let permission = format!("{module}.configuration.write");
    if state
        .platform
        .permissions
        .list()
        .await
        .iter()
        .any(|definition| definition.definition.id == permission)
    {
        require(&state, &principal, &permission).await?;
    }
    audit(
        &state,
        "platform.module.configuration.update",
        &module,
        Some("schema-validated configuration update requested; values omitted"),
    )
    .await?;
    match state.platform.set_configuration(&module, value).await {
        Ok(configuration) => {
            audit(
                &state,
                "platform.module.configuration.update.succeeded",
                &module,
                Some("configuration persisted while the module was disabled; values omitted"),
            )
            .await?;
            Ok(Json(configuration))
        }
        Err(error) => {
            audit(
                &state,
                "platform.module.configuration.update.failed",
                &module,
                Some(&error.to_string()),
            )
            .await?;
            Err(AppError::Conflict(error.to_string()))
        }
    }
}

async fn list_tasks(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
) -> AppResult<Json<Vec<super::TaskDefinition>>> {
    require(&state, &principal, "platform.tasks.read").await?;
    Ok(Json(state.platform.tasks.definitions().await))
}

async fn list_task_runs(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
) -> AppResult<Json<Vec<super::TaskRun>>> {
    require(&state, &principal, "platform.tasks.read").await?;
    Ok(Json(state.platform.tasks.runs().await))
}

async fn get_task_run(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
    Path(run): Path<String>,
) -> AppResult<Json<super::TaskRun>> {
    require(&state, &principal, "platform.tasks.read").await?;
    state
        .platform
        .tasks
        .run(&run)
        .await
        .map(Json)
        .ok_or_else(|| AppError::NotFound("task run does not exist".into()))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TriggerRequest {
    #[serde(default)]
    input: Value,
}

async fn trigger_task(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
    Path(task): Path<String>,
    Json(request): Json<TriggerRequest>,
) -> AppResult<(StatusCode, Json<super::TaskRun>)> {
    require(&state, &principal, "platform.tasks.trigger").await?;
    let run = state
        .platform
        .tasks
        .trigger(
            &principal.username,
            &task,
            request.input,
            &state.platform.permissions,
        )
        .await
        .map_err(|error| match error {
            super::TaskError::NotFound(_) => AppError::NotFound(error.to_string()),
            super::TaskError::Forbidden(_) => AppError::Forbidden(error.to_string()),
            _ => AppError::Conflict(error.to_string()),
        })?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

async fn list_notifications(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
) -> AppResult<Json<Vec<super::Notification>>> {
    require(&state, &principal, "platform.notifications.read").await?;
    Ok(Json(state.platform.notifications.list().await))
}

async fn acknowledge_notification(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<AuthenticatedPrincipal>,
    Path(notification): Path<String>,
) -> AppResult<Json<super::Notification>> {
    require(&state, &principal, "platform.notifications.ack").await?;
    state
        .platform
        .notifications
        .acknowledge(&notification, &principal.username)
        .await
        .map(Json)
        .ok_or_else(|| AppError::NotFound("notification does not exist".into()))
}

async fn require(
    state: &AppState,
    principal: &AuthenticatedPrincipal,
    permission: &str,
) -> AppResult<()> {
    state
        .platform
        .permissions
        .require(&principal.username, permission)
        .await
}

fn lifecycle_error(error: anyhow::Error) -> AppError {
    AppError::Conflict(error.to_string())
}

async fn audit(
    state: &AppState,
    action: &str,
    target: &str,
    detail: Option<&str>,
) -> AppResult<()> {
    crate::infra::database::insert_audit(state.db().as_ref(), action, target, detail).await?;
    Ok(())
}
