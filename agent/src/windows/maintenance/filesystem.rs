fn validate_optional_root(path: &Path, label: &str) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            validate_real_directory(path, label)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {label}")),
    }
}

fn validate_real_directory(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    ensure!(
        metadata.is_dir(),
        "{label} is not a directory: {}",
        path.display()
    );
    ensure!(
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
        "{label} is a reparse point: {}",
        path.display()
    );
    Ok(())
}

fn validate_regular_single_link(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    ensure!(metadata.is_file(), "{label} is not a regular file");
    ensure!(
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
        "{label} is a reparse point"
    );
    ensure!(
        file_link_count(path)? == 1,
        "{label} has multiple hard links"
    );
    Ok(())
}

fn validate_tree(root: &Path) -> anyhow::Result<()> {
    validate_real_directory(root, "managed directory")?;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to enumerate {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
                "managed tree contains a reparse point: {}",
                path.display()
            );
            if metadata.is_dir() {
                pending.push(path);
            } else {
                ensure!(
                    metadata.is_file(),
                    "managed tree contains a special file: {}",
                    path.display()
                );
                ensure!(
                    file_link_count(&path)? == 1,
                    "managed tree contains a multiply-linked file: {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

fn ensure_absent(path: &Path, label: &str) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => bail!("{label} already exists at {}", path.display()),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {label}")),
    }
}

fn remove_tree_no_reparse(root: &Path) -> anyhow::Result<()> {
    validate_tree(root)?;
    let mut directories = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        directories.push(directory.clone());
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
                "refusing to delete reparse point {}",
                path.display()
            );
            if metadata.is_dir() {
                pending.push(path);
            } else {
                fs::remove_file(&path)?;
            }
        }
    }
    for directory in directories.into_iter().rev() {
        fs::remove_dir(&directory)?;
    }
    Ok(())
}

fn write_new_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let result = file.write_all(bytes).and_then(|_| file.sync_all());
    drop(file);
    if let Err(error) = result {
        let _ = fs::remove_file(path);
        return Err(error).with_context(|| format!("failed to write {}", path.display()));
    }
    Ok(())
}

fn replace_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let replacement = path.with_extension("replacement");
    ensure_absent(&replacement, "stale private replacement file")?;
    write_new_private(&replacement, bytes)?;
    let source = wide_null(replacement.as_os_str());
    let destination = wide_null(path.as_os_str());
    let replaced = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        let error = std::io::Error::last_os_error();
        let _ = fs::remove_file(&replacement);
        return Err(error).with_context(|| format!("failed to replace {}", path.display()));
    }
    Ok(())
}

fn read_snapshot(paths: &FixedPaths) -> anyhow::Result<InstallSnapshot> {
    validate_real_directory(&paths.journal_root, "install journal")?;
    validate_tree(&paths.journal_root)?;
    validate_managed_dacl(&paths.journal_root, false)?;
    let snapshot: InstallSnapshot =
        serde_json::from_slice(&fs::read(paths.journal_root.join(SNAPSHOT_FILE))?)?;
    ensure!(
        snapshot.format == SNAPSHOT_FORMAT
            && snapshot.application_version == env!("CARGO_PKG_VERSION"),
        "unsupported maintenance journal version"
    );
    Ok(snapshot)
}

fn read_uninstall_snapshot(paths: &FixedPaths) -> anyhow::Result<UninstallSnapshot> {
    validate_real_directory(&paths.uninstall_journal_root, "uninstall journal")?;
    validate_tree(&paths.uninstall_journal_root)?;
    validate_managed_dacl(&paths.uninstall_journal_root, false)?;
    let snapshot: UninstallSnapshot =
        serde_json::from_slice(&fs::read(paths.uninstall_journal_root.join(SNAPSHOT_FILE))?)?;
    ensure!(
        snapshot.format == SNAPSHOT_FORMAT
            && snapshot.application_version == env!("CARGO_PKG_VERSION"),
        "unsupported uninstall journal version"
    );
    purge_started(paths)?;
    Ok(snapshot)
}

fn purge_started(paths: &FixedPaths) -> anyhow::Result<bool> {
    let marker = paths.uninstall_journal_root.join(PURGE_STARTED_FILE);
    match fs::read(&marker) {
        Ok(content) => {
            ensure!(
                content == PURGE_STARTED_CONTENT.as_bytes(),
                "purge transaction marker does not belong to the current package"
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("failed to read purge transaction marker"),
    }
}

fn create_protected_journal(path: &Path) -> anyhow::Result<()> {
    fs::create_dir(path).with_context(|| format!("failed to create {}", path.display()))?;
    validate_real_directory(path, "new maintenance journal")?;
    if let Err(error) = secure_system_admin_only(path, false) {
        let _ = remove_tree_no_reparse(path);
        return Err(error.context("failed to secure maintenance journal"));
    }
    validate_managed_dacl(path, false)
}

fn write_or_validate_state_marker(paths: &FixedPaths) -> anyhow::Result<()> {
    fs::create_dir_all(&paths.state_root)?;
    validate_real_directory(&paths.state_root, "state root")?;
    let marker = paths.state_root.join(STATE_MARKER);
    if marker.exists() {
        validate_marker_file(&marker)?;
    } else {
        write_new_private(&marker, STATE_MARKER_CONTENT.as_bytes())?;
        let service_sid = service_sid_string()?;
        if let Err(error) = apply_exact_acl(&marker, Some(&service_sid), false)
            .and_then(|_| validate_managed_dacl(&marker, true))
        {
            let _ = fs::remove_file(&marker);
            return Err(error.context("failed to secure the new state marker"));
        }
    }
    Ok(())
}

fn validate_state_marker(paths: &FixedPaths, require_service_access: bool) -> anyhow::Result<()> {
    let marker = paths.state_root.join(STATE_MARKER);
    validate_marker_file(&marker)?;
    validate_managed_dacl(&paths.state_root, require_service_access)?;
    validate_managed_dacl(&marker, require_service_access)
}

fn validate_marker_file(marker: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(marker)
        .with_context(|| format!("missing trusted state marker {}", marker.display()))?;
    ensure!(metadata.is_file(), "state marker is not a regular file");
    ensure!(
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
        "state marker is a reparse point"
    );
    ensure!(
        file_link_count(marker)? == 1,
        "state marker has multiple hard links"
    );
    ensure!(
        fs::read(marker)? == STATE_MARKER_CONTENT.as_bytes(),
        "state marker content is invalid"
    );
    Ok(())
}
