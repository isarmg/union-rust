#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, test))]
use anyhow::{Context, bail, ensure};

#[cfg(any(windows, test))]
fn program_security_descriptor(service_sid: &str) -> String {
    format!(
        "O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)\
         (A;OICI;0x1200a9;;;BU)(A;OICI;0x1200a9;;;{service_sid})"
    )
}

#[cfg(any(windows, test))]
fn managed_state_security_descriptor(service_sid: Option<&str>) -> String {
    let service_ace = service_sid
        .map(|sid| format!("(A;OICI;0x1301bf;;;{sid})"))
        .unwrap_or_default();
    format!("O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA){service_ace}(A;OICI;RC;;;OW)")
}

#[cfg(any(windows, test))]
fn parse_program_dacl(sddl: &str, service_sid: &str) -> anyhow::Result<()> {
    ensure!(
        sddl.starts_with("O:SY"),
        "program owner is not SYSTEM: {sddl}"
    );
    let dacl = sddl
        .split_once("D:")
        .map(|(_, value)| value)
        .context("program security descriptor has no DACL")?;
    let (control, _) = dacl
        .split_once('(')
        .context("program DACL contains no ACEs")?;
    ensure!(control == "P", "program DACL is not protected-only: {sddl}");
    let mut system = false;
    let mut admins = false;
    let mut users = false;
    let mut service = false;
    for ace in dacl.split('(').skip(1) {
        let ace = ace
            .split(')')
            .next()
            .context("malformed program DACL ACE")?;
        let fields = ace.split(';').collect::<Vec<_>>();
        ensure!(
            fields.len() == 6
                && fields[0] == "A"
                && matches!(fields[1], "OICI" | "CIOI")
                && fields[3].is_empty()
                && fields[4].is_empty(),
            "unexpected program DACL ACE: ({ace})"
        );
        match fields[5] {
            "SY" | "S-1-5-18" => {
                ensure!(!system, "duplicate program SYSTEM ACE");
                ensure!(fields[2] == "FA", "program SYSTEM ACE is not full access");
                system = true;
            }
            "BA" | "S-1-5-32-544" => {
                ensure!(!admins, "duplicate program Administrators ACE");
                ensure!(
                    fields[2] == "FA",
                    "program Administrators ACE is not full access"
                );
                admins = true;
            }
            "BU" | "S-1-5-32-545" => {
                ensure!(!users, "duplicate program BUILTIN\\Users ACE");
                ensure!(
                    matches!(fields[2], "0x1200a9" | "0x001200a9"),
                    "program BUILTIN\\Users ACE is not exactly read/execute"
                );
                users = true;
            }
            trustee if trustee == service_sid => {
                ensure!(!service, "duplicate program service SID ACE");
                ensure!(
                    matches!(fields[2], "0x1200a9" | "0x001200a9"),
                    "program service ACE is not exactly read/execute"
                );
                service = true;
            }
            trustee => bail!("unexpected program DACL trustee {trustee}"),
        }
    }
    ensure!(
        system && admins && users && service,
        "program DACL does not match the current SYSTEM, Administrators, Users and service SID template"
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("unionc-agent-maintenance is available only on Windows");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_maintenance::run() {
        eprintln!("UnionC Agent maintenance failed: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod windows_maintenance {
    use super::{
        managed_state_security_descriptor, parse_program_dacl, program_security_descriptor,
    };
    use std::{
        ffi::{OsStr, OsString, c_void},
        fs,
        mem::size_of,
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        },
        path::{Component, Path, PathBuf},
        ptr, thread,
        time::{Duration, Instant},
    };

    use anyhow::{Context, bail, ensure};
    use serde::{Deserialize, Serialize};
    use unionc_agent::{AgentConfig, service::WINDOWS_SERVICE_NAME};
    use windows::{
        Win32::{
            Foundation::{
                CloseHandle, ERROR_NOT_ALL_ASSIGNED, ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SUCCESS,
                GetLastError, HANDLE, LocalFree, SetLastError,
            },
            Security::{
                AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
                SE_PRIVILEGE_ENABLED, SE_RESTORE_NAME, SE_TAKE_OWNERSHIP_NAME,
                TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
            },
            Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
                FILE_FLAG_OPEN_REPARSE_POINT, GetFileInformationByHandle,
            },
            System::{
                Com::CoTaskMemFree,
                Services::{
                    ChangeServiceConfig2W, CloseServiceHandle, ControlService, OpenSCManagerW,
                    OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfig2W, QueryServiceConfigW,
                    QueryServiceStatusEx, SC_HANDLE, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO,
                    SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
                    SERVICE_CONFIG_SERVICE_SID_INFO, SERVICE_CONTROL_STOP,
                    SERVICE_FAILURE_ACTIONS_FLAG, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS,
                    SERVICE_RUNNING, SERVICE_SID_INFO, SERVICE_SID_TYPE_UNRESTRICTED,
                    SERVICE_START, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_PROCESS,
                    SERVICE_STOP, SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_WIN32_OWN_PROCESS,
                    StartServiceW,
                },
                Threading::{GetCurrentProcess, OpenProcessToken},
            },
            UI::Shell::{
                FOLDERID_ProgramData, FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
            },
        },
        core::PCWSTR,
    };

    const DIRECTORY_NAME: &str = "UnionC Agent";
    const AGENT_EXE: &str = "unionc-agent.exe";
    const CONFIG_FILE: &str = "config.json";
    const JOURNAL_DIRECTORY: &str =
        concat!("UnionC Agent.install-journal-", env!("CARGO_PKG_VERSION"));
    const UNINSTALL_JOURNAL_DIRECTORY: &str =
        concat!("UnionC Agent.uninstall-journal-", env!("CARGO_PKG_VERSION"));
    const PURGE_DIRECTORY: &str =
        concat!("UnionC Agent.purge-quarantine-", env!("CARGO_PKG_VERSION"));
    const SNAPSHOT_FORMAT: u32 = 2;
    const STATE_MARKER: &str = concat!(".unionc-agent-managed-", env!("CARGO_PKG_VERSION"));
    const STATE_MARKER_CONTENT: &str = concat!(
        "unionc-agent-windows-state-",
        env!("CARGO_PKG_VERSION"),
        "\r\n"
    );
    const SNAPSHOT_FILE: &str = "snapshot.json";
    const STATE_ACL_FILE: &str = "state-acl.json";
    const PROGRAM_ACL_FILE: &str = "program-acl.json";
    const PURGE_STARTED_FILE: &str = concat!("purge-started-v2-", env!("CARGO_PKG_VERSION"));
    const PURGE_STARTED_CONTENT: &str = concat!(
        "unionc-agent-purge-started-v2-",
        env!("CARGO_PKG_VERSION"),
        "\r\n"
    );
    const STOP_TIMEOUT: Duration = Duration::from_secs(30);

    #[derive(Debug)]
    struct FixedPaths {
        program_root: PathBuf,
        state_root: PathBuf,
        journal_root: PathBuf,
        uninstall_journal_root: PathBuf,
        quarantine_root: PathBuf,
        program_exe: PathBuf,
        config: PathBuf,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct InstallSnapshot {
        format: u32,
        application_version: String,
        program_existed: bool,
        state_existed: bool,
        original_service_sid_type: Option<u32>,
        original_failure_actions_on_non_crash: Option<bool>,
        original_service_running: bool,
        state_acl_saved: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct UninstallSnapshot {
        format: u32,
        application_version: String,
        state_acl_saved: bool,
        service_was_running: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AclSnapshot {
        format: u32,
        application_version: String,
        entries: Vec<AclSnapshotEntry>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AclSnapshotEntry {
        relative_path_utf16: Vec<u16>,
        is_directory: bool,
        sddl: String,
    }

    pub fn run() -> anyhow::Result<()> {
        let mut arguments = std::env::args_os().skip(1);
        let command = arguments
            .next()
            .context("expected one maintenance command")?;
        ensure!(
            arguments.next().is_none(),
            "maintenance commands take no arguments"
        );
        let command = command
            .to_str()
            .context("maintenance command must be valid Unicode")?;
        enable_restore_privileges()?;
        let paths = FixedPaths::discover()?;
        match command {
            "prepare-install" => prepare_install(&paths),
            "apply-install" => apply_install(&paths),
            "rollback-install" => rollback_install(&paths),
            "commit-install" => commit_install(&paths),
            "preflight-uninstall" => preflight_uninstall(&paths),
            "rollback-uninstall-preflight" => rollback_uninstall_preflight(&paths),
            "preserve-state" => preserve_state(&paths),
            "rollback-uninstall" => rollback_uninstall(&paths),
            "commit-uninstall" => commit_uninstall(&paths),
            "prepare-purge" => prepare_purge(&paths),
            "rollback-purge" => rollback_purge(&paths),
            "commit-purge" => commit_purge(&paths),
            _ => bail!(
                "unknown maintenance command; expected prepare-install, apply-install, \
                 rollback-install, commit-install, preflight-uninstall, preserve-state, \
                 rollback-uninstall-preflight, rollback-uninstall, commit-uninstall, \
                 prepare-purge, rollback-purge, or commit-purge"
            ),
        }
    }

    fn enable_restore_privileges() -> anyhow::Result<()> {
        let mut token = HANDLE::default();
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            )
        }
        .context("failed to open the maintenance process token")?;
        let result = (|| {
            for (name, label) in [
                (SE_RESTORE_NAME, "SeRestorePrivilege"),
                (SE_TAKE_OWNERSHIP_NAME, "SeTakeOwnershipPrivilege"),
            ] {
                let mut luid = Default::default();
                unsafe { LookupPrivilegeValueW(None, name, &mut luid) }
                    .with_context(|| format!("failed to resolve {label}"))?;
                let privileges = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES {
                        Luid: luid,
                        Attributes: SE_PRIVILEGE_ENABLED,
                    }],
                };
                unsafe { SetLastError(ERROR_SUCCESS) };
                unsafe { AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None) }
                    .with_context(|| format!("failed to enable {label}"))?;
                ensure!(
                    unsafe { GetLastError() } != ERROR_NOT_ALL_ASSIGNED,
                    "maintenance token does not hold {label}"
                );
            }
            Ok(())
        })();
        let close = unsafe { CloseHandle(token) }.context("failed to close process token");
        result.and(close)
    }

    impl FixedPaths {
        fn discover() -> anyhow::Result<Self> {
            let program_files = known_folder(&FOLDERID_ProgramFiles)?;
            let program_data = known_folder(&FOLDERID_ProgramData)?;
            ensure_absolute_root(&program_files, "Program Files")?;
            ensure_absolute_root(&program_data, "ProgramData")?;
            let program_root = program_files.join(DIRECTORY_NAME);
            let state_root = program_data.join(DIRECTORY_NAME);
            Ok(Self {
                program_exe: program_root.join(AGENT_EXE),
                config: state_root.join(CONFIG_FILE),
                journal_root: program_data.join(JOURNAL_DIRECTORY),
                uninstall_journal_root: program_data.join(UNINSTALL_JOURNAL_DIRECTORY),
                quarantine_root: program_data.join(PURGE_DIRECTORY),
                program_root,
                state_root,
            })
        }
    }

    fn known_folder(id: &windows::core::GUID) -> anyhow::Result<PathBuf> {
        let value = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }
            .context("SHGetKnownFolderPath failed")?;
        let text = unsafe { value.to_string() }.context("known folder path is invalid")?;
        unsafe { CoTaskMemFree(Some(value.0.cast())) };
        Ok(PathBuf::from(text))
    }

    fn ensure_absolute_root(path: &Path, label: &str) -> anyhow::Result<()> {
        ensure!(
            path.is_absolute(),
            "{label} did not resolve to an absolute path"
        );
        ensure!(
            path.parent().is_some(),
            "{label} resolved to a filesystem root"
        );
        Ok(())
    }

    fn prepare_install(paths: &FixedPaths) -> anyhow::Result<()> {
        ensure_absent(&paths.journal_root, "stale install journal")?;
        ensure_absent(&paths.uninstall_journal_root, "pending uninstall journal")?;
        ensure_absent(&paths.quarantine_root, "pending purge quarantine")?;
        let program_existed = validate_optional_root(&paths.program_root, "program root")?;
        if program_existed {
            validate_tree(&paths.program_root)?;
        }
        let state_existed = validate_optional_root(&paths.state_root, "state root")?;
        if state_existed {
            validate_tree(&paths.state_root)?;
        }
        let existing_service = open_agent_service(SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)?;
        ensure!(
            !program_existed || existing_service.is_some(),
            "an existing program root is not owned by the current UnionCAgent service"
        );
        let original_service_sid_type = if let Some(service) = existing_service.as_ref() {
            validate_agent_service(service, paths)?;
            validate_program_tree(&paths.program_root)?;
            ensure!(
                state_existed,
                "an existing UnionCAgent service has no trusted state root"
            );
            validate_state_marker(paths, true)?;
            let sid_type = query_service_sid_type(service)?;
            ensure!(
                sid_type == SERVICE_SID_TYPE_UNRESTRICTED,
                "the existing UnionCAgent service SID type is not unrestricted"
            );
            Some(sid_type)
        } else {
            None
        };
        let original_service_running = existing_service
            .as_ref()
            .map(service_is_active)
            .transpose()?
            .unwrap_or(false);
        let original_failure_actions_on_non_crash = existing_service
            .as_ref()
            .map(query_failure_actions_on_non_crash)
            .transpose()?;
        if state_existed && existing_service.is_none() {
            // A normal uninstall deliberately preserves the current identity
            // under the service-free ACL. The exact current marker and ACL are
            // required before a same-version reinstall may reuse that state.
            validate_state_marker(paths, false)?;
        }

        create_protected_journal(&paths.journal_root)?;

        let result = (|| {
            let snapshot = InstallSnapshot {
                format: SNAPSHOT_FORMAT,
                application_version: env!("CARGO_PKG_VERSION").to_owned(),
                program_existed,
                state_existed,
                original_service_sid_type,
                original_failure_actions_on_non_crash,
                original_service_running,
                state_acl_saved: state_existed,
            };
            write_new_private(
                &paths.journal_root.join(SNAPSHOT_FILE),
                &serde_json::to_vec_pretty(&snapshot)?,
            )?;
            validate_tree(&paths.journal_root)
        })();
        if result.is_err() {
            let _ = remove_tree_no_reparse(&paths.journal_root);
            return result;
        }
        if state_existed {
            save_acl(&paths.state_root, &paths.journal_root.join(STATE_ACL_FILE))?;
        }
        if program_existed {
            save_acl(
                &paths.program_root,
                &paths.journal_root.join(PROGRAM_ACL_FILE),
            )?;
        }
        validate_tree(&paths.journal_root)?;
        Ok(())
    }

    fn apply_install(paths: &FixedPaths) -> anyhow::Result<()> {
        let snapshot = read_snapshot(paths)?;
        ensure!(
            validate_optional_root(&paths.program_root, "program root")?,
            "MSI did not create the program root"
        );
        validate_tree(&paths.program_root)?;
        validate_regular_single_link(&paths.program_exe, "installed Agent executable")?;
        if snapshot.state_existed {
            ensure!(
                validate_optional_root(&paths.state_root, "state root")?,
                "the pre-existing Agent state root disappeared before apply"
            );
            validate_tree(&paths.state_root)?;
        } else {
            ensure_absent(
                &paths.state_root,
                "state root that appeared after install preflight",
            )?;
            fs::create_dir(&paths.state_root).with_context(|| {
                format!(
                    "failed to create the fixed Agent state root {}",
                    paths.state_root.display()
                )
            })?;
            validate_real_directory(&paths.state_root, "new state root")?;
            // Close the ProgramData inheritance window before service setup.
            secure_system_admin_only(&paths.state_root, false)?;
            validate_managed_dacl(&paths.state_root, false)?;
            write_new_private(
                &paths.config,
                &serde_json::to_vec_pretty(&AgentConfig::default())?,
            )?;
        }

        let service = open_agent_service(SERVICE_QUERY_CONFIG | SERVICE_CHANGE_CONFIG)?
            .context("MSI did not install the UnionCAgent service")?;
        validate_agent_service(&service, paths)?;
        set_service_sid_type(&service, SERVICE_SID_TYPE_UNRESTRICTED)?;
        ensure!(
            query_service_sid_type(&service)? == SERVICE_SID_TYPE_UNRESTRICTED,
            "UnionCAgent service SID type verification failed"
        );
        set_failure_actions_on_non_crash(&service, true)?;
        ensure!(
            query_failure_actions_on_non_crash(&service)?,
            "UnionCAgent non-crash failure restart policy did not verify"
        );

        secure_program_for_service(&paths.program_root)?;
        validate_program_tree(&paths.program_root)?;

        write_or_validate_state_marker(paths)?;
        validate_tree(&paths.state_root)?;
        secure_state_for_service(&paths.state_root)?;
        validate_state_marker(paths, true)?;
        Ok(())
    }

    fn rollback_install(paths: &FixedPaths) -> anyhow::Result<()> {
        if !paths.journal_root.exists() {
            return Ok(());
        }
        validate_real_directory(&paths.journal_root, "install journal")?;
        validate_tree(&paths.journal_root)?;
        let snapshot = read_snapshot(paths)?;
        let mut failures = Vec::new();

        let install_acl_snapshot = paths.journal_root.join(STATE_ACL_FILE);
        if snapshot.state_acl_saved
            && install_acl_snapshot.exists()
            && paths.state_root.exists()
            && let Err(error) = restore_acl(&paths.state_root, &install_acl_snapshot)
        {
            failures.push(format!("restore state ACL: {error:#}"));
        }
        let program_acl_snapshot = paths.journal_root.join(PROGRAM_ACL_FILE);
        if snapshot.program_existed
            && program_acl_snapshot.exists()
            && paths.program_root.exists()
            && let Err(error) = restore_acl(&paths.program_root, &program_acl_snapshot)
        {
            failures.push(format!("restore program ACL: {error:#}"));
        }
        if let Some(sid_type) = snapshot.original_service_sid_type
            && let Err(error) = restore_service_state(
                paths,
                sid_type,
                snapshot
                    .original_failure_actions_on_non_crash
                    .unwrap_or(true),
                snapshot.original_service_running,
                ProgramAclRestore::PreserveSnapshot,
            )
        {
            failures.push(format!("restore service state: {error:#}"));
        }

        if !snapshot.state_existed
            && paths.state_root.exists()
            && let Err(error) = remove_fresh_install_state(paths)
        {
            failures.push(format!("remove fresh-install state: {error:#}"));
        }

        if failures.is_empty() {
            remove_tree_no_reparse(&paths.journal_root)?;
            Ok(())
        } else {
            bail!(
                "rollback was incomplete; the protected journal was retained: {}",
                failures.join("; ")
            )
        }
    }

    fn commit_install(paths: &FixedPaths) -> anyhow::Result<()> {
        if !paths.journal_root.exists() {
            return Ok(());
        }
        validate_real_directory(&paths.journal_root, "install journal")?;
        validate_tree(&paths.journal_root)?;
        read_snapshot(paths)?;
        remove_tree_no_reparse(&paths.journal_root)
    }

    fn remove_fresh_install_state(paths: &FixedPaths) -> anyhow::Result<()> {
        validate_real_directory(&paths.state_root, "fresh-install state root")?;
        validate_tree(&paths.state_root)?;
        let marker = paths.state_root.join(STATE_MARKER);
        match fs::symlink_metadata(&marker) {
            Ok(_) => {
                // The root is protected as SYSTEM/Administrators immediately after creation;
                // the marker is then created with the service template before the recursive
                // transition. A failure may therefore leave the root in either exact mode.
                validate_marker_file(&marker)?;
                validate_managed_dacl(&marker, true)?;
                if validate_managed_dacl(&paths.state_root, false).is_err() {
                    validate_managed_dacl(&paths.state_root, true).context(
                        "fresh-install state root matches neither exact rollback-safe ACL mode",
                    )?;
                }
                remove_tree_no_reparse(&paths.state_root)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if validate_managed_dacl(&paths.state_root, false).is_ok() {
                    remove_tree_no_reparse(&paths.state_root)
                } else {
                    ensure!(
                        fs::read_dir(&paths.state_root)?.next().is_none(),
                        "an unmarked fresh-install state root is not protected by the exact pre-marker ACL"
                    );
                    fs::remove_dir(&paths.state_root)
                        .context("failed to remove the empty fresh-install state root")
                }
            }
            Err(error) => Err(error).context("failed to inspect the fresh-install state marker"),
        }
    }

    fn preflight_uninstall(paths: &FixedPaths) -> anyhow::Result<()> {
        ensure_absent(&paths.uninstall_journal_root, "stale uninstall journal")?;
        ensure!(
            validate_optional_root(&paths.program_root, "program root")?,
            "installed program root is missing"
        );
        validate_regular_single_link(&paths.program_exe, "installed Agent executable")?;
        validate_program_tree(&paths.program_root)?;
        let service = open_agent_service(SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)?
            .context("the installed UnionCAgent service is missing")?;
        validate_agent_service(&service, paths)?;
        ensure!(
            query_service_sid_type(&service)? == SERVICE_SID_TYPE_UNRESTRICTED,
            "the installed UnionCAgent service SID type is not unrestricted"
        );
        ensure!(
            query_failure_actions_on_non_crash(&service)?,
            "the installed UnionCAgent service does not restart after reported failures"
        );
        if validate_optional_root(&paths.state_root, "state root")? {
            validate_tree(&paths.state_root)?;
            validate_state_marker(paths, true)?;
        }
        let service_was_running = service_is_active(&service)?;
        create_protected_journal(&paths.uninstall_journal_root)?;
        let snapshot = UninstallSnapshot {
            format: SNAPSHOT_FORMAT,
            application_version: env!("CARGO_PKG_VERSION").to_owned(),
            state_acl_saved: false,
            service_was_running,
        };
        if let Err(error) = write_new_private(
            &paths.uninstall_journal_root.join(SNAPSHOT_FILE),
            &serde_json::to_vec_pretty(&snapshot)?,
        ) {
            let _ = remove_tree_no_reparse(&paths.uninstall_journal_root);
            return Err(error);
        }
        Ok(())
    }

    fn rollback_uninstall_preflight(paths: &FixedPaths) -> anyhow::Result<()> {
        if !paths.uninstall_journal_root.exists() {
            return Ok(());
        }
        let snapshot = read_uninstall_snapshot(paths)?;
        if snapshot.state_acl_saved || purge_started(paths)? {
            // A later, operation-specific rollback owns this journal. If that rollback failed,
            // retain the protected evidence and state needed for an administrator to recover.
            return Ok(());
        }
        remove_tree_no_reparse(&paths.uninstall_journal_root)
    }

    fn preserve_state(paths: &FixedPaths) -> anyhow::Result<()> {
        if !paths.state_root.exists() {
            return Ok(());
        }
        let mut snapshot = read_uninstall_snapshot(paths)?;
        validate_real_directory(&paths.state_root, "state root")?;
        validate_tree(&paths.state_root)?;
        let service = open_agent_service(SERVICE_QUERY_CONFIG)?
            .context("UnionCAgent service disappeared during uninstall")?;
        validate_agent_service(&service, paths)?;
        validate_state_marker(paths, true)?;

        let result = (|| {
            save_acl(
                &paths.state_root,
                &paths.uninstall_journal_root.join(STATE_ACL_FILE),
            )?;
            snapshot.state_acl_saved = true;
            replace_private(
                &paths.uninstall_journal_root.join(SNAPSHOT_FILE),
                &serde_json::to_vec_pretty(&snapshot)?,
            )?;
            secure_system_admin_only(&paths.state_root, true)?;
            validate_state_marker(paths, false)
        })();
        if let Err(error) = result {
            let acl_snapshot = paths.uninstall_journal_root.join(STATE_ACL_FILE);
            let rollback = if acl_snapshot.exists() {
                restore_acl(&paths.state_root, &acl_snapshot)
            } else {
                Ok(())
            };
            if rollback.is_ok() {
                let _ = remove_tree_no_reparse(&paths.uninstall_journal_root);
            }
            return Err(error.context(match rollback {
                Ok(()) => "state preservation failed; the original ACL was restored",
                Err(_) => {
                    "state preservation and ACL rollback failed; the protected uninstall journal was retained"
                }
            }));
        }
        Ok(())
    }

    fn rollback_uninstall(paths: &FixedPaths) -> anyhow::Result<()> {
        if !paths.uninstall_journal_root.exists() {
            return Ok(());
        }
        let snapshot = read_uninstall_snapshot(paths)?;
        if snapshot.state_acl_saved && paths.state_root.exists() {
            validate_tree(&paths.state_root)?;
            restore_acl(
                &paths.state_root,
                &paths.uninstall_journal_root.join(STATE_ACL_FILE),
            )?;
        }
        restore_service_state(
            paths,
            SERVICE_SID_TYPE_UNRESTRICTED,
            true,
            snapshot.service_was_running,
            ProgramAclRestore::SecureCurrent,
        )?;
        remove_tree_no_reparse(&paths.uninstall_journal_root)
    }

    fn commit_uninstall(paths: &FixedPaths) -> anyhow::Result<()> {
        if !paths.uninstall_journal_root.exists() {
            return Ok(());
        }
        read_uninstall_snapshot(paths)?;
        remove_tree_no_reparse(&paths.uninstall_journal_root)
    }

    fn prepare_purge(paths: &FixedPaths) -> anyhow::Result<()> {
        read_uninstall_snapshot(paths)?;
        ensure_absent(&paths.quarantine_root, "pending purge quarantine")?;
        if !paths.state_root.exists() {
            return Ok(());
        }
        validate_real_directory(&paths.state_root, "state root")?;
        validate_tree(&paths.state_root)?;
        let service = open_agent_service(SERVICE_QUERY_CONFIG)?
            .context("UnionCAgent service disappeared during purge")?;
        validate_agent_service(&service, paths)?;
        validate_state_marker(paths, true)?;
        write_new_private(
            &paths.uninstall_journal_root.join(PURGE_STARTED_FILE),
            PURGE_STARTED_CONTENT.as_bytes(),
        )?;
        fs::rename(&paths.state_root, &paths.quarantine_root).with_context(|| {
            format!(
                "failed to atomically quarantine {} as {}",
                paths.state_root.display(),
                paths.quarantine_root.display()
            )
        })?;
        validate_real_directory(&paths.quarantine_root, "purge quarantine")?;
        Ok(())
    }

    fn rollback_purge(paths: &FixedPaths) -> anyhow::Result<()> {
        if !paths.uninstall_journal_root.exists() {
            return Ok(());
        }
        let snapshot = read_uninstall_snapshot(paths)?;
        if paths.quarantine_root.exists() {
            validate_real_directory(&paths.quarantine_root, "purge quarantine")?;
            validate_tree(&paths.quarantine_root)?;
            ensure_absent(&paths.state_root, "replacement state root")?;
            fs::rename(&paths.quarantine_root, &paths.state_root)
                .context("failed to restore quarantined Agent state")?;
        }
        restore_service_state(
            paths,
            SERVICE_SID_TYPE_UNRESTRICTED,
            true,
            snapshot.service_was_running,
            ProgramAclRestore::SecureCurrent,
        )?;
        remove_tree_no_reparse(&paths.uninstall_journal_root)
    }

    fn commit_purge(paths: &FixedPaths) -> anyhow::Result<()> {
        let journal_existed = paths.uninstall_journal_root.exists();
        if journal_existed {
            read_uninstall_snapshot(paths)?;
        }
        let deletion = if paths.quarantine_root.exists() {
            validate_real_directory(&paths.quarantine_root, "purge quarantine")
                .and_then(|_| validate_tree(&paths.quarantine_root))
                .and_then(|_| remove_tree_no_reparse(&paths.quarantine_root))
                .with_context(|| {
                    format!(
                        "purge was committed but protected quarantine remains at {}; reboot and remove that fixed directory as Administrator",
                        paths.quarantine_root.display()
                    )
                })
        } else {
            Ok(())
        };
        let journal_cleanup = if journal_existed {
            remove_tree_no_reparse(&paths.uninstall_journal_root)
        } else {
            Ok(())
        };
        deletion.and(journal_cleanup)
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

    fn validate_state_marker(
        paths: &FixedPaths,
        require_service_access: bool,
    ) -> anyhow::Result<()> {
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

    fn apply_exact_acl(
        path: &Path,
        service_sid: Option<&str>,
        recursive: bool,
    ) -> anyhow::Result<()> {
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
            let metadata = fs::symlink_metadata(&target).with_context(|| {
                format!("ACL snapshot target disappeared: {}", target.display())
            })?;
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

    fn open_agent_service(access: u32) -> anyhow::Result<Option<ServiceHandle>> {
        let manager = unsafe { OpenSCManagerW(None, None, SC_MANAGER_CONNECT) }
            .context("failed to open the Service Control Manager")?;
        let manager = ServiceHandle(manager);
        match unsafe {
            OpenServiceW(
                manager.0,
                PCWSTR(wide_null(OsStr::new(WINDOWS_SERVICE_NAME)).as_ptr()),
                access,
            )
        } {
            Ok(service) => Ok(Some(ServiceHandle(service))),
            Err(error) if error.code().0 == hresult_from_win32(ERROR_SERVICE_DOES_NOT_EXIST.0) => {
                Ok(None)
            }
            Err(error) => Err(error).context("failed to open UnionCAgent service"),
        }
    }

    struct ServiceHandle(SC_HANDLE);

    impl Drop for ServiceHandle {
        fn drop(&mut self) {
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }

    fn validate_agent_service(service: &ServiceHandle, paths: &FixedPaths) -> anyhow::Result<()> {
        let buffer = query_service_config(service)?;
        let config = unsafe { &*(buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>()) };
        ensure!(
            config.dwServiceType == SERVICE_WIN32_OWN_PROCESS,
            "UnionCAgent is not an own-process service"
        );
        ensure!(
            config.dwStartType == SERVICE_AUTO_START,
            "UnionCAgent is not configured for automatic start"
        );
        let image_path = unsafe { config.lpBinaryPathName.to_string() }?;
        let arguments = split_windows_command_line(&image_path)?;
        ensure!(
            arguments.len() == 5,
            "UnionCAgent ImagePath argument count is invalid"
        );
        ensure!(
            arguments[0].eq_ignore_ascii_case(&paths.program_exe.to_string_lossy())
                && arguments[1] == "--windows-service"
                && arguments[2] == "run"
                && arguments[3] == "--config"
                && arguments[4].eq_ignore_ascii_case(&paths.config.to_string_lossy()),
            "UnionCAgent ImagePath does not match the fixed executable and arguments"
        );
        let start_name = unsafe { config.lpServiceStartName.to_string() }?;
        ensure!(
            is_local_service_name(&start_name),
            "UnionCAgent does not run as LOCAL SERVICE"
        );
        Ok(())
    }

    fn query_service_status(service: &ServiceHandle) -> anyhow::Result<SERVICE_STATUS_PROCESS> {
        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0;
        let buffer = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
                size_of::<SERVICE_STATUS_PROCESS>(),
            )
        };
        unsafe {
            QueryServiceStatusEx(service.0, SC_STATUS_PROCESS_INFO, Some(buffer), &mut needed)
        }
        .context("failed to query UnionCAgent service status")?;
        Ok(status)
    }

    fn service_is_active(service: &ServiceHandle) -> anyhow::Result<bool> {
        let state = query_service_status(service)?.dwCurrentState;
        if state == SERVICE_RUNNING {
            Ok(true)
        } else if state == SERVICE_STOPPED {
            Ok(false)
        } else {
            bail!(
                "UnionCAgent is not in a stable running/stopped state ({})",
                state.0
            )
        }
    }

    fn wait_for_stable_service_state(
        service: &ServiceHandle,
    ) -> anyhow::Result<windows::Win32::System::Services::SERVICE_STATUS_CURRENT_STATE> {
        let deadline = Instant::now() + STOP_TIMEOUT;
        loop {
            let state = query_service_status(service)?.dwCurrentState;
            if state == SERVICE_RUNNING || state == SERVICE_STOPPED {
                return Ok(state);
            }
            ensure!(
                state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING,
                "UnionCAgent entered unsupported rollback state {}",
                state.0
            );
            ensure!(
                Instant::now() < deadline,
                "UnionCAgent did not reach a stable state within 30 seconds"
            );
            thread::sleep(Duration::from_millis(250));
        }
    }

    fn wait_for_service_state(
        service: &ServiceHandle,
        expected: windows::Win32::System::Services::SERVICE_STATUS_CURRENT_STATE,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + STOP_TIMEOUT;
        loop {
            let state = query_service_status(service)?.dwCurrentState;
            if state == expected {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "UnionCAgent did not reach service state {} within 30 seconds (current {})",
                expected.0,
                state.0
            );
            thread::sleep(Duration::from_millis(250));
        }
    }

    fn restore_service_state(
        paths: &FixedPaths,
        sid_type: u32,
        failure_actions_on_non_crash: bool,
        should_be_running: bool,
        program_acl: ProgramAclRestore,
    ) -> anyhow::Result<()> {
        let service = open_agent_service(
            SERVICE_CHANGE_CONFIG
                | SERVICE_QUERY_CONFIG
                | SERVICE_QUERY_STATUS
                | SERVICE_START
                | SERVICE_STOP,
        )?
        .context("the rollback UnionCAgent service is not present")?;
        validate_agent_service(&service, paths)?;
        match program_acl {
            ProgramAclRestore::PreserveSnapshot => {
                validate_program_tree(&paths.program_root)?;
            }
            ProgramAclRestore::SecureCurrent => {
                secure_program_for_service(&paths.program_root)?;
                validate_program_tree(&paths.program_root)?;
            }
        }
        set_service_sid_type(&service, sid_type)?;
        ensure!(
            query_service_sid_type(&service)? == sid_type,
            "restored service SID type did not verify"
        );
        set_failure_actions_on_non_crash(&service, failure_actions_on_non_crash)?;
        ensure!(
            query_failure_actions_on_non_crash(&service)? == failure_actions_on_non_crash,
            "restored non-crash failure policy did not verify"
        );
        let current = wait_for_stable_service_state(&service)?;
        if should_be_running {
            if current == SERVICE_STOPPED {
                unsafe { StartServiceW(service.0, None) }
                    .context("failed to restart the rollback UnionCAgent service")?;
            }
            wait_for_service_state(&service, SERVICE_RUNNING)
        } else {
            if current != SERVICE_STOPPED {
                let mut status = SERVICE_STATUS::default();
                unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) }
                    .context("failed to stop the rollback UnionCAgent service")?;
            }
            wait_for_service_state(&service, SERVICE_STOPPED)
        }
    }

    fn query_service_config(service: &ServiceHandle) -> anyhow::Result<Vec<usize>> {
        let mut needed = 0;
        let first = unsafe { QueryServiceConfigW(service.0, None, 0, &mut needed) };
        ensure!(
            first.is_err() && needed >= size_of::<QUERY_SERVICE_CONFIGW>() as u32,
            "could not determine UnionCAgent configuration size"
        );
        let words = (needed as usize).div_ceil(size_of::<usize>());
        let mut buffer = vec![0usize; words];
        unsafe {
            QueryServiceConfigW(
                service.0,
                Some(buffer.as_mut_ptr().cast()),
                (buffer.len() * size_of::<usize>()) as u32,
                &mut needed,
            )
        }?;
        Ok(buffer)
    }

    fn query_service_sid_type(service: &ServiceHandle) -> anyhow::Result<u32> {
        let mut needed = 0;
        let first = unsafe {
            QueryServiceConfig2W(
                service.0,
                SERVICE_CONFIG_SERVICE_SID_INFO,
                None,
                &mut needed,
            )
        };
        ensure!(
            first.is_err() && needed as usize >= size_of::<SERVICE_SID_INFO>(),
            "could not size service SID query"
        );
        let mut buffer = vec![0u8; needed as usize];
        unsafe {
            QueryServiceConfig2W(
                service.0,
                SERVICE_CONFIG_SERVICE_SID_INFO,
                Some(&mut buffer),
                &mut needed,
            )
        }?;
        Ok(
            unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<SERVICE_SID_INFO>()) }
                .dwServiceSidType,
        )
    }

    fn set_service_sid_type(service: &ServiceHandle, sid_type: u32) -> anyhow::Result<()> {
        let info = SERVICE_SID_INFO {
            dwServiceSidType: sid_type,
        };
        unsafe {
            ChangeServiceConfig2W(
                service.0,
                SERVICE_CONFIG_SERVICE_SID_INFO,
                Some((&info as *const SERVICE_SID_INFO).cast::<c_void>()),
            )
        }
        .context("failed to configure the UnionCAgent service SID")
    }

    fn query_failure_actions_on_non_crash(service: &ServiceHandle) -> anyhow::Result<bool> {
        let mut needed = 0;
        let first = unsafe {
            QueryServiceConfig2W(
                service.0,
                SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
                None,
                &mut needed,
            )
        };
        ensure!(
            first.is_err() && needed as usize >= size_of::<SERVICE_FAILURE_ACTIONS_FLAG>(),
            "could not size the non-crash failure policy query"
        );
        let mut buffer = vec![0u8; needed as usize];
        unsafe {
            QueryServiceConfig2W(
                service.0,
                SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
                Some(&mut buffer),
                &mut needed,
            )
        }?;
        Ok(
            unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<SERVICE_FAILURE_ACTIONS_FLAG>()) }
                .fFailureActionsOnNonCrashFailures
                .as_bool(),
        )
    }

    fn set_failure_actions_on_non_crash(
        service: &ServiceHandle,
        enabled: bool,
    ) -> anyhow::Result<()> {
        let info = SERVICE_FAILURE_ACTIONS_FLAG {
            fFailureActionsOnNonCrashFailures: enabled.into(),
        };
        unsafe {
            ChangeServiceConfig2W(
                service.0,
                SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
                Some((&info as *const SERVICE_FAILURE_ACTIONS_FLAG).cast::<c_void>()),
            )
        }
        .context("failed to configure non-crash UnionCAgent recovery")
    }

    fn service_sid_string() -> anyhow::Result<String> {
        // Service SIDs are deterministic: S-1-5-80 plus five little-endian
        // u32 words from SHA-1(uppercase UTF-16 service name).
        use sha1::{Digest, Sha1};
        let mut bytes = Vec::new();
        for code_unit in WINDOWS_SERVICE_NAME.to_ascii_uppercase().encode_utf16() {
            bytes.extend_from_slice(&code_unit.to_le_bytes());
        }
        let digest = Sha1::digest(&bytes);
        let parts = digest
            .as_chunks::<4>()
            .0
            .iter()
            .map(|part| u32::from_le_bytes(*part))
            .map(|part| part.to_string())
            .collect::<Vec<_>>();
        ensure!(parts.len() == 5, "invalid SHA-1 service SID digest");
        Ok(format!("S-1-5-80-{}", parts.join("-")))
    }

    fn split_windows_command_line(command_line: &str) -> anyhow::Result<Vec<String>> {
        let wide = wide_null(OsStr::new(command_line));
        let mut count = 0;
        let raw = unsafe {
            windows::Win32::UI::Shell::CommandLineToArgvW(PCWSTR(wide.as_ptr()), &mut count)
        };
        ensure!(
            !raw.is_null() && count > 0,
            "failed to parse service ImagePath"
        );
        let values = unsafe { std::slice::from_raw_parts(raw, count as usize) }
            .iter()
            .map(|value| unsafe { value.to_string() })
            .collect::<Result<Vec<_>, _>>()?;
        unsafe { LocalFree(Some(windows::Win32::Foundation::HLOCAL(raw.cast()))) };
        Ok(values)
    }

    fn is_local_service_name(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "s-1-5-19"
                | "localservice"
                | "nt authority\\localservice"
                | "nt authority\\local service"
        )
    }

    fn wide_null(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn file_link_count(path: &Path) -> anyhow::Result<u32> {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(path)
            .with_context(|| {
                format!(
                    "failed to open {} without following reparse points",
                    path.display()
                )
            })?;
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
            .with_context(|| format!("failed to query link count for {}", path.display()))?;
        Ok(information.nNumberOfLinks)
    }

    fn hresult_from_win32(code: u32) -> i32 {
        (0x8007_0000u32 | code) as i32
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn service_sid_is_stable_and_has_five_subauthorities() {
            let sid = service_sid_string().unwrap();
            assert!(sid.starts_with("S-1-5-80-"));
            assert_eq!(sid.split('-').count(), 9);
            assert_eq!(sid, service_sid_string().unwrap());
        }
    }
}

#[cfg(test)]
mod program_acl_template_tests {
    use super::*;

    const SERVICE_SID: &str = "S-1-5-80-1-2-3-4-5";

    #[test]
    fn parser_accepts_only_the_current_program_template() {
        let obsolete_service_only = format!(
            "O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)\
             (A;OICI;0x1200a9;;;{SERVICE_SID})"
        );
        assert!(parse_program_dacl(&obsolete_service_only, SERVICE_SID).is_err());

        let tray_enabled = program_security_descriptor(SERVICE_SID);
        parse_program_dacl(&tray_enabled, SERVICE_SID).unwrap();

        let users_can_write = tray_enabled.replace("(A;OICI;0x1200a9;;;BU)", "(A;OICI;FA;;;BU)");
        assert!(parse_program_dacl(&users_can_write, SERVICE_SID).is_err());

        let unexpected_authenticated_users =
            tray_enabled.replace("(A;OICI;0x1200a9;;;BU)", "(A;OICI;0x1200a9;;;AU)");
        assert!(parse_program_dacl(&unexpected_authenticated_users, SERVICE_SID).is_err());
    }

    #[test]
    fn tray_execute_access_never_leaks_into_mutable_state_template() {
        let program = program_security_descriptor(SERVICE_SID);
        assert!(program.contains("(A;OICI;0x1200a9;;;BU)"));

        let installed_state = managed_state_security_descriptor(Some(SERVICE_SID));
        let preserved_state = managed_state_security_descriptor(None);
        assert!(!installed_state.contains(";;;BU)"));
        assert!(!preserved_state.contains(";;;BU)"));
        assert_eq!(
            installed_state,
            format!(
                "O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)\
                 (A;OICI;0x1301bf;;;{SERVICE_SID})(A;OICI;RC;;;OW)"
            )
        );
    }
}
