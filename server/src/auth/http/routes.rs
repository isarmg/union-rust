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
/// 最右项必须自身就是可解析的裸 IP；若它非法、为空或携带端口，整个解析失败，不能
/// 继续向左寻找攻击者可控的候选值。生产模式会据此返回 421，直接暴露反代配置错误。
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
        .next()?
        .trim()
        .parse::<std::net::IpAddr>()
        .ok()
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
