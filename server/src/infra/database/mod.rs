//! SQLite persistence entry point.
//!
//! UnionC is a single-node server. Its runtime database lives beside the
//! remaining server state, so a fresh installation does not need a separately
//! provisioned database service.

mod audit;
mod maintenance;

pub use audit::*;
pub use maintenance::*;

use std::{
    borrow::Cow,
    collections::HashMap,
    fs::{self, File, OpenOptions},
    ops::{Deref, DerefMut},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
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
const DATABASE_FILE_MODE: u32 = 0o600;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatabaseIdentityKind {
    InMemory,
    File {
        device: u64,
        inode: u64,
        owner_uid: u32,
        owner_gid: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DatabaseFileIdentity {
    device: u64,
    inode: u64,
    owner_uid: u32,
    owner_gid: u32,
    mode: u32,
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
        let file = private_database_file_identity(&path)?;
        Ok(Self {
            poisoned: identity_poison(&path, file.device, file.inode),
            path,
            kind: DatabaseIdentityKind::File {
                device: file.device,
                inode: file.inode,
                owner_uid: file.owner_uid,
                owner_gid: file.owner_gid,
            },
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
        let Some(expected) = self.expected_file_identity() else {
            return Ok(());
        };
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) => {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) {
                    self.invalidate();
                }
                return Err(anyhow::anyhow!(
                    "runtime SQLite database path {} is unavailable: {error}",
                    self.path.display()
                ));
            }
        };
        let current = match checked_database_file_identity(&metadata, &self.path) {
            Ok(current) => current,
            Err(error) => {
                self.invalidate();
                return Err(error);
            }
        };
        if let Err(error) = ensure_private_database_mode(&self.path, current) {
            self.invalidate();
            return Err(error);
        };
        if current != expected {
            self.poisoned.store(true, Ordering::SeqCst);
            anyhow::bail!(
                "runtime SQLite database path {} no longer has the identity, owner, and private permissions captured at startup",
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

    fn invalidate(&self) {
        self.poisoned.store(true, Ordering::SeqCst);
    }

    fn expected_file_identity(&self) -> Option<DatabaseFileIdentity> {
        let DatabaseIdentityKind::File {
            device,
            inode,
            owner_uid,
            owner_gid,
        } = self.kind
        else {
            return None;
        };
        Some(DatabaseFileIdentity {
            device,
            inode,
            owner_uid,
            owner_gid,
            mode: DATABASE_FILE_MODE,
        })
    }

    fn verify_reopenable(&self) -> anyhow::Result<()> {
        self.verify_reopenable_with(open_database_file_read_write)
    }

    fn verify_reopenable_with<Open>(&self, opener: Open) -> anyhow::Result<()>
    where
        Open: FnOnce(&Path) -> std::io::Result<File>,
    {
        self.verify()?;
        let Some(expected) = self.expected_file_identity() else {
            return Ok(());
        };
        let file = match opener(&self.path) {
            Ok(file) => file,
            Err(open_error) => {
                // A path/metadata change is conclusive and remains sticky. A
                // plain reopen failure (for example EMFILE or a transient MAC
                // denial) makes this probe unavailable without permanently
                // poisoning an otherwise unchanged identity.
                self.verify()?;
                return Err(anyhow::anyhow!(
                    "failed to reopen SQLite database {} read-write: {open_error}",
                    self.path.display()
                ));
            }
        };
        let opened_metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                self.verify()?;
                return Err(anyhow::anyhow!(
                    "failed to inspect reopened SQLite database {}: {error}",
                    self.path.display()
                ));
            }
        };
        let opened = match checked_database_file_identity(&opened_metadata, &self.path) {
            Ok(opened) => opened,
            Err(error) => {
                self.invalidate();
                return Err(error);
            }
        };
        if opened != expected {
            self.invalidate();
            anyhow::bail!(
                "reopened SQLite database {} does not match the private file captured at startup",
                self.path.display()
            );
        }
        // Close the pathname/fd race on the far side of the raw reopen.
        self.verify()
    }

    fn is_in_memory(&self) -> bool {
        matches!(self.kind, DatabaseIdentityKind::InMemory)
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

fn regular_file_identity(path: &Path) -> anyhow::Result<DatabaseFileIdentity> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| anyhow::anyhow!("failed to inspect {}: {error}", path.display()))?;
    checked_database_file_identity(&metadata, path)
}

fn opened_database_file_identity(file: &File, path: &Path) -> anyhow::Result<DatabaseFileIdentity> {
    let metadata = file.metadata().map_err(|error| {
        anyhow::anyhow!(
            "failed to inspect the opened SQLite database {}: {error}",
            path.display()
        )
    })?;
    checked_database_file_identity(&metadata, path)
}

fn checked_database_file_identity(
    metadata: &fs::Metadata,
    path: &Path,
) -> anyhow::Result<DatabaseFileIdentity> {
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
    Ok(DatabaseFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner_uid: metadata.uid(),
        owner_gid: metadata.gid(),
        mode: metadata.mode() & 0o7777,
    })
}

fn private_database_file_identity(path: &Path) -> anyhow::Result<DatabaseFileIdentity> {
    let identity = regular_file_identity(path)?;
    ensure_private_database_mode(path, identity)?;
    Ok(identity)
}

fn ensure_private_database_mode(path: &Path, identity: DatabaseFileIdentity) -> anyhow::Result<()> {
    if identity.mode != DATABASE_FILE_MODE {
        anyhow::bail!(
            "SQLite database path {} must have permissions 0600, found {:04o}",
            path.display(),
            identity.mode
        );
    }
    Ok(())
}

fn open_database_file_read_write(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

fn verify_database_path_reopens_as(
    path: &Path,
    expected: DatabaseFileIdentity,
) -> anyhow::Result<()> {
    let reopened = open_database_file_read_write(path).map_err(|error| {
        anyhow::anyhow!(
            "failed to reopen SQLite database {} after permission normalization: {error}",
            path.display()
        )
    })?;
    let reopened_identity = opened_database_file_identity(&reopened, path)?;
    if reopened_identity != expected {
        anyhow::bail!(
            "SQLite database path {} did not reopen as the normalized private file",
            path.display()
        );
    }
    let final_path_identity = private_database_file_identity(path)?;
    if final_path_identity != expected {
        anyhow::bail!(
            "SQLite database path {} changed after its private file was reopened",
            path.display()
        );
    }
    Ok(())
}

fn prepare_database_identity(
    path: &Path,
    create_if_missing: bool,
) -> anyhow::Result<DatabaseIdentity> {
    if path == Path::new(":memory:") {
        return DatabaseIdentity::capture_path(path.to_path_buf());
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => normalize_existing_database_file(path, &metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_if_missing => {
            create_private_database_file(path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(anyhow::anyhow!(
                "failed to open existing SQLite database {} (automatic creation is disabled): {error}",
                path.display()
            ));
        }
        Err(error) => {
            return Err(anyhow::anyhow!(
                "failed to inspect SQLite database path {} before opening: {error}",
                path.display()
            ));
        }
    }
    DatabaseIdentity::capture_path(path.to_path_buf())
}

fn normalize_existing_database_file(
    path: &Path,
    initial_metadata: &fs::Metadata,
) -> anyhow::Result<()> {
    // Reject symlinks and hard links before changing anything. The descriptor
    // and final pathname snapshots then ensure chmod did not cross a rename.
    let before = checked_database_file_identity(initial_metadata, path)?;
    let file = open_database_file_read_write(path).map_err(|error| {
        anyhow::anyhow!(
            "failed to open existing SQLite database {} read-write before permission normalization: {error}",
            path.display()
        )
    })?;
    let opened_before = opened_database_file_identity(&file, path)?;
    if opened_before != before {
        anyhow::bail!(
            "SQLite database path {} changed while it was being opened for permission normalization",
            path.display()
        );
    }
    if opened_before.mode != DATABASE_FILE_MODE {
        file.set_permissions(fs::Permissions::from_mode(DATABASE_FILE_MODE))
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to set SQLite database {} permissions to 0600: {error}",
                    path.display()
                )
            })?;
    }
    let opened_after = opened_database_file_identity(&file, path)?;
    if opened_after.device != opened_before.device
        || opened_after.inode != opened_before.inode
        || opened_after.owner_uid != opened_before.owner_uid
        || opened_after.owner_gid != opened_before.owner_gid
    {
        anyhow::bail!(
            "opened SQLite database {} changed identity or owner during permission normalization",
            path.display()
        );
    }
    ensure_private_database_mode(path, opened_after)?;
    let path_after = private_database_file_identity(path)?;
    if path_after != opened_after {
        anyhow::bail!(
            "SQLite database path {} changed during permission normalization",
            path.display()
        );
    }
    verify_database_path_reopens_as(path, opened_after)?;
    Ok(())
}

fn create_private_database_file(path: &Path) -> anyhow::Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(DATABASE_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            anyhow::anyhow!(
                "failed to create private SQLite database {}: {error}",
                path.display()
            )
        })?;
    let created = opened_database_file_identity(&file, path)?;
    // Creation modes are filtered through the process umask. Normalize the
    // already-open descriptor so even an unusually restrictive umask cannot
    // leave the canonical database at a mode other than the required 0600.
    file.set_permissions(fs::Permissions::from_mode(DATABASE_FILE_MODE))
        .map_err(|error| {
            anyhow::anyhow!(
                "failed to set new SQLite database {} permissions to 0600: {error}",
                path.display()
            )
        })?;
    let opened = opened_database_file_identity(&file, path)?;
    if opened.device != created.device
        || opened.inode != created.inode
        || opened.owner_uid != created.owner_uid
        || opened.owner_gid != created.owner_gid
    {
        anyhow::bail!(
            "new SQLite database {} changed identity or owner during permission normalization",
            path.display()
        );
    }
    ensure_private_database_mode(path, opened)?;
    let current = private_database_file_identity(path)?;
    if current != opened {
        anyhow::bail!(
            "SQLite database path {} changed while its private file was being created",
            path.display()
        );
    }
    verify_database_path_reopens_as(path, opened)?;
    Ok(())
}

fn pool_identity_result(
    identity: &OnceLock<DatabaseIdentity>,
) -> Result<(), sqlx_core::error::Error> {
    identity
        .get()
        .ok_or_else(|| {
            sqlx_core::error::Error::Protocol(
                "SQLite database identity was not initialized before opening the pool".to_string(),
            )
        })?
        .verify()
        .map_err(|error| sqlx_core::error::Error::Protocol(error.to_string()))
}

async fn verify_pool_connection(
    connection: &mut SqliteConnection,
    identity_slot: &OnceLock<DatabaseIdentity>,
) -> Result<(), sqlx_core::error::Error> {
    pool_identity_result(identity_slot)?;
    let Some(identity) = identity_slot.get() else {
        return Err(sqlx_core::error::Error::Protocol(
            "SQLite database identity was not initialized before checking a connection".to_string(),
        ));
    };
    if identity.is_in_memory() {
        return Ok(());
    }

    let mut moved = 0_i32;
    let mut handle = connection.lock_handle().await?;
    // SAFETY: `lock_handle` exclusively owns SQLx's sqlite3 handle for this
    // scope. `c"main"` is NUL-terminated, `moved` is a live `int`, and the direct
    // libsqlite3-sys version is pinned to SQLx's semver-exempt dependency.
    let result = unsafe {
        libsqlite3_sys::sqlite3_file_control(
            handle.as_raw_handle().as_ptr(),
            c"main".as_ptr(),
            libsqlite3_sys::SQLITE_FCNTL_HAS_MOVED,
            (&mut moved as *mut i32).cast(),
        )
    };
    drop(handle);
    if result != libsqlite3_sys::SQLITE_OK {
        identity.invalidate();
        return Err(sqlx_core::error::Error::Protocol(format!(
            "SQLite could not verify the open main database file (code {result})"
        )));
    }
    if moved != 0 {
        identity.invalidate();
        return Err(sqlx_core::error::Error::Protocol(
            "the open SQLite main database file was moved, replaced, or deleted".to_string(),
        ));
    }
    // Close the path/handle race on both sides: the path must still identify
    // the expected inode after SQLite has inspected its actual open handle.
    pool_identity_result(identity_slot)
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
    // Establish a private, reopenable file before SQLx touches it. This keeps
    // pool hooks active even for one-shot bootstrap creation and prevents the
    // main file from briefly existing with broader permissions while SQLite
    // may create its sidecars.
    let identity_before = prepare_database_identity(&path, create_if_missing)?;
    let pool_identity = Arc::new(OnceLock::new());
    pool_identity
        .set(identity_before.clone())
        .map_err(|_| anyhow::anyhow!("SQLite database identity was initialized more than once"))?;
    let before_acquire_identity = pool_identity.clone();
    let after_connect_identity = pool_identity.clone();
    let max_connections = if path == Path::new(":memory:") { 1 } else { 8 };
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(1800))
        .before_acquire(move |connection, _metadata| {
            let identity = before_acquire_identity.clone();
            Box::pin(async move {
                verify_pool_connection(connection, identity.as_ref())
                    .await
                    .map(|()| true)
            })
        })
        .after_connect(move |connection, _metadata| {
            let identity = after_connect_identity.clone();
            Box::pin(async move { verify_pool_connection(connection, identity.as_ref()).await })
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

    let identity_after = match DatabaseIdentity::capture_path(path.clone()) {
        Ok(identity) => identity,
        Err(error) => {
            identity_before.invalidate();
            pool.close().await;
            return Err(error);
        }
    };
    if identity_before == identity_after {
        return Ok(pool);
    }
    identity_before.invalidate();
    pool.close().await;
    anyhow::bail!(
        "SQLite database path {} changed while it was being opened",
        path.display()
    );
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
            schema_version      INTEGER PRIMARY KEY CHECK (schema_version = 2),
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
    version: 2,
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
    // Existing SQLite descriptors remain usable after chmod/chown or an ACL
    // denial. Prove once per health-cache interval that the current service
    // credentials can reopen the same private main file read-write.
    identity.verify_reopenable()?;
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
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(DATABASE_FILE_MODE))
            .unwrap();
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

    #[test]
    fn database_identity_rejects_permission_changes_stickily() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unionc.db");
        std::fs::File::create(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(DATABASE_FILE_MODE))
            .unwrap();
        let identity = DatabaseIdentity::capture_path(path.clone()).unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let error = identity.verify().unwrap_err();
        assert!(error.to_string().contains("permissions 0600"), "{error:#}");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(DATABASE_FILE_MODE))
            .unwrap();
        assert!(
            identity.verify().is_err(),
            "restoring the mode must not revive a process that observed an unsafe database"
        );
    }

    #[test]
    fn transient_reopen_failure_does_not_poison_an_unchanged_identity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unionc.db");
        create_private_database_file(&path).unwrap();
        let identity = DatabaseIdentity::capture_path(path).unwrap();

        let error = identity
            .verify_reopenable_with(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected reopen denial",
                ))
            })
            .unwrap_err();
        assert!(error.to_string().contains("injected reopen denial"));
        assert!(
            identity.verify().is_ok(),
            "a transient raw-open error must not permanently poison stable metadata"
        );
    }

    #[test]
    fn reopen_check_rejects_a_different_open_inode_stickily() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unionc.db");
        let alternate = directory.path().join("alternate.db");
        create_private_database_file(&path).unwrap();
        create_private_database_file(&alternate).unwrap();
        let identity = DatabaseIdentity::capture_path(path).unwrap();

        let error = identity
            .verify_reopenable_with(|_| open_database_file_read_write(&alternate))
            .unwrap_err();
        assert!(error.to_string().contains("does not match"), "{error:#}");
        assert!(
            identity.verify().is_err(),
            "an observed reopened inode mismatch must remain poisoned"
        );
    }

    #[tokio::test]
    async fn connection_normalizes_existing_permissions_before_capturing_identity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unionc.db");
        std::fs::File::create(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut settings = Settings::default();
        settings.database.url = path.display().to_string();

        let pool = connect_existing(&settings).await.unwrap();

        assert_eq!(
            regular_file_identity(&path).unwrap().mode,
            DATABASE_FILE_MODE
        );
        DatabaseIdentity::capture(&settings)
            .unwrap()
            .verify()
            .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn invalid_database_is_normalized_before_sqlx_opens_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unionc.db");
        std::fs::write(&path, b"not a SQLite database").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut settings = Settings::default();
        settings.database.url = path.display().to_string();

        let error = connect_existing(&settings).await.unwrap_err();

        assert!(
            error.to_string().contains("failed to open existing"),
            "{error:#}"
        );
        assert_eq!(
            regular_file_identity(&path).unwrap().mode,
            DATABASE_FILE_MODE,
            "permission normalization must happen before SQLx can reject the contents"
        );
    }

    #[tokio::test]
    async fn connection_rejects_a_symlink_without_chmodding_its_target() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.db");
        let symlink = directory.path().join("unionc.db");
        std::fs::File::create(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(&target, &symlink).unwrap();
        let mut settings = Settings::default();
        settings.database.url = symlink.display().to_string();

        let error = connect_existing(&settings).await.unwrap_err();

        assert!(
            error.to_string().contains("not a regular file"),
            "{error:#}"
        );
        assert_eq!(
            regular_file_identity(&target).unwrap().mode,
            0o644,
            "a rejected symlink must not change its target's permissions"
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
            "a subsequent pool acquisition must not adopt a replaced database path"
        );
        pool.close().await;
    }

    #[tokio::test]
    async fn open_handle_check_rejects_an_aba_path_swap() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unionc.db");
        let alternate = directory.path().join("alternate.db");
        let saved_expected = directory.path().join("saved-expected.db");
        let saved_alternate = directory.path().join("saved-alternate.db");

        for candidate in [&path, &alternate] {
            let mut settings = Settings::default();
            settings.database.url = candidate.display().to_string();
            let pool = connect(&settings).await.unwrap();
            initialize_schema(&pool).await.unwrap();
            pool.close().await;
        }
        let identity = DatabaseIdentity::capture_path(path.clone()).unwrap();
        let second_capture = DatabaseIdentity::capture_path(path.clone()).unwrap();

        std::fs::rename(&path, &saved_expected).unwrap();
        std::fs::rename(&alternate, &path).unwrap();
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(false);
        let mut wrong_connection = SqliteConnection::connect_with(&options).await.unwrap();
        std::fs::rename(&path, &saved_alternate).unwrap();
        std::fs::rename(&saved_expected, &path).unwrap();

        assert!(
            identity.verify().is_ok(),
            "path-only checks cannot distinguish the restored expected inode"
        );
        let installed = OnceLock::new();
        installed.set(identity.clone()).unwrap();
        let error = verify_pool_connection(&mut wrong_connection, &installed)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("moved, replaced, or deleted"));
        assert!(
            identity.verify().is_err(),
            "an fd/path mismatch must poison every shared identity capture"
        );
        assert!(
            second_capture.verify().is_err(),
            "separate captures of one canonical inode must share sticky invalidation"
        );
        wrong_connection.close().await.unwrap();
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

        assert_eq!(
            regular_file_identity(&path).unwrap().mode,
            DATABASE_FILE_MODE,
            "a newly created database must be private before schema initialization"
        );

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
