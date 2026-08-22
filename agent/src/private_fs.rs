use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use uuid::Uuid;

use crate::atomic_file;

const ATOMIC_TEMPORARY_PREFIX: &str = ".private-";
const ATOMIC_TEMPORARY_SUFFIX: &str = ".tmp";

/// Ownership to apply to a same-directory temporary file before publication.
#[derive(Debug, Clone, Copy)]
pub(crate) enum OwnerPolicy<'a> {
    /// Adopt the uid/gid of a private parent directory. On Windows the MSI ACL
    /// inheritance is authoritative, so this is intentionally a no-op.
    Parent(&'a Path),
    /// Preserve an existing target's uid/gid and mode when replacing it. This
    /// keeps package-created service-readable configuration readable after an
    /// administrator runs `pair` as root.
    PreserveTarget,
}

/// Create a directory and enforce the Agent's private-state mode on Unix.
pub(crate) fn ensure_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Publish private bytes atomically after flushing both the file and, where
/// supported, its parent directory. Every caller gets the same 0600 creation,
/// ownership inheritance, cleanup, replacement and durability semantics.
pub(crate) fn write_atomic(target: &Path, bytes: &[u8], owner: OwnerPolicy<'_>) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        "{ATOMIC_TEMPORARY_PREFIX}{}{ATOMIC_TEMPORARY_SUFFIX}",
        Uuid::new_v4()
    ));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        // A cleanup pass may run in another Agent process. Holding an advisory
        // lock lets it distinguish this live write from a file abandoned by a
        // process that died before the atomic rename.
        file.lock()?;
        match owner {
            OwnerPolicy::Parent(directory) => adopt_parent_owner(&file, directory)?,
            OwnerPolicy::PreserveTarget => preserve_target_metadata(&file, target)?,
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        atomic_file::replace(&temporary, target)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Remove temporary atomic-write files whose writer no longer owns the file
/// lock. A crash cannot execute `write_atomic`'s normal error cleanup, so a
/// bounded directory such as the report spool must reclaim those files later.
pub(crate) fn cleanup_atomic_temporaries(directory: &Path) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || !is_atomic_temporary(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        let file = match fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        match file.try_lock() {
            Ok(()) => match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            },
            Err(fs::TryLockError::WouldBlock) => {}
            Err(fs::TryLockError::Error(error)) => return Err(error),
        }
    }
    Ok(())
}

fn is_atomic_temporary(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(uuid) = name
        .strip_prefix(ATOMIC_TEMPORARY_PREFIX)
        .and_then(|name| name.strip_suffix(ATOMIC_TEMPORARY_SUFFIX))
    else {
        return false;
    };
    Uuid::parse_str(uuid).is_ok_and(|parsed| parsed.to_string() == uuid)
}

#[cfg(unix)]
pub(crate) fn adopt_parent_owner(file: &fs::File, parent: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(parent)?;
    set_owner(file, metadata.uid(), metadata.gid())
}

#[cfg(not(unix))]
pub(crate) fn adopt_parent_owner(_file: &fs::File, _parent: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn preserve_target_metadata(file: &fs::File, target: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = match fs::metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    set_owner(file, metadata.uid(), metadata.gid())?;
    file.set_permissions(fs::Permissions::from_mode(metadata.mode() & 0o777))
}

#[cfg(not(unix))]
fn preserve_target_metadata(_file: &fs::File, _target: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_owner(file: &fs::File, uid: u32, gid: u32) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: the descriptor belongs to this process; uid/gid came from a
    // filesystem object selected by the caller. The return value is checked.
    let result = unsafe { libc::fchown(file.as_raw_fd(), uid, gid) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("unionc-private-fs-{}", Uuid::new_v4()))
    }

    #[test]
    fn atomic_write_replaces_complete_content() {
        let directory = temp_dir();
        ensure_private_directory(&directory).unwrap();
        let target = directory.join("state");
        write_atomic(&target, b"old", OwnerPolicy::Parent(&directory)).unwrap();
        write_atomic(&target, b"new", OwnerPolicy::Parent(&directory)).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(fs::read_dir(&directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cleanup_only_removes_abandoned_atomic_writes() {
        let directory = temp_dir();
        ensure_private_directory(&directory).unwrap();
        let temporary = directory.join(format!(
            "{ATOMIC_TEMPORARY_PREFIX}{}{ATOMIC_TEMPORARY_SUFFIX}",
            Uuid::new_v4()
        ));
        let mut writer = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&temporary)
            .unwrap();
        writer.lock().unwrap();
        writer.write_all(b"in progress").unwrap();
        fs::write(directory.join(".private-not-a-uuid.tmp"), b"unrelated").unwrap();

        cleanup_atomic_temporaries(&directory).unwrap();
        assert!(
            temporary.exists(),
            "a live atomic write must not be removed"
        );

        drop(writer);
        cleanup_atomic_temporaries(&directory).unwrap();
        assert!(
            !temporary.exists(),
            "an abandoned atomic write must be removed"
        );
        assert!(directory.join(".private-not-a-uuid.tmp").exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_modes_and_existing_mode_are_preserved() {
        use std::os::unix::fs::PermissionsExt;

        let directory = temp_dir();
        ensure_private_directory(&directory).unwrap();
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let target = directory.join("config");
        write_atomic(&target, b"first", OwnerPolicy::Parent(&directory)).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        write_atomic(&target, b"second", OwnerPolicy::PreserveTarget).unwrap();
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
