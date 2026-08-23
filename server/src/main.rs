//! unionc 后端入口。
//!
//! 程序启动顺序：
//! 1. 初始化日志；
//! 2. 解析数据目录并打开内嵌 SQLite；
//! 3. 初始化或精确校验当前 schema，并读取数据库中的运行配置；
//! 4. 构造共享状态和路由；
//! 5. 绑定端口并启动 Axum HTTP 服务。

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use unionc::{http, startup};

mod systemd;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Serve,
    Version,
    Rekey,
    ResetAdminPassword,
    Backup {
        output: std::path::PathBuf,
    },
    Restore {
        input: std::path::PathBuf,
        force: bool,
    },
    IntegrityCheck,
}

fn parse_command(arguments: impl IntoIterator<Item = String>) -> anyhow::Result<Command> {
    let arguments: Vec<String> = arguments.into_iter().collect();
    let non_empty = |value: &str| !value.trim().is_empty();
    match arguments.as_slice() {
        [] => Ok(Command::Serve),
        [flag] if flag == "--version" => Ok(Command::Version),
        [command] if command == "rekey" => Ok(Command::Rekey),
        [command] if command == "reset-admin-password" => Ok(Command::ResetAdminPassword),
        [command] if command == "integrity-check" => Ok(Command::IntegrityCheck),
        [command, flag, output]
            if command == "backup" && flag == "--output" && non_empty(output) =>
        {
            Ok(Command::Backup {
                output: output.into(),
            })
        }
        [command, flag, input] if command == "restore" && flag == "--input" && non_empty(input) => {
            Ok(Command::Restore {
                input: input.into(),
                force: false,
            })
        }
        [command, flag, input, force]
            if command == "restore"
                && flag == "--input"
                && non_empty(input)
                && force == "--force" =>
        {
            Ok(Command::Restore {
                input: input.into(),
                force: true,
            })
        }
        _ => anyhow::bail!(
            "invalid arguments; expected one of: --version; rekey; reset-admin-password; \
             backup --output PATH; restore --input PATH [--force]; integrity-check"
        ),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化统一日志。EnvFilter 允许通过 RUST_LOG 覆盖日志级别。
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "unionc=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // 少量离线维护命令保持零额外 CLI 依赖；未知参数必须拒绝，避免拼错后意外启动服务。
    match parse_command(std::env::args().skip(1))? {
        Command::Version => {
            println!("{}", version_line());
            return Ok(());
        }
        Command::Rekey => return startup::rekey().await,
        Command::ResetAdminPassword => {
            let (username, password) = startup::reset_admin_password().await?;
            println!("管理员密码已重置：{username} / {password}");
            return Ok(());
        }
        Command::Backup { output } => return startup::backup_database(&output).await,
        Command::Restore { input, force } => {
            return startup::restore_database(&input, force).await;
        }
        Command::IntegrityCheck => return startup::integrity_check().await,
        Command::Serve => {}
    }

    let initialized = startup::initialize().await?;
    let state = initialized.state;
    let app = http::router(state.clone());

    let listener = tokio::net::TcpListener::bind(initialized.addr).await?;
    tracing::info!("unionc listening on http://{}", initialized.addr);
    systemd::report_ready()?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(state))
        .await?;

    Ok(())
}

fn version_line() -> String {
    format!("unionc {}", env!("CARGO_PKG_VERSION"))
}

async fn shutdown_signal(state: unionc::state::AppState) {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!("failed to install Ctrl+C handler: {err}");
        }
    };

    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => tracing::error!("failed to install SIGTERM handler: {err}"),
        }
    };
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    // Axum waits for every active response after this future returns. Notify
    // long-lived SSE streams first so they can end instead of blocking that
    // drain until their seven-day login session expires.
    state.request_shutdown();
    tracing::info!("shutdown signal received; draining HTTP connections");
}

#[cfg(test)]
mod tests {
    use super::{Command, parse_command, version_line};

    #[test]
    fn version_flag_is_exact_and_side_effect_free() {
        assert_eq!(
            parse_command(["--version".to_string()]).unwrap(),
            Command::Version
        );
        assert_eq!(version_line(), "unionc 0.3.4");
        assert!(parse_command(["-V".to_string()]).is_err());
        assert!(parse_command(["--version".to_string(), "extra".to_string()]).is_err());
    }

    #[test]
    fn maintenance_commands_reject_trailing_arguments() {
        assert_eq!(parse_command(Vec::<String>::new()).unwrap(), Command::Serve);
        assert_eq!(
            parse_command(["reset-admin-password".to_string()]).unwrap(),
            Command::ResetAdminPassword
        );
        assert!(
            parse_command(["reset-admin-password".to_string(), "--dry-run".to_string()]).is_err()
        );
        assert!(parse_command(["reset-admin-passwrod".to_string()]).is_err());
    }

    #[test]
    fn database_maintenance_commands_require_exact_flags_and_values() {
        assert_eq!(
            parse_command(["backup", "--output", "/tmp/unionc.db"].map(str::to_string)).unwrap(),
            Command::Backup {
                output: "/tmp/unionc.db".into()
            }
        );
        assert_eq!(
            parse_command(["restore", "--input", "/tmp/unionc.db", "--force"].map(str::to_string))
                .unwrap(),
            Command::Restore {
                input: "/tmp/unionc.db".into(),
                force: true
            }
        );
        assert!(parse_command(["backup", "/tmp/unionc.db"].map(str::to_string)).is_err());
        assert!(
            parse_command(["restore", "--force", "--input", "/tmp/unionc.db"].map(str::to_string))
                .is_err()
        );
        assert!(parse_command(["integrity-check", "--force"].map(str::to_string)).is_err());
    }
}
