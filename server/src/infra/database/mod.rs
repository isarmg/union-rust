//! SQLite persistence entry point.
//!
//! UnionC is a single-node server. Its runtime database lives beside the
//! remaining server state, so a fresh installation does not need a separately
//! provisioned database service.

mod audit;
mod maintenance;
mod settings;

pub use audit::*;
pub use maintenance::*;
pub use settings::*;

use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    ops::{Deref, DerefMut},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc, Mutex as StdMutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx_core::{
    connection::Connection, executor::Executor, query::query, raw_sql::raw_sql, row::Row,
    transaction::Transaction,
};
use sqlx_sqlite::{
    Sqlite, SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool,
    SqlitePoolOptions, SqliteSynchronous,
};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::config::Settings;

const DATABASE_FILE_NAME: &str = "unionc.db";
const WRITE_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// Project-wide runtime pool type.
pub type DbPool = SqlitePool;

/// SQLite has one writer per database file. This process-wide gate avoids
/// making in-process requests compete through `SQLITE_BUSY`; `BEGIN IMMEDIATE`
/// remains the cross-process correctness boundary.
static WRITE_GATE: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
static EXPECTED_SCHEMA_OBJECTS: tokio::sync::OnceCell<Vec<SchemaObject>> =
    tokio::sync::OnceCell::const_new();
/// Pool hooks capture the identity before `AppState` exists. This weak registry
/// makes later captures for the same canonical inode share one sticky poison
/// bit without keeping completed pools or test databases alive forever.
static DATABASE_IDENTITY_POISONS: OnceLock<StdMutex<IdentityPoisonRegistry>> = OnceLock::new();

type SchemaObject = (String, String, String, Option<String>);
type IdentityPoisonKey = (PathBuf, u64, u64);
type IdentityPoisonRegistry = HashMap<IdentityPoisonKey, Weak<AtomicBool>>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum DatabaseIdentityKind {
    InMemory,
    File { device: u64, inode: u64 },
}

/// The canonical runtime database path and the filesystem object opened at
/// startup. SQLite keeps an unlinked file usable through its existing file
/// descriptor, so a SQL-only health query cannot detect that future restarts
/// would open a different (or missing) database.
#[derive(Clone, Debug)]
pub struct DatabaseIdentity {
    path: PathBuf,
    kind: DatabaseIdentityKind,
    poisoned: Arc<AtomicBool>,
}

impl PartialEq for DatabaseIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.kind == other.kind
    }
}

impl Eq for DatabaseIdentity {}

impl DatabaseIdentity {
    pub fn capture(settings: &Settings) -> anyhow::Result<Self> {
        let identity = Self::capture_path(database_path(settings)?)?;
        identity.verify()?;
        Ok(identity)
    }

    fn capture_path(path: PathBuf) -> anyhow::Result<Self> {
        if path == Path::new(":memory:") {
            return Ok(Self {
                path,
                kind: DatabaseIdentityKind::InMemory,
                poisoned: Arc::new(AtomicBool::new(false)),
            });
        }
        let (device, inode) = regular_file_identity(&path)?;
        Ok(Self {
            poisoned: identity_poison(&path, device, inode),
            path,
            kind: DatabaseIdentityKind::File { device, inode },
        })
    }

    /// Ensure the configured path still names the exact database file that
    /// was present when application state was constructed.
    pub fn verify(&self) -> anyhow::Result<()> {
        if self.poisoned.load(Ordering::SeqCst) {
            anyhow::bail!(
                "runtime SQLite database identity was previously invalidated; restart is required"
            );
        }
        let DatabaseIdentityKind::File { device, inode } = self.kind else {
            return Ok(());
        };
        let current = regular_file_identity(&self.path).map_err(|error| {
            anyhow::anyhow!(
                "runtime SQLite database path {} is unavailable: {error:#}",
                self.path.display()
            )
        });
        let current = match current {
            Ok(current) => current,
            Err(error) => {
                self.poisoned.store(true, Ordering::SeqCst);
                return Err(error);
            }
        };
        if current != (device, inode) {
            self.poisoned.store(true, Ordering::SeqCst);
            anyhow::bail!(
                "runtime SQLite database path {} no longer identifies the file opened at startup",
                self.path.display()
            );
        }
        if self.poisoned.load(Ordering::SeqCst) {
            anyhow::bail!(
                "runtime SQLite database identity was invalidated concurrently; restart is required"
            );
        }
        Ok(())
    }
}

fn identity_poison(path: &Path, device: u64, inode: u64) -> Arc<AtomicBool> {
    let registry = DATABASE_IDENTITY_POISONS.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut registry = registry.lock().unwrap_or_else(|error| error.into_inner());
    registry.retain(|_, poison| poison.strong_count() > 0);
    let key = (path.to_path_buf(), device, inode);
    if let Some(poisoned) = registry.get(&key).and_then(Weak::upgrade) {
        return poisoned;
    }
    let poisoned = Arc::new(AtomicBool::new(false));
    registry.insert(key, Arc::downgrade(&poisoned));
    poisoned
}

fn regular_file_identity(path: &Path) -> anyhow::Result<(u64, u64)> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| anyhow::anyhow!("failed to inspect {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        anyhow::bail!(
            "SQLite database path {} is not a regular file",
            path.display()
        );
    }
    if metadata.nlink() != 1 {
        anyhow::bail!(
            "SQLite database path {} must have exactly one hard link",
            path.display()
        );
    }
    Ok((metadata.dev(), metadata.ino()))
}

fn optional_database_identity(
    path: &Path,
    create_if_missing: bool,
) -> anyhow::Result<Option<DatabaseIdentity>> {
    if path == Path::new(":memory:") {
        return Ok(Some(DatabaseIdentity::capture_path(path.to_path_buf())?));
    }
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(Some(DatabaseIdentity::capture_path(path.to_path_buf())?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_if_missing => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(anyhow::anyhow!(
            "failed to open existing SQLite database {} (automatic creation is disabled): {error}",
            path.display()
        )),
        Err(error) => Err(anyhow::anyhow!(
            "failed to inspect SQLite database path {} before opening: {error}",
            path.display()
        )),
    }
}

fn pool_identity_result(
    identity: &OnceLock<DatabaseIdentity>,
) -> Result<(), sqlx_core::error::Error> {
    identity
        .get()
        .map(DatabaseIdentity::verify)
        .transpose()
        .map(|_| ())
        .map_err(|error| sqlx_core::error::Error::Protocol(error.to_string()))
}

fn write_gate() -> Arc<Mutex<()>> {
    WRITE_GATE.get_or_init(|| Arc::new(Mutex::new(()))).clone()
}

/// An owning `BEGIN IMMEDIATE` transaction.
///
/// The wrapper retains the in-process writer permit until commit or rollback
/// finishes. If it is dropped early, SQLx schedules a rollback before reusing
/// the pooled connection.
pub struct WriteTransaction {
    transaction: Transaction<'static, Sqlite>,
    _permit: OwnedMutexGuard<()>,
}

impl WriteTransaction {
    /// Explicit connection accessor for helpers that accept a
    /// `&mut SqliteConnection`.
    pub fn connection(&mut self) -> &mut SqliteConnection {
        &mut self.transaction
    }

    pub async fn commit(self) -> anyhow::Result<()> {
        let Self {
            transaction,
            _permit,
        } = self;
        transaction.commit().await?;
        drop(_permit);
        Ok(())
    }

    pub async fn rollback(self) -> anyhow::Result<()> {
        let Self {
            transaction,
            _permit,
        } = self;
        transaction.rollback().await?;
        drop(_permit);
        Ok(())
    }
}

impl Deref for WriteTransaction {
    type Target = SqliteConnection;

    fn deref(&self) -> &Self::Target {
        &self.transaction
    }
}

impl DerefMut for WriteTransaction {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.transaction
    }
}

/// Start a write transaction without SQLite's deferred read-to-write transition.
///
/// Every read-modify-write operation must use this function. A deferred
/// `BEGIN` can deadlock with another connection after both have read a snapshot;
/// `BEGIN IMMEDIATE` reserves the sole writer slot before the first read.
pub async fn begin_write(pool: &DbPool) -> anyhow::Result<WriteTransaction> {
    let permit = write_gate().lock_owned().await;
    let connection = pool.acquire().await?;
    let transaction =
        Transaction::begin(connection, Some(Cow::Borrowed("BEGIN IMMEDIATE"))).await?;
    Ok(WriteTransaction {
        transaction,
        _permit: permit,
    })
}

/// Resolve the runtime database file. An empty internal override means the
/// fixed production location `<UNIONC_DATA_DIR>/unionc.db`.
///
/// Tests may provide either a `sqlite:` URL or a filesystem path through
/// `Settings.database.url`. This is not a user-facing database server setting.
pub fn database_path(settings: &Settings) -> anyhow::Result<PathBuf> {
    let raw = settings.database.url.trim();
    if raw.is_empty() {
        return Ok(crate::infra::paths::data_dir().join(DATABASE_FILE_NAME));
    }
    if raw.starts_with("sqlite:") {
        let options = SqliteConnectOptions::from_str(raw)?;
        return absolutize_database_path(options.get_filename());
    }
    if raw.contains("://") {
        anyhow::bail!("unsupported database URL; UnionC runtime storage is SQLite-only");
    }
    absolutize_database_path(Path::new(raw))
}

fn absolutize_database_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path == Path::new(":memory:") {
        return Ok(path.to_path_buf());
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn connect_options(
    settings: &Settings,
    create_if_missing: bool,
) -> anyhow::Result<(SqliteConnectOptions, PathBuf)> {
    let raw = settings.database.url.trim();
    let mut options = if raw.starts_with("sqlite:") {
        SqliteConnectOptions::from_str(raw)?
    } else {
        SqliteConnectOptions::new().filename(database_path(settings)?)
    };

    let path = absolutize_database_path(options.get_filename())?;
    if path != Path::new(":memory:") {
        if create_if_missing && let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Normalize relative sqlite: URLs before handing them to worker
        // threads whose current directory is an implementation detail.
        options = options.filename(&path);
    }

    options = options
        // Apply this after parsing the URL so an internal `?mode=rwc` test
        // override cannot accidentally weaken an existing-only production
        // open into a database-creating open.
        .create_if_missing(create_if_missing)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .busy_timeout(WRITE_BUSY_TIMEOUT);
    Ok((options, path))
}

async fn connect_with_policy(
    settings: &Settings,
    create_if_missing: bool,
) -> anyhow::Result<DbPool> {
    let (options, path) = connect_options(settings, create_if_missing)?;
    let identity_before = optional_database_identity(&path, create_if_missing)?;
    let pool_identity = Arc::new(OnceLock::new());
    let before_acquire_identity = pool_identity.clone();
    let after_connect_identity = pool_identity.clone();
    let max_connections = if path == Path::new(":memory:") { 1 } else { 8 };
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(1800))
        .before_acquire(move |_connection, _metadata| {
            let result = pool_identity_result(before_acquire_identity.as_ref()).map(|()| true);
            Box::pin(async move { result })
        })
        .after_connect(move |_connection, _metadata| {
            let result = pool_identity_result(after_connect_identity.as_ref());
            Box::pin(async move { result })
        })
        .connect_with(options)
        .await
        .map_err(|error| {
            if create_if_missing {
                anyhow::anyhow!("failed to open SQLite database {}: {error}", path.display())
            } else {
                anyhow::anyhow!(
                    "failed to open existing SQLite database {} (automatic creation is disabled): {error}",
                    path.display()
                )
            }
        })?;

    #[cfg(unix)]
    if path != Path::new(":memory:") {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    let identity_after = DatabaseIdentity::capture_path(path.clone())?;
    if let Some(identity_before) = identity_before
        && identity_before != identity_after
    {
        pool.close().await;
        anyhow::bail!(
            "SQLite database path {} changed while it was being opened",
            path.display()
        );
    }
    pool_identity
        .set(identity_after)
        .map_err(|_| anyhow::anyhow!("SQLite database identity was initialized more than once"))?;

    Ok(pool)
}

/// Open the embedded runtime database, creating its file when absent.
///
/// Callers must make the bootstrap decision before using this function.
pub async fn connect(settings: &Settings) -> anyhow::Result<DbPool> {
    connect_with_policy(settings, true).await
}

/// Open an already existing embedded runtime database without creating its
/// parent directory or file.
pub async fn connect_existing(settings: &Settings) -> anyhow::Result<DbPool> {
    connect_with_policy(settings, false).await
}

/// Create a lazy in-memory SQLite pool for unit tests that do not exercise
/// persistent database paths.
pub fn in_memory_pool() -> anyhow::Result<DbPool> {
    let options = SqliteConnectOptions::new()
        .in_memory(true)
        .shared_cache(true)
        .foreign_keys(true)
        .synchronous(SqliteSynchronous::Full)
        .busy_timeout(WRITE_BUSY_TIMEOUT);
    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .connect_lazy_with(options))
}

/// Install or verify the sole SQLite schema supported by this build.
pub async fn initialize_schema(pool: &DbPool) -> anyhow::Result<()> {
    let mut tx = begin_write(pool).await?;
    match initialize_schema_inner(tx.connection()).await {
        Ok(()) => tx.commit().await,
        Err(error) => {
            let rollback = tx.rollback().await;
            if let Err(rollback_error) = rollback {
                tracing::error!(%rollback_error, "failed to roll back SQLite schema initialization");
            }
            Err(error)
        }
    }
}

/// Verify an existing database without treating an empty file as a fresh
/// installation. Normal production startup and offline maintenance commands
/// use this path so missing data cannot be replaced by a plausible empty
/// service.
pub async fn verify_schema(pool: &DbPool) -> anyhow::Result<()> {
    let mut tx = begin_write(pool).await?;
    match verify_current_schema(tx.connection()).await {
        Ok(()) => tx.commit().await,
        Err(error) => {
            let rollback = tx.rollback().await;
            if let Err(rollback_error) = rollback {
                tracing::error!(%rollback_error, "failed to roll back SQLite schema verification");
            }
            Err(error)
        }
    }
}

/// Install or verify the sole schema supported by this build.
pub(super) async fn initialize_schema_inner(
    connection: &mut SqliteConnection,
) -> anyhow::Result<()> {
    let object_count: i64 = query(
        "SELECT COUNT(*) FROM sqlite_schema \
         WHERE type IN ('table','index','view','trigger') AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_one(&mut *connection)
    .await?
    .try_get(0)?;
    if object_count == 0 {
        return install_current_schema(connection).await;
    }
    verify_current_schema(connection).await
}

async fn install_current_schema(connection: &mut SqliteConnection) -> anyhow::Result<()> {
    query(
        r#"
        CREATE TABLE schema_metadata (
            schema_version      INTEGER PRIMARY KEY CHECK (schema_version = 1),
            application_version TEXT NOT NULL,
            checksum            TEXT NOT NULL
        ) STRICT
        "#,
    )
    .execute(&mut *connection)
    .await?;

    let checksum = schema_checksum(CURRENT_SCHEMA.sql);
    (&mut *connection)
        .execute(raw_sql(CURRENT_SCHEMA.sql))
        .await?;
    query("INSERT INTO schema_metadata(schema_version,application_version,checksum) VALUES(?,?,?)")
        .bind(CURRENT_SCHEMA.version)
        .bind(env!("CARGO_PKG_VERSION"))
        .bind(checksum)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

async fn verify_current_schema(connection: &mut SqliteConnection) -> anyhow::Result<()> {
    let installed = query(
        "SELECT schema_version,application_version,checksum \
         FROM schema_metadata ORDER BY schema_version",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| {
        anyhow::anyhow!(
            "database is not the current UnionC schema (missing or invalid schema metadata): {error}"
        )
    })?;
    if installed.len() != 1
        || installed[0].try_get::<i64, _>("schema_version")? != CURRENT_SCHEMA.version
        || installed[0].try_get::<String, _>("application_version")? != env!("CARGO_PKG_VERSION")
        || installed[0].try_get::<String, _>("checksum")? != schema_checksum(CURRENT_SCHEMA.sql)
    {
        anyhow::bail!("database is not the current UnionC schema (schema metadata mismatch)");
    }

    let actual = schema_objects(connection).await?;
    let expected = expected_schema_objects().await?;
    if actual.as_slice() != expected.as_slice() {
        anyhow::bail!("database is not the exact current UnionC SQLite schema");
    }
    Ok(())
}

async fn expected_schema_objects() -> anyhow::Result<&'static Vec<SchemaObject>> {
    EXPECTED_SCHEMA_OBJECTS
        .get_or_try_init(|| async {
            let options = SqliteConnectOptions::new()
                .in_memory(true)
                .foreign_keys(true);
            let mut reference = SqliteConnection::connect_with(&options).await?;
            install_current_schema(&mut reference).await?;
            let expected = schema_objects(&mut reference).await?;
            reference.close().await?;
            Ok(expected)
        })
        .await
}

async fn schema_objects(connection: &mut SqliteConnection) -> anyhow::Result<Vec<SchemaObject>> {
    query(
        r#"
        SELECT type,name,tbl_name,sql
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
        Ok((
            row.try_get("type")?,
            row.try_get("name")?,
            row.try_get("tbl_name")?,
            row.try_get("sql")?,
        ))
    })
    .collect()
}

struct CurrentSchema {
    version: i64,
    sql: &'static str,
}

const CURRENT_SCHEMA: CurrentSchema = CurrentSchema {
    version: 1,
    sql: include_str!("../../../schema/sqlite.sql"),
};

pub const fn current_schema_version() -> i64 {
    CURRENT_SCHEMA.version
}

fn schema_checksum(sql: &str) -> String {
    Sha256::digest(sql.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Encode all persisted timestamps as Unix microseconds.
pub fn to_epoch_micros(value: DateTime<Utc>) -> i64 {
    value.timestamp_micros()
}

/// Decode a persisted Unix-microsecond timestamp.
pub fn from_epoch_micros(value: i64) -> anyhow::Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_micros(value)
        .ok_or_else(|| anyhow::anyhow!("timestamp is outside chrono's supported range: {value}"))
}

pub fn now_epoch_micros() -> i64 {
    to_epoch_micros(Utc::now())
}

pub async fn ping(pool: &DbPool, identity: &DatabaseIdentity) -> anyhow::Result<()> {
    identity.verify()?;
    let mut connection = pool.acquire().await?;
    verify_current_schema(&mut connection).await?;
    // Catch a rename/replacement that raced the schema query. An ABA swap can
    // only evade this by restoring the same inode and therefore the same file.
    identity.verify()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx_core::row::Row;

    #[test]
    fn timestamp_codec_preserves_microseconds() {
        let original = DateTime::<Utc>::from_timestamp(1_700_000_000, 123_456_000).unwrap();
        assert_eq!(
            from_epoch_micros(to_epoch_micros(original)).unwrap(),
            original
        );
    }

    #[test]
    fn raw_database_paths_are_made_absolute() {
        let path = absolutize_database_path(Path::new("tmp/test.db")).unwrap();
        assert!(path.is_absolute());
        assert!(path.ends_with("tmp/test.db"));
    }

    #[test]
    fn database_identity_rejects_aliases_of_the_canonical_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unionc.db");
        std::fs::File::create(&path).unwrap();

        let hard_link = directory.path().join("unionc-hard-link.db");
        std::fs::hard_link(&path, &hard_link).unwrap();
        let hard_link_error = DatabaseIdentity::capture_path(path.clone()).unwrap_err();
        assert!(
            hard_link_error
                .to_string()
                .contains("must have exactly one hard link"),
            "{hard_link_error:#}"
        );

        std::fs::remove_file(&hard_link).unwrap();
        let symlink = directory.path().join("unionc-symlink.db");
        std::os::unix::fs::symlink(&path, &symlink).unwrap();
        let symlink_error = DatabaseIdentity::capture_path(symlink).unwrap_err();
        assert!(
            symlink_error.to_string().contains("is not a regular file"),
            "{symlink_error:#}"
        );
    }

    #[test]
    fn database_identity_rejects_a_hard_link_added_after_capture() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unionc.db");
        std::fs::File::create(&path).unwrap();
        let identity = DatabaseIdentity::capture_path(path.clone()).unwrap();

        let alias = directory.path().join("unexpected-alias.db");
        std::fs::hard_link(&path, &alias).unwrap();
        let error = identity.verify().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must have exactly one hard link"),
            "{error:#}"
        );
        std::fs::remove_file(alias).unwrap();
        assert!(
            identity.verify().is_err(),
            "an observed hard-link violation must remain poisoned until restart"
        );
    }

    #[tokio::test]
    async fn runtime_pool_rejects_sql_after_the_database_is_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unionc.db");
        let replacement = directory.path().join("replacement.db");
        let displaced = directory.path().join("displaced.db");

        let mut settings = Settings::default();
        settings.database.url = path.display().to_string();
        let pool = connect(&settings).await.unwrap();
        initialize_schema(&pool).await.unwrap();

        let mut replacement_settings = Settings::default();
        replacement_settings.database.url = replacement.display().to_string();
        let replacement_pool = connect(&replacement_settings).await.unwrap();
        initialize_schema(&replacement_pool).await.unwrap();
        replacement_pool.close().await;

        std::fs::rename(&path, &displaced).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let write = tokio::time::timeout(
            Duration::from_millis(250),
            query("INSERT INTO audit_logs(action,target) VALUES('test.replaced','database')")
                .execute(&pool),
        )
        .await;
        assert!(
            !matches!(write, Ok(Ok(_))),
            "no pooled connection may adopt or keep using a replaced database path"
        );
        pool.close().await;
    }

    #[tokio::test]
    async fn current_schema_and_immediate_write_transactions_work_on_a_file_database() {
        let path = std::env::temp_dir().join(format!(
            "unionc-database-core-{}-{}.db",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut settings = Settings::default();
        settings.database.url = path.display().to_string();
        let pool = connect(&settings).await.unwrap();

        initialize_schema(&pool).await.unwrap();
        initialize_schema(&pool).await.unwrap();
        let versions: i64 = query("SELECT COUNT(*) FROM schema_metadata")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
        assert_eq!(versions, 1);

        let mut tx = begin_write(&pool).await.unwrap();
        query(
            "INSERT INTO audit_logs(action,target,detail) \
             VALUES('test.transaction','database','yes')",
        )
        .execute(tx.connection())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let value: String = query("SELECT detail FROM audit_logs WHERE action='test.transaction'")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
        assert_eq!(value, "yes");

        let foreign_keys: i64 = query("PRAGMA foreign_keys")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
        assert_eq!(foreign_keys, 1);

        pool.close().await;
        for candidate in [
            path.clone(),
            PathBuf::from(format!("{}-wal", path.display())),
            PathBuf::from(format!("{}-shm", path.display())),
        ] {
            match std::fs::remove_file(candidate) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("failed to clean temporary database: {error}"),
            }
        }
    }
}
