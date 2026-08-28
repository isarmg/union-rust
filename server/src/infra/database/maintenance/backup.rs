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
    let pool = super::connect_existing(settings).await?;
    super::verify_schema(&pool).await?;

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
    validate_database_file(&path).await
}
