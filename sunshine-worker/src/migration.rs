//! Offline, auditable migration from UnionC's legacy SQLite `external_hosts`
//! rows to the worker-owned PostgreSQL schema.
//!
//! The importer must run while the legacy Union process is stopped. Every
//! batch stores exact destination ciphertext before and after the import. A
//! rollback refuses to overwrite any row changed after import.

use std::{collections::BTreeMap, path::Path};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::{Connection, PgPool, Row, SqliteConnection, sqlite::SqliteConnectOptions};
use uuid::Uuid;

use crate::{
    crypto::{SecretBox, decrypt_legacy_union},
    db::{self, StoredHost},
    error::{AppError, AppResult},
    model::{Host, HostSaveRequest, validate_host_request},
};

#[derive(Clone)]
pub struct LegacyKeys {
    pub current_id: String,
    pub current: [u8; 32],
    pub previous: BTreeMap<String, [u8; 32]>,
}

impl LegacyKeys {
    pub fn from_env() -> anyhow::Result<Self> {
        let current_id =
            std::env::var("UNIONC_SECRET_KEY_ID").unwrap_or_else(|_| "primary".to_string());
        validate_key_id(&current_id)?;
        let current = decode_key("UNIONC_SECRET_KEY", &std::env::var("UNIONC_SECRET_KEY")?)?;
        let mut previous = BTreeMap::new();
        if let Ok(value) = std::env::var("UNIONC_SECRET_KEY_PREVIOUS") {
            for entry in value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
            {
                let (id, encoded) = entry
                    .split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("invalid UNIONC_SECRET_KEY_PREVIOUS entry"))?;
                validate_key_id(id)?;
                anyhow::ensure!(
                    id != current_id,
                    "legacy previous key duplicates current id"
                );
                anyhow::ensure!(
                    previous
                        .insert(id.to_string(), decode_key("legacy previous key", encoded)?)
                        .is_none(),
                    "duplicate legacy previous key id {id}"
                );
            }
        }
        Ok(Self {
            current_id,
            current,
            previous,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogicalHost {
    pub id: String,
    pub name: String,
    pub host: String,
    pub web_port: u16,
    pub username: String,
    pub password: String,
    pub verify_tls: bool,
    pub position: i64,
    pub created_at_micros: i64,
    pub updated_at_micros: i64,
}

impl From<LogicalHost> for Host {
    fn from(value: LogicalHost) -> Self {
        Self {
            id: value.id,
            name: value.name,
            host: value.host,
            web_port: value.web_port,
            username: value.username,
            password: value.password,
            verify_tls: value.verify_tls,
            position: value.position,
            created_at_micros: value.created_at_micros,
            updated_at_micros: value.updated_at_micros,
        }
    }
}

#[derive(sqlx::FromRow)]
struct LegacyRow {
    host_id: String,
    address: String,
    config: String,
    secret: Option<String>,
    position: i64,
    created_at: i64,
    updated_at: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyConfig {
    name: String,
    web_port: u16,
    username: String,
    verify_tls: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BeforeEntry {
    host_id: String,
    previous: Option<StoredHost>,
}

#[derive(Debug, Serialize)]
pub struct ImportReport {
    pub batch_id: Uuid,
    pub source_fingerprint: String,
    pub rows: usize,
    pub verified: bool,
}

#[derive(Debug, Serialize)]
pub struct VerificationReport {
    pub batch_id: Uuid,
    pub status: String,
    pub rows: usize,
    pub exact_match: bool,
}

pub async fn read_legacy_sqlite(
    path: &Path,
    keys: &LegacyKeys,
) -> anyhow::Result<Vec<LogicalHost>> {
    anyhow::ensure!(path.is_file(), "legacy SQLite path is not a regular file");
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false);
    let mut connection = SqliteConnection::connect_with(&options).await?;
    let quick_check: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&mut connection)
        .await?;
    anyhow::ensure!(
        quick_check == "ok",
        "legacy SQLite quick_check failed: {quick_check}"
    );
    let mut transaction = connection.begin().await?;
    let rows = sqlx::query_as::<_, LegacyRow>(
        r#"SELECT host_id,address,config,secret,position,created_at,updated_at
           FROM external_hosts WHERE kind='sunshine'
           ORDER BY position,created_at,host_id"#,
    )
    .fetch_all(&mut *transaction)
    .await?;
    transaction.commit().await?;
    map_legacy_rows(rows, keys)
}

fn map_legacy_rows(rows: Vec<LegacyRow>, keys: &LegacyKeys) -> anyhow::Result<Vec<LogicalHost>> {
    let mut result = Vec::with_capacity(rows.len());
    let mut ids = BTreeMap::new();
    for row in rows {
        anyhow::ensure!(
            ids.insert(row.host_id.clone(), ()).is_none(),
            "duplicate legacy host id"
        );
        let config: LegacyConfig = serde_json::from_str(&row.config)?;
        let password = row
            .secret
            .as_deref()
            .map(|secret| {
                decrypt_legacy_union(secret, &keys.current_id, &keys.current, &keys.previous)
            })
            .transpose()
            .map_err(|_| anyhow::anyhow!("cannot decrypt legacy host {}", row.host_id))?
            .unwrap_or_default();
        validate_host_request(
            &HostSaveRequest {
                name: config.name.clone(),
                host: row.address.clone(),
                web_port: config.web_port,
                username: config.username.clone(),
                password: Some(password.clone()),
                verify_tls: config.verify_tls,
            },
            false,
        )
        .map_err(|error| anyhow::anyhow!("legacy host {} is invalid: {error}", row.host_id))?;
        anyhow::ensure!(row.position >= 0, "legacy host position is negative");
        result.push(LogicalHost {
            id: row.host_id,
            name: config.name,
            host: row.address,
            web_port: config.web_port,
            username: config.username,
            password,
            verify_tls: config.verify_tls,
            position: row.position,
            created_at_micros: row.created_at,
            updated_at_micros: row.updated_at,
        });
    }
    Ok(result)
}

pub async fn import_hosts(
    pool: &PgPool,
    secrets: &SecretBox,
    hosts: Vec<LogicalHost>,
) -> AppResult<ImportReport> {
    let fingerprint = fingerprint(&hosts, secrets.hmac_key());
    let imported = hosts
        .into_iter()
        .map(Host::from)
        .map(|host| db::encode_host(&host, secrets))
        .collect::<AppResult<Vec<_>>>()?;
    let batch_id = Uuid::new_v4();
    let now = db::now_micros().map_err(AppError::Internal)?;
    let mut transaction = pool.begin().await?;
    db::lock_writes(&mut transaction).await?;
    let mut before = Vec::with_capacity(imported.len());
    for row in &imported {
        let previous = sqlx::query_as::<_, StoredHost>(
            r#"SELECT host_id,name,address,web_port,username,secret,verify_tls,position,
                      created_at_micros,updated_at_micros
               FROM sunshine.hosts WHERE host_id=$1 FOR UPDATE"#,
        )
        .bind(&row.host_id)
        .fetch_optional(&mut *transaction)
        .await?;
        before.push(BeforeEntry {
            host_id: row.host_id.clone(),
            previous,
        });
    }
    for row in &imported {
        db::upsert_stored(&mut transaction, row).await?;
    }
    sqlx::query(
        r#"INSERT INTO sunshine.import_batches(
             batch_id,source_fingerprint,source_row_count,before_state,imported_state,status,imported_at_micros)
           VALUES($1,$2,$3,$4,$5,'applied',$6)"#,
    )
    .bind(batch_id)
    .bind(&fingerprint)
    .bind(i32::try_from(imported.len()).map_err(|_| AppError::BadRequest("too many hosts".into()))?)
    .bind(serde_json::to_value(&before).map_err(|error| AppError::Internal(error.into()))?)
    .bind(serde_json::to_value(&imported).map_err(|error| AppError::Internal(error.into()))?)
    .bind(now)
    .execute(&mut *transaction)
    .await?;
    db::insert_audit(
        &mut transaction,
        "sunshine.migration.import",
        &batch_id.to_string(),
        "offline-migration",
        Some(&format!(
            "rows={} source_hmac_sha256={fingerprint}",
            imported.len()
        )),
    )
    .await?;
    transaction.commit().await?;
    let verification = verify_batch(pool, batch_id).await?;
    Ok(ImportReport {
        batch_id,
        source_fingerprint: fingerprint,
        rows: imported.len(),
        verified: verification.exact_match,
    })
}

pub async fn verify_batch(pool: &PgPool, batch_id: Uuid) -> AppResult<VerificationReport> {
    let row =
        sqlx::query("SELECT status,imported_state FROM sunshine.import_batches WHERE batch_id=$1")
            .bind(batch_id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("import batch {batch_id} not found")))?;
    let status: String = row.try_get("status")?;
    let value: serde_json::Value = row.try_get("imported_state")?;
    let expected: Vec<StoredHost> =
        serde_json::from_value(value).map_err(|error| AppError::Internal(error.into()))?;
    let exact_match = if status == "applied" {
        rows_match(pool, &expected).await?
    } else {
        false
    };
    if exact_match {
        sqlx::query("UPDATE sunshine.import_batches SET verified_at_micros=$2 WHERE batch_id=$1")
            .bind(batch_id)
            .bind(db::now_micros().map_err(AppError::Internal)?)
            .execute(pool)
            .await?;
    }
    Ok(VerificationReport {
        batch_id,
        status,
        rows: expected.len(),
        exact_match,
    })
}

pub async fn rollback_batch(pool: &PgPool, batch_id: Uuid) -> AppResult<VerificationReport> {
    let mut transaction = pool.begin().await?;
    db::lock_writes(&mut transaction).await?;
    let row = sqlx::query(
        "SELECT status,before_state,imported_state FROM sunshine.import_batches WHERE batch_id=$1 FOR UPDATE",
    )
    .bind(batch_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("import batch {batch_id} not found")))?;
    let status: String = row.try_get("status")?;
    if status != "applied" {
        return Err(AppError::Conflict(format!(
            "import batch {batch_id} is already {status}"
        )));
    }
    let before: Vec<BeforeEntry> = serde_json::from_value(row.try_get("before_state")?)
        .map_err(|error| AppError::Internal(error.into()))?;
    let imported: Vec<StoredHost> = serde_json::from_value(row.try_get("imported_state")?)
        .map_err(|error| AppError::Internal(error.into()))?;

    // Fail closed if any operator/API write happened after import.
    for expected in &imported {
        let current = sqlx::query_as::<_, StoredHost>(
            r#"SELECT host_id,name,address,web_port,username,secret,verify_tls,position,
                      created_at_micros,updated_at_micros
               FROM sunshine.hosts WHERE host_id=$1 FOR UPDATE"#,
        )
        .bind(&expected.host_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if current.as_ref() != Some(expected) {
            return Err(AppError::Conflict(format!(
                "host {} changed after import; rollback refused",
                expected.host_id
            )));
        }
    }
    for entry in &before {
        if let Some(previous) = &entry.previous {
            db::upsert_stored(&mut transaction, previous).await?;
        } else {
            sqlx::query("DELETE FROM sunshine.hosts WHERE host_id=$1")
                .bind(&entry.host_id)
                .execute(&mut *transaction)
                .await?;
        }
    }
    sqlx::query(
        "UPDATE sunshine.import_batches SET status='rolled_back',rolled_back_at_micros=$2 WHERE batch_id=$1",
    )
    .bind(batch_id)
    .bind(db::now_micros().map_err(AppError::Internal)?)
    .execute(&mut *transaction)
    .await?;
    db::insert_audit(
        &mut transaction,
        "sunshine.migration.rollback",
        &batch_id.to_string(),
        "offline-migration",
        Some(&format!("rows={}", before.len())),
    )
    .await?;
    transaction.commit().await?;

    let exact_match = before_rows_match(pool, &before).await?;
    Ok(VerificationReport {
        batch_id,
        status: "rolled_back".into(),
        rows: before.len(),
        exact_match,
    })
}

async fn rows_match(pool: &PgPool, expected: &[StoredHost]) -> Result<bool, sqlx::Error> {
    for row in expected {
        if db::get_stored_host(pool, &row.host_id).await?.as_ref() != Some(row) {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn before_rows_match(pool: &PgPool, expected: &[BeforeEntry]) -> Result<bool, sqlx::Error> {
    for entry in expected {
        if db::get_stored_host(pool, &entry.host_id).await? != entry.previous {
            return Ok(false);
        }
    }
    Ok(true)
}

fn fingerprint(hosts: &[LogicalHost], key: &[u8; 32]) -> String {
    let canonical = serde_json::to_vec(hosts).expect("logical host serialization cannot fail");
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a 32-byte key");
    mac.update(&canonical);
    hex(mac.finalize().into_bytes().as_slice())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    value
}

fn decode_key(label: &str, value: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = STANDARD
        .decode(value.trim())
        .map_err(|_| anyhow::anyhow!("{label} must be base64"))?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("{label} must decode to 32 bytes"))
}

fn validate_key_id(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "invalid legacy key id"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::encrypt_legacy_for_test;

    #[tokio::test]
    async fn sqlite_mapping_preserves_every_legacy_field_and_password() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("union.db");
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::query(
            r#"CREATE TABLE external_hosts(
                 kind TEXT, host_id TEXT, address TEXT, config TEXT, secret TEXT,
                 position INTEGER, created_at INTEGER, updated_at INTEGER)"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let key = [8; 32];
        let encrypted = encrypt_legacy_for_test("legacy", &key, "p@ssword");
        sqlx::query(
            "INSERT INTO external_hosts VALUES('sunshine','host-a','2001:db8::5',?1,?2,7,123,456)",
        )
        .bind(r#"{"name":"Gaming PC","web_port":47990,"username":"admin","verify_tls":true}"#)
        .bind(encrypted)
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
        let hosts = read_legacy_sqlite(
            &path,
            &LegacyKeys {
                current_id: "legacy".into(),
                current: key,
                previous: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            hosts,
            vec![LogicalHost {
                id: "host-a".into(),
                name: "Gaming PC".into(),
                host: "2001:db8::5".into(),
                web_port: 47990,
                username: "admin".into(),
                password: "p@ssword".into(),
                verify_tls: true,
                position: 7,
                created_at_micros: 123,
                updated_at_micros: 456,
            }]
        );
    }

    #[test]
    fn fingerprint_is_stable_but_does_not_expose_plaintext() {
        let hosts = vec![LogicalHost {
            id: "id".into(),
            name: "name".into(),
            host: "localhost".into(),
            web_port: 47990,
            username: "admin".into(),
            password: "do-not-store-this".into(),
            verify_tls: true,
            position: 0,
            created_at_micros: 1,
            updated_at_micros: 2,
        }];
        let first = fingerprint(&hosts, &[5; 32]);
        assert_eq!(first, fingerprint(&hosts, &[5; 32]));
        assert_eq!(first.len(), 64);
        assert!(!first.contains("do-not-store-this"));
    }
}
