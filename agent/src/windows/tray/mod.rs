#[cfg(not(windows))]
pub(crate) fn entry() {
    eprintln!("unionc-agent-tray is available only on Windows");
    std::process::exit(2);
}

#[cfg(windows)]
pub(crate) fn entry() {
    if let Err(error) = windows_tray::entry() {
        windows_tray::show_error(&format!("UnionC Agent tray failed:\n\n{error:#}"));
        std::process::exit(1);
    }
}

/// Reconcile the Agent child's exit with the independent NDJSON reader.
///
/// When the child fails before its first event, the reader necessarily reaches
/// EOF and reports that no authorization key was requested. That is useful only
/// when the child produced no diagnostics; otherwise it hides the real failure.
#[cfg(any(windows, test))]
#[derive(Debug)]
struct MissingAuthorizationKeyEvent;

#[cfg(any(windows, test))]
impl std::fmt::Display for MissingAuthorizationKeyEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Agent did not request an authorization key")
    }
}

#[cfg(any(windows, test))]
impl std::error::Error for MissingAuthorizationKeyEvent {}

#[cfg(any(windows, test))]
fn reconcile_pairing_child(
    child_succeeded: bool,
    child_status: &str,
    event_result: anyhow::Result<()>,
    diagnostics: &str,
) -> anyhow::Result<()> {
    if child_succeeded {
        return event_result
            .map_err(|error| anyhow::anyhow!("pairing event processing failed: {error:#}"));
    }

    match event_result {
        Ok(()) if diagnostics.trim().is_empty() => {
            anyhow::bail!("browser pairing exited with {child_status}")
        }
        Ok(()) => anyhow::bail!(
            "Agent pairing failed ({child_status}): {}",
            diagnostics.trim()
        ),
        Err(error)
            if error
                .downcast_ref::<MissingAuthorizationKeyEvent>()
                .is_some()
                && !diagnostics.trim().is_empty() =>
        {
            anyhow::bail!(
                "Agent pairing failed ({child_status}): {}",
                diagnostics.trim()
            )
        }
        Err(error) if diagnostics.trim().is_empty() => {
            anyhow::bail!("browser pairing exited with {child_status}: {error:#}")
        }
        Err(error) => {
            anyhow::bail!(
                "pairing event processing failed: {error:#}; Agent pairing also failed ({child_status}): {}",
                diagnostics.trim()
            )
        }
    }
}

#[cfg(test)]
mod cross_platform_tests {
    use super::{MissingAuthorizationKeyEvent, reconcile_pairing_child};

    #[test]
    fn child_diagnostics_replace_the_secondary_missing_key_error() {
        let error = reconcile_pairing_child(
            false,
            "exit code 1",
            Err(MissingAuthorizationKeyEvent.into()),
            "Error: UnionC returned a non-JSON pairing response",
        )
        .unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.contains("UnionC returned a non-JSON pairing response"));
        assert!(!rendered.contains("Agent did not request an authorization key"));
    }

    #[test]
    fn missing_key_error_is_retained_when_the_child_has_no_diagnostics() {
        let error = reconcile_pairing_child(
            false,
            "exit code 1",
            Err(MissingAuthorizationKeyEvent.into()),
            "",
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("Agent did not request an authorization key"));
    }

    #[test]
    fn child_stderr_does_not_hide_a_real_event_protocol_error() {
        let event_message = "Agent activation URL origin differs from the confirmed server";
        let error = reconcile_pairing_child(
            false,
            "exit code 1",
            Err(anyhow::anyhow!(event_message)),
            "Error: pairing child was cancelled after the event-stream failure",
        )
        .unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.starts_with("pairing event processing failed"));
        assert!(rendered.contains(event_message));
        assert!(rendered.contains("pairing child was cancelled"));
    }
}

#[cfg(windows)]
mod windows_tray {
    use std::{
        collections::BTreeMap,
        ffi::{OsStr, OsString, c_void},
        fs,
        io::{BufRead, BufReader, Read, Write},
        mem::size_of,
        net::{TcpListener, TcpStream},
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            process::CommandExt,
        },
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    use anyhow::{Context, bail, ensure};
    use serde::{Deserialize, Serialize};
    use unionc_agent::{
        service::WINDOWS_SERVICE_NAME,
        tray_support::{
            MAX_LOCAL_HTTP_BODY_BYTES, MAX_LOCAL_HTTP_HEAD_BYTES, ServiceAction, TrayCommand,
            browser_url_matches_server_origin, constant_time_eq, encode_base64url,
            parse_tray_arguments, quote_windows_argument, validate_activation_code,
            validate_browser_url, validate_server_base,
        },
    };
    use windows::{
        Win32::{
            Foundation::{
                CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY,
                ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING, GENERIC_READ, GENERIC_WRITE,
                GetLastError, HANDLE, HLOCAL, HWND, LPARAM, LRESULT, LocalFree, POINT,
                WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
            },
            Security::{
                Authorization::{
                    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
                },
                GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_ELEVATION,
                TOKEN_QUERY, TokenElevation,
            },
            Storage::FileSystem::{
                CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_MODE,
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_EXISTING,
                PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
                WriteFile,
            },
            System::{
                Com::CoTaskMemFree,
                LibraryLoader::GetModuleHandleW,
                Pipes::{
                    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_NOWAIT,
                    PIPE_REJECT_REMOTE_CLIENTS, PIPE_WAIT, SetNamedPipeHandleState, WaitNamedPipeW,
                },
                Recovery::RegisterApplicationRestart,
                Services::{
                    CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW,
                    QueryServiceStatusEx, SC_HANDLE, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO,
                    SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START,
                    SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_PROCESS, SERVICE_STOP,
                    SERVICE_STOP_PENDING, SERVICE_STOPPED, StartServiceW,
                },
                SystemInformation::GetWindowsDirectoryW,
                Threading::{
                    CreateEventW, CreateMutexW, GetCurrentProcess, GetExitCodeProcess,
                    GetProcessId, OpenProcessToken, SetEvent, WaitForSingleObject,
                },
            },
            UI::{
                Shell::{
                    FOLDERID_LocalAppData, FOLDERID_ProgramData, FOLDERID_ProgramFiles,
                    KF_FLAG_DEFAULT, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                    NIM_MODIFY, NOTIFYICONDATAW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS,
                    SHELLEXECUTEINFOW, SHGetKnownFolderPath, Shell_NotifyIconW, ShellExecuteExW,
                    ShellExecuteW,
                },
                WindowsAndMessaging::{
                    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                    DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, HICON,
                    IDCANCEL, IDI_APPLICATION, IDOK, LoadIconW, MB_ICONERROR, MB_ICONINFORMATION,
                    MB_ICONWARNING, MB_OK, MB_OKCANCEL, MENU_ITEM_FLAGS, MESSAGEBOX_STYLE,
                    MF_STRING, MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW,
                    RegisterWindowMessageW, SW_HIDE, SW_SHOWNORMAL, SetForegroundWindow,
                    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage,
                    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY,
                    WM_LBUTTONDBLCLK, WM_RBUTTONUP, WNDCLASSW,
                },
            },
        },
        core::{PCWSTR, w},
    };

    const ICON_ID: u32 = 1;
    const TRAY_CALLBACK_MESSAGE: u32 = WM_APP + 41;
    const OPEN_CONFIGURATION_MESSAGE: u32 = WM_APP + 42;
    const EXIT_SERVICE_STOPPED_MESSAGE: u32 = WM_APP + 43;
    const REFRESH_TRAY_STATUS_MESSAGE: u32 = WM_APP + 44;
    const WINDOW_CLASS_NAME: &str = "UnionCAgentTrayMessageWindow";
    const COMMAND_OPEN_LOCAL: usize = 1001;
    const COMMAND_PAIR: usize = 1003;
    const COMMAND_STATUS: usize = 1004;
    const COMMAND_SERVICE: usize = 1005;
    const COMMAND_EXIT: usize = 1099;
    const CONFIG_DIRECTORY: &str = "UnionC Agent";
    const CONFIG_FILE: &str = "config.json";
    const AGENT_EXE: &str = "unionc-agent.exe";
    const TRAY_EXE: &str = "unionc-agent-tray.exe";
    const PRIVILEGED_OPERATION_MUTEX: &str =
        "Global\\UnionCAgentOperation-875F84C4-E2A4-4542-8DBF-C474E04826C4";
    const MAX_HTTP_CONNECTIONS: usize = 16;
    const MAX_NDJSON_LINE_BYTES: usize = 16 * 1024;
    const MAX_NDJSON_TOTAL_BYTES: usize = 128 * 1024;
    const MAX_CHILD_STDERR_BYTES: usize = 64 * 1024;
    const MAX_SERVER_HEALTH_BODY_BYTES: u64 = 16 * 1024;
    const SERVER_HEALTH_TIMEOUT: Duration = Duration::from_secs(4);
    const PAIR_OPERATION_TIMEOUT: Duration = Duration::from_secs(20 * 60);
    const PAIR_BROKER_EXIT_GRACE: Duration = Duration::from_secs(2 * 60);
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    static WINDOW_HANDLE: AtomicIsize = AtomicIsize::new(0);
    static TASKBAR_CREATED_MESSAGE: AtomicU32 = AtomicU32::new(0);
    static EXIT_PENDING: AtomicBool = AtomicBool::new(false);
    static LOCAL_SERVER: OnceLock<LocalControlServer> = OnceLock::new();

    // These files are same-module fragments rather than public submodules. The
    // tray workflows share security-sensitive state, so the split improves
    // reviewability without widening visibility or changing synchronization.
    include!("control_state.rs");
    include!("control_server.rs");
    include!("control_routes.rs");
    include!("control_response.rs");

    include!("configuration_ui.rs");

    include!("tray_shell.rs");

    include!("pairing_ipc.rs");

    include!("win32.rs");

    include!("preferences.rs");

    include!("tests.rs");
}
