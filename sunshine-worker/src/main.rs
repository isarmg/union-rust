use std::path::PathBuf;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::{Parser, Subcommand};
use unionc_sunshine_worker::{
    ServeConfig,
    crypto::SecretBox,
    db,
    http::{WorkerState, probe_loop, router},
    migration::{LegacyKeys, import_hosts, read_legacy_sqlite, rollback_batch, verify_batch},
};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "sunshine-worker",
    version,
    about = "UnionC private Sunshine module worker"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the private loopback API (the default command).
    Serve,
    /// Apply the module-owned PostgreSQL migrations.
    Migrate(DatabaseArgs),
    /// Import legacy Union SQLite Sunshine hosts as a reversible batch.
    ImportSqlite {
        #[command(flatten)]
        database: DatabaseArgs,
        #[arg(long)]
        sqlite: PathBuf,
    },
    /// Verify an applied import still matches every destination field exactly.
    VerifyImport {
        #[command(flatten)]
        database: DatabaseArgs,
        #[arg(long)]
        batch: Uuid,
    },
    /// Restore the exact pre-import rows; refuses if an imported row changed.
    RollbackImport {
        #[command(flatten)]
        database: DatabaseArgs,
        #[arg(long)]
        batch: Uuid,
    },
}

#[derive(clap::Args)]
struct DatabaseArgs {
    #[arg(long, env = "SUNSHINE_DATABASE_URL", hide_env_values = true)]
    database_url: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "unionc_sunshine_worker=info,tower_http=info".into()),
        )
        .init();
    match Cli::parse().command.unwrap_or(Command::Serve) {
        Command::Serve => serve().await,
        Command::Migrate(args) => {
            let pool = db::connect(&args.database_url).await?;
            db::migrate(&pool).await?;
            println!("{{\"status\":\"migrated\",\"schema\":\"sunshine\"}}");
            Ok(())
        }
        Command::ImportSqlite { database, sqlite } => {
            let pool = db::connect(&database.database_url).await?;
            db::migrate(&pool).await?;
            let legacy = read_legacy_sqlite(&sqlite, &LegacyKeys::from_env()?).await?;
            let report = import_hosts(&pool, &credential_box()?, legacy).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::VerifyImport { database, batch } => {
            let pool = db::connect(&database.database_url).await?;
            db::migrate(&pool).await?;
            let report = verify_batch(&pool, batch).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            anyhow::ensure!(report.exact_match, "import verification failed");
            Ok(())
        }
        Command::RollbackImport { database, batch } => {
            let pool = db::connect(&database.database_url).await?;
            db::migrate(&pool).await?;
            let report = rollback_batch(&pool, batch).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            anyhow::ensure!(report.exact_match, "rollback verification failed");
            Ok(())
        }
    }
}

async fn serve() -> anyhow::Result<()> {
    let config = ServeConfig::from_env()?;
    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;
    let state = WorkerState::new(
        pool,
        config.secrets,
        config.internal_auth,
        config.production,
    )?;
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(bind = %config.bind, schema = db::SCHEMA, "Sunshine private worker ready");
    let probe = tokio::spawn(probe_loop(state.clone()));
    let result = axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown())
        .await;
    probe.abort();
    result?;
    Ok(())
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::error!(%error, "failed to install SIGTERM handler"),
        }
    };
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

fn credential_box() -> anyhow::Result<SecretBox> {
    let encoded = std::env::var("SUNSHINE_CREDENTIAL_KEY")?;
    let bytes = STANDARD.decode(encoded.trim())?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("SUNSHINE_CREDENTIAL_KEY must decode to 32 bytes"))?;
    let id = std::env::var("SUNSHINE_CREDENTIAL_KEY_ID").unwrap_or_else(|_| "primary".to_string());
    SecretBox::new(id, key)
}
