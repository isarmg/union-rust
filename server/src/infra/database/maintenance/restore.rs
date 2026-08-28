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
    let _locks = acquire_offline_maintenance_locks(&target)?;
    let target_exists = path_exists(&target)?;
    if target_exists && !force {
        bail!(
            "database {} already exists; rerun with --force after stopping the service",
            target.display()
        );
    }
    let parent = target.parent().context("database path has no parent")?;
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
