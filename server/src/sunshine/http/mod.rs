//! Sunshine 多主机管理 handler。
//!
//! 路由结构：
//!   GET  /hosts                         列出所有主机
//!   POST /hosts                         新建主机
//!   PATCH /hosts/{id}                   部分更新主机
//!   DELETE /hosts/{id}                  删除主机
//!   GET  /hosts/{id}/status             TCP 可达性检测
//!   GET  /hosts/{id}/apps               Sunshine API 代理（以下同）
//!   ...（其余接口均在 /hosts/{id}/ 前缀下）
//!
//! # 代理模式说明
//!
//! `/hosts/{id}/apps` 等接口是"透明代理"：
//! 前端发请求给 union，union 找到对应主机的配置，
//! 用存储的用户名/密码向 Sunshine 发起认证请求，再把结果转发给前端。
//! 这样前端不需要存储 Sunshine 密码，也不存在跨域问题。

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use serde_json::Value;

use crate::sunshine::{
    SunshineClientUpdateRequest, SunshineCoverUploadRequest, SunshineHostInfo,
    SunshineHostPatchRequest, SunshineHostSaveRequest, SunshinePinRequest, SunshineProbeStatus,
    SunshineStatus, SunshineUnpairRequest,
};
use crate::{
    config::SunshineHostConfig,
    error::{AppError, AppResult},
    infra::database,
    infra::network,
    state::AppState,
};

use crate::sunshine::client;

mod common;
mod hosts;
mod proxy;

use hosts::*;
use proxy::*;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/services/sunshine/hosts",
            get(list_hosts).post(create_host),
        )
        .route(
            "/api/services/sunshine/hosts/{id}",
            patch(update_host).delete(delete_host),
        )
        .route("/api/services/sunshine/hosts/{id}/status", get(host_status))
        .route(
            "/api/services/sunshine/hosts/{id}/apps",
            get(apps_list).post(apps_save),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/apps/close",
            post(apps_close),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/apps/{index}",
            delete(apps_delete),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/clients",
            get(clients_list),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/clients/unpair",
            post(clients_unpair),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/clients/unpair-all",
            post(clients_unpair_all),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/clients/update",
            post(clients_update),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/config",
            get(config_get).post(config_save),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/config/locale",
            get(config_locale),
        )
        .route("/api/services/sunshine/hosts/{id}/api-logs", get(api_logs))
        .route("/api/services/sunshine/hosts/{id}/pin", post(pin))
        .route("/api/services/sunshine/hosts/{id}/restart", post(restart))
        .route(
            "/api/services/sunshine/hosts/{id}/reset-display",
            post(reset_display),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/covers/{index}",
            get(cover_get),
        )
        .route(
            "/api/services/sunshine/hosts/{id}/covers/upload",
            post(cover_upload),
        )
        // The largest accepted Sunshine write is a 1 MiB configuration object.
        // Keep the Sunshine contract explicit even though it currently matches
        // the 1 MiB application-wide fallback.
        .layer(DefaultBodyLimit::max(1024 * 1024))
}
