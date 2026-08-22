//! Sunshine host persistence.

use serde::{Deserialize, Serialize};
use sqlx_core::{query::query, row::Row};
use sqlx_sqlite::SqliteConnection;

use crate::config::{Settings, SunshineHostConfig};

use super::{DbPool, begin_write, now_epoch_micros};

/// Exact non-secret Sunshine configuration persisted by the current schema.
/// Identity and address have dedicated columns; the password has its own
/// encrypted column.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredSunshineHostConfig {
    name: String,
    web_port: u16,
    username: String,
    verify_tls: bool,
}

/// Load the environment-derived runtime configuration and merge the only
/// mutable application configuration that is persisted: Sunshine hosts.
///
/// Server binding and paths are deployment configuration. Persisting a second
/// encrypted copy of them in SQLite made
/// precedence ambiguous without providing a settings API, so SQLite is no
/// longer treated as a serialized `Settings` store.
pub async fn load_app_settings(pool: &DbPool, bootstrap: &Settings) -> anyhow::Result<Settings> {
    let mut settings = bootstrap.clone();
    settings.sunshine.hosts = load_sunshine_hosts(pool).await?;
    Ok(settings)
}

/// Re-encrypt every stored Sunshine password with the current key.
pub async fn rekey_secrets(pool: &DbPool) -> anyhow::Result<usize> {
    let mut tx = begin_write(pool).await?;
    let rows = query(
        "SELECT host_id,secret FROM external_hosts WHERE kind='sunshine' AND secret IS NOT NULL",
    )
    .fetch_all(tx.connection())
    .await?;

    for row in &rows {
        let host_id: String = row.try_get("host_id")?;
        let encrypted: String = row.try_get("secret")?;
        let plaintext = crate::infra::secrets::decrypt(&encrypted)?;
        let reencrypted = crate::infra::secrets::encrypt(&plaintext)?;
        query(
            "UPDATE external_hosts SET secret=?,updated_at=? \
             WHERE kind='sunshine' AND host_id=?",
        )
        .bind(reencrypted)
        .bind(now_epoch_micros())
        .bind(host_id)
        .execute(tx.connection())
        .await?;
    }

    tx.commit().await?;
    Ok(rows.len())
}

/// Insert one host and its audit event atomically.
pub async fn insert_sunshine_host(
    pool: &DbPool,
    host: &SunshineHostConfig,
    audit_detail: &str,
) -> anyhow::Result<()> {
    let mut tx = begin_write(pool).await?;
    let position: i64 =
        query("SELECT COALESCE(MAX(position), -1) + 1 FROM external_hosts WHERE kind='sunshine'")
            .fetch_one(tx.connection())
            .await?
            .try_get(0)?;
    insert_external_host(tx.connection(), host, position).await?;
    super::insert_audit_in_transaction(
        tx.connection(),
        "sunshine.host.create",
        &host.id,
        Some(audit_detail),
    )
    .await?;
    tx.commit().await
}

/// Update exactly one host and its audit event atomically.
///
/// `update_password=false` leaves the encrypted column byte-for-byte intact;
/// an omitted PATCH password therefore cannot accidentally rotate or clear it.
pub async fn update_sunshine_host(
    pool: &DbPool,
    host: &SunshineHostConfig,
    update_password: bool,
    audit_detail: &str,
) -> anyhow::Result<bool> {
    let mut tx = begin_write(pool).await?;
    let config = public_host_config(host)?;
    let updated_at = now_epoch_micros();
    let result = if update_password {
        let secret = encrypted_secret(&host.password)?;
        query(
            r#"
            UPDATE external_hosts
            SET address=?,config=?,secret=?,updated_at=?
            WHERE kind='sunshine' AND host_id=?
            "#,
        )
        .bind(&host.host)
        .bind(config)
        .bind(secret)
        .bind(updated_at)
        .bind(&host.id)
        .execute(tx.connection())
        .await?
    } else {
        query(
            r#"
            UPDATE external_hosts
            SET address=?,config=?,updated_at=?
            WHERE kind='sunshine' AND host_id=?
            "#,
        )
        .bind(&host.host)
        .bind(config)
        .bind(updated_at)
        .bind(&host.id)
        .execute(tx.connection())
        .await?
    };
    let found = result.rows_affected() > 0;
    if found {
        super::insert_audit_in_transaction(
            tx.connection(),
            "sunshine.host.update",
            &host.id,
            Some(audit_detail),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(found)
}

/// Delete exactly one host and its audit event atomically.
pub async fn delete_sunshine_host(pool: &DbPool, id: &str) -> anyhow::Result<bool> {
    let mut tx = begin_write(pool).await?;
    let result = query("DELETE FROM external_hosts WHERE kind='sunshine' AND host_id=?")
        .bind(id)
        .execute(tx.connection())
        .await?;
    let found = result.rows_affected() > 0;
    if found {
        super::insert_audit_in_transaction(
            tx.connection(),
            "sunshine.host.delete",
            id,
            Some("host removed"),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(found)
}

async fn insert_external_host(
    connection: &mut SqliteConnection,
    host: &SunshineHostConfig,
    position: i64,
) -> anyhow::Result<()> {
    query(
        r#"
        INSERT INTO external_hosts(kind,host_id,address,config,secret,position)
        VALUES('sunshine',?,?,?,?,?)
        "#,
    )
    .bind(&host.id)
    .bind(&host.host)
    .bind(public_host_config(host)?)
    .bind(encrypted_secret(&host.password)?)
    .bind(position)
    .execute(connection)
    .await?;
    Ok(())
}

fn public_host_config(host: &SunshineHostConfig) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&StoredSunshineHostConfig {
        name: host.name.clone(),
        web_port: host.web_port,
        username: host.username.clone(),
        verify_tls: host.verify_tls,
    })?)
}

pub(super) fn validate_stored_sunshine_host_config(raw: &str) -> anyhow::Result<()> {
    serde_json::from_str::<StoredSunshineHostConfig>(raw)?;
    Ok(())
}

fn encrypted_secret(secret: &str) -> anyhow::Result<Option<String>> {
    (!secret.is_empty())
        .then(|| crate::infra::secrets::encrypt(secret))
        .transpose()
}

pub async fn load_sunshine_hosts(pool: &DbPool) -> anyhow::Result<Vec<SunshineHostConfig>> {
    let rows = query(
        r#"
        SELECT host_id,address,config,secret
        FROM external_hosts
        WHERE kind='sunshine'
        ORDER BY position,created_at,host_id
        "#,
    )
    .fetch_all(pool)
    .await?;
    let mut hosts = Vec::with_capacity(rows.len());
    for row in rows {
        let config: StoredSunshineHostConfig =
            serde_json::from_str(&row.try_get::<String, _>("config")?)?;
        let password = row
            .try_get::<Option<String>, _>("secret")?
            .map(|value| crate::infra::secrets::decrypt(&value))
            .transpose()?
            .unwrap_or_default();
        hosts.push(SunshineHostConfig {
            id: row.try_get("host_id")?,
            name: config.name,
            host: row.try_get("address")?,
            web_port: config.web_port,
            username: config.username,
            password,
            verify_tls: config.verify_tls,
        });
    }
    Ok(hosts)
}
