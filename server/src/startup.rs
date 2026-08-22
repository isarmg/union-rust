//! UnionC 应用启动编排。

use std::net::{IpAddr, SocketAddr};

use crate::{
    config::{
        LocalConfig, RetentionSettings, RuntimeEnvironment, Settings, ensure_layout,
        load_local_config, save_local_config,
    },
    infra::database,
    infra::secrets,
    state::AppState,
};

pub struct InitializedApp {
    pub addr: SocketAddr,
    pub state: AppState,
}

pub async fn initialize() -> anyhow::Result<InitializedApp> {
    // 必须在任何配置读写之前解析数据目录：管理员配置与主密钥都落在这里，
    // 解析晚了就会先按相对路径读一次、扑空、然后"首次启动"重新 bootstrap。
    crate::infra::paths::init()?;
    let runtime = RuntimeEnvironment::from_environment()?;
    let bootstrap_settings = Settings::load(&runtime)?;
    if bootstrap_settings.production && bootstrap_settings.server.proxy_secret.is_empty() {
        anyhow::bail!(
            "UNIONC_PROXY_SECRET is required when serving in production; configure the same \
             64-character lowercase hexadecimal value in the trusted reverse proxy"
        );
    }
    let allow_bootstrap = bootstrap_allowed(
        bootstrap_settings.production,
        std::env::var("UNIONC_ALLOW_BOOTSTRAP").ok().as_deref(),
    );
    ensure_layout()?;
    let database_path = database::database_path(&bootstrap_settings)?;
    database::hold_server_database_lock(&database_path)?;
    secrets::init(runtime.mode)?;
    let local_config = load_or_create_local_config(&bootstrap_settings, allow_bootstrap).await?;
    // SQLite 是 Server 数据目录内的固定持久层。只有开发环境或显式 bootstrap 才能创建；
    // 正常生产启动只打开并精确校验既有数据库，不能用空库掩盖数据文件丢失。
    let (settings, db) = prepare_database(bootstrap_settings, allow_bootstrap).await?;
    let addr = listen_address(&settings)?;
    let dummy_password_hash = hash_password(uuid::Uuid::new_v4().to_string()).await?;
    // 建立差值采样基线并启动唯一的采样循环，使首个请求即返回有效读数。
    let resources = crate::system::ResourceMonitor::start().await;
    let state = AppState::new(settings, db, dummy_password_hash, local_config, resources)?;
    start_service_status_probe(state.clone());
    // 内存态和持久历史分别回收；SQLite 在 HTTP 服务启动前已经完成打开与校验，因此
    // 保留期任务在每次正常启动中都存在，不会出现“配置数据库后忘记重启而不清理”的窗口。
    start_memory_gc(state.clone());
    start_database_retention(state.clone(), runtime.retention);
    Ok(InitializedApp { addr, state })
}

/// `unionc rekey` 子命令：用当前密钥重新加密全部存量密文。
///
/// 不启动 HTTP 服务，跑完即退出。典型的轮换流程见 `secrets::Keyring` 的文档。
pub async fn rekey() -> anyhow::Result<()> {
    crate::infra::paths::init()?;
    let runtime = RuntimeEnvironment::from_environment()?;
    let settings = Settings::load(&runtime)?;
    ensure_layout()?;
    let database_path = database::database_path(&settings)?;
    let _locks = database::acquire_offline_maintenance_locks(&database_path)?;
    secrets::init(runtime.mode)?;
    let pool = database::connect_existing(&settings).await?;
    database::verify_schema(&pool).await?;

    let key_id = secrets::current_key_id()?;
    let hosts = database::rekey_secrets(&pool).await?;
    tracing::info!(
        "已用密钥 '{key_id}' 重新加密 {hosts} 台 Sunshine 主机的凭据；\
         确认无误后即可移除 UNIONC_SECRET_KEY_PREVIOUS 并重启"
    );
    Ok(())
}

/// `unionc reset-admin-password`：离线生成并写入新的管理员密码。
///
/// 只读取/替换本地私有配置，不连接数据库，也不改动业务数据、Agent 凭据或主密钥。
/// 命令会把随机密码输出一次；调用方随后必须重启 Server，使旧内存会话全部失效。
pub async fn reset_admin_password() -> anyhow::Result<(String, String)> {
    crate::infra::paths::init()?;
    let runtime = RuntimeEnvironment::from_environment()?;
    let settings = Settings::load(&runtime)?;
    ensure_layout()?;
    let database_path = database::database_path(&settings)?;
    let _database_lock = database::acquire_database_lock(&database_path)?;
    let (config, password) = config_with_reset_password(load_local_config()?).await?;
    let username = config.admin_username.clone();
    save_local_config(&config)?;
    Ok((username, password))
}

async fn config_with_reset_password(
    mut config: LocalConfig,
) -> anyhow::Result<(LocalConfig, String)> {
    let password = generate_admin_password();
    config.admin_password_hash = hash_password(password.clone()).await?;
    Ok((config, password))
}

fn generate_admin_password() -> String {
    uuid::Uuid::new_v4().to_string().replace('-', "")
}

async fn load_or_create_local_config(
    settings: &Settings,
    allow_bootstrap: bool,
) -> anyhow::Result<LocalConfig> {
    match load_local_config() {
        Ok(config) => Ok(config),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            create_local_config(settings, allow_bootstrap).await
        }
        Err(error) => Err(error),
    }
}

/// 首次启动时创建管理员配置。
///
/// # 生产环境为什么要显式开关
///
/// "配置文件不存在"有两种成因：真的是首次部署，或者**数据目录指错了**。二者在代码里
/// 无法区分，而后者的后果是悄悄新建一个管理员账号，让运维以为数据丢了。因此生产环境
/// 要求显式设置 `UNIONC_ALLOW_BOOTSTRAP=1` 才允许创建——正常重启永远走不到这里，
/// 一旦走到就说明数据目录需要人工确认。
async fn create_local_config(
    settings: &Settings,
    allow_bootstrap: bool,
) -> anyhow::Result<LocalConfig> {
    let config_path = crate::infra::paths::local_config_path();
    if !allow_bootstrap {
        anyhow::bail!(
            "生产环境下未找到管理员配置 {}。若这是首次部署，请设置 UNIONC_ALLOW_BOOTSTRAP=1 \
             与 UNIONC_BOOTSTRAP_PASSWORD 后重启；否则请检查 UNIONC_DATA_DIR 是否指向了正确的\
             数据目录——直接新建账号会掩盖数据目录配置错误",
            config_path.display()
        );
    }
    let configured_password = std::env::var("UNIONC_BOOTSTRAP_PASSWORD").ok();
    let password = match configured_password.as_deref() {
        // 与改密接口共用同一套规则：下限 12 字符，上限 72 **字节**。
        // 上限不是形式主义——bcrypt 会静默截断到 72 字节，一个更长的 bootstrap 密码
        // 会让运维以为自己设了强密码，实际生效的只有前 72 字节。
        Some(password) => {
            crate::auth::http::validate_new_password(password)
                .map_err(|error| anyhow::anyhow!("UNIONC_BOOTSTRAP_PASSWORD 不合法：{error}"))?;
            password.to_string()
        }
        None if settings.production => {
            anyhow::bail!("UNIONC_BOOTSTRAP_PASSWORD is required in production")
        }
        None => uuid::Uuid::new_v4().to_string().replace('-', ""),
    };
    let config = LocalConfig {
        application_version: env!("CARGO_PKG_VERSION").to_string(),
        admin_username: "admin".to_string(),
        admin_password_hash: hash_password(password.clone()).await?,
    };
    save_local_config(&config)?;
    if configured_password.is_some() {
        tracing::warn!("首次启动：管理员账号已由部署提供的初始密码创建，请立即修改密码。");
    } else {
        eprintln!("首次启动管理员：admin / {password}");
    }
    Ok(config)
}

fn bootstrap_allowed(production: bool, environment_value: Option<&str>) -> bool {
    !production || environment_value.is_some_and(|value| value.trim() == "1")
}

async fn hash_password(password: String) -> anyhow::Result<String> {
    crate::auth::http::validate_bcrypt_input(&password, "密码")
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    tokio::task::spawn_blocking(move || bcrypt::hash(password, bcrypt::DEFAULT_COST))
        .await
        .map_err(|error| anyhow::anyhow!("bcrypt task error: {error}"))?
        .map_err(|error| anyhow::anyhow!("bcrypt hash error: {error}"))
}

async fn prepare_database(
    bootstrap: Settings,
    allow_bootstrap: bool,
) -> anyhow::Result<(Settings, database::DbPool)> {
    let db = if allow_bootstrap {
        // Creation is a one-shot bootstrap privilege, not a lifetime pool
        // option. Close the create-enabled pool after initialization and use
        // an existing-only pool for every runtime acquisition so unlinking the
        // canonical path can never manufacture a plausible empty database.
        let bootstrap_db = database::connect(&bootstrap).await?;
        database::initialize_schema(&bootstrap_db).await?;
        bootstrap_db.close().await;
        let runtime_db = database::connect_existing(&bootstrap).await?;
        database::verify_schema(&runtime_db).await?;
        runtime_db
    } else {
        let db = database::connect_existing(&bootstrap).await?;
        database::verify_schema(&db).await?;
        db
    };
    let settings = database::load_app_settings(&db, &bootstrap).await?;
    Ok((settings, db))
}

fn listen_address(settings: &Settings) -> anyhow::Result<SocketAddr> {
    let bind_ip: IpAddr = settings
        .server
        .bind
        .trim()
        .trim_matches(['[', ']'])
        .parse()?;
    Ok(SocketAddr::new(bind_ip, settings.server.port))
}

/// 启动 Sunshine 的快、慢两级探测。
///
/// 快 worker 每 5 秒并发跑 TCP，立即发布 `/api/services`/SSE 快照；慢 worker
/// 每 30 秒在最新已发布 TCP 批次上追加 TLS/认证/API 检查。慢 worker 始终只有
/// 一个 await 在飞，但不会阻塞快 worker。配置变更通过 generation 记账：若变更发生在
/// 慢轮次期间，完成后会立即对最新批次补跑，不会并发重入。
fn start_service_status_probe(state: AppState) {
    const STATUS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
    const HEALTH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

    let (batch_sender, batch_receiver) = tokio::sync::watch::channel::<
        Option<(u64, crate::sunshine::status::ServiceProbeBatch)>,
    >(None);
    let status_state = state.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(STATUS_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut configuration_generation = 0_u64;
        loop {
            let configuration_changed = tokio::select! {
                biased;
                () = status_state.hosts.sunshine_health_refresh.notified() => true,
                _ = ticker.tick() => false,
            };
            if configuration_changed {
                configuration_generation = configuration_generation.wrapping_add(1);
            }
            // A configuration change can happen while network I/O is in
            // flight. The probe helper publishes only if the complete input
            // host list is still current; a discarded batch is followed by the
            // Notify permit left by the CRUD operation.
            if let Some(batch) =
                crate::sunshine::status::probe_and_publish_services(&status_state).await
            {
                batch_sender.send_replace(Some((configuration_generation, batch)));
            }
        }
    });

    tokio::spawn(run_service_health_probe(
        state,
        batch_receiver,
        HEALTH_INTERVAL,
    ));
}

struct HealthProbeCadence {
    completed_generation: Option<u64>,
    next_periodic: tokio::time::Instant,
}

impl HealthProbeCadence {
    fn starting_at(now: tokio::time::Instant) -> Self {
        Self {
            completed_generation: None,
            next_periodic: now,
        }
    }

    fn is_due(&self, generation: u64, now: tokio::time::Instant) -> bool {
        self.completed_generation != Some(generation) || now >= self.next_periodic
    }

    fn completed(
        &mut self,
        generation: u64,
        now: tokio::time::Instant,
        interval: std::time::Duration,
    ) {
        self.completed_generation = Some(generation);
        // Completion-relative cadence prevents a slow round from causing an
        // immediate catch-up burst. There is exactly one slow round in flight.
        self.next_periodic = now + interval;
    }
}

async fn run_service_health_probe(
    state: AppState,
    mut batches: tokio::sync::watch::Receiver<
        Option<(u64, crate::sunshine::status::ServiceProbeBatch)>,
    >,
    interval: std::time::Duration,
) {
    let mut latest: Option<(u64, crate::sunshine::status::ServiceProbeBatch)> = None;
    let mut cadence = HealthProbeCadence::starting_at(tokio::time::Instant::now());
    loop {
        if let Some((generation, batch)) = latest.as_ref()
            && cadence.is_due(*generation, tokio::time::Instant::now())
        {
            let generation = *generation;
            crate::sunshine::status::probe_and_publish_health(&state, batch.clone()).await;
            cadence.completed(generation, tokio::time::Instant::now(), interval);
            continue;
        }

        let changed = if latest.is_some() {
            tokio::select! {
                biased;
                changed = batches.changed() => changed,
                () = tokio::time::sleep_until(cadence.next_periodic) => continue,
            }
        } else {
            batches.changed().await
        };
        if changed.is_err() {
            return;
        }
        latest = batches.borrow_and_update().clone();
    }
}

/// 内存态回收周期。
///
/// 会话与限流桶的回收都很廉价，但把它们塞进 24 小时的数据库清理循环里，就意味着
/// 一台注销的主机要留着桶到明天。拆成独立的短周期任务，两者各自按自己的时间尺度运行。
const MEMORY_GC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// 进程内状态的周期性回收。
///
/// 这两项都不挂在请求热路径上顺带完成：鉴权取写锁清过期会话会让所有已认证请求串行化，
/// 每次上报 `retain()` 一遍全部限流桶会给全系统最高频的写路径挂上一段 O(主机数) 的
/// 临界区。集中到这里之后，两条热路径都只剩一次哈希查找。
fn start_memory_gc(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(MEMORY_GC_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await; // 首个 tick 立即返回
        loop {
            ticker.tick().await;
            let expired = crate::auth::http::prune_expired_sessions(&state).await;
            if expired > 0 {
                tracing::info!("maintenance: removed {expired} expired sessions");
            }
            let buckets = state
                .agents
                .prune_report_buckets(std::time::Instant::now())
                .await;
            if buckets > 0 {
                tracing::debug!("maintenance: reclaimed {buckets} idle agent rate-limit buckets");
            }
            // 登录桶与上报桶同理：热路径只清理自己查阅的那几个 Vec，
            // 遍历整张 map 丢弃空桶放在这里。
            let login_buckets = crate::auth::http::prune_login_buckets(&state).await;
            if login_buckets > 0 {
                tracing::debug!("maintenance: reclaimed {login_buckets} idle login buckets");
            }
        }
    });
}

fn start_database_retention(state: AppState, retention: RetentionSettings) {
    tokio::spawn(async move {
        loop {
            match database::prune_audit_history(state.db().as_ref(), retention.audit_days).await {
                Ok(removed) if removed > 0 => {
                    tracing::info!("maintenance: removed {removed} old audit rows")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!("maintenance: failed to prune audit history: {error}"),
            }
            match crate::monitoring::store::prune_monitoring_history(
                state.db().as_ref(),
                retention.telemetry_days,
            )
            .await
            {
                Ok(removed) if removed > 0 => {
                    tracing::info!("maintenance: removed {removed} old monitoring reports")
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!("maintenance: failed to prune monitoring history: {error}")
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
        }
    });
}

/// Create an online, self-contained SQLite snapshot and integrity manifest.
pub async fn backup_database(output: &std::path::Path) -> anyhow::Result<()> {
    crate::infra::paths::init()?;
    let runtime = RuntimeEnvironment::from_environment()?;
    let settings = Settings::load(&runtime)?;
    ensure_layout()?;
    secrets::init(runtime.mode)?;
    let manifest = database::backup_database(&settings, output).await?;
    println!(
        "SQLite 备份已创建：{} (sha256={}, schema={}, key_id={})",
        output.display(),
        manifest.database_sha256,
        manifest.schema_version,
        manifest.encryption_key_id
    );
    Ok(())
}

/// Restore a validated backup. A running Server holds the database lock, so
/// this operation fails before replacing any file unless the service stopped.
pub async fn restore_database(input: &std::path::Path, force: bool) -> anyhow::Result<()> {
    crate::infra::paths::init()?;
    let runtime = RuntimeEnvironment::from_environment()?;
    let settings = Settings::load(&runtime)?;
    ensure_layout()?;
    secrets::init(runtime.mode)?;
    let previous = database::restore_database(&settings, input, force).await?;
    match previous {
        Some(database::RecoveryPoint::Validated { database }) => println!(
            "SQLite 备份已恢复；替换前数据库已保留为可再次恢复的验证恢复点：{}",
            database.display()
        ),
        Some(database::RecoveryPoint::UnverifiedForensicCopy {
            database,
            validation_error,
        }) => println!(
            "SQLite 备份已恢复；警告：替换前数据库校验失败，原始文件仅保留为未验证取证副本：{}。该文件没有 manifest，不能直接用于 restore。校验错误：{}",
            database.display(),
            validation_error
        ),
        None => println!("SQLite 备份已恢复到全新数据目录"),
    }
    Ok(())
}

/// Run SQLite integrity, foreign-key, schema and encrypted-value checks.
pub async fn integrity_check() -> anyhow::Result<()> {
    crate::infra::paths::init()?;
    let runtime = RuntimeEnvironment::from_environment()?;
    let settings = Settings::load(&runtime)?;
    ensure_layout()?;
    secrets::init(runtime.mode)?;
    let version = database::integrity_check(&settings).await?;
    println!("SQLite 完整性检查通过；schema={version}");
    Ok(())
}

#[cfg(test)]
mod password_reset_tests {
    use super::{
        HealthProbeCadence, LocalConfig, bootstrap_allowed, config_with_reset_password,
        generate_admin_password, prepare_database,
    };
    use crate::{config::Settings, infra::database};

    #[test]
    fn production_bootstrap_requires_the_exact_explicit_switch() {
        assert!(bootstrap_allowed(false, None));
        assert!(bootstrap_allowed(false, Some("0")));
        assert!(!bootstrap_allowed(true, None));
        assert!(!bootstrap_allowed(true, Some("true")));
        assert!(!bootstrap_allowed(true, Some("01")));
        assert!(bootstrap_allowed(true, Some(" 1 ")));
    }

    #[tokio::test]
    async fn startup_database_creation_follows_the_bootstrap_policy() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing/unionc.db");
        let mut settings = Settings::default();
        settings.database.url = format!("sqlite://{}?mode=rwc", path.display());

        let error = prepare_database(settings.clone(), false)
            .await
            .err()
            .expect("normal production startup must reject a missing database");
        assert!(
            error.to_string().contains("automatic creation is disabled"),
            "{error:#}"
        );
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());

        let (_, bootstrap_pool) = prepare_database(settings.clone(), true)
            .await
            .expect("explicit bootstrap creates the current database");
        database::verify_schema(&bootstrap_pool).await.unwrap();
        bootstrap_pool.close().await;
        assert!(path.is_file());

        let (_, normal_pool) = prepare_database(settings, false)
            .await
            .expect("normal production startup reopens the current database");
        database::verify_schema(&normal_pool).await.unwrap();

        let displaced = path.with_extension("displaced");
        std::fs::rename(&path, &displaced).expect("displace the runtime database");
        let acquisition =
            tokio::time::timeout(std::time::Duration::from_millis(250), normal_pool.acquire())
                .await;
        assert!(
            !matches!(acquisition, Ok(Ok(_))),
            "the runtime pool must reject the displaced database identity"
        );
        assert!(
            !path.exists(),
            "a runtime pool must not retain bootstrap creation privileges"
        );
        normal_pool.close().await;
    }

    #[test]
    fn generated_admin_password_is_strong_and_shell_friendly() {
        let first = generate_admin_password();
        let second = generate_admin_password();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|value| value.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn reset_replaces_only_the_password_hash() {
        let original = LocalConfig {
            application_version: env!("CARGO_PKG_VERSION").to_string(),
            admin_username: "operator".to_string(),
            admin_password_hash: bcrypt::hash("old-password-value", 4).unwrap(),
        };
        let (updated, password) = config_with_reset_password(original.clone()).await.unwrap();
        assert_eq!(updated.admin_username, original.admin_username);
        assert_ne!(updated.admin_password_hash, original.admin_password_hash);
        assert!(bcrypt::verify(password, &updated.admin_password_hash).unwrap());
    }

    #[test]
    fn health_cadence_runs_initially_on_change_and_periodically_without_waiting() {
        let interval = std::time::Duration::from_secs(30);
        let started = tokio::time::Instant::now();
        let mut cadence = HealthProbeCadence::starting_at(started);

        assert!(
            cadence.is_due(0, started),
            "the first TCP batch must trigger health"
        );
        cadence.completed(0, started, interval);
        assert!(!cadence.is_due(0, started + interval / 2));
        assert!(
            cadence.is_due(1, started + interval / 2),
            "a configuration generation must trigger health immediately"
        );

        cadence.completed(1, started + interval / 2, interval);
        assert!(!cadence.is_due(1, started + interval));
        assert!(cadence.is_due(1, started + interval + interval / 2));
    }
}
