/// bcrypt 只取密码的前 72 **字节**，多余部分被静默丢弃。
///
/// 这不是"超长会报错"——`bcrypt` crate 直接截断且不返回任何错误。后果是一个隐蔽的
/// 认证等价类：前 72 字节相同的两个不同密码互相可以登录。实测：
///
/// ```text
/// hash("A"×72 + "SUFFIX-ONE")
/// verify("A"×72 + "SUFFIX-TWO-DIFFERENT", 上述 hash) == true
/// ```
///
/// 用户以为自己设了一个 100 字符的强密码，实际只有前 72 字节生效。与其静默截断，
/// 不如明确拒绝——72 字节对任何合理密码都绰绰有余，而"密码尾部不生效"是用户
/// 永远不会自己发现的。
///
/// 注意口径是**字节**而非字符：一个中文字符占 3 字节，24 个汉字就到上限了，
/// 因此错误信息里要说清是字节。
pub(crate) const MIN_PASSWORD_CHARS: usize = 12;

fn validate_username(username: &str) -> AppResult<&str> {
    let username = username.trim();
    if username.is_empty()
        || username.len() > MAX_USERNAME_BYTES
        || username.chars().any(char::is_control)
    {
        return Err(AppError::BadRequest(format!(
            "用户名必须包含 1-{MAX_USERNAME_BYTES} 字节且不能含控制字符"
        )));
    }
    Ok(username)
}

pub(crate) fn validate_bcrypt_input(password: &str, field: &str) -> AppResult<()> {
    if password.is_empty() || password.len() > MAX_BCRYPT_INPUT_BYTES {
        return Err(AppError::BadRequest(format!(
            "{field}必须包含 1-{MAX_BCRYPT_INPUT_BYTES} 字节（当前 {} 字节）",
            password.len()
        )));
    }
    Ok(())
}

pub(crate) fn validate_new_password(password: &str) -> AppResult<()> {
    validate_bcrypt_input(password, "新密码")?;
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(AppError::BadRequest(format!(
            "新密码至少需要 {MIN_PASSWORD_CHARS} 个字符"
        )));
    }
    Ok(())
}

fn bcrypt_permit(state: &AppState) -> AppResult<tokio::sync::OwnedSemaphorePermit> {
    state
        .auth
        .bcrypt_limit
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::TooManyRequests("密码校验繁忙，请稍后再试".to_string()))
}

async fn verify_bcrypt(state: &AppState, password: String, hash: String) -> AppResult<bool> {
    validate_bcrypt_input(&password, "密码")?;
    let permit = bcrypt_permit(state)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        bcrypt::verify(password, &hash)
    })
    .await
    .map_err(|error| AppError::Anyhow(anyhow::anyhow!("bcrypt task error: {error}")))?
    .map_err(|error| AppError::Anyhow(anyhow::anyhow!("bcrypt verify error: {error}")))
}

async fn hash_bcrypt(state: &AppState, password: String) -> AppResult<String> {
    validate_bcrypt_input(&password, "新密码")?;
    let permit = bcrypt_permit(state)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        bcrypt::hash(password, bcrypt::DEFAULT_COST)
    })
    .await
    .map_err(|error| AppError::Anyhow(anyhow::anyhow!("bcrypt task error: {error}")))?
    .map_err(|error| AppError::Anyhow(anyhow::anyhow!("bcrypt hash error: {error}")))
}

async fn persist_local_config_blocking(
    config: crate::config::LocalConfig,
) -> anyhow::Result<crate::config::LocalConfig> {
    tokio::task::spawn_blocking(move || {
        save_local_config(&config)?;
        Ok(config)
    })
    .await
    .map_err(|error| anyhow::anyhow!("local config persistence task failed: {error}"))?
}

/// Execute a password replacement as one serialized state transition.
///
/// The dedicated one-permit gate covers verification through publication, so
/// two requests that both present the old password cannot both succeed. The
/// config read lock is released before bcrypt and disk I/O; the newly persisted
/// snapshot is published only after atomic file replacement and fsync succeed.
async fn replace_password_with<P, Fut>(
    state: &AppState,
    current_password: String,
    new_password: String,
    persist: P,
) -> AppResult<()>
where
    P: FnOnce(crate::config::LocalConfig) -> Fut + Send,
    Fut: std::future::Future<Output = anyhow::Result<crate::config::LocalConfig>> + Send,
{
    validate_bcrypt_input(&current_password, "当前密码")?;
    validate_new_password(&new_password)?;

    let _change_permit = state
        .auth
        .password_change_gate
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| AppError::Anyhow(anyhow::anyhow!("password change gate was closed")))?;

    let current_config = state.auth.local_config.read().await.clone();
    if !verify_bcrypt(
        state,
        current_password,
        current_config.admin_password_hash.clone(),
    )
    .await?
    {
        return Err(AppError::BadRequest("当前密码不正确".to_string()));
    }

    let mut updated_config = current_config;
    updated_config.admin_password_hash = hash_bcrypt(state, new_password).await?;
    let persisted_config = persist(updated_config).await.map_err(AppError::Anyhow)?;
    *state.auth.local_config.write().await = persisted_config;
    Ok(())
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// POST /api/auth/login — JSON 登录并建立浏览器 Cookie 会话。
pub(crate) async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<LoginRequest>,
) -> AppResult<Response> {
    let client = require_reverse_proxy_contract(&state, &headers, "登录接口")?;
    let user = authenticate(&state, &payload.username, payload.password, client).await?;
    create_login_response(&state, user).await
}

async fn create_login_response(state: &AppState, username: String) -> AppResult<Response> {
    let token = uuid::Uuid::new_v4().to_string();
    let csrf_token = uuid::Uuid::new_v4().simple().to_string();
    let expires_at = chrono::Utc::now() + chrono::Duration::days(7);
    let expired = {
        let mut sessions = state.auth.sessions.write().await;
        let now = chrono::Utc::now();
        let mut expired = Vec::new();
        sessions.retain(|token, session| {
            if session.expires_at <= now {
                expired.push(token.clone());
                false
            } else {
                true
            }
        });
        sessions.insert(
            token.clone(),
            LocalSession {
                username: username.clone(),
                expires_at,
                csrf_token: csrf_token.clone(),
            },
        );
        expired
    };
    cancel_session_streams(state, &expired).await;

    let mut response = Json(LoginResponse { username }).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // 会话令牌保持 HttpOnly，不暴露给 JavaScript。
    headers.append(
        header::SET_COOKIE,
        cookie_header(&session_cookie_value(
            &token,
            state.settings.production,
            SESSION_MAX_AGE,
        ))?,
    );
    // CSRF 令牌必须**能被前端读取**并回填到请求头，因此不设 HttpOnly。
    // 这不降低安全性：双提交模式的前提正是"跨站请求读不到本站 cookie"。
    headers.append(
        header::SET_COOKIE,
        cookie_header(&csrf_cookie_value(
            &csrf_token,
            state.settings.production,
            SESSION_MAX_AGE,
        ))?,
    );
    Ok(response)
}
