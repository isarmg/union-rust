//! Sunshine 主机 CRUD、状态与日志端点。

use super::{common::*, *};

// ─── 主机 CRUD ────────────────────────────────────────────────────────────────

/// 列出所有已配置的 Sunshine 主机，并附带后台任务维护的最近健康快照。
///
/// 这个读路径刻意不访问 Sunshine。配置页每 30 秒刷新一次，如果每个浏览器都在
/// handler 里发起 `/api/apps`，一台只接受 TCP 却不回应 HTTP 的主机会让整页卡满
/// 15 秒超时。探测集中在唯一后台任务中，列表只做两次短内存读。
pub(crate) async fn list_hosts(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<SunshineHostInfo>>> {
    // Hold both guards in the canonical host -> health order while composing
    // one response. Releasing the host guard before reading health allowed a
    // concurrent PATCH/DELETE to pair old hosts with a new health snapshot.
    let hosts = state.sunshine.hosts.read().await;
    let health = state.sunshine.health.read().await;
    let infos = hosts
        .iter()
        .map(|host| host_info(host, health.get(&host.id)))
        .collect();
    Ok(Json(infos))
}

/// 新建 Sunshine 主机配置。
pub(crate) async fn create_host(
    State(state): State<AppState>,
    Json(req): Json<SunshineHostSaveRequest>,
) -> AppResult<(StatusCode, Json<SunshineHostInfo>)> {
    validate_host_request(&req, state.settings.production)?;
    let new_host = SunshineHostConfig {
        id: uuid::Uuid::new_v4().to_string(), // 生成唯一 ID，用于路由中的 {id} 参数
        name: req.name.trim().to_string(),
        host: network::normalize_host(&req.host),
        web_port: req.web_port,
        username: req.username.trim().to_string(),
        password: req.password.unwrap_or_default(), // 密码可选，未提供时为空字符串
        verify_tls: req.verify_tls,
    };
    let info = finish_sunshine_mutation(create_host_and_publish(
        state,
        new_host,
        std::future::ready(()),
    ))
    .await?;
    Ok((StatusCode::CREATED, Json(info)))
}

async fn create_host_and_publish(
    state: AppState,
    new_host: SunshineHostConfig,
    after_database_commit: impl std::future::Future<Output = ()> + Send,
) -> AppResult<SunshineHostInfo> {
    let _settings_guard = state.sunshine.settings_lock.lock().await;
    let mut hosts = state.sunshine.hosts.read().await.clone();
    hosts.push(new_host.clone());
    let audit_detail = format!(
        "name={} host={} port={} verify_tls={}",
        new_host.name, new_host.host, new_host.web_port, new_host.verify_tls
    );
    // The whole mutation runs in a detached task. The database helper commits
    // internally, so request cancellation must not be allowed to stop this
    // task between that commit and publishing the matching memory snapshot.
    let mut stored_hosts = state.sunshine.hosts.write().await;
    let mut health = state.sunshine.health.write().await;
    database::insert_sunshine_host(state.db().as_ref(), &new_host, &audit_detail).await?;
    after_database_commit.await;
    *stored_hosts = hosts;
    health.insert(
        new_host.id.clone(),
        crate::sunshine::SunshineHostHealth::pending(),
    );
    drop(health);
    drop(stored_hosts);
    drop(_settings_guard);
    state.sunshine.health_refresh.notify_one();
    Ok(host_info(&new_host, None))
}

/// 更新主机配置（按 ID）。
///
/// 密码是可选更新：如果请求中没有提供，则保留原来的值。
/// 这样前端不需要每次都传完整配置，可以只更新部分字段。
pub(crate) async fn update_host(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SunshineHostPatchRequest>,
) -> AppResult<Json<SunshineHostInfo>> {
    if req.is_empty() {
        return Err(AppError::BadRequest(
            "至少需要提供一个要更新的字段".to_string(),
        ));
    }
    let info = finish_sunshine_mutation(update_host_and_publish(
        state,
        id,
        req,
        std::future::ready(()),
    ))
    .await?;
    Ok(Json(info))
}

async fn update_host_and_publish(
    state: AppState,
    id: String,
    req: SunshineHostPatchRequest,
    after_database_commit: impl std::future::Future<Output = ()> + Send,
) -> AppResult<SunshineHostInfo> {
    let update_password = req.password.is_some();
    let _settings_guard = state.sunshine.settings_lock.lock().await;
    let mut hosts = state.sunshine.hosts.read().await.clone();
    let host = hosts
        .iter_mut()
        .find(|h| h.id == id)
        .ok_or_else(|| AppError::NotFound(format!("Sunshine 主机 '{id}' 不存在")))?;

    if let Some(name) = req.name {
        host.name = name.trim().to_string();
    }
    if let Some(address) = req.host {
        host.host = network::normalize_host(&address);
    }
    if let Some(web_port) = req.web_port {
        host.web_port = web_port;
    }
    if let Some(username) = req.username {
        host.username = username.trim().to_string();
    }
    if let Some(pw) = req.password {
        host.password = pw;
    }
    if let Some(verify_tls) = req.verify_tls {
        host.verify_tls = verify_tls;
    }
    let complete_request = SunshineHostSaveRequest {
        name: host.name.clone(),
        host: host.host.clone(),
        web_port: host.web_port,
        username: host.username.clone(),
        password: update_password.then(|| host.password.clone()),
        verify_tls: host.verify_tls,
    };
    validate_host_request(&complete_request, state.settings.production)?;
    let host_clone = host.clone();

    let audit_detail = format!(
        "name={} host={} port={} verify_tls={}",
        host_clone.name, host_clone.host, host_clone.web_port, host_clone.verify_tls
    );
    let mut stored_hosts = state.sunshine.hosts.write().await;
    let mut health = state.sunshine.health.write().await;
    let found = database::update_sunshine_host(
        state.db().as_ref(),
        &host_clone,
        update_password,
        &audit_detail,
    )
    .await?;
    if !found {
        return Err(AppError::NotFound(format!("Sunshine 主机 '{id}' 不存在")));
    }
    after_database_commit.await;
    *stored_hosts = hosts;
    health.insert(
        host_clone.id.clone(),
        crate::sunshine::SunshineHostHealth::pending(),
    );
    drop(health);
    drop(stored_hosts);
    drop(_settings_guard);
    state.sunshine.health_refresh.notify_one();
    Ok(host_info(&host_clone, None))
}

/// 删除主机配置（按 ID）。
///
/// `retain` 保留所有不匹配 id 的主机，相当于"过滤掉"指定 id 的主机。
/// 通过比较删除前后的长度来判断是否真的找到并删除了目标主机。
pub(crate) async fn delete_host(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<axum::http::StatusCode> {
    finish_sunshine_mutation(delete_host_and_publish(state, id, std::future::ready(()))).await
}

async fn delete_host_and_publish(
    state: AppState,
    id: String,
    after_database_commit: impl std::future::Future<Output = ()> + Send,
) -> AppResult<StatusCode> {
    let _settings_guard = state.sunshine.settings_lock.lock().await;
    let mut hosts = state.sunshine.hosts.read().await.clone();
    if !hosts.iter().any(|host| host.id == id) {
        return Err(AppError::NotFound(format!("Sunshine 主机 '{id}' 不存在")));
    }
    let mut stored_hosts = state.sunshine.hosts.write().await;
    let mut health = state.sunshine.health.write().await;
    let found = database::delete_sunshine_host(state.db().as_ref(), &id).await?;
    if !found {
        return Err(AppError::NotFound(format!("Sunshine 主机 '{id}' 不存在")));
    }
    after_database_commit.await;
    hosts.retain(|host| host.id != id);
    *stored_hosts = hosts;
    health.remove(&id);
    drop(health);
    drop(stored_hosts);
    drop(_settings_guard);
    state.sunshine.health_refresh.notify_one();
    Ok(axum::http::StatusCode::NO_CONTENT) // 删除成功返回 204 No Content
}

// ─── 单主机状态 ───────────────────────────────────────────────────────────────

/// 获取指定 Sunshine 主机的运行状态（进程状态、TCP 可达性等）。
pub(crate) async fn host_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<SunshineStatus>> {
    let hosts = state.sunshine.hosts.read().await;
    let host = hosts
        .iter()
        .find(|host| host.id == id)
        .cloned()
        .ok_or_else(|| AppError::NotFound(format!("Sunshine 主机 '{id}' 不存在")))?;
    if state.settings.production && !host.verify_tls {
        return Err(AppError::BadRequest(
            "该 Sunshine 主机已禁用 TLS 验证；请先编辑配置并启用验证".to_string(),
        ));
    }
    let health = state.sunshine.health.read().await;
    let status = crate::sunshine::status::sunshine_host_status(&host, health.get(&id));
    // Make the consistency scope explicit; both guards cover response assembly.
    drop(health);
    drop(hosts);
    Ok(Json(status))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{
        config::{LocalConfig, Settings},
        infra::database,
    };
    use tokio::sync::oneshot;

    fn state_with_host(host: SunshineHostConfig) -> AppState {
        let mut settings = Settings::default();
        settings.database.url = ":memory:".to_string();
        settings.sunshine.hosts = vec![host];
        AppState::new(
            settings,
            database::in_memory_pool().expect("in-memory test pool"),
            "unused".into(),
            LocalConfig {
                application_version: env!("CARGO_PKG_VERSION").to_string(),
                admin_username: "admin".into(),
                admin_password_hash: "unused".into(),
            },
            crate::system::ResourceMonitor::frozen(Default::default()),
        )
        .expect("capture in-memory database identity")
    }

    async fn initialized_state() -> AppState {
        let mut settings = Settings::default();
        settings.database.url = ":memory:".to_string();
        let pool = database::in_memory_pool().expect("in-memory test pool");
        database::initialize_schema(&pool)
            .await
            .expect("initialize test schema");
        AppState::new(
            settings,
            pool,
            "unused".into(),
            LocalConfig {
                application_version: env!("CARGO_PKG_VERSION").to_string(),
                admin_username: "admin".into(),
                admin_password_hash: "unused".into(),
            },
            crate::system::ResourceMonitor::frozen(Default::default()),
        )
        .expect("capture in-memory database identity")
    }

    fn commit_pause() -> (
        impl std::future::Future<Output = ()> + Send + 'static,
        oneshot::Receiver<()>,
        oneshot::Sender<()>,
    ) {
        let (committed_tx, committed_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        (
            async move {
                let _ = committed_tx.send(());
                let _ = release_rx.await;
            },
            committed_rx,
            release_tx,
        )
    }

    fn request_waiter<T>(
        request_id: &'static str,
        mutation: impl std::future::Future<Output = AppResult<T>> + Send + 'static,
    ) -> tokio::task::JoinHandle<AppResult<T>>
    where
        T: Send + 'static,
    {
        tokio::spawn(database::with_audit_context(
            database::AuditContext {
                actor: "test-admin".to_string(),
                request_id: Some(request_id.to_string()),
            },
            finish_sunshine_mutation(mutation),
        ))
    }

    async fn cancel_waiter_after_commit<T>(
        state: &AppState,
        waiter: tokio::task::JoinHandle<AppResult<T>>,
        committed: oneshot::Receiver<()>,
        release: oneshot::Sender<()>,
    ) where
        T: Send + 'static,
    {
        tokio::time::timeout(Duration::from_secs(2), committed)
            .await
            .expect("database commit barrier timed out")
            .expect("mutation ended before the database commit barrier");
        waiter.abort();
        match waiter.await {
            Err(error) if error.is_cancelled() => {}
            _ => panic!("request waiter was not cancelled at the commit barrier"),
        }
        release
            .send(())
            .expect("detached mutation stopped when its waiter was cancelled");
        tokio::time::timeout(
            Duration::from_secs(2),
            state.sunshine.health_refresh.notified(),
        )
        .await
        .expect("detached mutation did not publish its memory snapshot");
    }

    /// 回归：旧实现会对黑洞地址执行 TCP/HTTP 探测；列表现在只能读内存快照，必须在
    /// 远低于单次 TCP 探测超时（500ms）的时间内返回 pending。
    ///
    /// 不在测试里绑定本地端口：部分受限 CI 禁止 `TcpListener::bind`，网络沙箱策略
    /// 不应被误报为业务失败。
    #[tokio::test]
    async fn list_hosts_never_waits_for_a_hanging_sunshine_server() {
        let host = SunshineHostConfig {
            id: "hanging-host".into(),
            host: "192.0.2.1".into(),
            web_port: 47990,
            verify_tls: false,
            ..SunshineHostConfig::default()
        };
        let response = tokio::time::timeout(
            Duration::from_millis(250),
            list_hosts(State(state_with_host(host))),
        )
        .await
        .expect("list handler must not perform upstream I/O")
        .expect("list hosts");

        assert_eq!(response.0.len(), 1);
        assert_eq!(response.0[0].probe_status, SunshineProbeStatus::Pending);
        assert_eq!(response.0[0].reachable, None);
        assert_eq!(response.0[0].connected, None);
    }

    #[tokio::test]
    async fn cancelled_requests_preserve_publication_and_audit_context() {
        let state = initialized_state().await;
        let host = SunshineHostConfig {
            id: "cancelled-mutation-host".into(),
            name: "before-update".into(),
            host: "192.0.2.10".into(),
            username: "admin".into(),
            password: String::new(),
            verify_tls: false,
            ..SunshineHostConfig::default()
        };

        let (pause, committed, release) = commit_pause();
        let create_waiter = request_waiter(
            "cancelled-create-request",
            create_host_and_publish(state.clone(), host.clone(), pause),
        );
        cancel_waiter_after_commit(&state, create_waiter, committed, release).await;
        assert!(
            state
                .sunshine
                .hosts
                .read()
                .await
                .iter()
                .any(|stored| stored.id == host.id),
            "cancelled create committed SQLite but did not publish memory"
        );
        assert_eq!(
            database::load_sunshine_hosts(state.db().as_ref())
                .await
                .expect("reload created host")
                .len(),
            1
        );

        state.sunshine.health.write().await.insert(
            host.id.clone(),
            crate::sunshine::SunshineHostHealth::completed(true, &Ok(())),
        );
        let (pause, committed, release) = commit_pause();
        let update_waiter = request_waiter(
            "cancelled-update-request",
            update_host_and_publish(
                state.clone(),
                host.id.clone(),
                SunshineHostPatchRequest {
                    name: Some("after-update".into()),
                    ..SunshineHostPatchRequest::default()
                },
                pause,
            ),
        );
        cancel_waiter_after_commit(&state, update_waiter, committed, release).await;
        let memory_name = state.sunshine.hosts.read().await[0].name.clone();
        let stored = database::load_sunshine_hosts(state.db().as_ref())
            .await
            .expect("reload updated host");
        assert_eq!(memory_name, "after-update");
        assert_eq!(stored[0].name, memory_name);
        let health = state.sunshine.health.read().await;
        assert_eq!(health[&host.id].reachable, None);
        drop(health);

        let (pause, committed, release) = commit_pause();
        let delete_waiter = request_waiter(
            "cancelled-delete-request",
            delete_host_and_publish(state.clone(), host.id.clone(), pause),
        );
        cancel_waiter_after_commit(&state, delete_waiter, committed, release).await;
        assert!(state.sunshine.hosts.read().await.is_empty());
        assert!(state.sunshine.health.read().await.is_empty());
        assert!(
            database::load_sunshine_hosts(state.db().as_ref())
                .await
                .expect("reload after delete")
                .is_empty()
        );

        let audit = database::list_audit_logs(state.db().as_ref(), None, 10)
            .await
            .expect("load Sunshine mutation audit rows");
        for (action, request_id) in [
            ("sunshine.host.create", "cancelled-create-request"),
            ("sunshine.host.update", "cancelled-update-request"),
            ("sunshine.host.delete", "cancelled-delete-request"),
        ] {
            let entry = audit
                .entries
                .iter()
                .find(|entry| entry.action == action)
                .unwrap_or_else(|| panic!("missing {action} audit row"));
            assert_eq!(entry.actor, "test-admin");
            assert_eq!(entry.request_id.as_deref(), Some(request_id));
        }
    }
}
