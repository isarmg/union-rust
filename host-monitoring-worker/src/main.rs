use clap::Parser;
use union_host_monitoring_worker::{
    config::{Cli, Command},
    http::{AppState, router},
    import, store,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .init();
    match Cli::parse().command {
        Command::Serve(common) => {
            let config = common.validate()?;
            let pool = store::connect(&config.database_url).await?;
            store::migrate(&pool).await?;
            let listener = tokio::net::TcpListener::bind(config.bind).await?;
            tracing::info!(bind=%config.bind, "Union private host-monitoring worker ready");
            axum::serve(listener, router(AppState::new(pool, config.gateway)))
                .with_graceful_shutdown(shutdown())
                .await?;
        }
        Command::Migrate(database) => {
            let pool = store::connect(&database.database_url).await?;
            store::migrate(&pool).await?;
        }
        Command::ImportSqlite {
            database,
            sqlite,
            evidence,
        } => {
            let pool = store::connect(&database.database_url).await?;
            let result = import::import_sqlite(&pool, &sqlite, &evidence).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::VerifyImport {
            database,
            import_id,
        } => {
            let pool = store::connect(&database.database_url).await?;
            let result = import::verify(&pool, import_id).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            if !result.valid {
                anyhow::bail!("import verification failed");
            }
        }
        Command::RollbackImport {
            database,
            import_id,
            evidence,
        } => {
            let pool = store::connect(&database.database_url).await?;
            let result = import::rollback(&pool, import_id, &evidence).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }
    Ok(())
}

async fn shutdown() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl-C handler")
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}
