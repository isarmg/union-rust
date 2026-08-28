//! UnionC 应用启动编排。

use std::net::{IpAddr, SocketAddr};

use crate::{
    config::{
        LayoutIntent, LocalConfig, RetentionSettings, RuntimeEnvironment, Settings, ensure_layout,
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
    ensure_layout(if allow_bootstrap {
        LayoutIntent::Bootstrap
    } else {
        LayoutIntent::ExistingOnly
    })?;
    let database_path = database::database_path(&bootstrap_settings)?;
    database::hold_server_database_lock(&database_path)?;
    secrets::init(runtime.mode)?;
    let local_config = load_or_create_local_config(&bootstrap_settings, allow_bootstrap).await?;
    // SQLite 是 Server 数据目录内的固定持久层。只有开发环境或显式 bootstrap 才能创建；
    // 正常生产启动只打开并精确校验既有数据库，不能用空库掩盖数据文件丢失。
    let (settings, db) = prepare_database(bootstrap_settings, allow_bootstrap).await?;
    let addr = listen_address(&settings)?;
    let dummy_password_hash = hash_password(uuid::Uuid::new_v4().to_string()).await?;
    let state = AppState::new(settings, db, dummy_password_hash, local_config)?;
    // 内存态和持久历史分别回收；SQLite 在 HTTP 服务启动前已经完成打开与校验，因此
    // 保留期任务在每次正常启动中都存在，不会出现“配置数据库后忘记重启而不清理”的窗口。
    start_memory_gc(state.clone());
    start_database_retention(state.clone(), runtime.retention);
    Ok(InitializedApp { addr, state })
}

/// `unionc reset-admin-password`：离线生成并写入新的管理员密码。
///
/// 只读取/替换本地私有配置，不连接数据库，也不改动平台审计、模块状态或主密钥。
/// 命令会把随机密码输出一次；调用方随后必须重启 Server，使旧内存会话全部失效。
pub async fn reset_admin_password() -> anyhow::Result<(String, String)> {
    crate::infra::paths::init()?;
    let runtime = RuntimeEnvironment::from_environment()?;
    let settings = Settings::load(&runtime)?;
    ensure_layout(LayoutIntent::ExistingOnly)?;
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
    Ok((bootstrap, db))
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

/// 内存态回收周期。
///
/// 会话与登录限流桶的回收都很廉价，但把它们塞进 24 小时的数据库清理循环会让
/// 无效内存状态滞留过久。拆成独立的短周期任务，各自按合适的时间尺度运行。
const MEMORY_GC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// 进程内状态的周期性回收。
///
/// 这两项都不挂在请求热路径上顺带完成：鉴权取写锁清过期会话会让所有已认证请求
/// 串行化，登录时扫描全部限流桶也会延长认证临界区。集中回收后，热路径只需查阅
/// 当前请求对应的状态。
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
            // 登录限流热路径只清理自己查阅的 Vec；全表回收放在这里。
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
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
        }
    });
}

/// Create an online, self-contained SQLite snapshot and integrity manifest.
pub async fn backup_database(output: &std::path::Path) -> anyhow::Result<()> {
    crate::infra::paths::init()?;
    let runtime = RuntimeEnvironment::from_environment()?;
    let settings = Settings::load(&runtime)?;
    ensure_layout(LayoutIntent::ExistingOnly)?;
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
    ensure_layout(LayoutIntent::Restore)?;
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

/// Run SQLite integrity, foreign-key and exact platform-schema checks.
pub async fn integrity_check() -> anyhow::Result<()> {
    crate::infra::paths::init()?;
    let runtime = RuntimeEnvironment::from_environment()?;
    let settings = Settings::load(&runtime)?;
    ensure_layout(LayoutIntent::ExistingOnly)?;
    secrets::init(runtime.mode)?;
    let version = database::integrity_check(&settings).await?;
    println!("SQLite 完整性检查通过；schema={version}");
    Ok(())
}

#[cfg(test)]
mod password_reset_tests {
    use super::{
        LocalConfig, bootstrap_allowed, config_with_reset_password, generate_admin_password,
        prepare_database,
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

        std::fs::create_dir(path.parent().unwrap()).unwrap();
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
}
