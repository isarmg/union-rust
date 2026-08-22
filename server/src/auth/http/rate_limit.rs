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
