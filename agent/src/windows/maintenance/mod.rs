#[cfg(any(windows, test))]
use anyhow::{Context, bail, ensure};

#[cfg(any(windows, test))]
fn checked_rollback_path_status(
    path: &std::path::Path,
    label: &str,
    status: std::io::Result<bool>,
) -> anyhow::Result<bool> {
    status.with_context(|| {
        format!(
            "failed to inspect {label} at {} before rollback",
            path.display()
        )
    })
}

#[cfg(windows)]
fn rollback_path_exists(path: &std::path::Path, label: &str) -> anyhow::Result<bool> {
    checked_rollback_path_status(path, label, path.try_exists())
}

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
pub(crate) fn entry() {
    eprintln!("unionc-agent-maintenance is available only on Windows");
    std::process::exit(2);
}

#[cfg(windows)]
pub(crate) fn entry() {
    if let Err(error) = windows_maintenance::run() {
        eprintln!("UnionC Agent maintenance failed: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod windows_maintenance {
    use super::{
        managed_state_security_descriptor, parse_program_dacl, program_security_descriptor,
        rollback_path_exists,
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

    // Keep the installer transaction as one private module while splitting its
    // implementation by responsibility. This avoids widening access to the
    // rollback journal and fixed-path invariants merely to shorten this file.
    include!("transaction.rs");

    include!("filesystem.rs");

    include!("acl.rs");

    include!("service.rs");
    include!("tests.rs");
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

#[cfg(test)]
mod rollback_path_tests {
    use super::*;

    #[test]
    fn metadata_errors_are_not_treated_as_missing_rollback_paths() {
        let path = std::path::Path::new("protected-journal");
        assert!(!checked_rollback_path_status(path, "install journal", Ok(false)).unwrap());

        let error = checked_rollback_path_status(
            path,
            "install journal",
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "simulated metadata denial",
            )),
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("failed to inspect install journal"));
        assert!(message.contains("simulated metadata denial"));
    }
}
