//! 运行时数据目录解析。
//!
//! # 为什么需要这个模块
//!
//! 三个关键文件落在数据目录里：
//!
//! ```text
//! <数据目录>/unionc.secret        AES-256 主密钥
//! <数据目录>/unionc-config.json   管理员 bcrypt 哈希
//! <数据目录>/unionc.db            SQLite 业务数据、凭据哈希与加密配置
//! ```
//!
//! 若用**相对路径**表达这个目录，"文件在哪"就取决于**进程的工作目录**。从别的目录
//! 启动服务时，`load_local_config()` 得到 `NotFound`，而"配置不存在"与"首次启动"
//! 无法区分，于是会**静默新建一个管理员账号**，原来的账号和数据看上去凭空消失。
//! 密钥同理——开发环境会直接生成一把新密钥，导致数据库里所有既有密文全部解不开。
//!
//! 因此这里把数据目录**一次性解析成绝对路径**并在进程内固定下来，同时把解析结果打进
//! 启动日志，使"我的数据到底在哪"成为一个可观测的事实，而不是依赖 CWD 的隐含约定。
//!
//! # 解析顺序
//!
//! 1. `UNIONC_DATA_DIR` —— 部署应显式设置（systemd unit 已默认设置为 `/var/lib/unionc`）；
//! 2. 回退 `<当前工作目录>/unionc/data` —— 保持仓库内 `cargo run` 的开发体验不变。
//!
//! 两种情况都会被规范化成绝对路径。

use std::{
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

const DATA_DIR_ENV: &str = "UNIONC_DATA_DIR";
const DEFAULT_RELATIVE_DATA_DIR: &str = "unionc/data";

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 解析并固定数据目录，返回绝对路径。
///
/// 由 `startup` 在**任何配置读写之前**调用。重复调用返回首次解析的结果，
/// 使库使用方（集成测试）不必关心调用顺序。
pub fn init() -> anyhow::Result<&'static Path> {
    if let Some(existing) = DATA_DIR.get() {
        return Ok(existing.as_path());
    }
    let resolved = resolve()?;
    tracing::info!("unionc 数据目录: {}", resolved.display());
    Ok(DATA_DIR.get_or_init(|| resolved).as_path())
}

/// 数据目录的绝对路径。
///
/// 未显式 `init()` 时按同样规则惰性解析，解析失败则退回相对路径——这条兜底只为
/// 让单元测试不至于 panic，正常启动路径一定走过 `init()`。
pub fn data_dir() -> &'static Path {
    DATA_DIR
        .get_or_init(|| resolve().unwrap_or_else(|_| PathBuf::from(DEFAULT_RELATIVE_DATA_DIR)))
        .as_path()
}

/// 管理员本地配置（含 bcrypt 哈希）。
pub fn local_config_path() -> PathBuf {
    data_dir().join("unionc-config.json")
}

/// AES-256 主密钥。仅开发环境使用；生产强制走 `UNIONC_SECRET_KEY`。
pub fn secret_key_path() -> PathBuf {
    data_dir().join("unionc.secret")
}

fn resolve() -> anyhow::Result<PathBuf> {
    let candidate = match std::env::var_os(DATA_DIR_ENV) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => std::env::current_dir()?.join(DEFAULT_RELATIVE_DATA_DIR),
    };
    Ok(absolutize(candidate)?)
}

/// 转成词法规范化的绝对路径。目录此时可能尚未创建，因此不能用会跟随
/// 符号链接的 `canonicalize`。
pub(crate) fn normalize_absolute(path: PathBuf) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "unsupported data-directory path prefix",
                ));
            }
        }
    }
    Ok(normalized)
}

fn absolutize(path: PathBuf) -> std::io::Result<PathBuf> {
    normalize_absolute(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_input_is_anchored_to_an_absolute_path() {
        let resolved = absolutize(PathBuf::from("unionc/data")).expect("absolutize");
        assert!(
            resolved.is_absolute(),
            "数据目录必须是绝对路径，否则启动目录一变，账号与密钥就会'消失'"
        );
        assert!(resolved.ends_with("unionc/data"));
    }

    #[test]
    fn absolute_input_is_preserved() {
        let resolved = absolutize(PathBuf::from("/var/lib/unionc")).expect("absolutize");
        assert_eq!(resolved, PathBuf::from("/var/lib/unionc"));
    }

    #[test]
    fn dot_and_parent_components_are_normalized_without_following_links() {
        assert_eq!(
            absolutize(PathBuf::from("/tmp/example/../unionc/./data")).unwrap(),
            PathBuf::from("/tmp/unionc/data")
        );
        assert_eq!(
            absolutize(PathBuf::from("/tmp/..")).unwrap(),
            PathBuf::from("/")
        );
    }

    #[test]
    fn derived_paths_all_live_under_the_data_directory() {
        for path in [local_config_path(), secret_key_path()] {
            assert!(
                path.starts_with(data_dir()),
                "{} 必须位于数据目录之内",
                path.display()
            );
        }
    }
}
