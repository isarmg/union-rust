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
