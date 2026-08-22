/// POST /api/auth/logout — 删除会话，清除 cookie。
pub(crate) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Response> {
    if let Some(token) = extract_token(&headers) {
        revoke_session(&state, &token).await;
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    let out = response.headers_mut();
    out.append(
        header::SET_COOKIE,
        cookie_header(&session_cookie_value("", state.settings.production, 0))?,
    );
    out.append(
        header::SET_COOKIE,
        cookie_header(&csrf_cookie_value("", state.settings.production, 0))?,
    );
    Ok(response)
}

async fn cancel_session_streams(state: &AppState, tokens: &[String]) {
    if tokens.is_empty() {
        return;
    }
    {
        let mut cancellations = state.auth.session_revocations.lock().await;
        for token in tokens {
            if let Some(sender) = cancellations.remove(token) {
                let _ = sender.send(true);
            }
        }
    }
    state
        .auth
        .sse_tickets
        .lock()
        .await
        .retain(|_, ticket| !tokens.iter().any(|token| token == &ticket.session_token));
}

async fn revoke_session(state: &AppState, token: &str) {
    state.auth.sessions.write().await.remove(token);
    cancel_session_streams(state, &[token.to_string()]).await;
}

async fn revoke_user_sessions(state: &AppState, username: &str) {
    let revoked = {
        let mut sessions = state.auth.sessions.write().await;
        let mut revoked = Vec::new();
        sessions.retain(|token, session| {
            let remove = session.username == username;
            if remove {
                revoked.push(token.clone());
            }
            !remove
        });
        revoked
    };
    cancel_session_streams(state, &revoked).await;
}

/// Validate a session and atomically attach an SSE cancellation receiver.
///
/// Holding the session read lock until the cancellation sender exists closes
/// the race where logout could otherwise happen between validation and
/// subscription. Logout needs the corresponding write lock and will therefore
/// always observe and signal the newly-created sender.
pub(crate) async fn sse_session_cancellation(
    state: &AppState,
    token: &str,
) -> AppResult<SseSessionCancellation> {
    let now = chrono::Utc::now();
    let sessions = state.auth.sessions.read().await;
    let expires_at = match sessions.get(token) {
        Some(session) if session.expires_at > now => session.expires_at,
        _ => return Err(AppError::Unauthorized),
    };
    let remaining = (expires_at - now)
        .to_std()
        .map_err(|_| AppError::Unauthorized)?;
    let mut cancellations = state.auth.session_revocations.lock().await;
    let receiver = cancellations
        .entry(token.to_string())
        .or_insert_with(|| tokio::sync::watch::channel(false).0)
        .subscribe();
    Ok(SseSessionCancellation::new(
        receiver,
        tokio::time::Instant::now() + remaining,
    ))
}

const SESSION_MAX_AGE: u64 = 604_800; // 7 天，与 LocalSession.expires_at 保持一致

/// 会话 cookie 名。生产环境用 `__Host-` 前缀（要求 Secure + Path=/ + 无 Domain）。
pub(crate) const SESSION_COOKIE: &str = "session";
pub(crate) const SECURE_SESSION_COOKIE: &str = "__Host-session";
/// CSRF cookie 名。前端需要读取它，因此不设 HttpOnly。
pub(crate) const CSRF_COOKIE: &str = "csrf";
pub(crate) const SECURE_CSRF_COOKIE: &str = "__Host-csrf";
/// 前端回填 CSRF 令牌所用的请求头。
pub(crate) const CSRF_HEADER: &str = "x-csrf-token";

fn session_cookie_value(token: &str, secure: bool, max_age: u64) -> String {
    format!(
        "{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{}",
        if secure {
            SECURE_SESSION_COOKIE
        } else {
            SESSION_COOKIE
        },
        if secure { "; Secure" } else { "" }
    )
}

fn csrf_cookie_value(token: &str, secure: bool, max_age: u64) -> String {
    format!(
        "{}={token}; Path=/; SameSite=Strict; Max-Age={max_age}{}",
        if secure {
            SECURE_CSRF_COOKIE
        } else {
            CSRF_COOKIE
        },
        if secure { "; Secure" } else { "" }
    )
}

fn cookie_header(cookie: &str) -> AppResult<HeaderValue> {
    HeaderValue::from_str(cookie)
        .map_err(|error| AppError::Anyhow(anyhow::anyhow!("invalid session cookie: {error}")))
}

/// GET /api/auth/me — 返回当前登录用户名。
pub(crate) async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<UserInfoResponse>> {
    let token = extract_token(&headers).ok_or(AppError::Unauthorized)?;
    let user = local_session_user(&state, &token).await?;
    Ok(Json(UserInfoResponse { username: user }))
}

/// POST /api/auth/change-password — 修改密码，并使该账号的全部会话失效。
pub(crate) async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ChangePasswordRequest>,
) -> AppResult<StatusCode> {
    // 与登录接口走同一个反向代理契约。
    //
    // 这个接口的配额同样按 (IP, 用户名) 分桶，因此它和登录一样依赖 XFF：若只在登录
    // 处强制透传，攻击者拿到一个会话后剥掉 XFF 就能让改密的按账号配额失效，
    // 只剩全局桶与 4 个信号量兜底——而爆破成功的收益是直接改掉管理员密码。
    // 前提虽然更强（需要已认证会话），但成本只是复用同一个函数，没有理由留缺口。
    let client = require_reverse_proxy_contract(&state, &headers, "改密接口")?;
    let token = extract_token(&headers).ok_or(AppError::Unauthorized)?;
    let username = local_session_user(&state, &token).await?;
    // 改密与登录共用同一套配额。
    //
    // 这个接口每次调用要跑**两次** bcrypt（校验旧密码 + 哈希新密码），是全站单位请求
    // CPU 开销最高的一个。只靠 4 个信号量的并发约束、不加速率约束是不够的：一个已认证
    // 会话可以持续调用来占满密码校验通道，让**登录**接口一并返回 429。
    // 复用按用户名的窗口计数即可，无需新增机制。
    consume_password_attempt(&state, &username, client).await?;
    replace_password_with(
        &state,
        payload.current_password,
        payload.new_password,
        persist_local_config_blocking,
    )
    .await?;
    if let Err(error) = database::insert_audit(
        state.db().as_ref(),
        "auth.password.change",
        &username,
        Some("administrator password changed"),
    )
    .await
    {
        tracing::warn!("管理员密码已修改，但审计日志写入失败：{error}");
    }

    Ok(StatusCode::NO_CONTENT)
}

/// 按 token 取出会话。
///
/// # 为什么走读锁
///
/// 每次鉴权都取**写锁**（为了顺带清理过期会话）会让所有已认证请求的鉴权阶段完全
/// 串行化。这里走读锁快路径：命中且未过期即返回，只有在会话确实过期或不存在时才
/// 升级为写锁做清理。常规请求因此可以并发鉴权。
///
/// 从不再被访问的过期会话由 `startup::start_memory_gc` 的后台任务定期清理，
/// 不依赖鉴权路径顺带回收。
pub(crate) async fn local_session(state: &AppState, token: &str) -> AppResult<LocalSession> {
    let now = chrono::Utc::now();
    {
        let sessions = state.auth.sessions.read().await;
        if let Some(session) = sessions.get(token) {
            if session.expires_at > now {
                return Ok(session.clone());
            }
        } else {
            return Err(AppError::Unauthorized);
        }
    }
    // 走到这里说明会话存在但已过期：立刻移除，避免过期令牌被反复试探。
    revoke_session(state, token).await;
    Err(AppError::Unauthorized)
}

pub(crate) async fn local_session_user(state: &AppState, token: &str) -> AppResult<String> {
    Ok(local_session(state, token).await?.username)
}

/// 清理所有已过期的登录尝试记录。由后台维护任务调用，返回回收的桶数。
pub async fn prune_login_buckets(state: &AppState) -> usize {
    let now = std::time::Instant::now();
    let mut attempts = state.auth.login_attempts.lock().await;
    let before = attempts.by_ip.len() + attempts.by_ip_username.len();
    prune_login_attempts(&mut attempts, now);
    before - (attempts.by_ip.len() + attempts.by_ip_username.len())
}

/// 清理所有已过期会话。由后台维护任务调用。
pub async fn prune_expired_sessions(state: &AppState) -> usize {
    let now = chrono::Utc::now();
    let expired = {
        let mut sessions = state.auth.sessions.write().await;
        let mut expired = Vec::new();
        sessions.retain(|token, session| {
            if session.expires_at <= now {
                expired.push(token.clone());
                false
            } else {
                true
            }
        });
        expired
    };
    let removed = expired.len();
    cancel_session_streams(state, &expired).await;
    removed
}
