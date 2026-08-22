//! 启动环境覆盖、本地私有配置和目录初始化。

use std::{
    env::VarError,
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, chown},
    path::Path,
    str::FromStr,
};

use anyhow::{Context, bail};

use super::*;

const LOCAL_CONFIG_DIR_MODE: u32 = 0o700;
const LOCAL_CONFIG_FILE_MODE: u32 = 0o600;
type LocalConfigResult<T> = Result<T, LocalConfigError>;

const DEFAULT_AUDIT_RETENTION_DAYS: i64 = 90;
const MIN_AUDIT_RETENTION_DAYS: i64 = 7;
const DEFAULT_TELEMETRY_RETENTION_DAYS: i64 = 30;
const MIN_TELEMETRY_RETENTION_DAYS: i64 = 1;
const MAX_RETENTION_DAYS: i64 = 3650;

/// Process security mode, parsed exactly once before any mode-sensitive work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    Development,
    Production,
}

impl RuntimeMode {
    pub fn from_environment() -> anyhow::Result<Self> {
        let value = unicode_environment_variable("UNIONC_ENV")?;
        parse_runtime_mode(value.as_deref())
    }

    pub const fn is_production(self) -> bool {
        matches!(self, Self::Production)
    }
}

fn parse_runtime_mode(value: Option<&str>) -> anyhow::Result<RuntimeMode> {
    value
        .map(str::parse)
        .unwrap_or(Ok(RuntimeMode::Development))
}

impl FromStr for RuntimeMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "development" => Ok(Self::Development),
            "production" => Ok(Self::Production),
            _ => bail!(
                "invalid UNIONC_ENV value {value:?}; expected exactly 'development' or 'production'"
            ),
        }
    }
}

/// Validated retention policy used by the two database maintenance jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionSettings {
    pub audit_days: i64,
    pub telemetry_days: i64,
}

/// Snapshot of every environment override used by `Settings` and startup.
///
/// Keeping this immutable snapshot prevents a second `UNIONC_ENV` read from
/// selecting different security behavior in the secret store and HTTP stack.
#[derive(Clone)]
pub struct RuntimeEnvironment {
    pub mode: RuntimeMode,
    pub retention: RetentionSettings,
    server_bind: Option<String>,
    server_port: Option<u16>,
    proxy_secret: Option<String>,
}

impl RuntimeEnvironment {
    pub fn from_environment() -> anyhow::Result<Self> {
        let mode = RuntimeMode::from_environment()?;
        let server_bind = unicode_environment_variable("UNIONC_SERVER_BIND")?
            .map(|value| parse_bind_address(&value))
            .transpose()?;
        let server_port = unicode_environment_variable("UNIONC_SERVER_PORT")?
            .map(|value| parse_server_port(&value))
            .transpose()?;
        let proxy_secret = unicode_environment_variable("UNIONC_PROXY_SECRET")?
            .map(|value| parse_proxy_secret(&value))
            .transpose()?;
        let retention = RetentionSettings {
            audit_days: parse_retention_days(
                "UNIONC_RETENTION_DAYS",
                unicode_environment_variable("UNIONC_RETENTION_DAYS")?.as_deref(),
                DEFAULT_AUDIT_RETENTION_DAYS,
                MIN_AUDIT_RETENTION_DAYS,
            )?,
            telemetry_days: parse_retention_days(
                "UNIONC_TELEMETRY_RETENTION_DAYS",
                unicode_environment_variable("UNIONC_TELEMETRY_RETENTION_DAYS")?.as_deref(),
                DEFAULT_TELEMETRY_RETENTION_DAYS,
                MIN_TELEMETRY_RETENTION_DAYS,
            )?,
        };
        Ok(Self {
            mode,
            retention,
            server_bind,
            server_port,
            proxy_secret,
        })
    }
}

impl Settings {
    pub fn load(runtime: &RuntimeEnvironment) -> anyhow::Result<Self> {
        let mut settings = Settings::default();
        settings.apply_runtime_environment(runtime)?;
        Ok(settings)
    }

    fn apply_runtime_environment(&mut self, runtime: &RuntimeEnvironment) -> anyhow::Result<()> {
        self.production = runtime.mode.is_production();
        if let Some(bind) = runtime.server_bind.as_ref() {
            self.server.bind.clone_from(bind);
        }
        if let Some(port) = runtime.server_port {
            self.server.port = port;
        }
        if let Some(secret) = runtime.proxy_secret.as_ref() {
            self.server.proxy_secret.clone_from(secret);
        }
        if self.production {
            let bind = self
                .server
                .bind
                .trim()
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .map_err(|_| anyhow::anyhow!("invalid server bind address"))?;
            if !bind.is_loopback() {
                anyhow::bail!(
                    "production unionc must bind to a loopback address behind the reverse proxy"
                );
            }
        }
        Ok(())
    }
}

fn unicode_environment_variable(name: &str) -> anyhow::Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => bail!("{name} must contain valid UTF-8"),
    }
}

fn parse_bind_address(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    let address = value
        .trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>()
        .map_err(|_| anyhow::anyhow!("invalid UNIONC_SERVER_BIND"))?;
    Ok(address.to_string())
}

fn parse_server_port(value: &str) -> anyhow::Result<u16> {
    let port = value
        .trim()
        .parse::<u16>()
        .map_err(|_| anyhow::anyhow!("invalid UNIONC_SERVER_PORT"))?;
    if port == 0 {
        bail!("UNIONC_SERVER_PORT must be greater than zero");
    }
    Ok(port)
}

fn parse_proxy_secret(value: &str) -> anyhow::Result<String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("UNIONC_PROXY_SECRET must be exactly 64 lowercase hexadecimal characters");
    }
    Ok(value.to_string())
}

fn parse_retention_days(
    name: &str,
    value: Option<&str>,
    default: i64,
    minimum: i64,
) -> anyhow::Result<i64> {
    let Some(value) = value else {
        return Ok(default);
    };
    let days = value
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("{name} must be an integer number of days"))?;
    if !(minimum..=MAX_RETENTION_DAYS).contains(&days) {
        bail!("{name} must be between {minimum} and {MAX_RETENTION_DAYS} days (got {days})");
    }
    Ok(days)
}

pub fn load_local_config() -> anyhow::Result<LocalConfig> {
    let path = crate::infra::paths::local_config_path();
    let path = path.as_path();
    ensure_private_config_file(path)?;
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read local config {}", path.display()))?;
    let config = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse local config {}", path.display()))?;
    Ok(normalize_local_config(&config)?)
}

pub fn save_local_config(config: &LocalConfig) -> anyhow::Result<()> {
    let config = normalize_local_config(config)?;
    let path = crate::infra::paths::local_config_path();
    replace_local_config_file(&path, &config)
}

fn replace_local_config_file(path: &Path, config: &LocalConfig) -> anyhow::Result<()> {
    let directory = path.parent().context("local config path has no parent")?;
    ensure_private_config_directory(directory)?;
    let existing_owner = match fs::symlink_metadata(path) {
        Ok(_) => {
            ensure_private_config_file(path)?;
            let metadata = fs::metadata(path)?;
            Some((metadata.uid(), metadata.gid()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("failed to inspect existing local config"),
    };
    let temporary = directory.join(format!(".unionc-config.{}.tmp", uuid::Uuid::new_v4()));
    let result = write_local_config_file(&temporary, config)
        .and_then(|()| {
            if let Some((uid, gid)) = existing_owner {
                chown(&temporary, Some(uid), Some(gid))
                    .context("failed to preserve local config ownership")?;
                fs::File::open(&temporary)?.sync_all()?;
            }
            Ok(())
        })
        .and_then(|()| {
            fs::rename(&temporary, path).with_context(|| "failed to replace local config")
        })
        .and_then(|()| fs::File::open(directory)?.sync_all().map_err(Into::into));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    ensure_private_config_file(path)
}

/// 建立运行时目录布局。
///
/// 全部路径都来自 `paths` 模块派生的绝对路径。这里不创建任何**相对**目录——
/// 那样换个工作目录启动就会在别处留下空壳目录。
pub fn ensure_layout() -> std::io::Result<()> {
    let data_dir = crate::infra::paths::data_dir();
    fs::create_dir_all(data_dir)?;
    // 数据目录含主密钥与管理员哈希，必须是 0700。
    fs::set_permissions(data_dir, fs::Permissions::from_mode(LOCAL_CONFIG_DIR_MODE))?;
    Ok(())
}

fn normalize_local_config(config: &LocalConfig) -> LocalConfigResult<LocalConfig> {
    if config.application_version != env!("CARGO_PKG_VERSION") {
        return Err(LocalConfigError::UnsupportedApplicationVersion);
    }
    let username = config.admin_username.trim();
    if username.is_empty() {
        return Err(LocalConfigError::EmptyAdminUsername);
    }
    if username.len() > crate::auth::MAX_USERNAME_BYTES || username.chars().any(char::is_control) {
        return Err(LocalConfigError::InvalidAdminUsername);
    }
    let hash = config.admin_password_hash.trim();
    if hash.is_empty() {
        return Err(LocalConfigError::EmptyAdminPasswordHash);
    }
    bcrypt::HashParts::from_str(hash).map_err(|_| LocalConfigError::InvalidAdminPasswordHash)?;
    Ok(LocalConfig {
        application_version: env!("CARGO_PKG_VERSION").to_string(),
        admin_username: username.to_string(),
        admin_password_hash: hash.to_string(),
    })
}

fn ensure_private_config_directory(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("local config directory is not a regular directory");
    }
    fs::set_permissions(path, fs::Permissions::from_mode(LOCAL_CONFIG_DIR_MODE))?;
    Ok(())
}

fn ensure_private_config_file(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("local config path is not a regular file");
    }
    fs::set_permissions(path, fs::Permissions::from_mode(LOCAL_CONFIG_FILE_MODE))?;
    Ok(())
}

fn write_local_config_file(path: &Path, config: &LocalConfig) -> anyhow::Result<()> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(LOCAL_CONFIG_FILE_MODE)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, config)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_mode_accepts_only_the_two_exact_documented_values() {
        assert_eq!(parse_runtime_mode(None).unwrap(), RuntimeMode::Development);
        assert_eq!(
            "development".parse::<RuntimeMode>().unwrap(),
            RuntimeMode::Development
        );
        assert_eq!(
            "production".parse::<RuntimeMode>().unwrap(),
            RuntimeMode::Production
        );
        for invalid in ["", " ", "Production", "prod", "production "] {
            assert!(
                invalid.parse::<RuntimeMode>().is_err(),
                "ambiguous runtime mode {invalid:?} must fail closed"
            );
        }
    }

    #[test]
    fn retention_values_reject_invalid_and_out_of_range_input() {
        assert_eq!(
            parse_retention_days("TEST_RETENTION", None, 90, 7).unwrap(),
            90
        );
        assert_eq!(
            parse_retention_days("TEST_RETENTION", Some("7"), 90, 7).unwrap(),
            7
        );
        assert_eq!(
            parse_retention_days("TEST_RETENTION", Some("3650"), 90, 7).unwrap(),
            3650
        );
        for invalid in ["", " 30", "thirty", "6", "3651", "-1"] {
            assert!(
                parse_retention_days("TEST_RETENTION", Some(invalid), 90, 7).is_err(),
                "invalid retention {invalid:?} must stop startup"
            );
        }
    }

    #[test]
    fn proxy_secret_is_bounded_and_header_safe() {
        assert!(parse_proxy_secret(&"a".repeat(64)).is_ok());
        for invalid in [
            "short".to_string(),
            "a".repeat(65),
            format!("{} ", "a".repeat(63)),
            format!("{}\n", "a".repeat(63)),
            "密".repeat(32),
            "CHANGE_ME_TO_AN_INDEPENDENT_RANDOM_VALUE".to_string(),
            "A".repeat(64),
            format!("{}#", "a".repeat(63)),
        ] {
            assert!(parse_proxy_secret(&invalid).is_err());
        }
    }

    #[test]
    fn atomic_config_replace_preserves_owner_and_private_mode() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unionc-config.json");
        let original = LocalConfig {
            application_version: env!("CARGO_PKG_VERSION").to_string(),
            admin_username: "admin".to_string(),
            admin_password_hash: bcrypt::hash("original-password", 4).unwrap(),
        };
        write_local_config_file(&path, &original).unwrap();
        let before = fs::metadata(&path).unwrap();

        let replacement = LocalConfig {
            application_version: env!("CARGO_PKG_VERSION").to_string(),
            admin_username: original.admin_username.clone(),
            admin_password_hash: bcrypt::hash("replacement-password", 4).unwrap(),
        };
        replace_local_config_file(&path, &replacement).unwrap();

        let after = fs::metadata(&path).unwrap();
        assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
        assert_eq!(after.mode() & 0o777, LOCAL_CONFIG_FILE_MODE);
        let persisted: LocalConfig = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted.application_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(persisted.admin_username, original.admin_username);
        assert!(bcrypt::verify("replacement-password", &persisted.admin_password_hash).unwrap());
    }

    #[test]
    fn local_config_requires_the_exact_application_version() {
        let hash = bcrypt::hash("current-password", 4).unwrap();
        let missing = serde_json::json!({
            "admin_username": "admin",
            "admin_password_hash": hash
        });
        assert!(serde_json::from_value::<LocalConfig>(missing).is_err());

        let other = LocalConfig {
            application_version: "0.3.1".to_string(),
            admin_username: "admin".to_string(),
            admin_password_hash: bcrypt::hash("current-password", 4).unwrap(),
        };
        assert!(matches!(
            normalize_local_config(&other),
            Err(LocalConfigError::UnsupportedApplicationVersion)
        ));
    }
}
