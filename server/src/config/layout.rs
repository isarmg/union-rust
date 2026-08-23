//! 数据目录的安全认领与创建。
//!
//! `UNIONC_DATA_DIR` 可能来自服务环境，也可能来自一次性维护命令。它不是因为
//! “是绝对路径”就天然属于 UnionC：对未经验证的路径直接 `chmod`，以 root
//! 运行时甚至可能改掉 `/`、`/tmp` 或其他应用目录的权限。本模块因此只沿逐级
//! `O_NOFOLLOW` 打开的目录描述符工作，并把创建权限集中在明确允许 bootstrap
//! 的入口。

use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail};
use rustix::{
    fd::OwnedFd,
    fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat},
};

const DATA_DIRECTORY_MODE: Mode = Mode::RWXU;
const MARKER_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const DATA_DIRECTORY_MARKER: &str = ".unionc-data-directory";
const DATA_DIRECTORY_MARKER_CONTENT: &[u8] = b"unionc-data-directory-v1\n";
const MAX_MARKER_BYTES: u64 = 128;

/// 缺失数据目录的处理意图。枚举值保留调用来源，避免以后把 restore 的例外
/// 无意扩散到普通维护命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayoutIntent {
    ExistingOnly,
    Bootstrap,
    Restore,
}

impl LayoutIntent {
    const fn may_create(self) -> bool {
        matches!(self, Self::Bootstrap | Self::Restore)
    }

    const fn label(self) -> &'static str {
        match self {
            Self::ExistingOnly => "existing-only operation",
            Self::Bootstrap => "bootstrap",
            Self::Restore => "restore",
        }
    }
}

struct OpenedDirectory {
    fd: OwnedFd,
    created_leaf: bool,
}

pub(crate) fn ensure_layout(intent: LayoutIntent) -> anyhow::Result<()> {
    ensure_layout_at(crate::infra::paths::data_dir(), intent)
}

fn ensure_layout_at(path: &Path, intent: LayoutIntent) -> anyhow::Result<()> {
    ensure_layout_at_with_hook(path, intent, || {})
}

fn ensure_layout_at_with_hook(
    path: &Path,
    intent: LayoutIntent,
    before_final_reopen: impl FnOnce(),
) -> anyhow::Result<()> {
    let normalized = crate::infra::paths::normalize_absolute(path.to_path_buf())?;
    if is_reserved_data_root(&normalized) {
        bail!(
            "refusing to use reserved system directory {} as UNIONC_DATA_DIR",
            normalized.display()
        );
    }

    let opened = open_directory_tree(&normalized, intent.may_create(), intent.label())?;
    let initial = rustix::fs::fstat(&opened.fd)
        .with_context(|| format!("failed to inspect data directory {}", normalized.display()))?;
    validate_data_directory_metadata(&normalized, &initial)?;

    match validate_marker(&opened.fd, &initial)? {
        true => {}
        false if opened.created_leaf => create_marker(&opened.fd, &initial)?,
        false if directory_is_empty(&opened.fd)? && intent.may_create() => {
            // An existing empty directory is claimable only when its owner has
            // already made it exactly 0700. We deliberately do not chmod it:
            // an empty `/srv`, mount root, or shared workspace is still not
            // automatically UnionC's property.
            create_marker(&opened.fd, &initial)?;
        }
        false if directory_is_empty(&opened.fd)? => {
            bail!(
                "{} requires an already initialized UnionC data directory at {}",
                intent.label(),
                normalized.display()
            );
        }
        false => {
            bail!(
                "refusing to claim non-empty directory {} without a valid UnionC data marker",
                normalized.display()
            );
        }
    }

    // Marker publication and the directory entry itself are part of startup's
    // durable precondition. Validate the marker once more before any caller can
    // create a key, config, lock, or database.
    rustix::fs::fsync(&opened.fd).with_context(|| {
        format!(
            "failed to make data-directory metadata durable at {}",
            normalized.display()
        )
    })?;
    if !validate_marker(&opened.fd, &initial)? {
        bail!("UnionC data-directory marker disappeared during validation");
    }

    before_final_reopen();

    // Re-resolve from `/` after all mutations. If an ancestor or leaf was
    // replaced while the held fd was being validated, the replacement has
    // never been chmodded and startup fails on the identity mismatch.
    let reopened = open_directory_tree(&normalized, false, "final validation")?;
    let current = rustix::fs::fstat(&reopened.fd)?;
    if !same_directory_identity(&initial, &current) {
        bail!(
            "data directory path changed during validation: {}",
            normalized.display()
        );
    }
    validate_data_directory_metadata(&normalized, &current)?;
    if !validate_marker(&reopened.fd, &current)? {
        bail!("UnionC data-directory marker disappeared during final validation");
    }
    Ok(())
}

fn open_directory_tree(
    path: &Path,
    may_create: bool,
    operation: &str,
) -> anyhow::Result<OpenedDirectory> {
    let components = normal_components(path)?;
    let mut current = rustix::fs::open("/", directory_open_flags(), Mode::empty())
        .context("failed to open filesystem root while resolving UNIONC_DATA_DIR")?;
    let mut resolved = PathBuf::from("/");
    let mut created_leaf = false;

    for (index, component) in components.iter().enumerate() {
        validate_search_directory(&resolved, &rustix::fs::fstat(&current)?)?;
        let is_leaf = index + 1 == components.len();
        let next = match rustix::fs::openat(
            &current,
            component,
            directory_open_flags(),
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) if may_create => {
                rustix::fs::mkdirat(&current, component, DATA_DIRECTORY_MODE).with_context(
                    || {
                        format!(
                            "failed to create dedicated data-directory component {}",
                            resolved.join(component).display()
                        )
                    },
                )?;
                let fd =
                    rustix::fs::openat(&current, component, directory_open_flags(), Mode::empty())
                        .with_context(|| {
                            format!(
                                "failed to open newly created data-directory component {}",
                                resolved.join(component).display()
                            )
                        })?;
                // mkdir mode is filtered through umask. Only this inode, proven
                // to have been created by the current call, may be normalized.
                rustix::fs::fchmod(&fd, DATA_DIRECTORY_MODE)?;
                rustix::fs::fsync(&fd)?;
                rustix::fs::fsync(&current)?;
                created_leaf = is_leaf;
                fd
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)).with_context(|| {
                    format!(
                        "{operation} could not safely open data-directory component {}; every component must exist as a real directory and must not be a symbolic link",
                        resolved.join(component).display()
                    )
                });
            }
        };
        resolved.push(component);
        current = next;
    }

    Ok(OpenedDirectory {
        fd: current,
        created_leaf,
    })
}

fn normal_components(path: &Path) -> anyhow::Result<Vec<OsString>> {
    if !path.is_absolute() {
        bail!("UNIONC_DATA_DIR must resolve to an absolute path");
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(value) => components.push(value.to_os_string()),
            Component::ParentDir => bail!("UNIONC_DATA_DIR must not contain '..'"),
            Component::Prefix(_) => bail!("unsupported UNIONC_DATA_DIR path prefix"),
        }
    }
    if components.is_empty() {
        bail!("filesystem root cannot be used as UNIONC_DATA_DIR");
    }
    Ok(components)
}

fn directory_open_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn validate_search_directory(path: &Path, stat: &Stat) -> anyhow::Result<()> {
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        bail!("{} is not a directory", path.display());
    }
    let mode = Mode::from_raw_mode(stat.st_mode);
    if mode.intersects(Mode::WGRP | Mode::WOTH) && !mode.contains(Mode::SVTX) {
        bail!(
            "data-directory ancestor {} is writable by another user without the sticky bit",
            path.display()
        );
    }
    Ok(())
}

fn validate_data_directory_metadata(path: &Path, stat: &Stat) -> anyhow::Result<()> {
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        bail!("UNIONC_DATA_DIR is not a directory: {}", path.display());
    }
    let expected_uid = rustix::process::geteuid().as_raw();
    if stat.st_uid != expected_uid {
        bail!(
            "UNIONC_DATA_DIR {} is owned by uid {}, but the effective service uid is {}; run UnionC as the directory owner",
            path.display(),
            stat.st_uid,
            expected_uid
        );
    }
    let mode = Mode::from_raw_mode(stat.st_mode);
    if mode != DATA_DIRECTORY_MODE {
        bail!(
            "UNIONC_DATA_DIR {} must already have exact permissions 0700 (found {:04o}); refusing to chmod an untrusted existing path",
            path.display(),
            mode.as_raw_mode()
        );
    }
    Ok(())
}

fn directory_is_empty(directory: &OwnedFd) -> anyhow::Result<bool> {
    let entries = Dir::read_from(directory)?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            return Ok(false);
        }
    }
    Ok(true)
}

fn validate_marker(directory: &OwnedFd, directory_stat: &Stat) -> anyhow::Result<bool> {
    let before =
        match rustix::fs::statat(directory, DATA_DIRECTORY_MARKER, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
    validate_private_regular_file(
        DATA_DIRECTORY_MARKER,
        &before,
        directory_stat,
        MARKER_FILE_MODE,
    )?;
    if before.st_size < 0 || before.st_size as u64 > MAX_MARKER_BYTES {
        bail!("UnionC data-directory marker is unexpectedly large");
    }

    let marker = rustix::fs::openat(
        directory,
        DATA_DIRECTORY_MARKER,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .context("failed to open UnionC data-directory marker")?;
    let opened = rustix::fs::fstat(&marker)?;
    validate_private_regular_file(
        DATA_DIRECTORY_MARKER,
        &opened,
        directory_stat,
        MARKER_FILE_MODE,
    )?;
    if !same_file_identity(&before, &opened) {
        bail!("UnionC data-directory marker changed while opening it");
    }

    let mut content = Vec::new();
    File::from(marker)
        .take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut content)?;
    if content != DATA_DIRECTORY_MARKER_CONTENT {
        bail!("UnionC data-directory marker has invalid content");
    }

    let after = rustix::fs::statat(directory, DATA_DIRECTORY_MARKER, AtFlags::SYMLINK_NOFOLLOW)?;
    if !same_file_identity(&opened, &after) {
        bail!("UnionC data-directory marker changed while reading it");
    }
    Ok(true)
}

fn create_marker(directory: &OwnedFd, directory_stat: &Stat) -> anyhow::Result<()> {
    let marker = rustix::fs::openat(
        directory,
        DATA_DIRECTORY_MARKER,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        MARKER_FILE_MODE,
    )
    .context("failed to create UnionC data-directory ownership marker")?;
    rustix::fs::fchmod(&marker, MARKER_FILE_MODE)?;
    let mut marker = File::from(marker);
    marker.write_all(DATA_DIRECTORY_MARKER_CONTENT)?;
    marker.sync_all()?;
    drop(marker);
    rustix::fs::fsync(directory)?;
    if !validate_marker(directory, directory_stat)? {
        bail!("failed to publish UnionC data-directory marker");
    }
    Ok(())
}

fn validate_private_regular_file(
    name: &str,
    stat: &Stat,
    directory_stat: &Stat,
    expected_mode: Mode,
) -> anyhow::Result<()> {
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        bail!("{name} is not a private regular file");
    }
    if stat.st_nlink != 1 {
        bail!("{name} must have exactly one hard link");
    }
    if stat.st_uid != directory_stat.st_uid || stat.st_gid != directory_stat.st_gid {
        bail!("{name} ownership does not match UNIONC_DATA_DIR");
    }
    let mode = Mode::from_raw_mode(stat.st_mode);
    if mode != expected_mode {
        bail!(
            "{name} must have exact permissions {:04o} (found {:04o})",
            expected_mode.as_raw_mode(),
            mode.as_raw_mode()
        );
    }
    Ok(())
}

fn same_directory_identity(left: &Stat, right: &Stat) -> bool {
    same_file_identity(left, right)
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
        && Mode::from_raw_mode(left.st_mode) == Mode::from_raw_mode(right.st_mode)
}

fn same_file_identity(left: &Stat, right: &Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn is_reserved_data_root(path: &Path) -> bool {
    const RESERVED: &[&str] = &[
        "/", "/bin", "/boot", "/dev", "/dev/shm", "/etc", "/home", "/lib", "/lib64", "/media",
        "/mnt", "/opt", "/proc", "/root", "/run", "/sbin", "/srv", "/sys", "/tmp", "/usr", "/var",
        "/var/tmp",
    ];
    RESERVED.iter().any(|reserved| path == Path::new(reserved))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    use super::*;

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path).unwrap().mode() & 0o7777
    }

    fn marker_path(path: &Path) -> PathBuf {
        path.join(DATA_DIRECTORY_MARKER)
    }

    #[test]
    fn bootstrap_creates_private_tree_and_durable_marker() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("one/two/data");

        ensure_layout_at(&path, LayoutIntent::Bootstrap).unwrap();

        for created in [
            parent.path().join("one"),
            parent.path().join("one/two"),
            path.clone(),
        ] {
            assert!(created.is_dir());
            assert_eq!(mode(&created), 0o700);
        }
        let marker = marker_path(&path);
        assert_eq!(
            std::fs::read(&marker).unwrap(),
            DATA_DIRECTORY_MARKER_CONTENT
        );
        let metadata = std::fs::metadata(&marker).unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o600);
        assert_eq!(metadata.nlink(), 1);

        ensure_layout_at(&path, LayoutIntent::ExistingOnly).unwrap();
    }

    #[test]
    fn existing_only_never_creates_a_missing_directory() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("missing/data");
        let error = ensure_layout_at(&path, LayoutIntent::ExistingOnly).unwrap_err();
        assert!(
            error.to_string().contains("could not safely open"),
            "{error:#}"
        );
        assert!(!parent.path().join("missing").exists());
    }

    #[test]
    fn empty_existing_directory_requires_private_mode_and_creation_intent() {
        let parent = tempfile::tempdir().unwrap();
        let private = parent.path().join("private");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();

        let error = ensure_layout_at(&private, LayoutIntent::ExistingOnly).unwrap_err();
        assert!(
            error.to_string().contains("already initialized"),
            "{error:#}"
        );
        assert!(!marker_path(&private).exists());
        ensure_layout_at(&private, LayoutIntent::Restore).unwrap();
        assert!(marker_path(&private).is_file());

        let public = parent.path().join("public");
        std::fs::create_dir(&public).unwrap();
        std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = ensure_layout_at(&public, LayoutIntent::Bootstrap).unwrap_err();
        assert!(
            error.to_string().contains("exact permissions 0700"),
            "{error:#}"
        );
        assert_eq!(mode(&public), 0o755, "a rejected path must not be chmodded");
    }

    #[test]
    fn unmarked_nonempty_directory_is_rejected_even_if_contents_look_private() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("unmarked");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let database = path.join("unionc.db");
        std::fs::write(&database, b"unmarked-placeholder").unwrap();
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();

        let error = ensure_layout_at(&path, LayoutIntent::ExistingOnly).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("without a valid UnionC data marker")
        );
        assert_eq!(std::fs::read(&database).unwrap(), b"unmarked-placeholder");
        assert_eq!(mode(&database), 0o600);
        assert!(!marker_path(&path).exists());
    }

    #[test]
    fn unknown_nonempty_directory_is_rejected_without_mutation() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("foreign");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let canary = path.join("do-not-touch");
        std::fs::write(&canary, b"unchanged").unwrap();

        let error = ensure_layout_at(&path, LayoutIntent::Bootstrap).unwrap_err();

        assert!(error.to_string().contains("refusing to claim"), "{error:#}");
        assert_eq!(mode(&path), 0o700);
        assert_eq!(std::fs::read(&canary).unwrap(), b"unchanged");
        assert!(!marker_path(&path).exists());
    }

    #[test]
    fn leaf_and_ancestor_symlinks_never_change_their_targets() {
        let parent = tempfile::tempdir().unwrap();
        let victim = parent.path().join("victim");
        std::fs::create_dir(&victim).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let leaf = parent.path().join("leaf");
        symlink(&victim, &leaf).unwrap();

        assert!(ensure_layout_at(&leaf, LayoutIntent::Bootstrap).is_err());
        assert_eq!(mode(&victim), 0o755);

        let ancestor = parent.path().join("ancestor");
        symlink(&victim, &ancestor).unwrap();
        assert!(ensure_layout_at(&ancestor.join("data"), LayoutIntent::Bootstrap).is_err());
        assert_eq!(mode(&victim), 0o755);
        assert!(!victim.join("data").exists());
    }

    #[test]
    fn invalid_or_hard_linked_marker_is_rejected() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("data");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let marker = marker_path(&path);
        std::fs::write(&marker, b"wrong\n").unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(ensure_layout_at(&path, LayoutIntent::Bootstrap).is_err());

        std::fs::write(&marker, DATA_DIRECTORY_MARKER_CONTENT).unwrap();
        std::fs::hard_link(&marker, path.join("marker-alias")).unwrap();
        let error = ensure_layout_at(&path, LayoutIntent::Bootstrap).unwrap_err();
        assert!(
            error.to_string().contains("exactly one hard link"),
            "{error:#}"
        );
    }

    #[test]
    fn writable_non_sticky_ancestor_is_rejected_before_creation() {
        let parent = tempfile::tempdir().unwrap();
        std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let path = parent.path().join("nested/data");

        let error = ensure_layout_at(&path, LayoutIntent::Bootstrap).unwrap_err();

        assert!(
            error.to_string().contains("writable by another user"),
            "{error:#}"
        );
        assert!(!parent.path().join("nested").exists());
    }

    #[test]
    fn replacement_during_validation_is_not_chmodded_or_accepted() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("data");
        let displaced = parent.path().join("displaced");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();

        let error = ensure_layout_at_with_hook(&path, LayoutIntent::Bootstrap, || {
            std::fs::rename(&path, &displaced).unwrap();
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        })
        .unwrap_err();

        assert!(error.to_string().contains("path changed"), "{error:#}");
        assert_eq!(mode(&path), 0o755, "replacement must never be chmodded");
        assert!(marker_path(&displaced).is_file());
        assert!(!marker_path(&path).exists());
    }

    #[test]
    fn reserved_system_roots_are_classified_without_touching_them() {
        for path in ["/", "/tmp", "/var/tmp", "/dev/shm", "/root", "/srv"] {
            assert!(is_reserved_data_root(Path::new(path)), "{path}");
        }
        assert!(!is_reserved_data_root(Path::new("/tmp/unionc-test")));
        assert!(!is_reserved_data_root(Path::new("/var/lib/unionc")));
    }
}
