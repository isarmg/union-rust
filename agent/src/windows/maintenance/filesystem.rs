fn maintenance_path_utf16_units(path: &Path) -> usize {
    path.as_os_str().encode_wide().count()
}

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
    let mut pending = Vec::new();
    let mut discovered = 0;
    let mut path_payload_bytes = 0;
    enqueue_bounded_tree_path(
        &mut pending,
        &mut discovered,
        root.to_path_buf(),
        maintenance_path_utf16_units(root),
        &mut path_payload_bytes,
        MAX_MAINTENANCE_TREE_NODES,
        MAX_MAINTENANCE_PATH_BYTES,
        "managed tree traversal",
    )?;
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
                let path_utf16_units = maintenance_path_utf16_units(&path);
                enqueue_bounded_tree_path(
                    &mut pending,
                    &mut discovered,
                    path,
                    path_utf16_units,
                    &mut path_payload_bytes,
                    MAX_MAINTENANCE_TREE_NODES,
                    MAX_MAINTENANCE_PATH_BYTES,
                    "managed tree traversal queue",
                )?;
            } else {
                record_bounded_tree_node(
                    &mut discovered,
                    MAX_MAINTENANCE_TREE_NODES,
                    "managed tree traversal",
                )?;
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
    let mut pending = Vec::new();
    let mut discovered = 0;
    let mut path_payload_bytes = 0;
    enqueue_bounded_tree_path(
        &mut pending,
        &mut discovered,
        root.to_path_buf(),
        maintenance_path_utf16_units(root),
        &mut path_payload_bytes,
        MAX_MAINTENANCE_TREE_NODES,
        MAX_MAINTENANCE_PATH_BYTES,
        "managed tree removal",
    )?;
    while let Some(directory) = pending.pop() {
        try_push_bounded_path(
            &mut directories,
            directory.clone(),
            maintenance_path_utf16_units(&directory),
            &mut path_payload_bytes,
            MAX_MAINTENANCE_TREE_NODES,
            MAX_MAINTENANCE_PATH_BYTES,
            "managed tree removal directory list",
        )?;
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
                "refusing to delete reparse point {}",
                path.display()
            );
            if metadata.is_dir() {
                let path_utf16_units = maintenance_path_utf16_units(&path);
                enqueue_bounded_tree_path(
                    &mut pending,
                    &mut discovered,
                    path,
                    path_utf16_units,
                    &mut path_payload_bytes,
                    MAX_MAINTENANCE_TREE_NODES,
                    MAX_MAINTENANCE_PATH_BYTES,
                    "managed tree removal queue",
                )?;
            } else {
                record_bounded_tree_node(
                    &mut discovered,
                    MAX_MAINTENANCE_TREE_NODES,
                    "managed tree removal",
                )?;
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

struct LocalSecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
        }
    }
}

fn create_system_admin_only_directory(path: &Path, label: &str) -> anyhow::Result<()> {
    let descriptor_text = protected_directory_security_descriptor();
    let wide_descriptor = wide_null(OsStr::new(&descriptor_text));
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide_descriptor.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .context("failed to build the protected directory security descriptor")?;
    let descriptor = LocalSecurityDescriptor(descriptor);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .context("SECURITY_ATTRIBUTES size does not fit in a DWORD")?,
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: false.into(),
    };
    let wide_path = wide_null(path.as_os_str());
    unsafe { CreateDirectoryW(PCWSTR(wide_path.as_ptr()), Some(&attributes)) }
        .with_context(|| format!("failed to create protected {label} {}", path.display()))?;

    if let Err(error) =
        validate_real_directory(path, label).and_then(|_| validate_managed_dacl(path, false))
    {
        let cleanup = remove_tree_no_reparse(path);
        return match cleanup {
            Ok(()) => Err(error.context(format!("failed to verify protected {label}"))),
            Err(cleanup_error) => Err(error.context(format!(
                "failed to verify protected {label}; cleanup also failed: {cleanup_error:#}"
            ))),
        };
    }
    Ok(())
}

fn create_protected_journal(path: &Path) -> anyhow::Result<()> {
    create_system_admin_only_directory(path, "new maintenance journal")
}

fn write_or_validate_state_marker(paths: &FixedPaths) -> anyhow::Result<()> {
    validate_real_directory(
        &paths.state_root,
        "state root required before marker update",
    )?;
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
