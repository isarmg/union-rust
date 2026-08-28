use std::{
    net::{IpAddr, SocketAddr},
    str::FromStr,
};

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "union-host-monitoring-worker", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Apply this module's PostgreSQL migrations, then serve its private HTTP API.
    Serve(Common),
    /// Apply only this module's PostgreSQL migrations.
    Migrate(Database),
    /// Transactionally import the legacy Union SQLite monitoring tables.
    ImportSqlite {
        #[command(flatten)]
        database: Database,
        #[arg(long)]
        sqlite: std::path::PathBuf,
        #[arg(long)]
        evidence: std::path::PathBuf,
    },
    /// Re-run target row-count and logical-digest validation for an import batch.
    VerifyImport {
        #[command(flatten)]
        database: Database,
        #[arg(long)]
        import_id: uuid::Uuid,
    },
    /// Delete only rows attributed to a previously completed import, preserving evidence.
    RollbackImport {
        #[command(flatten)]
        database: Database,
        #[arg(long)]
        import_id: uuid::Uuid,
        #[arg(long)]
        evidence: std::path::PathBuf,
    },
}

#[derive(Debug, Clone, clap::Args)]
pub struct Database {
    #[arg(long, env = "UNION_HOST_MONITORING_DATABASE_URL")]
    pub database_url: String,
}

#[derive(Debug, Clone, clap::Args)]
pub struct Common {
    #[command(flatten)]
    pub database: Database,
    #[arg(
        long,
        env = "UNION_HOST_MONITORING_BIND",
        default_value = "127.0.0.1:18105"
    )]
    pub bind: SocketAddr,
}

impl Common {
    pub fn validate(&self) -> anyhow::Result<ValidatedConfig> {
        let bind = match std::env::var("UNION_PLUGIN_BIND") {
            Ok(value) => value
                .parse::<SocketAddr>()
                .map_err(|_| anyhow::anyhow!("UNION_PLUGIN_BIND must be a socket address"))?,
            Err(std::env::VarError::NotPresent) => self.bind,
            Err(error) => return Err(error.into()),
        };
        if !bind.ip().is_loopback() {
            anyhow::bail!(
                "UNION host-monitoring worker must use a loopback bind; got {}",
                bind
            );
        }
        if !self.database.database_url.starts_with("postgresql://")
            && !self.database.database_url.starts_with("postgres://")
        {
            anyhow::bail!("host-monitoring requires a PostgreSQL database URL");
        }
        Ok(ValidatedConfig {
            bind,
            database_url: self.database.database_url.clone(),
            gateway: sarmg_platform_gateway::GatewayIdentity::from_env(
                crate::auth::MODULE_AUDIENCE,
                crate::auth::MODULE_PREFIX,
            )?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ValidatedConfig {
    pub bind: SocketAddr,
    pub database_url: String,
    pub gateway: sarmg_platform_gateway::GatewayIdentity,
}

pub fn forwarded_ip(value: &str) -> Option<IpAddr> {
    IpAddr::from_str(value.split(',').next()?.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_public_bind_and_short_secret() {
        let common = Common {
            database: Database {
                database_url: "postgresql://localhost/union".into(),
            },
            bind: "0.0.0.0:18105".parse().unwrap(),
        };
        assert!(
            common
                .validate()
                .unwrap_err()
                .to_string()
                .contains("loopback")
        );
    }

    #[test]
    fn shared_gateway_contract_accepts_only_host_monitoring_identity() {
        let token = "ab".repeat(32);
        let identity = sarmg_platform_gateway::GatewayIdentity::new(
            sarmg_platform_gateway::PROTOCOL,
            crate::auth::MODULE_AUDIENCE,
            token,
            crate::auth::MODULE_PREFIX,
            crate::auth::MODULE_AUDIENCE,
            crate::auth::MODULE_PREFIX,
        );
        assert!(identity.is_ok());
    }
}
