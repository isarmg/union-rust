//! SQLite backup, restore and integrity maintenance support.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx_core::{connection::Connection, query::query, row::Row};
use sqlx_sqlite::{SqliteConnectOptions, SqliteConnection};

use crate::config::Settings;

use super::database_path;

const BACKUP_FORMAT_VERSION: u32 = 2;
const SERVER_LOCK_FILE_NAME: &str = ".unionc-server.lock";
const MAINTENANCE_LOCK_FILE_NAME: &str = ".unionc-maintenance.lock";
static SERVER_DATABASE_LOCK: OnceLock<DatabaseFileLock> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    pub format_version: u32,
    pub application_version: String,
    pub created_at: String,
    pub database_file: String,
    pub database_sha256: String,
    pub schema_version: i64,
    pub encryption_key_id: String,
}

/// The copy retained before a restore replaces an existing database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryPoint {
    /// A complete database/manifest pair accepted by `restore_database`.
    Validated { database: PathBuf },
    /// An exact, durable database-family copy kept for forensic recovery.
    /// It deliberately has no manifest and cannot be passed back to restore.
    UnverifiedForensicCopy {
        database: PathBuf,
        validation_error: String,
    },
}

/// Advisory process lock guarding operations that replace the database file.
///
/// SQLite's own locks protect individual transactions. This separate fixed
/// lock prevents two UnionC Server processes from running with independent
/// in-memory sessions and prevents restore from renaming a live file.
pub struct DatabaseFileLock {
    _file: File,
}

pub fn acquire_database_lock(database: &Path) -> anyhow::Result<DatabaseFileLock> {
    let file = open_fixed_lock_file(database, SERVER_LOCK_FILE_NAME)?;
    file.try_lock().map_err(|error| {
        anyhow::anyhow!(
            "UnionC Server is already using {}; stop the service before replacing the database: {error}",
            database.display()
        )
    })?;
    Ok(DatabaseFileLock { _file: file })
}

/// Serialize explicit database maintenance commands without excluding the
/// running Server. This is deliberately a different inode from the singleton
/// Server lock: online backup remains available, while backup, integrity,
/// restore and other maintenance operations cannot race around WAL sidecars or
/// file publication.
pub fn acquire_maintenance_lock(database: &Path) -> anyhow::Result<DatabaseFileLock> {
    let file = open_fixed_lock_file(database, MAINTENANCE_LOCK_FILE_NAME)?;
    file.try_lock().map_err(|error| {
        anyhow::anyhow!(
            "another UnionC database maintenance command is already using {}: {error}",
            database.display()
        )
    })?;
    Ok(DatabaseFileLock { _file: file })
}

fn open_fixed_lock_file(database: &Path, file_name: &str) -> anyhow::Result<File> {
    let parent = database
        .parent()
        .context("database path has no parent directory")?;
    fs::create_dir_all(parent)?;
    let lock_path = parent.join(file_name);

    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&lock_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let before = fs::symlink_metadata(&lock_path).with_context(|| {
                format!("failed to inspect database lock {}", lock_path.display())
            })?;
            validate_lock_metadata(&lock_path, &before)?;

            // The pre-open check gives a useful error for a stable symlink. The
            // post-open inode comparison closes the check/open race without
            // ever writing through a substituted path.
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)
                .with_context(|| format!("failed to open database lock {}", lock_path.display()))?;
            let opened = file.metadata()?;
            let after = fs::symlink_metadata(&lock_path)?;
            validate_lock_metadata(&lock_path, &after)?;
            if !opened.is_file()
                || opened.nlink() != 1
                || opened.dev() != after.dev()
                || opened.ino() != after.ino()
            {
                bail!(
                    "database lock path changed while opening it: {}",
                    lock_path.display()
                );
            }
            file
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to create database lock {}", lock_path.display())
            });
        }
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        bail!(
            "opened database lock is not a private regular file: {}",
            lock_path.display()
        );
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn validate_lock_metadata(path: &Path, metadata: &fs::Metadata) -> anyhow::Result<()> {
    if !metadata.file_type().is_file() {
        bail!("database lock is not a regular file: {}", path.display());
    }
    if metadata.nlink() != 1 {
        bail!("database lock must not be hard-linked: {}", path.display());
    }
    Ok(())
}

/// Hold the single-server lock until process exit.
pub fn hold_server_database_lock(database: &Path) -> anyhow::Result<()> {
    if SERVER_DATABASE_LOCK.get().is_some() {
        return Ok(());
    }
    let lock = acquire_database_lock(database)?;
    SERVER_DATABASE_LOCK
        .set(lock)
        .map_err(|_| anyhow::anyhow!("database server lock was initialized concurrently"))?;
    Ok(())
}

/// Create a transactionally consistent online snapshot using SQLite itself.
pub async fn backup_database(settings: &Settings, output: &Path) -> anyhow::Result<BackupManifest> {
    let output = absolute_path(output)?;
    let final_manifest = manifest_path(&output);
    ensure_new_output(&output)?;
    ensure_new_output(&final_manifest)?;

    let parent = output.parent().context("backup output has no parent")?;
    let nonce = uuid::Uuid::new_v4();
    let staging = parent.join(format!(".unionc-backup-{nonce}.db"));
    let staging_manifest = parent.join(format!(".unionc-backup-{nonce}.manifest.json"));
    let _staging_guard = DatabaseFamilyGuard::new(staging.clone());
    let _manifest_guard = FileGuard::new(staging_manifest.clone());

    let source = database_path(settings)?;
    ensure_regular_file(&source, "live SQLite database")?;
    let _maintenance_lock = acquire_maintenance_lock(&source)?;
    // Backup never initializes schema: a wrong data directory must fail
    // instead of silently creating and backing up an empty database, and an
    // operator-requested snapshot must not mutate schema as a side effect.
    validate_database_file(&source).await?;
    let pool = super::connect(settings).await?;

    let staging_text = staging
        .to_str()
        .context("backup output path is not valid UTF-8")?;
    // VACUUM INTO reads a consistent source snapshot and emits one
    // self-contained database, independent of the live WAL/SHM files.
    let vacuum_result = query("VACUUM main INTO ?")
        .bind(staging_text)
        .execute(&pool)
        .await;
    pool.close().await;
    vacuum_result?;

    fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
    File::open(&staging)?.sync_all()?;
    let schema_version = validate_database_file(&staging).await?;
    validate_encrypted_values(&staging).await?;
    let database_sha256 = sha256_file(&staging)?;
    let manifest = build_manifest(&output, database_sha256, schema_version)?;
    write_manifest(&staging_manifest, &manifest)?;

    // Hard links provide an atomic, no-replace publication primitive on the
    // same filesystem. A racing caller's path is never overwritten or removed.
    fs::hard_link(&staging_manifest, &final_manifest)
        .context("failed to publish backup manifest without overwriting an existing file")?;
    if let Err(error) = fs::hard_link(&staging, &output) {
        remove_if_same_file(&final_manifest, &staging_manifest);
        return Err(error)
            .context("failed to publish backup database without overwriting an existing file");
    }
    sync_parent(&output).with_context(|| {
        format!(
            "backup was published at {}, but directory durability could not be confirmed",
            output.display()
        )
    })?;
    Ok(manifest)
}

/// Validate the live database without mutating it.
pub async fn integrity_check(settings: &Settings) -> anyhow::Result<i64> {
    let path = database_path(settings)?;
    let _maintenance_lock = acquire_maintenance_lock(&path)?;
    let version = validate_database_file(&path).await?;
    validate_encrypted_values(&path).await?;
    Ok(version)
}

/// Replace the stopped Server's database with a validated backup.
pub async fn restore_database(
    settings: &Settings,
    input: &Path,
    force: bool,
) -> anyhow::Result<Option<RecoveryPoint>> {
    let input = absolute_path(input)?;
    ensure_regular_file(&input, "backup input")?;
    // Read the manifest once. The copied staging database, rather than the
    // mutable input path, is subsequently hashed and validated against it.
    let manifest = read_manifest(&input)?;

    let target = database_path(settings)?;
    if target == Path::new(":memory:") {
        bail!("cannot restore an in-memory database");
    }
    let _lock = acquire_database_lock(&target)?;
    let _maintenance_lock = acquire_maintenance_lock(&target)?;
    let target_exists = path_exists(&target)?;
    if target_exists && !force {
        bail!(
            "database {} already exists; rerun with --force after stopping the service",
            target.display()
        );
    }
    let parent = target.parent().context("database path has no parent")?;
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(".unionc-restore-{}.db", uuid::Uuid::new_v4()));
    let _staging_guard = DatabaseFamilyGuard::new(staging.clone());
    copy_private_file(&input, &staging)?;
    // Authenticate the exact copied bytes before opening SQLite. The input is
    // never opened: all subsequent work targets this private copy.
    let staged_sha256 = sha256_file(&staging)?;
    validate_manifest_bytes(&manifest, &staged_sha256)?;
    let staged_version = validate_database_file(&staging).await?;
    if staged_version != manifest.schema_version {
        bail!("backup manifest schema version does not match the database");
    }
    validate_encrypted_values(&staging).await?;
    checkpoint_database(&staging).await?;
    remove_checkpoint_sidecars(&staging)?;
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
    File::open(&staging)?.sync_all()?;

    let previous = if target_exists {
        Some(create_pre_restore_recovery_point(&target).await?)
    } else {
        None
    };
    if target_exists {
        let recovery = previous
            .as_ref()
            .context("existing database did not produce a pre-restore recovery point")?;
        prepare_current_database_for_replacement(&target, recovery).await?;
    }

    let publication = if target_exists {
        // POSIX rename atomically exchanges the directory entry. The original
        // remains reachable through `previous`, so there is no crash window in
        // which the canonical database path is absent.
        fs::rename(&staging, &target)
    } else {
        // A fresh restore must not overwrite a path created after preflight.
        fs::hard_link(&staging, &target)
    };
    publication.with_context(|| {
        if target_exists {
            "failed to atomically replace the current database"
        } else {
            "failed to publish restored database without overwriting an unexpected target"
        }
    })?;
    sync_parent(&target).with_context(|| {
        format!(
            "restored database was published at {}, but directory durability could not be confirmed",
            target.display()
        )
    })?;
    Ok(previous)
}

/// Ensure no WAL from the displaced database can be replayed beside the new
/// main file. Validation and checkpoint are first proven on a private copy; if
/// that fails and canonical has sidecars, restore refuses without opening or
/// mutating canonical. Otherwise, a successful canonical checkpoint makes the
/// old main file self-contained before sidecars are removed, so any later
/// publication error or crash still leaves a usable canonical database. A
/// corrupt single-file database can be replaced because its raw bytes were
/// already durably retained.
async fn prepare_current_database_for_replacement(
    target: &Path,
    recovery: &RecoveryPoint,
) -> anyhow::Result<()> {
    if let RecoveryPoint::UnverifiedForensicCopy {
        database,
        validation_error,
    } = recovery
    {
        let sidecars = existing_database_sidecars(target)?;
        if !sidecars.is_empty() {
            bail!(
                "the current database could not be validated and cannot safely be replaced while SQLite sidecars remain at {}; the unchanged raw family is retained as an unverified forensic copy at {} without a restore manifest; validation error: {}",
                sidecars
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                database.display(),
                validation_error
            );
        }
        sync_parent(target)?;
        return Ok(());
    }

    match checkpoint_database(target).await {
        Ok(()) => remove_checkpoint_sidecars(target)?,
        Err(error) => {
            let sidecars = existing_database_sidecars(target)?;
            if !sidecars.is_empty() {
                bail!(
                    "failed to checkpoint the current database and cannot safely replace it while SQLite sidecars remain at {}: {error:#}",
                    sidecars
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
    let remaining = existing_database_sidecars(target)?;
    if !remaining.is_empty() {
        bail!(
            "refusing to replace the current database while SQLite sidecars remain at {}",
            remaining
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    sync_parent(target)?;
    Ok(())
}

/// Preserve the database family before inspecting it. A damaged current
/// database is the most important restore case, so validation failure changes
/// the result classification but never prevents a validated replacement from
/// being installed.
async fn create_pre_restore_recovery_point(target: &Path) -> anyhow::Result<RecoveryPoint> {
    ensure_regular_file(target, "current SQLite database")?;
    let parent = target.parent().context("database path has no parent")?;
    let nonce = uuid::Uuid::new_v4();
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let raw = parent.join(format!(
        "unionc.pre-restore-unverified-{timestamp}-{nonce}.db"
    ));
    ensure_new_output(&raw)?;
    ensure_new_output(&manifest_path(&raw))?;
    let mut raw_guard = DatabaseFamilyGuard::new(raw.clone());
    copy_database_family_private(target, &raw)?;
    sync_parent(&raw).with_context(|| {
        format!(
            "raw pre-restore copy was written at {}, but directory durability could not be confirmed",
            raw.display()
        )
    })?;

    // Never open the sole raw family: SQLite may update shared-memory state
    // even for a read-only validation. All inspection and checkpoint work is
    // performed on a second private candidate so an unverified result remains
    // byte-for-byte forensic evidence.
    let validated =
        create_validated_recovery_point(&raw, parent, &timestamp.to_string(), nonce).await;
    match validated {
        Ok(database) => Ok(RecoveryPoint::Validated { database }),
        Err(error) => Ok(retain_unverified_recovery_point(&mut raw_guard, raw, error)),
    }
}

async fn create_validated_recovery_point(
    raw: &Path,
    parent: &Path,
    timestamp: &str,
    nonce: uuid::Uuid,
) -> anyhow::Result<PathBuf> {
    let staging = parent.join(format!(".unionc-pre-restore-validated-{nonce}.db"));
    let _staging_guard = DatabaseFamilyGuard::new(staging.clone());
    copy_database_family_private(raw, &staging)?;
    let expected_schema_version = validate_database_file(&staging).await?;
    validate_encrypted_values(&staging).await?;
    checkpoint_database(&staging)
        .await
        .context("failed to make the pre-restore copy self-contained")?;
    remove_checkpoint_sidecars(&staging)?;
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
    File::open(&staging)?.sync_all()?;

    let schema_version = validate_database_file(&staging).await?;
    if schema_version != expected_schema_version {
        bail!("pre-restore schema changed while creating its validated copy");
    }
    validate_encrypted_values(&staging).await?;
    let database_sha256 = sha256_file(&staging)?;

    let backup = parent.join(format!("unionc.pre-restore-{timestamp}-{nonce}.db"));
    let final_manifest = manifest_path(&backup);
    let staging_manifest = parent.join(format!(".unionc-pre-restore-{nonce}.manifest.json"));
    ensure_new_output(&backup)?;
    ensure_new_output(&final_manifest)?;
    let _manifest_guard = FileGuard::new(staging_manifest.clone());

    let manifest = build_manifest(&backup, database_sha256, schema_version)?;
    write_manifest(&staging_manifest, &manifest)?;

    // Publish without replacing any racing path. Publishing the manifest
    // first means a crash can at worst leave an inert manifest; an error while
    // linking the database removes that manifest before restore can continue.
    fs::hard_link(&staging_manifest, &final_manifest)
        .context("failed to publish pre-restore manifest without overwriting an existing file")?;
    if let Err(error) = fs::hard_link(&staging, &backup) {
        remove_if_same_file(&final_manifest, &staging_manifest);
        return Err(error).with_context(|| {
            format!(
                "failed to retain the pre-restore database at {}",
                backup.display()
            )
        });
    }
    sync_parent(&backup).with_context(|| {
        format!(
            "pre-restore recovery point was published at {}, but directory durability could not be confirmed",
            backup.display()
        )
    })?;
    Ok(backup)
}

fn retain_unverified_recovery_point(
    guard: &mut DatabaseFamilyGuard,
    database: PathBuf,
    error: anyhow::Error,
) -> RecoveryPoint {
    let validation_error = format!("{error:#}");
    tracing::warn!(
        path = %database.display(),
        error = %validation_error,
        "retained an unverified forensic copy of the replaced SQLite database"
    );
    guard.disarm();
    RecoveryPoint::UnverifiedForensicCopy {
        database,
        validation_error,
    }
}

async fn open_read_only(path: &Path) -> anyhow::Result<SqliteConnection> {
    ensure_regular_file(path, "SQLite database")?;
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(30));
    Ok(SqliteConnection::connect_with(&options).await?)
}

pub(super) async fn validate_database_file(path: &Path) -> anyhow::Result<i64> {
    let (expected_metadata, expected_schema) = reference_database_metadata().await?;
    validate_database_file_against(path, &expected_metadata, &expected_schema).await
}

async fn validate_database_file_against(
    path: &Path,
    expected_metadata: &SchemaMetadata,
    expected_schema: &[SchemaObject],
) -> anyhow::Result<i64> {
    let mut connection = open_read_only(path).await?;
    let rows = query("PRAGMA integrity_check")
        .fetch_all(&mut connection)
        .await?;
    for row in rows {
        let result: String = row.try_get(0)?;
        if result != "ok" {
            bail!(
                "SQLite integrity_check failed for {}: {result}",
                path.display()
            );
        }
    }
    let foreign_key_errors = query("PRAGMA foreign_key_check")
        .fetch_all(&mut connection)
        .await?;
    if !foreign_key_errors.is_empty() {
        bail!(
            "SQLite foreign_key_check found {} violation(s) in {}",
            foreign_key_errors.len(),
            path.display()
        );
    }
    let actual_metadata = load_schema_metadata(&mut connection)
        .await
        .context("database does not contain valid UnionC schema metadata")?;
    let actual_schema = load_schema_objects(&mut connection).await?;
    if actual_metadata != *expected_metadata {
        bail!(
            "unsupported UnionC SQLite schema metadata in {}: expected {:?}, found {:?}",
            path.display(),
            expected_metadata,
            actual_metadata
        );
    }
    if actual_schema != expected_schema {
        bail!(
            "UnionC SQLite schema mismatch in {}: {}",
            path.display(),
            describe_schema_mismatch(expected_schema, &actual_schema)
        );
    }

    Ok(actual_metadata.version)
}

#[derive(Debug, PartialEq, Eq)]
struct SchemaMetadata {
    version: i64,
    application_version: String,
    checksum: String,
}

#[derive(Debug, PartialEq, Eq)]
struct SchemaObject {
    object_type: String,
    name: String,
    table_name: String,
    sql: Option<String>,
}

async fn load_schema_metadata(connection: &mut SqliteConnection) -> anyhow::Result<SchemaMetadata> {
    let rows = query(
        "SELECT schema_version AS version,application_version,checksum \
         FROM schema_metadata ORDER BY schema_version",
    )
    .fetch_all(connection)
    .await?;
    if rows.len() != 1 {
        bail!(
            "schema_metadata must contain exactly one row; found {}",
            rows.len()
        );
    }
    let row = &rows[0];
    Ok(SchemaMetadata {
        version: row.try_get("version")?,
        application_version: row.try_get("application_version")?,
        checksum: row.try_get("checksum")?,
    })
}

async fn load_schema_objects(
    connection: &mut SqliteConnection,
) -> anyhow::Result<Vec<SchemaObject>> {
    query(
        r#"
        SELECT type AS object_type,name,tbl_name AS table_name,sql
        FROM sqlite_schema
        WHERE type IN ('table','index','view','trigger')
          AND name NOT LIKE 'sqlite_%'
        ORDER BY type,name,tbl_name
        "#,
    )
    .fetch_all(connection)
    .await?
    .into_iter()
    .map(|row| {
        Ok(SchemaObject {
            object_type: row.try_get("object_type")?,
            name: row.try_get("name")?,
            table_name: row.try_get("table_name")?,
            sql: row.try_get("sql")?,
        })
    })
    .collect()
}

/// Build the expected metadata and schema with the exact bundled SQLite engine
/// used to validate the live file. Comparing `sqlite_schema` catches missing,
/// additional or altered tables, columns, CHECK constraints, foreign keys,
/// STRICT declarations and explicit indexes; checking only schema version
/// would accept a damaged database whose metadata happened to survive.
async fn reference_database_metadata() -> anyhow::Result<(SchemaMetadata, Vec<SchemaObject>)> {
    let options = SqliteConnectOptions::new()
        .in_memory(true)
        .foreign_keys(true);
    let mut connection = SqliteConnection::connect_with(&options).await?;
    super::initialize_schema_inner(&mut connection).await?;
    let metadata = load_schema_metadata(&mut connection).await?;
    let schema = load_schema_objects(&mut connection).await?;
    connection.close().await?;
    Ok((metadata, schema))
}

fn describe_schema_mismatch(expected: &[SchemaObject], actual: &[SchemaObject]) -> String {
    let difference = expected
        .iter()
        .zip(actual)
        .find(|(expected, actual)| expected != actual);
    if let Some((expected, actual)) = difference {
        if expected.object_type == actual.object_type
            && expected.name == actual.name
            && expected.table_name == actual.table_name
        {
            return format!(
                "definition differs for {} {} on {}",
                expected.object_type, expected.name, expected.table_name
            );
        }
        return format!(
            "expected {} {} on {}, found {} {} on {}",
            expected.object_type,
            expected.name,
            expected.table_name,
            actual.object_type,
            actual.name,
            actual.table_name
        );
    }
    format!(
        "expected {} table/index objects, found {}",
        expected.len(),
        actual.len()
    )
}

pub(super) async fn validate_encrypted_values(path: &Path) -> anyhow::Result<()> {
    let mut connection = open_read_only(path).await?;
    let rows = query("SELECT config,secret FROM external_hosts")
        .fetch_all(&mut connection)
        .await?;
    for row in rows {
        let config: String = row.try_get("config")?;
        super::settings::validate_stored_sunshine_host_config(&config)
            .context("backup contains invalid Sunshine host configuration")?;
        if let Some(encrypted) = row.try_get::<Option<String>, _>("secret")? {
            crate::infra::secrets::decrypt(&encrypted)
                .context("backup contains a Sunshine secret that cannot be decrypted")?;
        }
    }
    Ok(())
}

fn read_manifest(path: &Path) -> anyhow::Result<BackupManifest> {
    let manifest_path = manifest_path(path);
    ensure_regular_file(&manifest_path, "backup manifest")?;
    let manifest = match fs::read_to_string(&manifest_path) {
        Ok(raw) => serde_json::from_str::<BackupManifest>(&raw)
            .with_context(|| format!("invalid backup manifest {}", manifest_path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "backup manifest {} is missing; restore requires the database and manifest generated by `unionc backup`",
                manifest_path.display()
            )
        }
        Err(error) => return Err(error.into()),
    };
    Ok(manifest)
}

fn validate_manifest_bytes(manifest: &BackupManifest, database_sha256: &str) -> anyhow::Result<()> {
    if manifest.format_version != BACKUP_FORMAT_VERSION {
        bail!(
            "unsupported backup manifest format {}",
            manifest.format_version
        );
    }
    if manifest.database_sha256 != database_sha256 {
        bail!("backup SHA-256 does not match its manifest");
    }
    if manifest.application_version != env!("CARGO_PKG_VERSION") {
        bail!(
            "backup was created by application version {}; expected {}",
            manifest.application_version,
            env!("CARGO_PKG_VERSION")
        );
    }
    let expected_schema_version = super::current_schema_version();
    if manifest.schema_version != expected_schema_version {
        bail!(
            "backup manifest schema does not match the current schema: expected {}, found {}",
            expected_schema_version,
            manifest.schema_version
        );
    }
    Ok(())
}

fn build_manifest(
    database: &Path,
    database_sha256: String,
    schema_version: i64,
) -> anyhow::Result<BackupManifest> {
    Ok(BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        application_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        database_file: database
            .file_name()
            .and_then(|name| name.to_str())
            .context("backup output has no valid file name")?
            .to_string(),
        database_sha256,
        schema_version,
        encryption_key_id: crate::infra::secrets::current_key_id()?.to_string(),
    })
}

fn write_manifest(path: &Path, manifest: &BackupManifest) -> anyhow::Result<()> {
    ensure_new_output(path)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, manifest)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn manifest_path(database: &Path) -> PathBuf {
    let mut name: OsString = database.as_os_str().to_owned();
    name.push(".manifest.json");
    PathBuf::from(name)
}

fn ensure_new_output(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("refusing to overwrite existing path {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        bail!(
            "output parent directory does not exist: {}",
            parent.display()
        );
    }
    Ok(())
}

fn ensure_regular_file(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!("{label} is not a regular file: {}", path.display());
    }
    Ok(())
}

fn path_exists(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn absolute_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn copy_private_file(source: &Path, target: &Path) -> anyhow::Result<()> {
    ensure_new_output(target)?;
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(target)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    Ok(())
}

/// Copy a stopped SQLite database together with any WAL/SHM sidecars without
/// opening or mutating it. Every copied file is synced before this returns.
fn copy_database_family_private(source: &Path, target: &Path) -> anyhow::Result<()> {
    copy_private_file(source, target)?;
    for suffix in ["-wal", "-shm"] {
        let source_sidecar = database_sidecar(source, suffix);
        let metadata = match fs::symlink_metadata(&source_sidecar) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file() {
            bail!(
                "refusing unexpected SQLite sidecar type: {}",
                source_sidecar.display()
            );
        }
        copy_private_file(&source_sidecar, &database_sidecar(target, suffix))?;
    }
    Ok(())
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(super) async fn checkpoint_database(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(30));
    let mut connection = SqliteConnection::connect_with(&options).await?;
    let checkpoint = query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(&mut connection)
        .await?;
    let busy: i64 = checkpoint.try_get(0)?;
    let log_frames: i64 = checkpoint.try_get(1)?;
    let checkpointed_frames: i64 = checkpoint.try_get(2)?;
    connection.close().await?;
    if busy != 0 {
        bail!(
            "SQLite WAL checkpoint could not complete because the database is busy (log_frames={log_frames}, checkpointed_frames={checkpointed_frames})"
        );
    }
    Ok(())
}

fn database_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn existing_database_sidecars(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut existing = Vec::new();
    for suffix in ["-wal", "-shm"] {
        let sidecar = database_sidecar(path, suffix);
        match fs::symlink_metadata(&sidecar) {
            Ok(metadata) if metadata.file_type().is_file() => existing.push(sidecar),
            Ok(_) => bail!(
                "refusing unexpected SQLite sidecar type: {}",
                sidecar.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(existing)
}

pub(super) fn remove_checkpoint_sidecars(path: &Path) -> anyhow::Result<()> {
    for suffix in ["-wal", "-shm"] {
        let sidecar = database_sidecar(path, suffix);
        let metadata = match fs::symlink_metadata(&sidecar) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file() {
            bail!(
                "refusing unexpected SQLite sidecar type: {}",
                sidecar.display()
            );
        }
        if suffix == "-wal" && metadata.len() != 0 {
            bail!(
                "SQLite WAL {} is not empty after checkpoint",
                sidecar.display()
            );
        }
        fs::remove_file(&sidecar)?;
    }
    Ok(())
}

fn remove_if_same_file(path: &Path, reference: &Path) {
    let Ok(path_metadata) = fs::symlink_metadata(path) else {
        return;
    };
    let Ok(reference_metadata) = fs::symlink_metadata(reference) else {
        return;
    };
    if path_metadata.file_type().is_file()
        && reference_metadata.file_type().is_file()
        && path_metadata.dev() == reference_metadata.dev()
        && path_metadata.ino() == reference_metadata.ino()
    {
        let _ = fs::remove_file(path);
    }
}

pub(super) struct DatabaseFamilyGuard {
    path: PathBuf,
    armed: bool,
}

impl DatabaseFamilyGuard {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DatabaseFamilyGuard {
    fn drop(&mut self) {
        if self.armed {
            remove_database_family(&self.path);
        }
    }
}

struct FileGuard {
    path: PathBuf,
}

impl FileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(super) fn remove_database_family(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(database_sidecar(path, suffix));
    }
}

pub(super) fn sync_parent(path: &Path) -> anyhow::Result<()> {
    let parent = path.parent().context("path has no parent directory")?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
