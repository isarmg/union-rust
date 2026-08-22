//! 认证与账号管理 handler。
//!
//! # 认证流程
//!
//! 管理台使用 JSON 登录，验证成功后通过 HttpOnly Cookie 建立会话。
//!
//! Token 是随机 UUID，仅保存在进程内存；有效期 7 天，重启或改密后失效。

use crate::auth::{
    ChangePasswordRequest, LoginRequest, LoginResponse, MAX_BCRYPT_INPUT_BYTES, MAX_USERNAME_BYTES,
    UserInfoResponse,
};
use crate::{
    config::save_local_config,
    error::{AppError, AppResult},
    infra::database,
    state::{AppState, LocalSession, LoginAttemptState, SseSessionCancellation},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};

const LOGIN_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_LOGIN_ATTEMPTS: usize = 5;
const MAX_LOGIN_ATTEMPTS_PER_IP: usize = 10;
/// 全局桶只作最后兜底（防 bcrypt 资源耗尽），阈值必须显著高于单 IP 上限，
/// 否则它本身就成了"任何人都能触发的管理员锁定"开关。真正的防刷靠按 IP 分桶。
const MAX_GLOBAL_LOGIN_ATTEMPTS: usize = 600;
const AUTH_JSON_BODY_LIMIT: usize = 4 * 1024;
const TRUSTED_PROXY_HEADER: &str = "x-unionc-proxy-secret";

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/auth/login",
            post(login).layer(DefaultBodyLimit::max(AUTH_JSON_BODY_LIMIT)),
        )
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/me", get(me))
        .route(
            "/api/auth/change-password",
            post(change_password).layer(DefaultBodyLimit::max(AUTH_JSON_BODY_LIMIT)),
        )
}

/// 从 Cookie 头提取 session cookie 的值。
pub(crate) fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookie_str = headers.get(header::COOKIE).and_then(|v| v.to_str().ok())?;
    let mut regular = None;
    for part in cookie_str.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("__Host-session=") {
            return Some(value.to_string());
        }
        if regular.is_none()
            && let Some(value) = part.strip_prefix("session=")
        {
            regular = Some(value.to_string());
        }
    }
    regular
}

/// 管理台只使用 HttpOnly Cookie，不把长效会话令牌暴露给 JavaScript。
pub(crate) fn extract_token(headers: &HeaderMap) -> Option<String> {
    session_cookie(headers)
}

/// 从 `X-Forwarded-For` 解析发起请求的客户端地址。
///
/// # 为什么取**最右**一项
///
/// XFF 由各级代理自左向右追加：每一跳把"它看到的对端地址"接在末尾。因此最后一项
/// 是**离本服务最近的那个代理**写入的，即该代理看到的真实对端。
///
/// 攻击者可以自行伪造整个头部，但我们的反代会在其后追加真实对端地址：
///
/// ```text
/// 攻击者发送:  X-Forwarded-For: 1.2.3.4, 5.6.7.8
/// 反代转发后:  X-Forwarded-For: 1.2.3.4, 5.6.7.8, <真实IP>
///                                                  ^^^^^^^^ 取这个
/// ```
///
/// 若改取最左项，攻击者只需每次请求换一个伪造 IP 就能完全绕过按 IP 的限流。
///
/// # 为什么必须遍历**全部** XFF 头，而不是 `headers.get()`
///
/// 反代写 XFF 有两种做法，取决于它的配置：
///
/// ```text
/// 追加到同一行（nginx $proxy_add_x_forwarded_for）：
///   X-Forwarded-For: 1.2.3.4, <真实IP>
///
/// 另起一行（Traefik、部分 ALB、手写 add_header 的配置）：
///   X-Forwarded-For: 1.2.3.4          ← 攻击者自带的
///   X-Forwarded-For: <真实IP>          ← 反代追加的
/// ```
///
/// `HeaderMap::get()` **只返回第一个**同名头。在第二种部署下它取到的是攻击者
/// 完全可控的那一行，于是"取最右项"这个防线守的是攻击者自己写的列表——每次请求
/// 换一个伪造 IP 就能让 `by_ip` 与 `by_ip_username` 两层配额同时失效，只剩
/// 600 次/分钟的全局桶兜底。而这一切不产生任何信号：登录正常、日志正常。
///
/// 因此改用 `get_all()` 取**最后一个** header value，再在其中取最右项——
/// 两种写法下拿到的都是离本服务最近的那一跳写入的地址。
///
/// # 前提假设
///
/// 本实现假定 UnionC 前面**恰好有一层可信反向代理**（部署形态即如此：生产强制
/// 绑定回环，只能经反代访问）。若将来在反代之前再叠加 CDN 等额外跳数，必须改为
/// "从右往左跳过 N 个可信代理"，否则取到的会是反代之间的内网地址。
pub(crate) fn client_ip(headers: &HeaderMap) -> Option<std::net::IpAddr> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .next_back()?
        .to_str()
        .ok()?
        .rsplit(',')
        .find_map(|entry| entry.trim().parse::<std::net::IpAddr>().ok())
}

/// 校验请求确实经由预期的反向代理链路抵达，并返回解析出的客户端 IP。
///
/// # 为什么 XFF 也必须是硬要求
///
/// 生产部署强制回环绑定，因此**所有**流量都经过反向代理。若只把
/// `X-Forwarded-Proto` 做成硬失败、`X-Forwarded-For` 缺失时软降级放行，
/// 三层配额里按 IP、按 (IP, 用户名) 的两层会同时失效，只剩 600 次/分钟的全局兜底。
///
/// 危险之处在于**这不产生任何信号**：一份只配了 XFP 的反代能通过启动检查、
/// 能正常登录、日志里也不会有异常，而防爆破能力已经放宽了两个数量级。
/// 两个请求头承担的都是"这条链路可信"的前提，严格程度没有理由不对称——
/// 因此这里把它们并为同一个契约，缺任一项都在**第一个请求**上就明确报错。
pub(crate) fn require_reverse_proxy_contract(
    state: &AppState,
    headers: &HeaderMap,
    what: &str,
) -> AppResult<Option<std::net::IpAddr>> {
    let client = client_ip(headers);
    if !state.settings.production {
        return Ok(client);
    }
    if headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        != Some("https")
    {
        return Err(AppError::MisdirectedRequest(format!(
            "{what}只能经 HTTPS 反向代理访问：请求缺少 X-Forwarded-Proto: https"
        )));
    }
    client.ok_or_else(|| {
        AppError::MisdirectedRequest(format!(
            "{what}的请求缺少可解析的 X-Forwarded-For；反向代理必须透传客户端地址，\
                 否则按 IP 与按账号的登录限流都会静默失效。请在反代上启用 X-Forwarded-For"
        ))
    })?;
    let supplied_secret = headers
        .get(TRUSTED_PROXY_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if state.settings.server.proxy_secret.is_empty()
        || !constant_time_text_eq(&state.settings.server.proxy_secret, supplied_secret)
    {
        return Err(AppError::MisdirectedRequest(format!(
            "{what}缺少可信反向代理证明；请让反代覆盖写入 X-UnionC-Proxy-Secret"
        )));
    }
    Ok(client)
}

fn constant_time_text_eq(expected: &str, actual: &str) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .as_bytes()
        .iter()
        .zip(actual.as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

async fn authenticate(
    state: &AppState,
    username: &str,
    password: String,
    client: Option<std::net::IpAddr>,
) -> AppResult<String> {
    let username = validate_username(username)?;
    validate_bcrypt_input(&password, "密码")?;
    let key = username.to_ascii_lowercase();
    let now = std::time::Instant::now();
    {
        let mut attempts = state.auth.login_attempts.lock().await;
        if login_quota_exhausted(&mut attempts, &key, client, now) {
            return Err(AppError::TooManyRequests(
                "登录尝试过于频繁，请一分钟后再试".to_string(),
            ));
        }
        // 在昂贵校验前占用名额，避免并发请求同时穿过限流检查。
        record_login_attempt(&mut attempts, &key, client, now);
    }

    let config = state.auth.local_config.read().await;
    // 用户名按 ASCII 大小写**不敏感**比对，与上面限流用的 `key` 保持同一口径。
    //
    // 两处口径不一致会产生一个很难自查的现象：`Admin` 与 `admin` 落进同一个限流桶
    // （因为 key 做了 to_ascii_lowercase），却被判成两个不同的账号——于是管理员
    // 大小写敲错时不仅登不上（走 dummy hash 必然失败），还照常消耗自己的配额，
    // 而错误信息只有一句"账号或密码错误"。
    //
    // 选择放宽而不是收紧，是因为用户名在这里只是账号标识、不承担任何熵：口令强度
    // 完全由密码提供。让大小写敏感把一个纯粹的输入习惯问题变成认证失败，没有收益。
    let known_user = username.eq_ignore_ascii_case(&config.admin_username);
    let hash = if known_user {
        config.admin_password_hash.clone()
    } else {
        (*state.auth.dummy_password_hash).clone()
    };
    let configured_username = config.admin_username.clone();
    drop(config);
    let valid = verify_bcrypt(state, password, hash).await?;

    match (valid, known_user) {
        (true, true) => {
            // 成功登录释放该来源的配额，避免管理员多次手误后被自己的桶挡住。
            // 只清与本次来源相关的两个桶——全局桶是资源兜底，不该被一次成功登录重置。
            if let Some(address) = client {
                let mut attempts = state.auth.login_attempts.lock().await;
                attempts.by_ip.remove(&address);
                attempts.by_ip_username.remove(&(address, key.clone()));
            }
            Ok(configured_username)
        }
        _ => Err(AppError::Unauthorized),
    }
}

/// 为一次昂贵的密码运算占用配额。与 `authenticate` 共用窗口，因此登录与改密加起来
/// 才是总配额——否则改密就成了绕过登录限流的旁路。
async fn consume_password_attempt(
    state: &AppState,
    username: &str,
    client: Option<std::net::IpAddr>,
) -> AppResult<()> {
    let key = validate_username(username)?.to_ascii_lowercase();
    let now = std::time::Instant::now();
    let mut attempts = state.auth.login_attempts.lock().await;
    if login_quota_exhausted(&mut attempts, &key, client, now) {
        return Err(AppError::TooManyRequests(
            "密码操作过于频繁，请一分钟后再试".to_string(),
        ));
    }
    record_login_attempt(&mut attempts, &key, client, now);
    Ok(())
}

// ─── 限流桶的共享实现 ─────────────────────────────────────────────────────────
//
// 登录与改密共用这一份"清理过期 → 判断超额 → 记账"实现。各自抄一遍的话，改配额策略
// 就要记得改两处，而两份实现连判断顺序都容易走偏。
//
// # 热路径与回收的分工
//
// 在**每次**登录尝试里 `retain` 一遍 `by_ip` 与 `by_ip_username` 两张 map，等于在全局
// 锁内做 O(桶数) 的扫描。登录是一条**外部可任意触发**的路径，而分布式撞库会同时推高
// 桶数与调用频率——那样正好在最需要扛住的时候最慢。因此拆成两半：
//   * 热路径只清理**本次真正查阅的那 3 个 Vec**（global、by_ip[addr]、
//     by_ip_username[(addr,key)]）。每个 Vec 的长度天然被窗口内的配额上限压住，
//     因此这是常数级开销，且顺带保证了它们不会无界增长；
//   * 遍历整张 map 丢弃空桶交给 `startup::start_memory_gc`——回收不紧急，
//     一个空桶只占几十字节。
//
// 这与 `state.rs` 里 `allow_report` 的结论是同一个模式。

/// 就地丢弃窗口外的记录，返回窗口内的剩余条数。
fn prune_window(values: &mut Vec<std::time::Instant>, now: std::time::Instant) -> usize {
    values.retain(|instant| now.duration_since(*instant) < LOGIN_WINDOW);
    values.len()
}

/// 遍历整张 map 丢弃已空的桶。**只**由后台维护任务调用。
pub fn prune_login_attempts(attempts: &mut LoginAttemptState, now: std::time::Instant) {
    prune_window(&mut attempts.global, now);
    attempts
        .by_ip_username
        .retain(|_, values| prune_window(values, now) > 0);
    attempts
        .by_ip
        .retain(|_, values| prune_window(values, now) > 0);
}

/// 判断本次尝试是否超出任一层配额，同时清理这几个桶里的过期记录。
///
/// 三层的分工：
/// * `by_ip_username` —— 遏制针对**单个账号**的暴力破解，同时因为键里带 IP，
///   打满它只会锁住攻击者自己，不会波及真正的管理员（见 `LoginAttemptState` 的说明）；
/// * `by_ip` —— 遏制单一来源的撞库（换用户名也绕不开）；
/// * `global` —— 仅作 bcrypt 资源耗尽的最后兜底，阈值显著高于前两者。
fn login_quota_exhausted(
    attempts: &mut LoginAttemptState,
    key: &str,
    client: Option<std::net::IpAddr>,
    now: std::time::Instant,
) -> bool {
    if prune_window(&mut attempts.global, now) >= MAX_GLOBAL_LOGIN_ATTEMPTS {
        return true;
    }
    let Some(address) = client else {
        // 取不到来源 IP（反代未透传 XFF）时，两个分桶都无从建立，只剩全局桶兜底。
        // 生产环境下 `require_reverse_proxy_contract` 已把这种请求挡在门外。
        return false;
    };
    let by_ip = attempts
        .by_ip
        .get_mut(&address)
        .is_some_and(|values| prune_window(values, now) >= MAX_LOGIN_ATTEMPTS_PER_IP);
    let by_account = attempts
        .by_ip_username
        .get_mut(&(address, key.to_string()))
        .is_some_and(|values| prune_window(values, now) >= MAX_LOGIN_ATTEMPTS);
    by_ip || by_account
}

fn record_login_attempt(
    attempts: &mut LoginAttemptState,
    key: &str,
    client: Option<std::net::IpAddr>,
    now: std::time::Instant,
) {
    attempts.global.push(now);
    if let Some(address) = client {
        attempts.by_ip.entry(address).or_default().push(now);
        attempts
            .by_ip_username
            .entry((address, key.to_string()))
            .or_default()
            .push(now);
    }
}

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

async fn revoke_user_sessions_except(state: &AppState, username: &str, retained_token: &str) {
    let revoked = {
        let mut sessions = state.auth.sessions.write().await;
        let mut revoked = Vec::new();
        sessions.retain(|token, session| {
            let remove = token != retained_token && session.username == username;
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

/// POST /api/auth/change-password — 修改密码，使其他设备会话失效。
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

    revoke_user_sessions_except(&state, &username, &token).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn password_state(password_hash: String) -> AppState {
        AppState::new(
            crate::config::Settings::default(),
            crate::infra::database::in_memory_pool().expect("in-memory database"),
            password_hash.clone(),
            crate::config::LocalConfig {
                application_version: env!("CARGO_PKG_VERSION").to_string(),
                admin_username: "admin".to_string(),
                admin_password_hash: password_hash,
            },
            crate::system::ResourceMonitor::frozen(Default::default()),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn persistence_failure_does_not_publish_the_new_password() {
        let old_hash = bcrypt::hash("old-password-value", 4).unwrap();
        let state = password_state(old_hash.clone());
        let result = replace_password_with(
            &state,
            "old-password-value".to_string(),
            "replacement-password-value".to_string(),
            |_config| async { Err(anyhow::anyhow!("simulated fsync failure")) },
        )
        .await;

        assert!(result.is_err());
        assert_eq!(
            state.auth.local_config.read().await.admin_password_hash,
            old_hash,
            "memory must remain on the last successfully persisted snapshot"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_changes_using_the_same_old_password_cannot_both_commit() {
        let old_hash = bcrypt::hash("old-password-value", 4).unwrap();
        let state = password_state(old_hash);
        let commits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let spawn_change = |new_password: &'static str| {
            let state = state.clone();
            let commits = commits.clone();
            tokio::spawn(async move {
                replace_password_with(
                    &state,
                    "old-password-value".to_string(),
                    new_password.to_string(),
                    move |config| async move {
                        commits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        Ok(config)
                    },
                )
                .await
            })
        };

        let (first, second) = tokio::join!(
            spawn_change("first-replacement-password"),
            spawn_change("second-replacement-password")
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_ne!(first.is_ok(), second.is_ok());
        assert_eq!(commits.load(std::sync::atomic::Ordering::SeqCst), 1);

        let final_hash = state
            .auth
            .local_config
            .read()
            .await
            .admin_password_hash
            .clone();
        let first_won = bcrypt::verify("first-replacement-password", &final_hash).unwrap();
        let second_won = bcrypt::verify("second-replacement-password", &final_hash).unwrap();
        assert_ne!(first_won, second_won);
    }

    #[tokio::test]
    async fn revoking_a_session_notifies_its_established_sse_stream() {
        let state = password_state(bcrypt::hash("old-password-value", 4).unwrap());
        state.auth.sessions.write().await.insert(
            "revoked-session".to_string(),
            LocalSession {
                username: "admin".to_string(),
                expires_at: chrono::Utc::now() + chrono::Duration::minutes(5),
                csrf_token: "csrf".to_string(),
            },
        );
        let mut cancellation = sse_session_cancellation(&state, "revoked-session")
            .await
            .unwrap();

        revoke_session(&state, "revoked-session").await;
        tokio::time::timeout(std::time::Duration::from_secs(1), cancellation.cancelled())
            .await
            .expect("SSE cancellation must be signalled promptly");
        assert!(cancellation.is_cancelled());
    }

    #[tokio::test]
    async fn password_change_revokes_other_session_streams_but_keeps_the_caller() {
        let state = password_state(bcrypt::hash("old-password-value", 4).unwrap());
        let expires_at = chrono::Utc::now() + chrono::Duration::minutes(5);
        for token in ["current-session", "other-session"] {
            state.auth.sessions.write().await.insert(
                token.to_string(),
                LocalSession {
                    username: "admin".to_string(),
                    expires_at,
                    csrf_token: "csrf".to_string(),
                },
            );
        }
        let mut current = sse_session_cancellation(&state, "current-session")
            .await
            .unwrap();
        let mut other = sse_session_cancellation(&state, "other-session")
            .await
            .unwrap();

        revoke_user_sessions_except(&state, "admin", "current-session").await;

        tokio::time::timeout(std::time::Duration::from_secs(1), other.cancelled())
            .await
            .expect("revoked device SSE must close promptly");
        assert!(other.is_cancelled());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), current.cancelled())
                .await
                .is_err(),
            "the session performing the password change should remain connected"
        );
        assert!(!current.is_cancelled());
    }

    fn headers(forwarded_for: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in forwarded_for {
            headers.append("x-forwarded-for", HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    fn ip(value: &str) -> std::net::IpAddr {
        value.parse().unwrap()
    }

    /// 单行 XFF：取最右项，即离本服务最近的那一跳写入的地址。
    #[test]
    fn takes_the_rightmost_entry_of_a_single_header() {
        assert_eq!(
            client_ip(&headers(&["1.2.3.4, 5.6.7.8, 203.0.113.9"])),
            Some(ip("203.0.113.9"))
        );
    }

    /// 反代另起一行追加 XFF 时（Traefik、部分 ALB），攻击者自带的那一行是**第一个**。
    ///
    /// 回归：此前用 `HeaderMap::get()` 只读第一个头，于是这里会返回攻击者完全可控的
    /// `1.2.3.4`——每次请求换一个伪造 IP 即可绕过按 IP 与按 (IP,用户名) 的两层登录
    /// 限流，且不产生任何异常信号。
    #[test]
    fn a_spoofed_first_header_cannot_shadow_the_proxy_appended_one() {
        assert_eq!(
            client_ip(&headers(&["1.2.3.4", "203.0.113.9"])),
            Some(ip("203.0.113.9")),
            "必须采信最后一个 XFF 头，而不是攻击者自带的第一个"
        );
        // 攻击者在自己那一行里塞多少伪造项都不影响结果。
        assert_eq!(
            client_ip(&headers(&["9.9.9.9, 8.8.8.8", "203.0.113.9"])),
            Some(ip("203.0.113.9"))
        );
    }

    /// 伪造项无法通过"不可解析"把取值挤回上一项。
    #[test]
    fn unparseable_entries_are_skipped_within_the_trusted_header() {
        assert_eq!(
            client_ip(&headers(&["1.2.3.4", "203.0.113.9, not-an-ip"])),
            Some(ip("203.0.113.9"))
        );
    }

    #[test]
    fn missing_or_unparseable_forwarded_for_yields_none() {
        assert_eq!(client_ip(&HeaderMap::new()), None);
        assert_eq!(client_ip(&headers(&["not-an-ip"])), None);
        assert_eq!(client_ip(&headers(&[""])), None);
    }

    /// IPv6 与带端口的写法。带端口的项不可解析为 `IpAddr`，会被跳过——
    /// 这是刻意的：宁可取不到来源、退回全局桶，也不要把端口误当作地址的一部分。
    #[test]
    fn ipv6_entries_are_parsed() {
        assert_eq!(
            client_ip(&headers(&["1.2.3.4", "2001:db8::1"])),
            Some(ip("2001:db8::1"))
        );
    }
}
