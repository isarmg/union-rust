use std::ffi::OsString;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use sqlx_core::{query::query, row::Row};
use unionc::{config::Settings, infra::database};

fn unique_directory() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "unionc-sqlite-maintenance-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

async fn marker_value(path: &Path, key: &str) -> Option<String> {
    let mut settings = Settings::default();
    settings.database.url = path.display().to_string();
    let pool = database::connect(&settings).await.unwrap();
    let value = query(
        "SELECT detail FROM audit_logs WHERE action='test.snapshot' AND target=? ORDER BY id DESC LIMIT 1",
    )
        .bind(key)
        .fetch_optional(&pool)
        .await
        .unwrap()
        .map(|row| row.get(0));
    pool.close().await;
    value
}

async fn mutate_copy(source: &Path, target: &Path, statement: &str) -> Settings {
    std::fs::copy(source, target).unwrap();
    let mut settings = Settings::default();
    settings.database.url = target.display().to_string();
    let pool = database::connect(&settings).await.unwrap();
    query(statement).execute(&pool).await.unwrap();
    pool.close().await;
    settings
}

fn manifest_path(database: &Path) -> PathBuf {
    let mut name: OsString = database.as_os_str().to_owned();
    name.push(".manifest.json");
    PathBuf::from(name)
}

fn sha256_file(path: &Path) -> String {
    let mut file = std::fs::File::open(path).unwrap();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    format!("{:x}", digest.finalize())
}

fn write_test_manifest(database_path: &Path, schema_version: i64) {
    let manifest = database::BackupManifest {
        format_version: 2,
        application_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        database_file: database_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        database_sha256: sha256_file(database_path),
        schema_version,
        encryption_key_id: "maintenance-test".to_string(),
    };
    std::fs::write(
        manifest_path(database_path),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn sidecar_path(database: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = database.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_sidecars(database: &Path) {
    for suffix in ["-wal", "-shm"] {
        match std::fs::remove_file(sidecar_path(database, suffix)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("failed to remove test sidecar: {error}"),
        }
    }
}

fn private_file_mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn assert_no_restore_staging_files(directory: &Path) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy();
        assert!(!name.starts_with(".unionc-restore-"), "left {name} behind");
        assert!(
            !name.starts_with(".unionc-pre-restore-"),
            "left {name} behind"
        );
    }
}

fn unverified_recovery_files(directory: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .map(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with("unionc.pre-restore-unverified-") && name.ends_with(".db")
                })
                .unwrap_or(false)
        })
        .collect()
}

#[tokio::test]
async fn backup_restore_and_integrity_use_a_validated_atomic_snapshot() {
    let directory = unique_directory();
    std::fs::create_dir(&directory).unwrap();

    // This integration-test binary contains one test, so initializing the
    // process-global keyring here cannot race another test.
    unsafe {
        std::env::set_var(
            "UNIONC_SECRET_KEY",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        );
        std::env::set_var("UNIONC_SECRET_KEY_ID", "maintenance-test");
    }
    unionc::infra::secrets::init(unionc::config::RuntimeMode::Development)
        .expect("initialize test keyring");

    let live = directory.join("unionc.db");
    let backup = directory.join("snapshot.db");
    let mut settings = Settings::default();
    settings.database.url = live.display().to_string();

    let pool = database::connect(&settings).await.unwrap();
    database::initialize_schema(&pool).await.unwrap();
    query(
        "INSERT INTO audit_logs(action,target,detail) VALUES('test.snapshot','snapshot-value','before')",
    )
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let blocked_backup = directory.join("blocked-by-maintenance-lock.db");
    let maintenance_lock = database::acquire_maintenance_lock(&live).unwrap();
    let error = database::acquire_offline_maintenance_locks(&live)
        .err()
        .expect("offline maintenance must not bypass an active maintenance lock");
    assert!(
        error
            .to_string()
            .contains("another UnionC database maintenance command"),
        "offline rekey/restore must serialize with online maintenance: {error:#}"
    );
    let error = database::backup_database(&settings, &blocked_backup)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("another UnionC database maintenance command")
    );
    assert!(!blocked_backup.exists());
    drop(maintenance_lock);
    assert_eq!(
        std::fs::metadata(directory.join(".unionc-maintenance.lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    // The Server singleton uses a separate inode. Holding its lock models an
    // online Server and must not prevent an explicit online backup.
    let server_lock = database::acquire_database_lock(&live).unwrap();
    let manifest = database::backup_database(&settings, &backup).await.unwrap();
    drop(server_lock);
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.application_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest.encryption_key_id, "maintenance-test");
    assert!(backup.is_file());
    assert!(directory.join("snapshot.db.manifest.json").is_file());

    let old_version_backup = directory.join("old-version-snapshot.db");
    std::fs::copy(&backup, &old_version_backup).unwrap();
    let mut old_version_manifest = serde_json::to_value(&manifest).unwrap();
    old_version_manifest["application_version"] = serde_json::json!("0.3.1");
    std::fs::write(
        manifest_path(&old_version_backup),
        serde_json::to_vec_pretty(&old_version_manifest).unwrap(),
    )
    .unwrap();
    let mut old_version_target = Settings::default();
    old_version_target.database.url = directory
        .join("old-version-target.db")
        .display()
        .to_string();
    let error = database::restore_database(&old_version_target, &old_version_backup, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("application version 0.3.1"));

    let missing_version_backup = directory.join("missing-version-snapshot.db");
    std::fs::copy(&backup, &missing_version_backup).unwrap();
    let mut missing_version_manifest = serde_json::to_value(&manifest).unwrap();
    missing_version_manifest
        .as_object_mut()
        .unwrap()
        .remove("application_version");
    std::fs::write(
        manifest_path(&missing_version_backup),
        serde_json::to_vec_pretty(&missing_version_manifest).unwrap(),
    )
    .unwrap();
    let mut missing_version_target = Settings::default();
    missing_version_target.database.url = directory
        .join("missing-version-target.db")
        .display()
        .to_string();
    let error = database::restore_database(&missing_version_target, &missing_version_backup, false)
        .await
        .unwrap_err();
    let detail = format!("{error:#}");
    assert!(
        detail.contains("missing field `application_version`"),
        "{detail}"
    );

    let old_shape_backup = directory.join("old-shape-snapshot.db");
    std::fs::copy(&backup, &old_shape_backup).unwrap();
    let mut old_shape_manifest = serde_json::to_value(&manifest).unwrap();
    let old_shape_object = old_shape_manifest.as_object_mut().unwrap();
    old_shape_object.remove("schema_version");
    old_shape_object.insert("schema_versions".to_string(), serde_json::json!([1]));
    std::fs::write(
        manifest_path(&old_shape_backup),
        serde_json::to_vec_pretty(&old_shape_manifest).unwrap(),
    )
    .unwrap();
    let mut old_shape_target = Settings::default();
    old_shape_target.database.url = directory.join("old-shape-target.db").display().to_string();
    let error = database::restore_database(&old_shape_target, &old_shape_backup, false)
        .await
        .unwrap_err();
    let detail = format!("{error:#}");
    assert!(detail.contains("schema_versions"), "{detail}");

    let old_format_backup = directory.join("old-format-snapshot.db");
    std::fs::copy(&backup, &old_format_backup).unwrap();
    let mut old_format_manifest = serde_json::to_value(&manifest).unwrap();
    old_format_manifest["format_version"] = serde_json::json!(1);
    std::fs::write(
        manifest_path(&old_format_backup),
        serde_json::to_vec_pretty(&old_format_manifest).unwrap(),
    )
    .unwrap();
    let mut old_format_target = Settings::default();
    old_format_target.database.url = directory.join("old-format-target.db").display().to_string();
    let error = database::restore_database(&old_format_target, &old_format_backup, false)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported backup manifest format 1")
    );

    // Restore accepts only the exact schema supported by this build. A
    // correctly hashed artifact with an additional obsolete table is still a
    // non-current database and must be rejected without rewriting it.
    let noncurrent_backup = directory.join("noncurrent-snapshot.db");
    std::fs::copy(&backup, &noncurrent_backup).unwrap();
    let mut noncurrent_settings = Settings::default();
    noncurrent_settings.database.url = noncurrent_backup.display().to_string();
    let noncurrent_pool = database::connect(&noncurrent_settings).await.unwrap();
    query("CREATE TABLE obsolete_settings(key TEXT PRIMARY KEY) STRICT")
        .execute(&noncurrent_pool)
        .await
        .unwrap();
    query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(&noncurrent_pool)
        .await
        .unwrap();
    noncurrent_pool.close().await;
    remove_sidecars(&noncurrent_backup);
    write_test_manifest(&noncurrent_backup, 1);
    let noncurrent_target = directory.join("noncurrent-restored.db");
    let mut noncurrent_target_settings = Settings::default();
    noncurrent_target_settings.database.url = noncurrent_target.display().to_string();
    let error = database::restore_database(&noncurrent_target_settings, &noncurrent_backup, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("schema mismatch"));
    assert!(!noncurrent_target.exists());

    let collision = directory.join("existing-backup.db");
    std::fs::write(&collision, b"owned by another operation").unwrap();
    assert!(
        database::backup_database(&settings, &collision)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(&collision).unwrap(),
        b"owned by another operation"
    );

    let pool = database::connect(&settings).await.unwrap();
    query(
        "UPDATE audit_logs SET detail='after' WHERE action='test.snapshot' AND target='snapshot-value'",
    )
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert_eq!(
        marker_value(&live, "snapshot-value").await.as_deref(),
        Some("after")
    );

    let original_metadata = std::fs::metadata(&live).unwrap();
    let previous = database::restore_database(&settings, &backup, true)
        .await
        .unwrap()
        .expect("an existing database must be retained for recovery");
    let previous = match previous {
        database::RecoveryPoint::Validated { database } => database,
        database::RecoveryPoint::UnverifiedForensicCopy { .. } => {
            panic!("a healthy current database must produce a validated recovery point")
        }
    };
    assert!(previous.is_file());
    let previous_metadata = std::fs::metadata(&previous).unwrap();
    assert_ne!(
        (previous_metadata.dev(), previous_metadata.ino()),
        (original_metadata.dev(), original_metadata.ino())
    );
    let restored_metadata = std::fs::metadata(&live).unwrap();
    assert_ne!(
        (restored_metadata.dev(), restored_metadata.ino()),
        (original_metadata.dev(), original_metadata.ino())
    );
    let previous_manifest_path = manifest_path(&previous);
    assert!(previous_manifest_path.is_file());
    assert_eq!(private_file_mode(&previous), 0o600);
    assert_eq!(private_file_mode(&previous_manifest_path), 0o600);
    let previous_manifest: database::BackupManifest =
        serde_json::from_slice(&std::fs::read(&previous_manifest_path).unwrap()).unwrap();
    assert_eq!(
        previous_manifest.database_file,
        previous.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(previous_manifest.schema_version, 1);
    assert_eq!(previous_manifest.encryption_key_id, "maintenance-test");
    assert_eq!(previous_manifest.database_sha256.len(), 64);
    assert_eq!(
        marker_value(&live, "snapshot-value").await.as_deref(),
        Some("before")
    );
    assert_eq!(database::integrity_check(&settings).await.unwrap(), 1);
    assert_no_restore_staging_files(&directory);

    // The recovery point created by restore is itself a first-class backup:
    // feeding it back to the same CLI operation atomically restores the old
    // database and creates another complete recovery point for the displaced
    // database.
    let displaced = database::restore_database(&settings, &previous, true)
        .await
        .unwrap()
        .expect("the restored database must also be retained for recovery");
    let displaced = match displaced {
        database::RecoveryPoint::Validated { database } => database,
        database::RecoveryPoint::UnverifiedForensicCopy { .. } => {
            panic!("a healthy restored database must produce a validated recovery point")
        }
    };
    assert!(displaced.is_file());
    let displaced_metadata = std::fs::metadata(&displaced).unwrap();
    assert_ne!(
        (displaced_metadata.dev(), displaced_metadata.ino()),
        (restored_metadata.dev(), restored_metadata.ino())
    );
    let rolled_back_metadata = std::fs::metadata(&live).unwrap();
    assert_ne!(
        (rolled_back_metadata.dev(), rolled_back_metadata.ino()),
        (restored_metadata.dev(), restored_metadata.ino())
    );
    assert!(manifest_path(&displaced).is_file());
    assert_eq!(private_file_mode(&displaced), 0o600);
    assert_eq!(private_file_mode(&manifest_path(&displaced)), 0o600);
    assert_eq!(
        marker_value(&live, "snapshot-value").await.as_deref(),
        Some("after")
    );
    assert_eq!(database::integrity_check(&settings).await.unwrap(), 1);
    assert_no_restore_staging_files(&directory);

    let bad_metadata = directory.join("bad-metadata.db");
    let bad_metadata_settings = mutate_copy(
        &backup,
        &bad_metadata,
        "UPDATE schema_metadata SET checksum='tampered' WHERE schema_version=1",
    )
    .await;
    let error = database::integrity_check(&bad_metadata_settings)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("schema metadata"));

    let missing_table = directory.join("missing-table.db");
    let missing_table_settings =
        mutate_copy(&backup, &missing_table, "DROP TABLE audit_logs").await;
    let error = database::integrity_check(&missing_table_settings)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("schema mismatch"));

    let missing_index = directory.join("missing-index.db");
    let missing_index_settings = mutate_copy(
        &backup,
        &missing_index,
        "DROP INDEX idx_audit_logs_created_at",
    )
    .await;
    let error = database::integrity_check(&missing_index_settings)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("schema mismatch"));

    let invalid_host = directory.join("invalid-host.db");
    std::fs::copy(&backup, &invalid_host).unwrap();
    let mut invalid_host_settings = Settings::default();
    invalid_host_settings.database.url = invalid_host.display().to_string();
    let pool = database::connect(&invalid_host_settings).await.unwrap();
    query(
        "INSERT INTO external_hosts(kind,host_id,address,config,secret) \
         VALUES('sunshine','invalid-host','127.0.0.1','{\"web_port\":\"bad\"}',NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    let error = database::integrity_check(&invalid_host_settings)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid Sunshine host configuration")
    );

    // Build an offline current-database fixture whose latest committed value
    // exists in a non-empty WAL. Restore must preserve/checkpoint that family
    // and remove its sidecars before publishing the unrelated backup main.
    let wal_source = directory.join("wal-current-source.db");
    let mut wal_source_settings = Settings::default();
    wal_source_settings.database.url = wal_source.display().to_string();
    let wal_pool = database::connect(&wal_source_settings).await.unwrap();
    database::initialize_schema(&wal_pool).await.unwrap();
    let mut wal_connection = wal_pool.acquire().await.unwrap();
    query("PRAGMA wal_autocheckpoint=0")
        .execute(&mut *wal_connection)
        .await
        .unwrap();
    query(
        "INSERT INTO audit_logs(action,target,detail) VALUES('test.snapshot','snapshot-value','from-wal')",
    )
        .execute(&mut *wal_connection)
        .await
        .unwrap();
    let source_wal = sidecar_path(&wal_source, "-wal");
    assert!(std::fs::metadata(&source_wal).unwrap().len() > 0);

    remove_sidecars(&live);
    std::fs::copy(&wal_source, &live).unwrap();
    std::fs::copy(&source_wal, sidecar_path(&live, "-wal")).unwrap();
    let source_shm = sidecar_path(&wal_source, "-shm");
    if source_shm.exists() {
        std::fs::copy(&source_shm, sidecar_path(&live, "-shm")).unwrap();
    }
    drop(wal_connection);
    wal_pool.close().await;
    assert!(
        std::fs::metadata(sidecar_path(&live, "-wal"))
            .unwrap()
            .len()
            > 0
    );

    let wal_recovery = database::restore_database(&settings, &backup, true)
        .await
        .unwrap()
        .expect("the WAL-backed current database must be retained");
    let wal_recovery = match wal_recovery {
        database::RecoveryPoint::Validated { database } => database,
        database::RecoveryPoint::UnverifiedForensicCopy {
            validation_error, ..
        } => panic!("valid WAL family was not validated: {validation_error}"),
    };
    assert!(!sidecar_path(&live, "-wal").exists());
    assert!(!sidecar_path(&live, "-shm").exists());
    assert_eq!(
        marker_value(&live, "snapshot-value").await.as_deref(),
        Some("before")
    );

    let _ = database::restore_database(&settings, &wal_recovery, true)
        .await
        .unwrap();
    assert_eq!(
        marker_value(&live, "snapshot-value").await.as_deref(),
        Some("from-wal")
    );
    let _ = database::restore_database(&settings, &backup, true)
        .await
        .unwrap();
    assert_eq!(
        marker_value(&live, "snapshot-value").await.as_deref(),
        Some("before")
    );

    // A corrupt main with sidecars cannot be safely checkpointed. Replacement
    // is refused without renaming canonical, while an exact unverified family
    // remains available for forensic work.
    remove_sidecars(&live);
    let damaged_bytes = b"damaged current SQLite database";
    let damaged_wal = b"damaged SQLite WAL evidence";
    let damaged_shm = b"damaged SQLite SHM evidence";
    std::fs::write(&live, damaged_bytes).unwrap();
    std::fs::write(sidecar_path(&live, "-wal"), damaged_wal).unwrap();
    std::fs::write(sidecar_path(&live, "-shm"), damaged_shm).unwrap();
    let unverified_before_rejection = unverified_recovery_files(&directory);
    let canonical_metadata_before_unsafe_wal = std::fs::metadata(&live).unwrap();
    let error = database::restore_database(&settings, &backup, true)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("sidecars remain"));
    assert_eq!(std::fs::read(&live).unwrap(), damaged_bytes);
    assert_eq!(
        std::fs::read(sidecar_path(&live, "-wal")).unwrap(),
        damaged_wal
    );
    assert_eq!(
        std::fs::read(sidecar_path(&live, "-shm")).unwrap(),
        damaged_shm
    );
    let canonical_metadata_after_unsafe_wal = std::fs::metadata(&live).unwrap();
    assert_eq!(
        (
            canonical_metadata_before_unsafe_wal.dev(),
            canonical_metadata_before_unsafe_wal.ino()
        ),
        (
            canonical_metadata_after_unsafe_wal.dev(),
            canonical_metadata_after_unsafe_wal.ino()
        )
    );
    let rejected_raw = unverified_recovery_files(&directory)
        .into_iter()
        .find(|path| !unverified_before_rejection.contains(path))
        .expect("checkpoint refusal must retain the raw family first");
    assert!(
        error
            .to_string()
            .contains(&rejected_raw.display().to_string())
    );
    assert_eq!(std::fs::read(&rejected_raw).unwrap(), damaged_bytes);
    assert_eq!(
        std::fs::read(sidecar_path(&rejected_raw, "-wal")).unwrap(),
        damaged_wal
    );
    assert_eq!(
        std::fs::read(sidecar_path(&rejected_raw, "-shm")).unwrap(),
        damaged_shm
    );
    assert_eq!(private_file_mode(&rejected_raw), 0o600);
    assert_eq!(
        private_file_mode(&sidecar_path(&rejected_raw, "-wal")),
        0o600
    );
    assert_eq!(
        private_file_mode(&sidecar_path(&rejected_raw, "-shm")),
        0o600
    );
    assert!(!manifest_path(&rejected_raw).exists());

    // A corrupt current database must not prevent disaster recovery. Its raw
    // bytes are retained under an explicitly unverified name without a
    // manifest, while the validated input still replaces the canonical file.
    remove_sidecars(&live);
    let forensic = database::restore_database(&settings, &backup, true)
        .await
        .unwrap()
        .expect("the damaged current database must still be retained");
    let (forensic, validation_error) = match forensic {
        database::RecoveryPoint::UnverifiedForensicCopy {
            database,
            validation_error,
        } => (database, validation_error),
        database::RecoveryPoint::Validated { .. } => {
            panic!("a damaged current database must not be labelled as validated")
        }
    };
    assert!(
        forensic
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("unverified")
    );
    assert!(!validation_error.is_empty());
    assert_eq!(std::fs::read(&forensic).unwrap(), damaged_bytes);
    assert_eq!(private_file_mode(&forensic), 0o600);
    assert!(!manifest_path(&forensic).exists());
    assert_eq!(
        marker_value(&live, "snapshot-value").await.as_deref(),
        Some("before")
    );
    assert_eq!(database::integrity_check(&settings).await.unwrap(), 1);

    let before_forensic_rejection = std::fs::metadata(&live).unwrap();
    let error = database::restore_database(&settings, &forensic, true)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("backup manifest"));
    let after_forensic_rejection = std::fs::metadata(&live).unwrap();
    assert_eq!(
        (
            before_forensic_rejection.dev(),
            before_forensic_rejection.ino()
        ),
        (
            after_forensic_rejection.dev(),
            after_forensic_rejection.ino()
        )
    );
    assert_no_restore_staging_files(&directory);

    // A changed input no longer matches the manifest. The live database must
    // remain untouched because restore validates the copied staging file first.
    std::fs::write(&backup, b"not a sqlite database").unwrap();
    let before_failed_restore = std::fs::metadata(&live).unwrap();
    assert!(
        database::restore_database(&settings, &backup, true)
            .await
            .is_err()
    );
    let after_failed_restore = std::fs::metadata(&live).unwrap();
    assert_eq!(
        (after_failed_restore.dev(), after_failed_restore.ino()),
        (before_failed_restore.dev(), before_failed_restore.ino())
    );
    assert_eq!(
        marker_value(&live, "snapshot-value").await.as_deref(),
        Some("before")
    );
    assert_no_restore_staging_files(&directory);

    let lock_probe = directory.join("lock-probe");
    std::fs::create_dir(&lock_probe).unwrap();
    let victim = lock_probe.join("victim");
    std::fs::write(&victim, b"must stay unchanged").unwrap();
    std::os::unix::fs::symlink(&victim, lock_probe.join(".unionc-maintenance.lock")).unwrap();
    let error = database::acquire_maintenance_lock(&lock_probe.join("unionc.db"))
        .err()
        .expect("a symlink must never be accepted as a maintenance lock");
    assert!(error.to_string().contains("not a regular file"));
    assert_eq!(std::fs::read(&victim).unwrap(), b"must stay unchanged");

    std::fs::remove_dir_all(&directory).unwrap();
}
