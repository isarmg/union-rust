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

/// Locks required by an offline operation that both mutates the live database
/// and must exclude every explicit maintenance command.
pub struct OfflineMaintenanceLocks {
    _server: DatabaseFileLock,
    _maintenance: DatabaseFileLock,
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

/// Exclude the running Server first, then serialize with online backup,
/// integrity checks, restore, and other maintenance. Both locks use
/// non-blocking acquisition, so a competing command fails instead of creating
/// a lock-order deadlock.
pub fn acquire_offline_maintenance_locks(
    database: &Path,
) -> anyhow::Result<OfflineMaintenanceLocks> {
    let server = acquire_database_lock(database)?;
    let maintenance = acquire_maintenance_lock(database)?;
    Ok(OfflineMaintenanceLocks {
        _server: server,
        _maintenance: maintenance,
    })
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
