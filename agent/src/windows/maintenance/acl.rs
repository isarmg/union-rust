fn secure_system_admin_only(path: &Path, recursive: bool) -> anyhow::Result<()> {
    apply_exact_acl(path, None, recursive)
}

fn secure_state_for_service(path: &Path) -> anyhow::Result<()> {
    let service_sid = service_sid_string()?;
    apply_exact_acl(path, Some(&service_sid), true)
}

fn secure_program_for_service(path: &Path) -> anyhow::Result<()> {
    let service_sid = service_sid_string()?;
    // The service keeps its isolated read/execute grant while BUILTIN\Users
    // receives the same non-writing rights needed to start the per-user tray
    // companion from HKLM\...\Run. Mutable state remains under the separate
    // `apply_exact_acl` template and deliberately never receives this ACE.
    let descriptor = program_security_descriptor(&service_sid);
    apply_descriptor_recursively(path, &descriptor)
}

fn validate_program_tree(path: &Path) -> anyhow::Result<()> {
    validate_tree(path)?;
    let mut pending = vec![path.to_path_buf()];
    while let Some(target) = pending.pop() {
        validate_program_dacl(&target)?;
        if fs::symlink_metadata(&target)?.is_dir() {
            for child in fs::read_dir(&target)? {
                pending.push(child?.path());
            }
        }
    }
    Ok(())
}

fn validate_program_dacl(path: &Path) -> anyhow::Result<()> {
    let sddl = security_descriptor_sddl(path)?;
    parse_program_dacl(&sddl, &service_sid_string()?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProgramAclRestore {
    /// An install rollback has already restored the exact protected ACL
    /// snapshot from the current installation transaction.
    PreserveSnapshot,
    /// Uninstall rollback restores the currently installed tray-aware
    /// product, so rebuild and verify the current exact template.
    SecureCurrent,
}

fn apply_descriptor_recursively(path: &Path, descriptor: &str) -> anyhow::Result<()> {
    validate_tree(path)?;
    let mut targets = vec![path.to_path_buf()];
    let mut index = 0;
    while index < targets.len() {
        let current = targets[index].clone();
        if fs::symlink_metadata(&current)?.is_dir() {
            for entry in fs::read_dir(&current)? {
                targets.push(entry?.path());
            }
        }
        index += 1;
    }
    for target in targets {
        set_managed_security_descriptor(&target, descriptor)?;
    }
    Ok(())
}

fn apply_exact_acl(path: &Path, service_sid: Option<&str>, recursive: bool) -> anyhow::Result<()> {
    let descriptor = managed_state_security_descriptor(service_sid);
    let mut targets = vec![path.to_path_buf()];
    if recursive {
        validate_tree(path)?;
        let mut index = 0;
        while index < targets.len() {
            let current = targets[index].clone();
            if fs::symlink_metadata(&current)?.is_dir() {
                for entry in fs::read_dir(&current)? {
                    targets.push(entry?.path());
                }
            }
            index += 1;
        }
    }
    for target in targets {
        set_managed_security_descriptor(&target, &descriptor)?;
    }
    Ok(())
}

fn save_acl(root: &Path, destination: &Path) -> anyhow::Result<()> {
    validate_tree(root)?;
    let mut pending = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(target) = pending.pop() {
        let metadata = fs::symlink_metadata(&target)?;
        if metadata.is_dir() {
            for child in fs::read_dir(&target)? {
                pending.push(child?.path());
            }
        }
        let relative = target
            .strip_prefix(root)
            .with_context(|| format!("{} escaped the fixed managed root", target.display()))?;
        entries.push(AclSnapshotEntry {
            relative_path_utf16: relative.as_os_str().encode_wide().collect(),
            is_directory: metadata.is_dir(),
            sddl: security_descriptor_sddl(&target)?,
        });
    }
    entries.sort_by(|left, right| left.relative_path_utf16.cmp(&right.relative_path_utf16));
    let snapshot = AclSnapshot {
        format: SNAPSHOT_FORMAT,
        application_version: env!("CARGO_PKG_VERSION").to_owned(),
        entries,
    };
    write_new_private(destination, &serde_json::to_vec(&snapshot)?)
}

fn restore_acl(root: &Path, source: &Path) -> anyhow::Result<()> {
    validate_tree(root)?;
    let snapshot: AclSnapshot = serde_json::from_slice(&fs::read(source)?)?;
    ensure!(
        snapshot.format == SNAPSHOT_FORMAT
            && snapshot.application_version == env!("CARGO_PKG_VERSION"),
        "unsupported ACL snapshot version"
    );
    ensure!(!snapshot.entries.is_empty(), "ACL snapshot is empty");
    let mut entries = snapshot.entries;
    let mut unique_paths = std::collections::BTreeSet::new();
    for entry in &entries {
        ensure!(
            unique_paths.insert(entry.relative_path_utf16.clone()),
            "ACL snapshot contains a duplicate path"
        );
    }
    ensure!(
        unique_paths.contains(&Vec::new()),
        "ACL snapshot does not contain its managed root"
    );
    entries.sort_by_key(|entry| {
        PathBuf::from(OsString::from_wide(&entry.relative_path_utf16))
            .components()
            .count()
    });
    for entry in entries {
        let relative = PathBuf::from(OsString::from_wide(&entry.relative_path_utf16));
        ensure!(
            relative
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
            "ACL snapshot contains a non-relative managed path"
        );
        let target = if relative.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(&relative)
        };
        let metadata = fs::symlink_metadata(&target)
            .with_context(|| format!("ACL snapshot target disappeared: {}", target.display()))?;
        ensure!(
            metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
            "ACL snapshot target became a reparse point: {}",
            target.display()
        );
        ensure!(
            metadata.is_dir() == entry.is_directory,
            "ACL snapshot target changed type: {}",
            target.display()
        );
        if metadata.is_file() {
            ensure!(
                file_link_count(&target)? == 1,
                "ACL snapshot target became multiply linked: {}",
                target.display()
            );
        }
        restore_security_descriptor(&target, &entry.sddl)?;
        ensure!(
            security_descriptor_sddl(&target)? == entry.sddl,
            "restored owner/DACL did not verify for {}",
            target.display()
        );
    }
    Ok(())
}

fn validate_managed_dacl(path: &Path, require_service_access: bool) -> anyhow::Result<()> {
    let sddl = security_descriptor_sddl(path)?;
    ensure!(
        sddl.starts_with("O:SY"),
        "managed state owner is not SYSTEM: {sddl}"
    );
    let dacl = sddl
        .split("D:")
        .nth(1)
        .context("security descriptor has no DACL")?;
    let (control, _) = dacl
        .split_once('(')
        .context("managed state DACL contains no ACEs")?;
    ensure!(
        control == "P",
        "managed state DACL control flags are not exactly protected-only: {sddl}"
    );
    let service_sid = service_sid_string()?;
    let mut system = false;
    let mut admins = false;
    let mut owner_rights = false;
    let mut service = false;
    for ace in dacl.split('(').skip(1) {
        let ace = ace.split(')').next().context("malformed DACL ACE")?;
        let fields = ace.split(';').collect::<Vec<_>>();
        ensure!(
            fields.len() == 6 && fields[0] == "A",
            "unexpected DACL ACE: ({ace})"
        );
        ensure!(
            matches!(fields[1], "OICI" | "CIOI"),
            "managed state ACE flags are not exactly object/container inherit: ({ace})"
        );
        ensure!(
            fields[3].is_empty() && fields[4].is_empty(),
            "managed state contains an object-specific ACE: ({ace})"
        );
        match fields[5] {
            "SY" | "S-1-5-18" => {
                ensure!(!system, "managed state contains duplicate SYSTEM ACEs");
                ensure!(
                    fields[2] == "FA",
                    "SYSTEM does not have exactly full access"
                );
                system = true;
            }
            "BA" | "S-1-5-32-544" => {
                ensure!(
                    !admins,
                    "managed state contains duplicate Administrators ACEs"
                );
                ensure!(
                    fields[2] == "FA",
                    "Administrators do not have exactly full access"
                );
                admins = true;
            }
            "OW" | "S-1-3-4" => {
                ensure!(
                    !owner_rights,
                    "managed state contains duplicate OWNER RIGHTS ACEs"
                );
                ensure!(
                    fields[2] == "RC",
                    "OWNER RIGHTS does not have ReadPermissions only"
                );
                owner_rights = true;
            }
            trustee if service_sid == trustee => {
                ensure!(
                    !service,
                    "managed state contains duplicate service SID ACEs"
                );
                ensure!(
                    matches!(fields[2], "0x1301bf" | "0x001301bf"),
                    "service SID does not have exactly Modify access"
                );
                service = true;
            }
            trustee => bail!("unexpected state DACL trustee {trustee}"),
        }
    }
    ensure!(
        system && admins && owner_rights,
        "managed state DACL is incomplete"
    );
    ensure!(
        !require_service_access || service,
        "managed state DACL lacks the service SID"
    );
    ensure!(
        require_service_access || !service,
        "preserved state still grants the service SID"
    );
    Ok(())
}

fn security_descriptor_sddl(path: &Path) -> anyhow::Result<String> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
                SDDL_REVISION_1, SE_FILE_OBJECT,
            },
            DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        },
    };
    let wide = wide_null(path.as_os_str());
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let result = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    ensure!(result == 0, "GetNamedSecurityInfoW failed with {result}");
    let mut text = ptr::null_mut();
    let mut length = 0;
    let converted = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut text,
            &mut length,
        )
    };
    unsafe { LocalFree(descriptor.cast()) };
    ensure!(
        converted != 0,
        "failed to convert state security descriptor to SDDL"
    );
    let utf16 = unsafe { std::slice::from_raw_parts(text, length as usize) };
    let content_length = utf16
        .iter()
        .position(|code_unit| *code_unit == 0)
        .unwrap_or(utf16.len());
    let result = String::from_utf16(&utf16[..content_length])
        .context("state security descriptor is not valid UTF-16");
    unsafe { LocalFree(text.cast()) };
    result
}

fn set_managed_security_descriptor(path: &Path, sddl: &str) -> anyhow::Result<()> {
    set_security_descriptor(path, sddl, true)
}

fn restore_security_descriptor(path: &Path, sddl: &str) -> anyhow::Result<()> {
    let dacl = sddl
        .split_once("D:")
        .map(|(_, value)| value)
        .context("saved security descriptor has no DACL")?;
    let control = dacl.split_once('(').map(|(value, _)| value).unwrap_or(dacl);
    set_security_descriptor(path, sddl, control.contains('P'))
}

fn set_security_descriptor(path: &Path, sddl: &str, protected: bool) -> anyhow::Result<()> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetFileSecurityW,
            UNPROTECTED_DACL_SECURITY_INFORMATION,
        },
    };
    let wide_sddl = wide_null(OsStr::new(sddl));
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide_sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    ensure!(
        converted != 0,
        "failed to build the exact Agent security descriptor"
    );
    let wide_path = wide_null(path.as_os_str());
    let applied = unsafe {
        SetFileSecurityW(
            wide_path.as_ptr(),
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | if protected {
                    PROTECTED_DACL_SECURITY_INFORMATION
                } else {
                    UNPROTECTED_DACL_SECURITY_INFORMATION
                },
            descriptor,
        )
    };
    unsafe { LocalFree(descriptor.cast()) };
    ensure!(
        applied != 0,
        "failed to apply the exact security descriptor to {}",
        path.display()
    );
    Ok(())
}
