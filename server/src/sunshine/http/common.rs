//! Sunshine 主机校验、查找和持久化。

use super::*;

// ─── 请求校验 ─────────────────────────────────────────────────────────────────

/// 验证 host 字段是有效的 IPv4、IPv6 或域名。
pub(crate) fn validate_host(host: &str) -> AppResult<()> {
    let h = host.trim();
    if network::is_valid_host(h) {
        return Ok(());
    }
    Err(AppError::InvalidHost(format!(
        "无效的 host 值 '{h}'，请提供有效的 IPv4、IPv6 或域名"
    )))
}

pub(crate) fn validate_host_request(
    req: &SunshineHostSaveRequest,
    production: bool,
) -> AppResult<()> {
    validate_host(&req.host)?;
    validate_required_text("主机名称", &req.name, 128)?;
    validate_required_text("管理用户名", &req.username, 256)?;
    if req
        .password
        .as_ref()
        .is_some_and(|value| value.len() > 4096)
    {
        return Err(AppError::BadRequest(
            "Sunshine 密码不能超过 4096 个字符".to_string(),
        ));
    }
    if req.web_port == 0 {
        return Err(AppError::BadRequest("API 端口必须大于 0".to_string()));
    }
    if production && !req.verify_tls {
        return Err(AppError::BadRequest(
            "生产环境不允许关闭 Sunshine TLS 证书验证".to_string(),
        ));
    }
    Ok(())
}

fn validate_required_text(label: &str, value: &str, max_chars: usize) -> AppResult<()> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::BadRequest(format!("{label}不能为空")));
    }
    if value.chars().count() > max_chars {
        return Err(AppError::BadRequest(format!(
            "{label}不能超过 {max_chars} 个字符"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(AppError::BadRequest(format!("{label}不能包含控制字符")));
    }
    Ok(())
}

pub(crate) fn validate_proxy_json_object(
    label: &str,
    value: &Value,
    max_bytes: usize,
) -> AppResult<()> {
    if !value.is_object() {
        return Err(AppError::BadRequest(format!(
            "{label} must be a JSON object"
        )));
    }
    let size = serde_json::to_vec(value)
        .map_err(|err| AppError::BadRequest(format!("invalid {label}: {err}")))?
        .len();
    if size > max_bytes {
        return Err(AppError::BadRequest(format!(
            "{label} exceeds the maximum size of {} KiB",
            max_bytes / 1024
        )));
    }
    Ok(())
}

pub(crate) fn validate_client_id(value: &str) -> AppResult<&str> {
    validate_opaque_string("client uuid", value, 128)
}

pub(crate) fn validate_pin_request(pin: &str, name: &str) -> AppResult<(String, String)> {
    let pin = pin.trim();
    if !(4..=8).contains(&pin.len()) || !pin.chars().all(|value| value.is_ascii_digit()) {
        return Err(AppError::BadRequest(
            "Sunshine PIN must be 4 to 8 digits".to_string(),
        ));
    }
    let name = validate_opaque_string("client name", name, 80)?.to_string();
    Ok((pin.to_string(), name))
}

pub(crate) fn validate_cover_upload(key: &str, value: &str) -> AppResult<(String, String)> {
    let key = validate_opaque_string("cover key", key, 512)?.to_string();
    let value = value.trim();
    if value.len() > 2048 {
        return Err(AppError::BadRequest(
            "cover URL cannot be longer than 2048 characters".to_string(),
        ));
    }
    let parsed = reqwest::Url::parse(value)
        .map_err(|_| AppError::BadRequest("cover URL must be an absolute URL".to_string()))?;
    match parsed.scheme() {
        "http" | "https" if parsed.host_str().is_some() => Ok((key, parsed.to_string())),
        _ => Err(AppError::BadRequest(
            "cover URL must use http or https".to_string(),
        )),
    }
}

fn validate_opaque_string<'a>(label: &str, value: &'a str, max_chars: usize) -> AppResult<&'a str> {
    if value.chars().any(char::is_control) {
        return Err(AppError::BadRequest(format!(
            "{label} cannot contain control characters"
        )));
    }
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::BadRequest(format!("{label} cannot be empty")));
    }
    if value.chars().count() > max_chars {
        return Err(AppError::BadRequest(format!(
            "{label} cannot be longer than {max_chars} characters"
        )));
    }
    Ok(value)
}

// ─── 辅助：按 ID 查找主机 ─────────────────────────────────────────────────────

/// 按主机 ID 查找 Sunshine 主机配置，找不到则返回 404 错误。
///
/// `state.hosts.sunshine` 是 `RwLock<Vec<SunshineHostConfig>>`：
/// - `RwLock` 允许多个读者同时访问，但写者独占
/// - `.read().await` 获取读锁（等待写锁释放后才能获取）
/// - 此处只需要读取，所以用读锁（性能更好）
pub(crate) async fn find_host(state: &AppState, id: &str) -> AppResult<SunshineHostConfig> {
    let hosts = state.hosts.sunshine.read().await;
    let host = hosts
        .iter()
        .find(|h| h.id == id) // 线性搜索（主机数量通常很少，无需索引）
        .cloned() // `.cloned()` 从引用 `&SunshineHostConfig` 复制出一个新的拥有值
        .ok_or_else(|| AppError::NotFound(format!("Sunshine 主机 '{id}' 不存在")))?;
    if state.settings.production && !host.verify_tls {
        return Err(AppError::BadRequest(
            "该 Sunshine 主机已禁用 TLS 验证；请先编辑配置并启用验证".to_string(),
        ));
    }
    Ok(host)
}

/// Run a Sunshine mutation independently from the HTTP request future.
///
/// Dropping a Tokio `JoinHandle` detaches its task. Therefore, once this
/// function has spawned the mutation, client disconnects can stop waiting for
/// the response but cannot interrupt the mutation. Tokio task-local values are
/// not inherited by spawned tasks, so capture the authenticated identity before
/// spawning and restore it in the child.
pub(crate) async fn finish_sunshine_mutation<T>(
    mutation: impl std::future::Future<Output = AppResult<T>> + Send + 'static,
) -> AppResult<T>
where
    T: Send + 'static,
{
    let audit_context = database::current_audit_context().ok_or_else(|| {
        AppError::Anyhow(anyhow::anyhow!(
            "Sunshine mutation is missing its authenticated audit context"
        ))
    })?;
    tokio::spawn(database::with_audit_context(audit_context, mutation))
        .await
        .map_err(|error| {
            AppError::Anyhow(anyhow::anyhow!("Sunshine mutation task failed: {error}"))
        })?
}

/// Complete one external Sunshine mutation and its audit attempt outside the
/// request task.
///
/// Once the operation future starts, dropping the HTTP handler must not cancel
/// it. If the future returns success, the same child task makes the local audit
/// attempt before it finishes.
pub(crate) async fn finish_sunshine_upstream_mutation<T, F>(
    state: &AppState,
    action: &'static str,
    target: String,
    detail: Option<String>,
    mutation: F,
) -> AppResult<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = AppResult<T>> + Send + 'static,
{
    let state = state.clone();
    finish_sunshine_mutation(async move {
        let response = mutation.await?;
        audit_best_effort(&state, action, &target, detail.as_deref()).await;
        Ok(response)
    })
    .await
}

/// 外部 Sunshine 操作无法与本地数据库事务原子提交。
///
/// 上游已经成功后，审计 INSERT 失败不能把整个请求伪装成失败，否则前端重试会重复执行
/// restart、unpair 等非幂等操作。此处记录告警，业务响应仍反映上游的真实结果。
pub(crate) async fn audit_best_effort(
    state: &AppState,
    action: &str,
    target: &str,
    detail: Option<&str>,
) {
    if let Err(error) = database::insert_audit(state.db().as_ref(), action, target, detail).await {
        tracing::warn!(
            action,
            target,
            "审计日志写入失败（操作已生效，仅记录缺失）：{error}"
        );
    }
}

/// 将主机配置转换为脱敏的展示信息（不包含密码明文）。
///
/// 密码字段转换为布尔值 `password_set`，前端只需要知道"是否已配置密码"，
/// 不需要（也不应该）知道密码内容。这是 API 设计的安全最佳实践。
pub(crate) fn host_info(
    host: &SunshineHostConfig,
    health: Option<&crate::state::SunshineHostHealth>,
) -> SunshineHostInfo {
    let pending = crate::state::SunshineHostHealth::pending();
    let health = health.unwrap_or(&pending);
    let probe_status = if health.reachable.is_some() && health.connected.is_some() {
        SunshineProbeStatus::Complete
    } else {
        SunshineProbeStatus::Pending
    };
    SunshineHostInfo {
        id: host.id.clone(),
        name: host.name.clone(),
        host: host.host.clone(),
        web_port: host.web_port,
        username: host.username.clone(),
        password_set: !host.password.is_empty(), // 密码是否已设置（不返回密码本身）
        verify_tls: host.verify_tls,
        web_url: client::web_url(host),
        probe_status,
        reachable: health.reachable,
        connected: health.connected,
        connection_error: health.connection_error.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;
    use crate::{
        config::{LocalConfig, Settings},
        sunshine::SunshineHostSaveRequest,
    };

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

    fn audit_context(request_id: &str) -> database::AuditContext {
        database::AuditContext {
            actor: "test-admin".to_string(),
            request_id: Some(request_id.to_string()),
        }
    }

    fn request() -> SunshineHostSaveRequest {
        SunshineHostSaveRequest {
            name: "host".to_string(),
            host: "192.168.1.2".to_string(),
            web_port: 47990,
            username: "admin".to_string(),
            password: None,
            verify_tls: true,
        }
    }

    /// 名称、账号、端口、地址都必须校验；缺一即整体拒绝。
    #[test]
    fn rejects_incomplete_or_invalid_host_requests() {
        assert!(validate_host_request(&request(), false).is_ok());

        let mut blank_name = request();
        blank_name.name = "   ".to_string();
        assert!(validate_host_request(&blank_name, false).is_err());

        let mut blank_user = request();
        blank_user.username = String::new();
        assert!(validate_host_request(&blank_user, false).is_err());

        let mut zero_port = request();
        zero_port.web_port = 0;
        assert!(validate_host_request(&zero_port, false).is_err());

        let mut bad_host = request();
        bad_host.host = "not a host".to_string();
        assert!(validate_host_request(&bad_host, false).is_err());
    }

    /// 生产环境不允许关闭 TLS 校验，开发环境允许。
    #[test]
    fn production_requires_tls_verification() {
        let mut insecure = request();
        insecure.verify_tls = false;
        assert!(validate_host_request(&insecure, false).is_ok());
        assert!(validate_host_request(&insecure, true).is_err());
    }

    #[test]
    fn rejects_oversized_or_control_character_fields() {
        let mut long_name = request();
        long_name.name = "n".repeat(129);
        assert!(validate_host_request(&long_name, false).is_err());

        let mut control_user = request();
        control_user.username = "admin\noperator".to_string();
        assert!(validate_host_request(&control_user, false).is_err());

        let mut long_password = request();
        long_password.password = Some("p".repeat(4097));
        assert!(validate_host_request(&long_password, false).is_err());
    }

    #[test]
    fn host_info_distinguishes_pending_from_confirmed_unreachable() {
        let host = SunshineHostConfig::default();

        let pending = host_info(&host, None);
        assert_eq!(pending.probe_status, SunshineProbeStatus::Pending);
        assert_eq!(pending.reachable, None);
        assert_eq!(pending.connected, None);
        assert!(pending.connection_error.is_some());

        let failure = Err("Sunshine Web 端口不可达".to_string());
        let unreachable = crate::state::SunshineHostHealth::completed(false, &failure);
        let complete = host_info(&host, Some(&unreachable));
        assert_eq!(complete.probe_status, SunshineProbeStatus::Complete);
        assert_eq!(complete.reachable, Some(false));
        assert_eq!(complete.connected, Some(false));
        assert_eq!(
            complete.connection_error.as_deref(),
            Some("Sunshine Web 端口不可达")
        );
    }

    #[tokio::test]
    async fn cancelled_upstream_mutation_finishes_its_attributed_audit() {
        let state = initialized_state().await;
        let (effect_tx, effect_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let request_state = state.clone();
        let request = tokio::spawn(database::with_audit_context(
            audit_context("cancelled-upstream-request"),
            async move {
                finish_sunshine_upstream_mutation(
                    &request_state,
                    "sunshine.test.mutation",
                    "test-host".to_string(),
                    Some("safe detail".to_string()),
                    async move {
                        let _ = effect_tx.send(());
                        let _ = release_rx.await;
                        Ok(serde_json::json!({"status": true}))
                    },
                )
                .await
            },
        ));

        tokio::time::timeout(std::time::Duration::from_secs(2), effect_rx)
            .await
            .expect("upstream effect barrier timed out")
            .expect("upstream mutation stopped before its effect barrier");
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        release_tx
            .send(())
            .expect("detached upstream mutation stopped with its request waiter");

        let audit = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let page = database::list_audit_logs(state.db().as_ref(), None, 10)
                    .await
                    .expect("load upstream mutation audit rows");
                if page
                    .entries
                    .iter()
                    .any(|entry| entry.action == "sunshine.test.mutation")
                {
                    break page;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached upstream mutation did not finish its audit attempt");
        let entries = audit
            .entries
            .iter()
            .filter(|entry| entry.action == "sunshine.test.mutation")
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].target, "test-host");
        assert_eq!(entries[0].detail.as_deref(), Some("safe detail"));
        assert_eq!(entries[0].actor, "test-admin");
        assert_eq!(
            entries[0].request_id.as_deref(),
            Some("cancelled-upstream-request")
        );
    }

    #[tokio::test]
    async fn failed_operation_result_does_not_write_success_audit() {
        let state = initialized_state().await;
        let result = database::with_audit_context(
            audit_context("failed-upstream-request"),
            finish_sunshine_upstream_mutation(
                &state,
                "sunshine.test.failure",
                "test-host".to_string(),
                None,
                async { Err::<serde_json::Value, _>(AppError::Upstream("expected".to_string())) },
            ),
        )
        .await;
        assert!(matches!(result, Err(AppError::Upstream(_))));
        let audit = database::list_audit_logs(state.db().as_ref(), None, 10)
            .await
            .expect("load audit rows after failed upstream mutation");
        assert!(
            audit
                .entries
                .iter()
                .all(|entry| entry.action != "sunshine.test.failure")
        );
    }

    #[tokio::test]
    async fn missing_audit_context_rejects_before_polling_upstream_mutation() {
        let state = initialized_state().await;
        let polled = Arc::new(AtomicBool::new(false));
        let mutation_polled = polled.clone();
        let result = finish_sunshine_upstream_mutation(
            &state,
            "sunshine.test.no_context",
            "test-host".to_string(),
            None,
            async move {
                mutation_polled.store(true, Ordering::SeqCst);
                Ok(serde_json::json!({"status": true}))
            },
        )
        .await;
        assert!(result.is_err());
        assert!(
            !polled.load(Ordering::SeqCst),
            "an unauditable upstream mutation must not start"
        );
    }
}
