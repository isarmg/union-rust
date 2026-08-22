#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("unionc-agent-tray is available only on Windows");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
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
            validate_browser_url, validate_host_name, validate_server_base,
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

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TrayPreferences {
        application_version: CurrentPackageVersion,
        server: String,
        name: Option<String>,
    }

    #[derive(Clone)]
    struct ExpiringToken {
        value: String,
        expires: Instant,
    }

    #[derive(Clone)]
    struct BrowserSession {
        bearer: String,
        expires: Instant,
    }

    struct LocalControlState {
        bootstrap_tokens: Mutex<Vec<ExpiringToken>>,
        sessions: Mutex<Vec<BrowserSession>>,
        operations: Mutex<Vec<BrowserOperation>>,
        active_pairings: AtomicUsize,
        active_service_operations: AtomicUsize,
        preferences_path: PathBuf,
    }

    #[derive(Clone, Serialize)]
    struct BrowserOperation {
        id: String,
        kind: &'static str,
        phase: &'static str,
        message: String,
        terminal: bool,
        success: Option<bool>,
    }

    fn create_operation(
        state: &Arc<LocalControlState>,
        kind: &'static str,
        phase: &'static str,
        message: impl Into<String>,
    ) -> String {
        let operation = BrowserOperation {
            id: random_secret(),
            kind,
            phase,
            message: message.into(),
            terminal: false,
            success: None,
        };
        let id = operation.id.clone();
        let mut operations = lock(&state.operations);
        if operations.len() >= 32 {
            let remove = operations
                .iter()
                .position(|operation| operation.terminal)
                .unwrap_or(0);
            operations.remove(remove);
        }
        operations.push(operation);
        id
    }

    fn update_operation(
        state: &Arc<LocalControlState>,
        id: &str,
        phase: &'static str,
        message: impl Into<String>,
        outcome: Option<bool>,
    ) {
        if let Some(operation) = lock(&state.operations)
            .iter_mut()
            .find(|operation| operation.id == id)
        {
            operation.phase = phase;
            operation.message = message.into();
            operation.terminal = outcome.is_some();
            operation.success = outcome;
        }
    }

    struct LocalControlServer {
        origin: String,
        state: Arc<LocalControlState>,
    }

    struct ActiveConnection(Arc<AtomicUsize>);

    impl Drop for ActiveConnection {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    struct PairingSlot(Arc<LocalControlState>);

    fn claim_pairing_slot(state: &Arc<LocalControlState>) -> anyhow::Result<PairingSlot> {
        ensure!(
            state
                .active_pairings
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "配对操作正在进行中，请等待其完成后再试"
        );
        Ok(PairingSlot(Arc::clone(state)))
    }

    impl Drop for PairingSlot {
        fn drop(&mut self) {
            self.0.active_pairings.fetch_sub(1, Ordering::AcqRel);
        }
    }

    struct ServiceOperationSlot(Arc<LocalControlState>);

    fn claim_service_operation(
        state: &Arc<LocalControlState>,
    ) -> anyhow::Result<ServiceOperationSlot> {
        ensure!(
            state
                .active_service_operations
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "服务操作正在进行中，请等待其完成后再试"
        );
        Ok(ServiceOperationSlot(Arc::clone(state)))
    }

    impl Drop for ServiceOperationSlot {
        fn drop(&mut self) {
            self.0
                .active_service_operations
                .fetch_sub(1, Ordering::AcqRel);
        }
    }

    struct SensitiveActivationCode(Vec<u8>);

    impl SensitiveActivationCode {
        fn new(value: String) -> Self {
            Self(value.into_bytes())
        }

        fn as_bytes(&self) -> &[u8] {
            &self.0
        }
    }

    impl Drop for SensitiveActivationCode {
        fn drop(&mut self) {
            self.0.fill(0);
        }
    }

    struct PairIpcServer {
        pipe: TransferHandle,
    }

    impl PairIpcServer {
        fn create(nonce: &str) -> anyhow::Result<Self> {
            validate_callback_nonce(nonce)?;
            let name = wide(&pair_pipe_name(nonce));
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    w!("D:P(A;;GA;;;BA)(A;;GA;;;SY)"),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    None,
                )
            }
            .context("failed to create the protected pairing pipe descriptor")?;
            let _descriptor = LocalSecurityDescriptor(descriptor);
            let attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            };
            let pipe = unsafe {
                CreateNamedPipeW(
                    PCWSTR(name.as_ptr()),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    MAX_LOCAL_HTTP_BODY_BYTES as u32,
                    MAX_LOCAL_HTTP_BODY_BYTES as u32,
                    0,
                    Some(&attributes),
                )
            };
            ensure!(
                !pipe.is_invalid(),
                "failed to create the protected pairing pipe: {}",
                std::io::Error::last_os_error()
            );
            Ok(Self {
                pipe: TransferHandle::new(KernelHandle(pipe)),
            })
        }

        fn serve(
            self,
            process: TransferHandle,
            server: &str,
            activation_code: SensitiveActivationCode,
        ) -> anyhow::Result<()> {
            let pipe = self.pipe.into_kernel();
            let process = process.into_kernel();
            let expected_pid = unsafe { GetProcessId(process.0) };
            ensure!(
                expected_pid != 0,
                "failed to identify the elevated pairing broker"
            );
            let deadline = Instant::now() + PAIR_OPERATION_TIMEOUT;
            loop {
                match unsafe { ConnectNamedPipe(pipe.0, None) } {
                    Ok(()) => break,
                    Err(_) => match unsafe { GetLastError() } {
                        ERROR_PIPE_CONNECTED => break,
                        ERROR_PIPE_LISTENING => {}
                        error => bail!("protected pairing pipe connection failed: {error:?}"),
                    },
                }
                match unsafe { WaitForSingleObject(process.0, 0) } {
                    WAIT_TIMEOUT => {}
                    WAIT_OBJECT_0 => {
                        let mut exit_code = u32::MAX;
                        unsafe { GetExitCodeProcess(process.0, &mut exit_code) }
                            .context("failed to inspect the elevated pairing broker")?;
                        bail!(
                            "the elevated pairing broker exited before requesting the authorization key (exit code {exit_code})"
                        );
                    }
                    result => bail!("failed to wait for the elevated pairing broker: {result:?}"),
                }
                ensure!(
                    Instant::now() < deadline,
                    "timed out waiting for the elevated pairing broker"
                );
                thread::sleep(Duration::from_millis(100));
            }

            let mut client_pid = 0_u32;
            unsafe { GetNamedPipeClientProcessId(pipe.0, &mut client_pid) }
                .context("failed to identify the pairing pipe client")?;
            ensure!(
                client_pid == expected_pid,
                "pairing pipe client is not the broker launched by this tray"
            );
            let mode = PIPE_WAIT;
            unsafe { SetNamedPipeHandleState(pipe.0, Some(&mode), None, None) }
                .context("failed to enter blocking pairing pipe mode")?;
            let message = read_pipe_frame(pipe.0, MAX_LOCAL_HTTP_BODY_BYTES)?;
            let message: PairIpcMessage =
                serde_json::from_slice(&message).context("invalid pairing pipe message")?;
            validate_pair_ipc_message(&message, server)?;
            write_pipe_frame(pipe.0, activation_code.as_bytes())?;
            // The pipe and Agent stdin now own the transient transport copies;
            // erase the standard tray's retained allocation before the long
            // activation/poll wait.
            drop(activation_code);

            // Sending the key is only the midpoint of pairing: the Agent still
            // has to activate, poll, commit credentials, and the broker may
            // need to restart the service. Keep the standard-user pairing slot
            // until this exact ShellExecuteEx process exits successfully so a
            // second click cannot launch another UAC prompt mid-transaction.
            let process_deadline = Instant::now() + PAIR_OPERATION_TIMEOUT + PAIR_BROKER_EXIT_GRACE;
            let remaining = process_deadline.saturating_duration_since(Instant::now());
            let wait_millis = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX - 1);
            let wait = unsafe { WaitForSingleObject(process.0, wait_millis) };
            ensure!(
                wait == WAIT_OBJECT_0,
                "timed out or failed while waiting for the elevated pairing broker ({wait:?})"
            );
            let mut exit_code = u32::MAX;
            unsafe { GetExitCodeProcess(process.0, &mut exit_code) }
                .context("failed to inspect the elevated pairing broker result")?;
            ensure!(
                exit_code == 0,
                "elevated pairing broker failed with exit code {exit_code}"
            );
            Ok(())
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

    #[derive(Debug)]
    struct LocalHttpRequest {
        method: String,
        target: String,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    }

    struct HttpResponse {
        status: &'static str,
        content_type: &'static str,
        extra_headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PairRequest {
        server: String,
        name: String,
        activation_code: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ServiceRequest {
        action: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OperationRequest {
        id: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StateRequest {}

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ConnectionRequest {
        server: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ServerHealthResponse {
        status: String,
        version: String,
        #[serde(rename = "uptime_seconds")]
        _uptime_seconds: i64,
    }

    struct ServerConnectionStatus {
        status: &'static str,
        message: String,
        version: Option<String>,
        latency_ms: Option<u64>,
    }

    #[derive(Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct PairIpcMessage {
        generation: String,
        request_id: String,
        activation_url: String,
        pairing_endpoint: String,
    }

    #[derive(Debug, Clone, Copy, Default)]
    struct CurrentPackageVersion;

    impl Serialize for CurrentPackageVersion {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            serializer.serialize_str(env!("CARGO_PKG_VERSION"))
        }
    }

    impl<'de> Deserialize<'de> for CurrentPackageVersion {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            let version = String::deserialize(deserializer)?;
            if version == env!("CARGO_PKG_VERSION") {
                Ok(Self)
            } else {
                Err(serde::de::Error::custom(format!(
                    "pairing event belongs to Agent {version}, expected {}",
                    env!("CARGO_PKG_VERSION")
                )))
            }
        }
    }

    #[derive(Deserialize)]
    #[serde(tag = "event", deny_unknown_fields)]
    enum PairEvent {
        #[serde(rename = "pairing_waiting")]
        PairingWaiting {
            #[serde(rename = "version")]
            _version: CurrentPackageVersion,
            generation: String,
            request_id: String,
            activation_url: String,
            pairing_endpoint: String,
            #[serde(rename = "expires_at")]
            _expires_at: String,
            #[serde(rename = "poll_interval")]
            _poll_interval: u64,
        },
        #[serde(rename = "paired")]
        Paired {
            #[serde(rename = "version")]
            _version: CurrentPackageVersion,
            request_id: String,
            instance_id: String,
            #[serde(rename = "endpoint")]
            _endpoint: String,
        },
        #[serde(rename = "pairing_interrupted")]
        PairingInterrupted {
            #[serde(rename = "version")]
            _version: CurrentPackageVersion,
            #[serde(rename = "request_id")]
            _request_id: String,
        },
        #[serde(rename = "pairing_cancelled")]
        PairingCancelled {
            #[serde(rename = "version")]
            _version: CurrentPackageVersion,
        },
        #[serde(rename = "pairing_timeout")]
        PairingTimeout {
            #[serde(rename = "version")]
            _version: CurrentPackageVersion,
        },
    }

    pub fn entry() -> anyhow::Result<()> {
        let arguments = std::env::args_os()
            .skip(1)
            .map(|argument| {
                argument
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("tray arguments must be valid Unicode"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        match parse_tray_arguments(arguments)? {
            TrayCommand::Tray { open } => run_tray(open),
            TrayCommand::ElevatedPair {
                server,
                name,
                callback_nonce,
            } => elevated_pair(server, name, callback_nonce),
            TrayCommand::ElevatedService { action, notify } => elevated_service(action, notify),
            TrayCommand::ElevatedStopForExit => elevated_stop_for_exit(),
        }
    }

    pub fn show_error(message: &str) {
        message_box(message, "UnionC Agent", MB_OK | MB_ICONERROR);
    }

    impl LocalControlServer {
        fn start() -> anyhow::Result<Self> {
            let listener = TcpListener::bind(("127.0.0.1", 0))
                .context("failed to bind the private local configuration server")?;
            let port = listener.local_addr()?.port();
            let origin = format!("http://127.0.0.1:{port}");
            let preferences_path = local_preferences_path()?;
            let state = Arc::new(LocalControlState {
                bootstrap_tokens: Mutex::new(Vec::new()),
                sessions: Mutex::new(Vec::new()),
                operations: Mutex::new(Vec::new()),
                active_pairings: AtomicUsize::new(0),
                active_service_operations: AtomicUsize::new(0),
                preferences_path,
            });
            let thread_state = Arc::clone(&state);
            let thread_origin = origin.clone();
            thread::Builder::new()
                .name("unionc-tray-http".into())
                .spawn(move || serve_local(listener, thread_origin, thread_state))
                .context("failed to start the private local configuration server")?;
            Ok(Self { origin, state })
        }

        fn open_configuration(&self) -> anyhow::Result<()> {
            self.open_configuration_at("status")
        }

        fn open_configuration_at(&self, section: &str) -> anyhow::Result<()> {
            ensure!(
                matches!(section, "status" | "pair"),
                "invalid local page section"
            );
            let token = random_secret();
            {
                let mut tokens = lock(&self.state.bootstrap_tokens);
                retain_valid_tokens(&mut tokens);
                ensure!(tokens.len() < 32, "too many pending local browser sessions");
                tokens.push(ExpiringToken {
                    value: token.clone(),
                    expires: Instant::now() + Duration::from_secs(5 * 60),
                });
            }
            if let Err(error) = open_browser(&format!("{}/#{token}:{section}", self.origin)) {
                let mut tokens = lock(&self.state.bootstrap_tokens);
                if let Some(index) = tokens.iter().position(|candidate| {
                    constant_time_eq(candidate.value.as_bytes(), token.as_bytes())
                }) {
                    tokens.remove(index);
                }
                return Err(error);
            }
            Ok(())
        }
    }

    fn serve_local(listener: TcpListener, origin: String, state: Arc<LocalControlState>) {
        let active = Arc::new(AtomicUsize::new(0));
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else {
                continue;
            };
            if active.fetch_add(1, Ordering::AcqRel) >= MAX_HTTP_CONNECTIONS {
                active.fetch_sub(1, Ordering::AcqRel);
                let _ = write_response(
                    &mut stream,
                    response_json(
                        "503 Service Unavailable",
                        serde_json::json!({
                            "status": "error",
                            "code": "local_control_busy",
                            "message": "本地控制服务繁忙，请稍后重试"
                        }),
                    ),
                );
                continue;
            }
            let state = Arc::clone(&state);
            let origin = origin.clone();
            let active = ActiveConnection(Arc::clone(&active));
            let _ = thread::Builder::new()
                .name("unionc-tray-http-request".into())
                .spawn(move || {
                    let _active = active;
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                    let response = match read_request(&mut stream)
                        .and_then(|request| route_request(request, &origin, &state))
                    {
                        Ok(response) => response,
                        Err(error) => local_api_error_response(&error),
                    };
                    let _ = write_response(&mut stream, response);
                });
        }
    }

    fn local_api_error_response(error: &anyhow::Error) -> HttpResponse {
        let message = error.to_string();
        let (status, code, public_message) =
            if message.contains("session") || message.contains("browser capability") {
                (
                    "401 Unauthorized",
                    "session_expired",
                    "本地安全会话已失效，请从托盘菜单重新打开此页面".to_string(),
                )
            } else if message.contains("正在进行") || message.contains("too many") {
                (
                    "409 Conflict",
                    "operation_conflict",
                    "另一个操作正在进行，请等待其完成后再试".to_string(),
                )
            } else if message.contains("service is not installed")
                || message.contains("Service Control Manager")
            {
                (
                    "503 Service Unavailable",
                    "service_unavailable",
                    "Windows Agent 服务不可用，请检查安装状态".to_string(),
                )
            } else {
                ("400 Bad Request", "invalid_request", message)
            };
        response_json(
            status,
            serde_json::json!({
                "status": "error",
                "code": code,
                "message": public_message
            }),
        )
    }

    fn read_request(stream: &mut TcpStream) -> anyhow::Result<LocalHttpRequest> {
        let mut received = Vec::new();
        let header_end = loop {
            if let Some(index) = find_bytes(&received, b"\r\n\r\n") {
                break index + 4;
            }
            ensure!(
                received.len() < MAX_LOCAL_HTTP_HEAD_BYTES,
                "request headers are too large"
            );
            let mut chunk = [0_u8; 2048];
            let read = stream.read(&mut chunk)?;
            ensure!(read != 0, "connection closed before request headers");
            received.extend_from_slice(&chunk[..read]);
        };
        ensure!(
            header_end <= MAX_LOCAL_HTTP_HEAD_BYTES,
            "request headers are too large"
        );
        let head = std::str::from_utf8(&received[..header_end - 4])
            .context("request headers are not valid UTF-8")?;
        ensure!(head.is_ascii(), "request headers must be ASCII");
        let mut lines = head.split("\r\n");
        let request_line = lines.next().context("missing request line")?;
        let parts = request_line.split_ascii_whitespace().collect::<Vec<_>>();
        ensure!(parts.len() == 3, "malformed request line");
        ensure!(parts[2] == "HTTP/1.1", "only HTTP/1.1 is supported");
        ensure!(
            matches!(parts[0], "GET" | "POST"),
            "unsupported HTTP method"
        );
        ensure!(
            parts[1].starts_with('/'),
            "request target must be origin-form"
        );
        ensure!(
            !parts[1].contains(['?', '#']),
            "query strings and fragments are not accepted"
        );
        let method = parts[0].to_string();
        let target = parts[1].to_string();
        let mut headers = BTreeMap::new();
        for line in lines {
            ensure!(
                !line.starts_with([' ', '\t']),
                "folded headers are not accepted"
            );
            let (name, value) = line.split_once(':').context("malformed request header")?;
            ensure!(
                !name.is_empty()
                    && name.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(
                                byte,
                                b'!' | b'#'
                                    ..=b'\''
                                        | b'*'
                                        | b'+'
                                        | b'-'
                                        | b'.'
                                        | b'^'
                                        | b'_'
                                        | b'`'
                                        | b'|'
                                        | b'~'
                            )
                    }),
                "invalid request header name"
            );
            let name = name.to_ascii_lowercase();
            let value = value.trim().to_string();
            ensure!(
                headers.insert(name.clone(), value).is_none(),
                "duplicate request header: {name}"
            );
        }
        ensure!(
            !headers.contains_key("transfer-encoding"),
            "transfer encoding is not accepted"
        );
        let content_length = headers
            .get("content-length")
            .map(|value| value.parse::<usize>().context("invalid Content-Length"))
            .transpose()?
            .unwrap_or(0);
        ensure!(
            content_length <= MAX_LOCAL_HTTP_BODY_BYTES,
            "request body is too large"
        );
        let expected = header_end + content_length;
        ensure!(
            received.len() <= expected,
            "HTTP pipelining is not accepted"
        );
        while received.len() < expected {
            let remaining = expected - received.len();
            let mut chunk = [0_u8; 2048];
            let read_length = remaining.min(chunk.len());
            let read = stream.read(&mut chunk[..read_length])?;
            ensure!(read != 0, "connection closed before request body");
            received.extend_from_slice(&chunk[..read]);
        }
        Ok(LocalHttpRequest {
            method,
            target,
            headers,
            body: received[header_end..].to_vec(),
        })
    }

    fn route_request(
        request: LocalHttpRequest,
        origin: &str,
        state: &Arc<LocalControlState>,
    ) -> anyhow::Result<HttpResponse> {
        ensure!(
            request.headers.get("host").is_some_and(|host| {
                constant_time_eq(
                    host.as_bytes(),
                    origin.trim_start_matches("http://").as_bytes(),
                )
            }),
            "Host header does not match the loopback listener"
        );
        if request.method == "GET" && request.target == "/app.js" {
            ensure!(request.body.is_empty(), "GET request must not have a body");
            return Ok(response_javascript(APP_JAVASCRIPT));
        }
        if request.method == "POST" && request.target == "/session" {
            require_origin(&request, origin)?;
            require_json_request(&request)?;
            require_control_marker(&request)?;
            ensure!(
                request.body == b"{}",
                "session request body must be empty JSON"
            );
            let supplied = bearer(&request)?;
            let accepted = {
                let mut tokens = lock(&state.bootstrap_tokens);
                retain_valid_tokens(&mut tokens);
                tokens
                    .iter()
                    .position(|token| constant_time_eq(token.value.as_bytes(), supplied.as_bytes()))
                    .map(|index| tokens.remove(index))
            };
            ensure!(accepted.is_some(), "invalid or expired browser capability");
            let session = BrowserSession {
                bearer: random_secret(),
                expires: Instant::now() + Duration::from_secs(8 * 60 * 60),
            };
            let bearer = session.bearer.clone();
            let mut sessions = lock(&state.sessions);
            sessions.retain(|session| session.expires > Instant::now());
            ensure!(
                sessions.len() < 32,
                "too many active local browser sessions"
            );
            sessions.push(session);
            return Ok(response_json(
                "200 OK",
                serde_json::json!({ "bearer": bearer }),
            ));
        }
        if request.method == "GET" && request.target == "/" {
            ensure!(request.body.is_empty(), "GET request must not have a body");
            return Ok(render_configuration());
        }
        if request.method == "POST" {
            require_origin(&request, origin)?;
            require_json_request(&request)?;
            require_control_marker(&request)?;
            authenticate_session(&request, state)?;
            return match request.target.as_str() {
                "/state" => local_state_response(&request.body, state),
                "/connection" => server_connection_response(&request.body, state),
                "/pair" => start_pair_from_browser(&request.body, state),
                "/service" => change_service_from_browser(&request.body, state),
                "/operation" => operation_response(&request.body, state),
                _ => bail!("unknown local-control route"),
            };
        }
        bail!("unknown local-control route")
    }

    fn operation_response(
        body: &[u8],
        state: &Arc<LocalControlState>,
    ) -> anyhow::Result<HttpResponse> {
        let request: OperationRequest =
            serde_json::from_slice(body).context("invalid operation request JSON")?;
        ensure!(
            request.id.len() == 64 && request.id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "operation id is invalid"
        );
        let operation = lock(&state.operations)
            .iter()
            .find(|operation| constant_time_eq(operation.id.as_bytes(), request.id.as_bytes()))
            .cloned()
            .context("operation was not found or has expired")?;
        Ok(response_json(
            "200 OK",
            serde_json::to_value(operation).context("failed to serialize operation status")?,
        ))
    }

    fn start_pair_from_browser(
        body: &[u8],
        state: &Arc<LocalControlState>,
    ) -> anyhow::Result<HttpResponse> {
        let request: PairRequest =
            serde_json::from_slice(body).context("invalid pairing request JSON")?;
        let server = validate_server_base(&request.server)?;
        let name = validate_host_name(&request.name)?;
        validate_activation_code(&request.activation_code)?;
        let pairing_slot = claim_pairing_slot(state)?;
        let callback_nonce = random_secret();
        let ipc = PairIpcServer::create(&callback_nonce)?;
        let activation_code = SensitiveActivationCode::new(request.activation_code);
        let preferences_path = state.preferences_path.clone();
        let successful_preferences = TrayPreferences {
            application_version: CurrentPackageVersion,
            server: server.clone(),
            name: name.clone(),
        };
        let process = TransferHandle::new(launch_elevated_pair(
            &server,
            name.as_deref(),
            callback_nonce,
        )?);
        let operation_id = create_operation(
            state,
            "pair",
            "awaiting_uac",
            "请确认 Windows 用户账户控制提示",
        );
        let worker_operation_id = operation_id.clone();
        let operation_state = Arc::clone(state);
        let server_for_ipc = server.clone();
        thread::Builder::new()
            .name("unionc-pair-ipc".into())
            .spawn(move || {
                update_operation(
                    &operation_state,
                    &worker_operation_id,
                    "pairing",
                    "正在创建请求、提交授权并等待 Server 确认",
                    None,
                );
                let outcome = ipc
                    .serve(process, &server_for_ipc, activation_code)
                    .and_then(|()| {
                        save_preferences(&preferences_path, &successful_preferences)
                            .context("pairing succeeded but tray preferences could not be saved")
                    });
                // The broker has now exited (or the bounded wait failed), so
                // release exclusivity before showing any user-dismissed UI.
                drop(pairing_slot);
                match outcome {
                    Ok(()) => {
                        let service = query_service_state()
                            .map(|state| state.label().to_string())
                            .unwrap_or_else(|error| format!("无法查询：{error}"));
                        update_operation(
                            &operation_state,
                            &worker_operation_id,
                            "completed",
                            format!("配对成功；当前 Agent 服务状态：{service}"),
                            Some(true),
                        );
                    }
                    Err(error) => {
                        let rendered = format!("{error:#}");
                        let cancelled = rendered.contains(
                            "exited before requesting the authorization key (exit code 0)",
                        );
                        update_operation(
                            &operation_state,
                            &worker_operation_id,
                            if cancelled { "cancelled" } else { "failed" },
                            if cancelled {
                                "配对已由用户取消".to_string()
                            } else {
                                format!("配对失败：{error}")
                            },
                            Some(false),
                        );
                        if !cancelled {
                            show_error("UnionC Agent 配对失败；请在本地配置页面查看详情并重试。");
                        }
                    }
                }
                notify_tray_status_changed();
            })
            .context("failed to start the protected pairing channel")?;
        Ok(response_json(
            "202 Accepted",
            serde_json::json!({
                "status": "elevation_requested",
                "operation_id": operation_id,
                "message": "请确认 Windows 用户账户控制提示；授权密钥将由托盘直接提交，配对会自动完成。"
            }),
        ))
    }

    fn change_service_from_browser(
        body: &[u8],
        state: &Arc<LocalControlState>,
    ) -> anyhow::Result<HttpResponse> {
        let request: ServiceRequest =
            serde_json::from_slice(body).context("invalid service request JSON")?;
        let action = match request.action.as_str() {
            "start" => ServiceAction::Start,
            "stop" => ServiceAction::Stop,
            _ => bail!("service action must be start or stop"),
        };
        let operation_slot = claim_service_operation(state)?;
        let process = TransferHandle::new(
            launch_elevated_process(
                &[
                    "--elevated-service-browser".into(),
                    match action {
                        ServiceAction::Start => "start".into(),
                        ServiceAction::Stop => "stop".into(),
                    },
                ],
                true,
            )?
            .context("Windows did not return the elevated service process handle")?,
        );
        let operation_id = create_operation(
            state,
            "service",
            "awaiting_uac",
            "请确认 Windows 用户账户控制提示",
        );
        let worker_operation_id = operation_id.clone();
        let operation_state = Arc::clone(state);
        thread::Builder::new()
            .name("unionc-browser-service-operation".into())
            .spawn(move || {
                update_operation(
                    &operation_state,
                    &worker_operation_id,
                    "changing_service",
                    match action {
                        ServiceAction::Start => "正在启动 Agent 服务",
                        ServiceAction::Stop => "正在停止 Agent 服务",
                    },
                    None,
                );
                let outcome = wait_for_elevated_service_action(process, action);
                drop(operation_slot);
                match outcome {
                    Ok(()) => update_operation(
                        &operation_state,
                        &worker_operation_id,
                        "completed",
                        match action {
                            ServiceAction::Start => "Agent 服务已启动",
                            ServiceAction::Stop => "Agent 服务已停止；下次开机仍会自动启动",
                        },
                        Some(true),
                    ),
                    Err(error) => update_operation(
                        &operation_state,
                        &worker_operation_id,
                        "failed",
                        format!("服务操作失败：{error}"),
                        Some(false),
                    ),
                }
                notify_tray_status_changed();
            })
            .context("failed to start the browser service-operation waiter")?;
        Ok(response_json(
            "202 Accepted",
            serde_json::json!({
                "status": "elevation_requested",
                "operation_id": operation_id,
                "message": "请确认 Windows 用户账户控制提示。"
            }),
        ))
    }

    fn authenticate_session(
        request: &LocalHttpRequest,
        state: &Arc<LocalControlState>,
    ) -> anyhow::Result<()> {
        let supplied = bearer(request)?;
        let mut sessions = lock(&state.sessions);
        sessions.retain(|session| session.expires > Instant::now());
        ensure!(
            sessions
                .iter()
                .any(|session| constant_time_eq(session.bearer.as_bytes(), supplied.as_bytes())),
            "browser session is missing or expired"
        );
        Ok(())
    }

    fn local_state_response(
        body: &[u8],
        state: &Arc<LocalControlState>,
    ) -> anyhow::Result<HttpResponse> {
        let _: StateRequest = serde_json::from_slice(body).context("invalid state request JSON")?;
        let preferences = load_preferences(&state.preferences_path).unwrap_or_default();
        let service_state = query_service_state();
        let service = service_state
            .as_ref()
            .map(|state| state.label().to_string())
            .unwrap_or_else(|error| format!("不可用：{error}"));
        let service_code = service_state
            .map(ServiceState::code)
            .unwrap_or("unavailable");
        Ok(response_json(
            "200 OK",
            serde_json::json!({
                "service": service,
                "service_code": service_code,
                "server": preferences.server,
                "name": preferences.name,
                "version": env!("CARGO_PKG_VERSION")
            }),
        ))
    }

    fn server_connection_response(
        body: &[u8],
        state: &Arc<LocalControlState>,
    ) -> anyhow::Result<HttpResponse> {
        let request: ConnectionRequest =
            serde_json::from_slice(body).context("invalid connection request JSON")?;
        let server = if request.server.trim().is_empty() {
            load_preferences(&state.preferences_path)
                .unwrap_or_default()
                .server
        } else {
            request.server
        };
        let connection = probe_server_connection(&server);
        Ok(response_json(
            "200 OK",
            serde_json::json!({
                "status": connection.status,
                "message": connection.message,
                "version": connection.version,
                "latency_ms": connection.latency_ms
            }),
        ))
    }

    /// Lightweight management-origin reachability check for the standard-user tray.
    ///
    /// This deliberately calls only the public `/api/health` endpoint and never reads
    /// ProgramData credentials. It answers "can this desktop session reach UnionC?";
    /// the Server's host list remains authoritative for authenticated telemetry recency.
    fn probe_server_connection(server: &str) -> ServerConnectionStatus {
        if server.trim().is_empty() {
            return ServerConnectionStatus {
                status: "unconfigured",
                message: "尚未配置 Server 地址".to_string(),
                version: None,
                latency_ms: None,
            };
        }
        let server = match validate_server_base(server) {
            Ok(server) => server,
            Err(_) => {
                return ServerConnectionStatus {
                    status: "offline",
                    message: "Server 地址无效".to_string(),
                    version: None,
                    latency_ms: None,
                };
            }
        };
        let mut health_url = match reqwest::Url::parse(&server) {
            Ok(url) => url,
            Err(_) => {
                return ServerConnectionStatus {
                    status: "offline",
                    message: "Server 地址无效".to_string(),
                    version: None,
                    latency_ms: None,
                };
            }
        };
        health_url.set_path("/api/health");
        health_url.set_query(None);
        health_url.set_fragment(None);
        let client = match reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(SERVER_HEALTH_TIMEOUT)
            .build()
        {
            Ok(client) => client,
            Err(_) => {
                return ServerConnectionStatus {
                    status: "offline",
                    message: "无法初始化 Server 连接检测".to_string(),
                    version: None,
                    latency_ms: None,
                };
            }
        };
        let started = Instant::now();
        let response = match client
            .get(health_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
        {
            Ok(response) => response,
            Err(error) => {
                let message = if error.is_timeout() {
                    "Server 连接超时"
                } else if error.is_connect() {
                    "无法连接 Server（请检查地址、端口、网络或 TLS）"
                } else {
                    "Server 健康检查请求失败"
                };
                return ServerConnectionStatus {
                    status: "offline",
                    message: message.to_string(),
                    version: None,
                    latency_ms: None,
                };
            }
        };
        let status = response.status();
        if status != reqwest::StatusCode::OK {
            return ServerConnectionStatus {
                status: "offline",
                message: format!("Server 返回 HTTP {}", status.as_u16()),
                version: None,
                latency_ms: Some(elapsed_milliseconds(started)),
            };
        }
        let content_type_is_json = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(is_application_json);
        if !content_type_is_json {
            return ServerConnectionStatus {
                status: "offline",
                message: "Server 健康响应的 Content-Type 不是 application/json".to_string(),
                version: None,
                latency_ms: Some(elapsed_milliseconds(started)),
            };
        }
        let mut body = Vec::new();
        if response
            .take(MAX_SERVER_HEALTH_BODY_BYTES + 1)
            .read_to_end(&mut body)
            .is_err()
            || body.len() as u64 > MAX_SERVER_HEALTH_BODY_BYTES
        {
            return ServerConnectionStatus {
                status: "offline",
                message: "Server 健康响应读取失败或过大".to_string(),
                version: None,
                latency_ms: Some(elapsed_milliseconds(started)),
            };
        }
        let health: ServerHealthResponse = match serde_json::from_slice(&body) {
            Ok(health) => health,
            Err(_) => {
                return ServerConnectionStatus {
                    status: "offline",
                    message: "Server 未返回可用的 UnionC 健康状态（格式或版本信息无效）"
                        .to_string(),
                    version: None,
                    latency_ms: Some(elapsed_milliseconds(started)),
                };
            }
        };
        let ServerHealthResponse {
            status,
            version,
            _uptime_seconds: _,
        } = health;
        if status != "ok" {
            return ServerConnectionStatus {
                status: "offline",
                message: "Server 健康状态不可用".to_string(),
                version: None,
                latency_ms: Some(elapsed_milliseconds(started)),
            };
        }
        let latency_ms = elapsed_milliseconds(started);
        if version.trim().is_empty() || version.len() > 128 || version.chars().any(char::is_control)
        {
            return ServerConnectionStatus {
                status: "offline",
                message: "Server 版本信息不可用".to_string(),
                version: None,
                latency_ms: Some(latency_ms),
            };
        }
        if version != env!("CARGO_PKG_VERSION") {
            return ServerConnectionStatus {
                status: "offline",
                message: format!(
                    "Server 版本不匹配：需要 v{}，实际 v{version}",
                    env!("CARGO_PKG_VERSION")
                ),
                version: Some(version),
                latency_ms: Some(latency_ms),
            };
        }
        let message = format!("连接正常 · Server v{version} · {latency_ms} ms");
        ServerConnectionStatus {
            status: "online",
            message,
            version: Some(version),
            latency_ms: Some(latency_ms),
        }
    }

    fn elapsed_milliseconds(started: Instant) -> u64 {
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn is_application_json(value: &str) -> bool {
        value
            .split(';')
            .next()
            .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
    }

    fn render_configuration() -> HttpResponse {
        response_html(
            "200 OK",
            "<main id=app aria-busy=true><header><div><h1>UnionC Agent</h1><p class=subtitle>Windows 本地控制与配对</p></div><span id=version class=version></span></header>\
                 <div class=status-grid aria-label=Agent状态>\
                 <section class=status-card><h2>Windows 服务</h2><strong id=service aria-live=polite>正在建立安全的本地会话…</strong><p id=service-detail class=hint>服务状态来自 Windows SCM。</p></section>\
                 <section class=status-card><h2>管理端可达性</h2><strong id=connection aria-live=polite>尚未检测</strong><button id=check-connection class=secondary type=button>立即检测</button><p class=hint>这里只检查管理端 /api/health，不代表遥测已成功上报。</p></section></div>\
                 <section id=pair><h2>配对或重新配对</h2><p class=hint>配对会替换此机器当前绑定，并在必要时重启 Agent 服务。</p>\
                 <form data-endpoint=/pair><label for=server>服务器地址</label><input id=server name=server type=url maxlength=2048 required aria-describedby=server-hint placeholder=\"https://unionc.example.com\">\
                 <p id=server-hint class=hint>填写完整管理台 Origin（协议、主机和可选端口），不要附加路径。</p>\
                 <label for=device-name>设备名称（可选）</label><input id=device-name name=name maxlength=255>\
                 <label for=activation-code>一次性授权密钥</label><input id=activation-code name=activation_code type=password maxlength=256 required autocomplete=one-time-code spellcheck=false aria-describedby=code-hint placeholder=\"uci_…\">\
                 <p id=code-hint class=hint>密钥只通过受保护的本地通道提交，不会保存到托盘偏好。</p>\
                 <button id=pair-submit type=submit>开始配对</button></form></section>\
                 <section><h2>服务控制</h2><p class=hint>停止只影响本次开机；服务启动类型保持 Automatic，下次开机仍会自动运行。</p><div class=actions>\
                 <button data-service=start>启动服务</button><button data-service=stop class=danger>停止本次服务</button><button id=copy-diagnostics class=secondary type=button>复制脱敏诊断</button></div></section>\
                 <div id=result class=result role=status aria-live=polite></div>\
                 <footer>关闭此浏览器页面不会停止 Agent。若要本次彻底退出，请使用托盘菜单“停止本次服务并退出托盘”。</footer></main><script src=/app.js defer></script>",
        )
    }

    fn require_origin(request: &LocalHttpRequest, origin: &str) -> anyhow::Result<()> {
        ensure!(
            request
                .headers
                .get("origin")
                .is_some_and(|value| constant_time_eq(value.as_bytes(), origin.as_bytes())),
            "Origin header does not match the loopback listener"
        );
        Ok(())
    }

    fn require_json_request(request: &LocalHttpRequest) -> anyhow::Result<()> {
        ensure!(
            request
                .headers
                .get("content-type")
                .is_some_and(|value| value.eq_ignore_ascii_case("application/json")),
            "Content-Type must be application/json"
        );
        ensure!(
            request.headers.contains_key("content-length"),
            "Content-Length is required"
        );
        Ok(())
    }

    fn require_control_marker(request: &LocalHttpRequest) -> anyhow::Result<()> {
        ensure!(
            request
                .headers
                .get("x-unionc-tray")
                .is_some_and(|value| value == "1"),
            "missing local-control request marker"
        );
        Ok(())
    }

    fn bearer(request: &LocalHttpRequest) -> anyhow::Result<&str> {
        request
            .headers
            .get("authorization")
            .and_then(|value| value.strip_prefix("Bearer "))
            .filter(|value| value.len() == 64)
            .context("missing or invalid bearer capability")
    }

    fn response_html(status: &'static str, main: &str) -> HttpResponse {
        let body = format!(
            "<!doctype html><html lang=zh-CN><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>UnionC Agent</title><style>\
             :root{{font-family:Segoe UI,system-ui,sans-serif;color-scheme:light dark;line-height:1.45}}*{{box-sizing:border-box}}body{{margin:0;padding:24px;background:#f4f7fb;color:#172033}}main{{width:min(760px,100%);margin:3vh auto;padding:28px;background:#fff;border-radius:18px;box-shadow:0 12px 40px #16345b22}}header{{display:flex;justify-content:space-between;gap:20px;align-items:flex-start}}h1{{margin:0}}h2{{font-size:1.05rem;margin:0 0 12px}}.subtitle,.version,footer{{color:#58677c}}section{{border-top:1px solid #dce3ed;padding-top:20px;margin-top:24px}}.status-grid{{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:14px;margin-top:22px}}.status-card{{margin:0;padding:18px;border:1px solid #dce3ed;border-radius:12px}}label{{display:block;font-weight:600;margin:16px 0 6px}}input{{display:block;width:100%;padding:11px 12px;border:1px solid #aeb9c8;border-radius:8px;font:inherit}}button{{border:0;border-radius:8px;padding:10px 15px;background:#1769e0;color:#fff;font:inherit;font-weight:600;cursor:pointer}}button.secondary{{margin-top:12px;background:#e7eef9;color:#164b94}}button.danger{{background:#b42318}}button:disabled{{opacity:.55;cursor:not-allowed}}button:focus-visible,input:focus-visible{{outline:3px solid #75a7ff;outline-offset:2px}}.actions{{display:flex;flex-wrap:wrap;gap:10px}}.hint{{margin:7px 0;color:#58677c;font-size:.92rem}}.result{{min-height:3em;margin-top:22px;padding:12px 14px;border-radius:9px;background:#eef4ff}}.result[data-kind=error]{{background:#fde8e7;color:#8f1d16}}.result[data-kind=success]{{background:#e6f6ec;color:#176b39}}footer{{border-top:1px solid #dce3ed;margin-top:24px;padding-top:18px;font-size:.88rem}}@media(max-width:620px){{body{{padding:10px}}main{{padding:20px;margin:0 auto}}header{{display:block}}.status-grid{{grid-template-columns:1fr}}}}@media(prefers-color-scheme:dark){{body{{background:#10141d;color:#edf3ff}}main{{background:#1a2230}}section,footer,.status-card{{border-color:#39465a}}input{{background:#111824;color:#fff;border-color:#52627a}}.subtitle,.version,footer,.hint{{color:#b3bfd1}}button.secondary{{background:#2b3c57;color:#eaf2ff}}.result{{background:#202f47}}.result[data-kind=error]{{background:#4b2323;color:#ffd9d6}}.result[data-kind=success]{{background:#173c2a;color:#c9f7da}}}}@media(forced-colors:active){{button,input,.status-card,.result{{border:1px solid CanvasText}}button:focus-visible,input:focus-visible{{outline:2px solid Highlight}}}}</style></head><body>{main}</body></html>"
        );
        HttpResponse {
            status,
            content_type: "text/html; charset=utf-8",
            extra_headers: Vec::new(),
            body: body.into_bytes(),
        }
    }

    fn response_javascript(script: &'static str) -> HttpResponse {
        HttpResponse {
            status: "200 OK",
            content_type: "text/javascript; charset=utf-8",
            extra_headers: Vec::new(),
            body: script.as_bytes().to_vec(),
        }
    }

    fn response_json(status: &'static str, value: serde_json::Value) -> HttpResponse {
        HttpResponse {
            status,
            content_type: "application/json; charset=utf-8",
            extra_headers: Vec::new(),
            body: serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec()),
        }
    }

    fn write_response(stream: &mut TcpStream, response: HttpResponse) -> anyhow::Result<()> {
        let mut headers = format!(
            "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store, max-age=0\r\nPragma: no-cache\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'\r\n",
            response.status,
            response.content_type,
            response.body.len()
        );
        for (name, value) in response.extra_headers {
            ensure!(
                name.is_ascii() && value.is_ascii() && !value.contains(['\r', '\n']),
                "invalid local response header"
            );
            headers.push_str(&format!("{name}: {value}\r\n"));
        }
        headers.push_str("\r\n");
        stream.write_all(headers.as_bytes())?;
        stream.write_all(&response.body)?;
        stream.flush()?;
        Ok(())
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn retain_valid_tokens(tokens: &mut Vec<ExpiringToken>) {
        tokens.retain(|token| token.expires > Instant::now());
    }

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|error| error.into_inner())
    }

    const APP_JAVASCRIPT: &str = r#"(async()=>{
const app=document.getElementById('app');
const output=document.getElementById('result');
const connection=document.getElementById('connection');
const service=document.getElementById('service');
const serverInput=document.getElementById('server');
const nameInput=document.getElementById('device-name');
const actionButtons=[...document.querySelectorAll('button[data-service],#pair-submit')];
const headers=token=>({Authorization:'Bearer '+token,'Content-Type':'application/json','X-UnionC-Tray':'1'});
let bearer=sessionStorage.getItem('unioncTrayBearer')||'';
let operationPending=false;
let serviceCode='unknown';
let initialized=false;
let lastState={};
let lastConnection={status:'unknown',message:'尚未检测'};
let connectionPending=false;
let connectionGeneration=0;
let connectionTimer=0;
const rawHash=location.hash.slice(1);
let bootstrap='';
let target=rawHash==='pair'?'pair':'status';
const capability=rawHash.match(/^([0-9a-f]{64})(?::(pair|status))?$/);
if(capability){bootstrap=capability[1];target=capability[2]||'status'}
history.replaceState(null,'',target==='pair'?'#pair':'/');

function setResult(message,kind='info'){
  output.textContent=message;
  output.dataset.kind=kind;
  output.setAttribute('role',kind==='error'?'alert':'status');
}
function updateButtons(){
  document.getElementById('pair-submit').disabled=operationPending;
  document.querySelector('[data-service=start]').disabled=operationPending||serviceCode==='running'||serviceCode==='starting';
  document.querySelector('[data-service=stop]').disabled=operationPending||serviceCode==='stopped'||serviceCode==='stopping';
}
function setBusy(value){operationPending=value;app.setAttribute('aria-busy',String(value));updateButtons()}
async function api(path,body){
  const response=await fetch(path,{method:'POST',headers:headers(bearer),body:JSON.stringify(body)});
  const result=await response.json().catch(()=>({code:'invalid_response',message:'本地控制服务返回了无法解析的响应'}));
  if(!response.ok){
    if(response.status===401){sessionStorage.removeItem('unioncTrayBearer')}
    const error=new Error(result.message||('HTTP '+response.status));
    error.code=result.code||'request_failed';
    throw error;
  }
  return result;
}
async function refreshState(populate=false){
  const state=await api('/state',{});
  lastState=state;
  service.textContent=state.service;
  serviceCode=state.service_code||'unknown';
  document.getElementById('version').textContent=state.version?'v'+state.version:'';
  if(populate&&!initialized){serverInput.value=state.server||'';nameInput.value=state.name||'';initialized=true}
  updateButtons();
}
try{
  if(bootstrap){
    const response=await fetch('/session',{method:'POST',headers:headers(bootstrap),body:'{}'});
    const result=await response.json().catch(()=>({message:'本地会话交换失败'}));
    if(!response.ok)throw new Error(result.message||('HTTP '+response.status));
    bearer=result.bearer;
    sessionStorage.setItem('unioncTrayBearer',bearer);
  }
  if(!/^[0-9a-f]{64}$/.test(bearer))throw new Error('请从托盘菜单重新打开配置');
  await refreshState(true);
  app.setAttribute('aria-busy','false');
}catch(error){
  sessionStorage.removeItem('unioncTrayBearer');
  setResult('无法建立本地安全会话：'+error.message,'error');
  app.setAttribute('aria-busy','false');
  actionButtons.forEach(button=>{button.disabled=true});
  return;
}

async function checkConnection(){
  if(connectionPending||document.hidden)return;
  connectionPending=true;
  const generation=++connectionGeneration;
  const server=String(serverInput.value||'');
  connection.textContent='正在检测…';
  try{
    const result=await api('/connection',{server});
    lastConnection=result;
    if(generation===connectionGeneration&&server===String(serverInput.value||'')){
      connection.textContent=result.message;
      connection.dataset.status=result.status||'offline';
    }
  }catch(error){
    if(generation===connectionGeneration){connection.textContent='检测失败：'+error.message;connection.dataset.status='offline'}
  }finally{connectionPending=false}
}
function scheduleConnectionCheck(delay=30000){
  clearTimeout(connectionTimer);
  connectionTimer=setTimeout(()=>{if(!document.hidden)void checkConnection();scheduleConnectionCheck()},delay);
}
async function followOperation(id){
  const deadline=Date.now()+20*60*1000;
  while(Date.now()<deadline){
    const operation=await api('/operation',{id});
    setResult(operation.message,operation.terminal?(operation.success?'success':'error'):'info');
    if(operation.terminal){
      setBusy(false);
      await refreshState(false);
      void checkConnection();
      return;
    }
    await new Promise(resolve=>setTimeout(resolve,1000));
  }
  setBusy(false);
  throw new Error('操作状态等待超时；后台操作可能仍在继续，请稍后刷新状态');
}
async function startOperation(path,body){
  if(operationPending)return;
  setBusy(true);
  setResult('正在请求 Windows 权限确认…');
  try{
    const result=await api(path,body);
    setResult(result.message||'操作已开始');
    if(!result.operation_id)throw new Error('本地控制服务没有返回操作编号');
    await followOperation(result.operation_id);
  }catch(error){setBusy(false);setResult('操作失败：'+error.message,'error')}
}
document.getElementById('check-connection').addEventListener('click',()=>{void checkConnection()});
serverInput.addEventListener('input',()=>{connectionGeneration++;clearTimeout(connectionTimer);connectionTimer=setTimeout(()=>{void checkConnection();scheduleConnectionCheck()},700)});
document.addEventListener('visibilitychange',()=>{if(!document.hidden){void refreshState(false);void checkConnection()} });
document.querySelector('form[data-endpoint]').addEventListener('submit',event=>{
  event.preventDefault();
  const form=new FormData(event.currentTarget);
  const codeInput=event.currentTarget.elements.activation_code;
  const activationCode=String(form.get('activation_code')||'');
  codeInput.value='';
  void startOperation('/pair',{server:String(form.get('server')||''),name:String(form.get('name')||''),activation_code:activationCode});
});
document.querySelectorAll('[data-service]').forEach(button=>button.addEventListener('click',()=>{
  const action=button.dataset.service;
  if(action==='stop'&&!confirm('停止 Agent 服务只影响本次开机；下次启动 Windows 时仍会自动运行。继续吗？'))return;
  void startOperation('/service',{action});
}));
document.getElementById('copy-diagnostics').addEventListener('click',async()=>{
  const summary=[
    'UnionC Agent tray '+(lastState.version||'unknown'),
    'Service: '+(lastState.service_code||'unknown')+' ('+(lastState.service||'unknown')+')',
    'Management origin: '+(serverInput.value||'not configured'),
    'Reachability: '+(lastConnection.status||'unknown')+' ('+(lastConnection.message||'')+')',
    'Platform: Windows '+navigator.userAgent
  ].join('\n');
  try{await navigator.clipboard.writeText(summary);setResult('脱敏诊断已复制到剪贴板','success')}
  catch(_){setResult('浏览器未允许访问剪贴板，请手动复制页面中的状态信息','error')}
});
if(target==='pair'){document.getElementById('pair').scrollIntoView();serverInput.focus()}
void checkConnection();
scheduleConnectionCheck();
setInterval(()=>{if(!document.hidden&&!operationPending)void refreshState(false)},10000);
})();"#;

    fn run_tray(open: bool) -> anyhow::Result<()> {
        let Some(_single_instance) = create_single_instance_mutex(
            "Local\\UnionCAgentTray-4E473D9E-77F7-4F9C-AFE6-CC550E72F5A4",
            false,
        )?
        else {
            // HKLM Run, an explicit launch, and the current installer launcher
            // may race. One per-session tray already satisfies the request.
            if open {
                signal_existing_tray_to_open_configuration();
            }
            return Ok(());
        };
        let server = LocalControlServer::start()?;
        LOCAL_SERVER
            .set(server)
            .map_err(|_| anyhow::anyhow!("local configuration server was already initialized"))?;
        let restart = wide("--startup");
        unsafe { RegisterApplicationRestart(PCWSTR(restart.as_ptr()), Default::default()) }
            .context("failed to register the tray with Windows Restart Manager")?;

        let module =
            unsafe { GetModuleHandleW(None) }.context("failed to get tray module handle")?;
        let instance = windows::Win32::Foundation::HINSTANCE(module.0);
        let class_name = wide(WINDOW_CLASS_NAME);
        let icon =
            unsafe { LoadIconW(None, IDI_APPLICATION) }.context("failed to load the tray icon")?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hIcon: icon,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        ensure!(
            unsafe { RegisterClassW(&class) } != 0,
            "failed to register the tray window class: {}",
            std::io::Error::last_os_error()
        );
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(class_name.as_ptr()),
                w!("UnionC Agent Tray"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .context("failed to create the tray message window")?;
        WINDOW_HANDLE.store(window.0 as isize, Ordering::Release);
        let taskbar_message = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        TASKBAR_CREATED_MESSAGE.store(taskbar_message, Ordering::Release);
        add_tray_icon(window, icon)?;
        if open && let Err(error) = open_local_configuration() {
            show_error(&format!("无法打开本地配置：\n\n{error:#}"));
        }

        let mut message = MSG::default();
        loop {
            let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
            if result.0 == -1 {
                delete_tray_icon(window);
                bail!(
                    "tray message loop failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            if result.0 == 0 {
                break;
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }

    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == TASKBAR_CREATED_MESSAGE.load(Ordering::Acquire) {
            if let Ok(icon) = unsafe { LoadIconW(None, IDI_APPLICATION) } {
                let _ = add_tray_icon(window, icon);
            }
            return LRESULT(0);
        }
        match message {
            OPEN_CONFIGURATION_MESSAGE => {
                if let Err(error) = open_local_configuration() {
                    show_error(&format!("无法打开本地配置：\n\n{error:#}"));
                }
                LRESULT(0)
            }
            EXIT_SERVICE_STOPPED_MESSAGE => {
                if EXIT_PENDING.swap(false, Ordering::AcqRel) {
                    match query_service_state() {
                        Ok(ServiceState::Stopped) => {
                            let _ = unsafe { DestroyWindow(window) };
                        }
                        Ok(state) => show_error(&format!(
                            "Agent 服务尚未停止（{}），托盘将继续运行。",
                            state.label()
                        )),
                        Err(error) => show_error(&format!(
                            "无法确认 Agent 服务已停止，托盘将继续运行：\n\n{error:#}"
                        )),
                    }
                }
                LRESULT(0)
            }
            REFRESH_TRAY_STATUS_MESSAGE => {
                let _ = update_tray_tooltip(window);
                LRESULT(0)
            }
            TRAY_CALLBACK_MESSAGE => {
                let event = lparam.0 as u32;
                if matches!(event, WM_RBUTTONUP | WM_CONTEXTMENU) {
                    show_tray_menu(window);
                } else if event == WM_LBUTTONDBLCLK
                    && let Err(error) = open_local_configuration()
                {
                    show_error(&format!("无法打开本地配置：\n\n{error:#}"));
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = unsafe { DestroyWindow(window) };
                LRESULT(0)
            }
            WM_DESTROY => {
                delete_tray_icon(window);
                WINDOW_HANDLE.store(0, Ordering::Release);
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
        }
    }

    fn signal_existing_tray_to_open_configuration() {
        let class_name = wide(WINDOW_CLASS_NAME);
        for _ in 0..20 {
            if let Ok(window) = unsafe { FindWindowW(PCWSTR(class_name.as_ptr()), PCWSTR::null()) }
            {
                let _ = unsafe {
                    PostMessageW(
                        Some(window),
                        OPEN_CONFIGURATION_MESSAGE,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn add_tray_icon(window: HWND, icon: HICON) -> anyhow::Result<()> {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: window,
            uID: ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: TRAY_CALLBACK_MESSAGE,
            hIcon: icon,
            ..Default::default()
        };
        copy_wide_fixed(&mut data.szTip, &tray_tooltip());
        ensure!(
            unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool(),
            "Windows rejected the tray icon"
        );
        Ok(())
    }

    fn tray_tooltip() -> String {
        match query_service_state() {
            Ok(state) => format!("UnionC Agent · {}", state.label()),
            Err(_) => "UnionC Agent · 服务状态不可用".to_string(),
        }
    }

    fn update_tray_tooltip(window: HWND) -> anyhow::Result<()> {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: window,
            uID: ICON_ID,
            uFlags: NIF_TIP,
            ..Default::default()
        };
        copy_wide_fixed(&mut data.szTip, &tray_tooltip());
        ensure!(
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool(),
            "Windows rejected the tray status update"
        );
        Ok(())
    }

    fn notify_tray_status_changed() {
        let raw = WINDOW_HANDLE.load(Ordering::Acquire);
        if raw != 0 {
            let window = HWND(raw as *mut c_void);
            let _ = unsafe {
                PostMessageW(
                    Some(window),
                    REFRESH_TRAY_STATUS_MESSAGE,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
        }
    }

    fn delete_tray_icon(window: HWND) {
        let data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: window,
            uID: ICON_ID,
            ..Default::default()
        };
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
    }

    fn show_tray_menu(window: HWND) {
        let _ = update_tray_tooltip(window);
        let result = (|| -> anyhow::Result<()> {
            let menu = unsafe { CreatePopupMenu() }.context("failed to create tray menu")?;
            let _menu = MenuHandle(menu);
            append_menu(menu, COMMAND_OPEN_LOCAL, "打开本地配置")?;
            append_menu(menu, COMMAND_PAIR, "配对 / 重新配对")?;
            append_menu(menu, COMMAND_STATUS, "打开状态与连接检测")?;
            let action = match query_service_state() {
                Ok(ServiceState::Running) | Ok(ServiceState::StartPending) => ServiceAction::Stop,
                _ => ServiceAction::Start,
            };
            append_menu(
                menu,
                COMMAND_SERVICE,
                match action {
                    ServiceAction::Start => "启动 Agent 服务（需要管理员权限）",
                    ServiceAction::Stop => "停止 Agent 服务（需要管理员权限）",
                },
            )?;
            unsafe { AppendMenuW(menu, MENU_ITEM_FLAGS(0x0000_0800), 0, PCWSTR::null()) }
                .context("failed to add tray menu separator")?;
            append_menu(
                menu,
                COMMAND_EXIT,
                "停止本次服务并退出托盘（重启后自动运行）",
            )?;
            let mut point = POINT::default();
            unsafe { GetCursorPos(&mut point) }.context("failed to locate the tray menu")?;
            let _ = unsafe { SetForegroundWindow(window) };
            let selected = unsafe {
                TrackPopupMenu(
                    menu,
                    TPM_RIGHTBUTTON | TPM_RETURNCMD,
                    point.x,
                    point.y,
                    Some(0),
                    window,
                    None,
                )
            }
            .0 as usize;
            if selected != 0 {
                handle_tray_command(window, selected, action)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            show_error(&format!("托盘菜单操作失败：\n\n{error:#}"));
        }
    }

    fn handle_tray_command(
        window: HWND,
        command: usize,
        service_action: ServiceAction,
    ) -> anyhow::Result<()> {
        match command {
            COMMAND_OPEN_LOCAL | COMMAND_STATUS => open_local_configuration(),
            COMMAND_PAIR => open_pair_configuration(),
            COMMAND_SERVICE => launch_elevated_service(service_action),
            COMMAND_EXIT => {
                let confirmation = message_box(
                    "将停止本次开机中的 UnionC Agent 服务并彻底退出托盘。\n\n服务的 Automatic 启动类型不会改变；下次启动 Windows 时 Agent 仍会自动运行。",
                    "停止本次服务并退出？",
                    MB_OKCANCEL | MB_ICONWARNING,
                );
                if confirmation == IDOK.0 {
                    request_stop_service_and_exit(window)
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    fn append_menu(
        menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
        id: usize,
        label: &str,
    ) -> anyhow::Result<()> {
        let label = wide(label);
        unsafe { AppendMenuW(menu, MF_STRING, id, PCWSTR(label.as_ptr())) }
            .context("failed to add a tray menu item")
    }

    struct MenuHandle(windows::Win32::UI::WindowsAndMessaging::HMENU);

    impl Drop for MenuHandle {
        fn drop(&mut self) {
            let _ = unsafe { DestroyMenu(self.0) };
        }
    }

    fn open_local_configuration() -> anyhow::Result<()> {
        LOCAL_SERVER
            .get()
            .context("local configuration server is unavailable")?
            .open_configuration()
    }

    fn open_pair_configuration() -> anyhow::Result<()> {
        LOCAL_SERVER
            .get()
            .context("local configuration server is unavailable")?
            .open_configuration_at("pair")
    }

    fn launch_elevated_pair(
        server: &str,
        name: Option<&str>,
        callback_nonce: String,
    ) -> anyhow::Result<KernelHandle> {
        let mut arguments = vec![
            "--elevated-pair".to_string(),
            "--server-b64".to_string(),
            encode_base64url(server.as_bytes()),
        ];
        if let Some(name) = name {
            arguments.push("--name-b64".into());
            arguments.push(encode_base64url(name.as_bytes()));
        }
        arguments.extend(["--callback-nonce".into(), callback_nonce]);
        launch_elevated_process(&arguments, true)?
            .context("Windows did not return the elevated pairing process handle")
    }

    fn launch_elevated_service(action: ServiceAction) -> anyhow::Result<()> {
        launch_elevated(&[
            "--elevated-service".into(),
            match action {
                ServiceAction::Start => "start".into(),
                ServiceAction::Stop => "stop".into(),
            },
        ])
    }

    fn request_stop_service_and_exit(window: HWND) -> anyhow::Result<()> {
        ensure!(
            EXIT_PENDING
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "服务停止和退出操作已经在进行中"
        );
        let result = (|| {
            let process = TransferHandle::new(
                launch_elevated_process(&["--elevated-stop-for-exit".into()], true)?
                    .context("Windows did not return the elevated service process handle")?,
            );
            let window_raw = window.0 as isize;
            thread::Builder::new()
                .name("unionc-stop-and-exit".into())
                .spawn(move || {
                    let result = wait_for_elevated_service_stop(process);
                    match result {
                        Ok(()) => {
                            let window = HWND(window_raw as *mut c_void);
                            if unsafe {
                                PostMessageW(
                                    Some(window),
                                    EXIT_SERVICE_STOPPED_MESSAGE,
                                    WPARAM(0),
                                    LPARAM(0),
                                )
                            }
                            .is_err()
                            {
                                EXIT_PENDING.store(false, Ordering::Release);
                                show_error(
                                    "Agent 服务已停止，但无法通知托盘退出；托盘将继续运行。",
                                );
                            }
                        }
                        Err(error) => {
                            EXIT_PENDING.store(false, Ordering::Release);
                            show_error(&format!(
                                "Agent 服务未能停止，托盘将继续运行：\n\n{error:#}"
                            ));
                        }
                    }
                })
                .context("failed to start the service-stop waiter")?;
            Ok(())
        })();
        if result.is_err() {
            EXIT_PENDING.store(false, Ordering::Release);
        }
        result
    }

    fn wait_for_elevated_service_stop(process: TransferHandle) -> anyhow::Result<()> {
        wait_for_elevated_service_action(process, ServiceAction::Stop)
    }

    fn wait_for_elevated_service_action(
        process: TransferHandle,
        action: ServiceAction,
    ) -> anyhow::Result<()> {
        let process = process.into_kernel();
        let wait = unsafe { WaitForSingleObject(process.0, 60_000) };
        ensure!(
            wait == WAIT_OBJECT_0,
            "timed out or failed while waiting for the elevated stop operation ({wait:?})"
        );
        let mut exit_code = u32::MAX;
        unsafe { GetExitCodeProcess(process.0, &mut exit_code) }
            .context("failed to inspect the elevated stop operation")?;
        ensure!(
            exit_code == 0,
            "elevated stop operation failed with exit code {exit_code}"
        );
        let expected = match action {
            ServiceAction::Start => ServiceState::Running,
            ServiceAction::Stop => ServiceState::Stopped,
        };
        ensure!(
            query_service_state()? == expected,
            "elevated service operation exited without reaching {}",
            expected.label()
        );
        Ok(())
    }

    fn launch_elevated(arguments: &[String]) -> anyhow::Result<()> {
        launch_elevated_process(arguments, false).map(|_| ())
    }

    fn launch_elevated_process(
        arguments: &[String],
        retain_process: bool,
    ) -> anyhow::Result<Option<KernelHandle>> {
        let executable = installed_program_root()?.join(TRAY_EXE);
        ensure_fixed_regular_file(&executable, "installed tray executable")?;
        let executable_wide = wide_os(executable.as_os_str());
        let parameters = arguments
            .iter()
            .map(|argument| quote_windows_argument(argument))
            .collect::<Vec<_>>()
            .join(" ");
        let parameters = wide(&parameters);
        let directory = executable
            .parent()
            .context("installed tray executable has no parent")?;
        let directory = wide_os(directory.as_os_str());
        let mut execute = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_FLAG_NO_UI
                | if retain_process {
                    SEE_MASK_NOCLOSEPROCESS
                } else {
                    0
                },
            lpVerb: w!("runas"),
            lpFile: PCWSTR(executable_wide.as_ptr()),
            lpParameters: PCWSTR(parameters.as_ptr()),
            lpDirectory: PCWSTR(directory.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        unsafe { ShellExecuteExW(&mut execute) }
            .context("Windows elevation was cancelled or could not be started")?;
        if retain_process {
            ensure!(
                !execute.hProcess.is_invalid(),
                "Windows did not provide an elevated process handle"
            );
            Ok(Some(KernelHandle(execute.hProcess)))
        } else {
            Ok(None)
        }
    }

    fn open_browser(value: &str) -> anyhow::Result<()> {
        let value = validate_browser_url(value)?;
        let value = wide(&value);
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(value.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        ensure!(
            result.0 as isize > 32,
            "Windows could not open the default browser (ShellExecute code {})",
            result.0 as isize
        );
        Ok(())
    }

    fn message_box(message: &str, title: &str, style: MESSAGEBOX_STYLE) -> i32 {
        let message = wide(message);
        let title = wide(title);
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(message.as_ptr()),
                PCWSTR(title.as_ptr()),
                style,
            )
        }
        .0
    }

    fn elevated_pair(
        server: String,
        name: Option<String>,
        callback_nonce: String,
    ) -> anyhow::Result<()> {
        ensure_process_is_elevated()?;
        let _pair_mutex = create_single_instance_mutex(PRIVILEGED_OPERATION_MUTEX, true)?
            .context("another UnionC Agent pairing operation is already running")?;
        let server = validate_server_base(&server)?;
        let name = name
            .as_deref()
            .map(validate_host_name)
            .transpose()?
            .flatten();
        let origin = reqwest::Url::parse(&server)?.origin().ascii_serialization();
        let confirmation = format!(
            "即将把此 Windows 设备配对到：\n\n{origin}\n\n{}\n\n重新配对成功会替换当前设备绑定。只有确认该地址属于你的 UnionC 服务器时才继续。",
            name.as_deref()
                .map(|name| format!("设备名称：{name}"))
                .unwrap_or_else(|| "设备名称：使用本机名称".into())
        );
        match message_box(
            &confirmation,
            "确认 UnionC Agent 配对",
            MB_OKCANCEL | MB_ICONWARNING,
        ) {
            result if result == IDOK.0 => {}
            result if result == IDCANCEL.0 => return Ok(()),
            result => bail!("Windows could not display the pairing confirmation (result {result})"),
        }

        let original_state = wait_for_stable_service_state(Duration::from_secs(30))?;
        ensure!(
            matches!(
                original_state,
                ServiceState::Running | ServiceState::Stopped
            ),
            "Agent service is in unsupported state {}",
            original_state.label()
        );
        let outcome = run_hidden_pair(&server, name.as_deref(), &callback_nonce);
        match outcome {
            Ok(()) => {
                if original_state == ServiceState::Running {
                    restart_service()?;
                }
                Ok(())
            }
            Err(error) => Err(error.context(format!(
                "配对未完成；Agent 服务保持操作前的状态（{}）",
                original_state.label()
            ))),
        }
    }

    fn elevated_service(action: ServiceAction, notify: bool) -> anyhow::Result<()> {
        ensure_process_is_elevated()?;
        let _operation = create_single_instance_mutex(PRIVILEGED_OPERATION_MUTEX, true)?
            .context("another UnionC Agent pairing or service operation is already running")?;
        match action {
            ServiceAction::Start => start_service()?,
            ServiceAction::Stop => stop_service()?,
        }
        if notify {
            message_box(
                match action {
                    ServiceAction::Start => "UnionC Agent 服务已启动。",
                    ServiceAction::Stop => {
                        "UnionC Agent 服务已停止；Windows 下次启动时仍会自动运行。"
                    }
                },
                "UnionC Agent",
                MB_OK | MB_ICONINFORMATION,
            );
        }
        Ok(())
    }

    fn elevated_stop_for_exit() -> anyhow::Result<()> {
        ensure_process_is_elevated()?;
        let _operation = create_single_instance_mutex(PRIVILEGED_OPERATION_MUTEX, true)?
            .context("another UnionC Agent pairing or service operation is already running")?;
        stop_service()?;
        ensure!(
            query_service_state()? == ServiceState::Stopped,
            "Agent service did not remain stopped"
        );
        Ok(())
    }

    fn run_hidden_pair(
        server: &str,
        name: Option<&str>,
        callback_nonce: &str,
    ) -> anyhow::Result<()> {
        let agent = installed_program_root()?.join(AGENT_EXE);
        let state_root = installed_state_root()?;
        let config = state_root.join(CONFIG_FILE);
        ensure_fixed_regular_file(&agent, "installed Agent executable")?;
        ensure!(
            state_root.is_dir(),
            "installed Agent state directory is missing"
        );
        if config.exists() {
            ensure_fixed_regular_file(&config, "installed Agent configuration")?;
        }
        let cancel_event_name = format!("Local\\UnionCAgentPairCancel-{callback_nonce}");
        let cancel_event_name_wide = wide(&cancel_event_name);
        let cancel_event =
            unsafe { CreateEventW(None, true, false, PCWSTR(cancel_event_name_wide.as_ptr())) }
                .context("failed to create the pairing cancellation event")?;
        let cancel_event = KernelHandle(cancel_event);
        let mut command = Command::new(&agent);
        command
            .arg("pair")
            .arg("--tray-events")
            .arg("--tray-activation-stdin")
            .arg("--config")
            .arg(&config)
            .arg("--server")
            .arg(server)
            .arg("--tray-cancel-event")
            .arg(&cancel_event_name)
            .arg("--tray-deadline-seconds")
            .arg(PAIR_OPERATION_TIMEOUT.as_secs().to_string())
            .arg("--replace-pending-pairing");
        if let Some(name) = name {
            command.arg("--name").arg(name);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW);
        set_clean_agent_environment(&mut command)?;
        let child = command
            .spawn()
            .with_context(|| format!("failed to start {}", agent.display()))?;
        let mut child = PairChildGuard::new(child, cancel_event.0);
        let stdin = child
            .child_mut()
            .stdin
            .take()
            .context("pairing stdin pipe is unavailable")?;
        let stdout = child
            .child_mut()
            .stdout
            .take()
            .context("pairing stdout pipe is unavailable")?;
        let stderr = child
            .child_mut()
            .stderr
            .take()
            .context("pairing stderr pipe is unavailable")?;
        let server_for_output = server.to_string();
        let nonce_for_output = callback_nonce.to_string();
        let mut stdout_thread = Some(
            thread::Builder::new()
                .name("unionc-pair-stdout".into())
                .spawn(move || {
                    process_pair_events(stdout, stdin, &server_for_output, &nonce_for_output)
                })
                .context("failed to start pairing event reader")?,
        );
        let stderr_thread = thread::Builder::new()
            .name("unionc-pair-stderr".into())
            .spawn(move || drain_limited(stderr, MAX_CHILD_STDERR_BYTES))
            .context("failed to start pairing diagnostics reader")?;
        let deadline = Instant::now() + PAIR_OPERATION_TIMEOUT + Duration::from_secs(15);
        let mut completed_event_result = None;
        let mut cancellation_requested = false;
        let mut cancellation_requested_at = None;
        let mut forced_termination = false;
        let status = loop {
            if let Some(status) = child
                .child_mut()
                .try_wait()
                .context("failed to query browser pairing process")?
            {
                break status;
            }
            if stdout_thread
                .as_ref()
                .is_some_and(|reader| reader.is_finished())
            {
                let result = stdout_thread
                    .take()
                    .expect("checked above")
                    .join()
                    .map_err(|_| anyhow::anyhow!("pairing event reader panicked"))?;
                if result.is_err() && !cancellation_requested {
                    unsafe { SetEvent(cancel_event.0) }
                        .context("failed to cancel pairing after an event-stream error")?;
                    cancellation_requested = true;
                    cancellation_requested_at = Some(Instant::now());
                }
                completed_event_result = Some(result);
            }
            if Instant::now() >= deadline && !cancellation_requested {
                unsafe { SetEvent(cancel_event.0) }
                    .context("failed to signal the pairing safety deadline")?;
                cancellation_requested = true;
                cancellation_requested_at = Some(Instant::now());
            }
            if cancellation_requested_at
                .is_some_and(|requested| requested.elapsed() >= Duration::from_secs(30))
            {
                // The signed child normally observes the named event while its
                // in-flight reqwest future is safely dropped. Termination is a
                // final containment fallback only if that protocol fails for
                // 30 seconds; it is never the normal cancellation path.
                child
                    .child_mut()
                    .kill()
                    .context("failed to contain an unresponsive pairing child")?;
                forced_termination = true;
                break child
                    .child_mut()
                    .wait()
                    .context("failed to reap the contained pairing child")?;
            }
            thread::sleep(Duration::from_millis(100));
        };
        let event_result = if let Some(result) = completed_event_result {
            result
        } else {
            stdout_thread
                .take()
                .context("pairing event reader disappeared")?
                .join()
                .map_err(|_| anyhow::anyhow!("pairing event reader panicked"))?
        };
        let diagnostics = stderr_thread
            .join()
            .map_err(|_| anyhow::anyhow!("pairing diagnostics reader panicked"))??;
        child.disarm();
        ensure!(
            !forced_termination,
            "pairing child ignored graceful cancellation and was contained after 30 seconds"
        );
        super::reconcile_pairing_child(
            status.success(),
            &status.to_string(),
            event_result,
            &diagnostics,
        )
    }

    fn process_pair_events<R: Read, W: Write>(
        reader: R,
        writer: W,
        server: &str,
        callback_nonce: &str,
    ) -> anyhow::Result<()> {
        let mut reader = BufReader::with_capacity(4096, reader);
        let mut writer = Some(writer);
        let mut line = Vec::new();
        let mut total = 0_usize;
        let mut activation_sent = false;
        let mut waiting_request_id = None;
        let mut paired = false;
        loop {
            line.clear();
            let read = read_bounded_line(&mut reader, &mut line, MAX_NDJSON_LINE_BYTES)?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(read);
            ensure!(
                total <= MAX_NDJSON_TOTAL_BYTES,
                "pairing event stream is too large"
            );
            while line
                .last()
                .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
            {
                line.pop();
            }
            ensure!(
                !line.is_empty(),
                "pairing event stream contains an empty line"
            );
            let event: PairEvent =
                serde_json::from_slice(&line).context("Agent emitted invalid pairing NDJSON")?;
            match event {
                PairEvent::PairingWaiting {
                    _version: _,
                    generation,
                    request_id,
                    activation_url,
                    pairing_endpoint,
                    _expires_at: _,
                    _poll_interval: _,
                } => {
                    ensure!(
                        !activation_sent,
                        "Agent emitted more than one activation event"
                    );
                    let activation_url = validate_browser_url(&activation_url)?;
                    ensure!(
                        browser_url_matches_server_origin(&activation_url, server),
                        "Agent activation URL origin differs from the confirmed server"
                    );
                    canonical_uuid(&request_id, "Agent pairing request id")?;
                    canonical_uuid(&generation, "Agent pairing generation")?;
                    let activation_code = exchange_pairing_code(
                        callback_nonce,
                        &PairIpcMessage {
                            generation,
                            request_id: request_id.clone(),
                            activation_url,
                            pairing_endpoint,
                        },
                    )?;
                    let mut input = writer
                        .take()
                        .context("authorization-key stdin was already consumed")?;
                    input.write_all(activation_code.as_bytes())?;
                    input.write_all(b"\n")?;
                    input.flush()?;
                    drop(input);
                    drop(activation_code);
                    activation_sent = true;
                    waiting_request_id = Some(request_id);
                }
                PairEvent::Paired {
                    _version: _,
                    request_id,
                    instance_id,
                    _endpoint: _,
                } => {
                    ensure!(
                        activation_sent,
                        "Agent paired before the authorization key was supplied"
                    );
                    ensure!(!paired, "Agent emitted more than one paired event");
                    ensure!(
                        Some(request_id.as_str()) == waiting_request_id.as_deref(),
                        "Agent paired a different request than the one authorized"
                    );
                    canonical_uuid(&request_id, "Agent paired request id")?;
                    canonical_uuid(&instance_id, "Agent paired instance id")?;
                    paired = true;
                }
                PairEvent::PairingInterrupted {
                    _version: _,
                    _request_id,
                } => {
                    canonical_uuid(&_request_id, "Agent interrupted request id")?;
                    bail!("browser pairing was interrupted");
                }
                PairEvent::PairingCancelled { _version: _ } => {
                    bail!("browser pairing was cancelled safely")
                }
                PairEvent::PairingTimeout { _version: _ } => {
                    bail!("browser pairing reached its safety deadline")
                }
            }
        }
        if !activation_sent {
            return Err(super::MissingAuthorizationKeyEvent.into());
        }
        ensure!(paired, "Agent did not emit a successful pairing event");
        Ok(())
    }

    fn read_bounded_line<R: BufRead>(
        reader: &mut R,
        line: &mut Vec<u8>,
        limit: usize,
    ) -> anyhow::Result<usize> {
        let mut read = 0_usize;
        loop {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok(read);
            }
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| index + 1)
                .unwrap_or(available.len());
            ensure!(
                line.len().saturating_add(take) <= limit,
                "pairing event line is too large"
            );
            let ends_line = available.get(take - 1) == Some(&b'\n');
            line.extend_from_slice(&available[..take]);
            reader.consume(take);
            read += take;
            if ends_line {
                return Ok(read);
            }
        }
    }

    fn drain_limited<R: Read>(mut reader: R, limit: usize) -> anyhow::Result<String> {
        let mut retained = Vec::new();
        let mut buffer = [0_u8; 4096];
        let mut truncated = false;
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let remaining = limit.saturating_sub(retained.len());
            retained.extend_from_slice(&buffer[..read.min(remaining)]);
            truncated |= read > remaining;
        }
        let mut diagnostics = String::from_utf8_lossy(&retained).to_string();
        if truncated {
            diagnostics.push_str("\n[diagnostics truncated]");
        }
        Ok(diagnostics)
    }

    fn exchange_pairing_code(
        nonce: &str,
        message: &PairIpcMessage,
    ) -> anyhow::Result<SensitiveActivationCode> {
        validate_callback_nonce(nonce)?;
        let pipe_name = pair_pipe_name(nonce);
        let pipe_name_wide = wide(&pipe_name);
        let deadline = Instant::now() + Duration::from_secs(10);
        let pipe = loop {
            match unsafe {
                CreateFileW(
                    PCWSTR(pipe_name_wide.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_MODE::default(),
                    None,
                    OPEN_EXISTING,
                    SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    None,
                )
            } {
                Ok(pipe) => break KernelHandle(pipe),
                Err(_) => match unsafe { GetLastError() } {
                    ERROR_PIPE_BUSY | ERROR_FILE_NOT_FOUND if Instant::now() < deadline => {
                        let _ = unsafe { WaitNamedPipeW(PCWSTR(pipe_name_wide.as_ptr()), 200) };
                    }
                    error => bail!("could not connect to the protected tray pipe: {error:?}"),
                },
            }
        };
        let body = serde_json::to_vec(message)?;
        ensure!(
            body.len() <= MAX_LOCAL_HTTP_BODY_BYTES,
            "pairing IPC message is too large"
        );
        write_pipe_frame(pipe.0, &body)?;
        let code = read_pipe_frame(pipe.0, 256)?;
        let code =
            String::from_utf8(code).context("tray supplied a non-UTF-8 authorization key")?;
        validate_activation_code(&code)?;
        Ok(SensitiveActivationCode::new(code))
    }

    fn validate_pair_ipc_message(message: &PairIpcMessage, server: &str) -> anyhow::Result<()> {
        canonical_uuid(&message.generation, "pairing IPC generation")?;
        let request_id = canonical_uuid(&message.request_id, "pairing IPC request id")?;
        let expected_pairing_endpoint = format!(
            "{}/api/agent/v2/pairing-requests",
            server.trim_end_matches('/')
        );
        ensure!(
            message.pairing_endpoint == expected_pairing_endpoint,
            "Agent pairing endpoint differs from the confirmed server"
        );
        let pairing_endpoint = validate_browser_url(&message.pairing_endpoint)?;
        ensure!(
            browser_url_matches_server_origin(&pairing_endpoint, server),
            "Agent pairing endpoint origin differs from the confirmed server"
        );
        let activation_url = validate_browser_url(&message.activation_url)?;
        let activation = reqwest::Url::parse(&activation_url)?;
        ensure!(
            browser_url_matches_server_origin(&activation_url, &pairing_endpoint),
            "Agent activation URL origin differs from its pairing endpoint"
        );
        ensure!(
            activation.username().is_empty()
                && activation.password().is_none()
                && activation.query().is_none()
                && activation.fragment().is_none()
                && activation.path() == format!("/agent/activate/{request_id}"),
            "Agent activation URL does not match its pairing request id"
        );
        Ok(())
    }

    fn canonical_uuid(value: &str, label: &str) -> anyhow::Result<uuid::Uuid> {
        let parsed =
            uuid::Uuid::parse_str(value).with_context(|| format!("{label} is not a UUID"))?;
        ensure!(
            parsed.to_string() == value,
            "{label} must use canonical lowercase hyphenated text"
        );
        Ok(parsed)
    }

    fn validate_callback_nonce(nonce: &str) -> anyhow::Result<()> {
        ensure!(
            nonce.len() == 64 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid pairing pipe nonce"
        );
        Ok(())
    }

    fn pair_pipe_name(nonce: &str) -> String {
        format!(r"\\.\pipe\UnionCAgentPair-{nonce}")
    }

    fn read_pipe_frame(handle: HANDLE, limit: usize) -> anyhow::Result<Vec<u8>> {
        let mut length = [0_u8; 4];
        read_pipe_exact(handle, &mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        ensure!(length <= limit, "pairing pipe frame exceeds its size limit");
        let mut body = vec![0_u8; length];
        read_pipe_exact(handle, &mut body)?;
        Ok(body)
    }

    fn write_pipe_frame(handle: HANDLE, body: &[u8]) -> anyhow::Result<()> {
        let length = u32::try_from(body.len()).context("pairing pipe frame is too large")?;
        write_pipe_all(handle, &length.to_le_bytes())?;
        write_pipe_all(handle, body)
    }

    fn read_pipe_exact(handle: HANDLE, mut destination: &mut [u8]) -> anyhow::Result<()> {
        while !destination.is_empty() {
            let mut read = 0_u32;
            unsafe { ReadFile(handle, Some(destination), Some(&mut read), None) }
                .context("failed to read from the protected pairing pipe")?;
            ensure!(read != 0, "protected pairing pipe closed unexpectedly");
            let consumed = usize::try_from(read)?;
            ensure!(
                consumed <= destination.len(),
                "invalid pairing pipe read count"
            );
            destination = &mut destination[consumed..];
        }
        Ok(())
    }

    fn write_pipe_all(handle: HANDLE, mut body: &[u8]) -> anyhow::Result<()> {
        while !body.is_empty() {
            let mut written = 0_u32;
            unsafe { WriteFile(handle, Some(body), Some(&mut written), None) }
                .context("failed to write to the protected pairing pipe")?;
            ensure!(written != 0, "protected pairing pipe closed unexpectedly");
            let consumed = usize::try_from(written)?;
            ensure!(consumed <= body.len(), "invalid pairing pipe write count");
            body = &body[consumed..];
        }
        Ok(())
    }

    fn set_clean_agent_environment(command: &mut Command) -> anyhow::Result<()> {
        command.env_clear();
        let windows_directory = windows_directory()?;
        let program_data = known_folder(&FOLDERID_ProgramData)?;
        let state_root = program_data.join(CONFIG_DIRECTORY);
        command.env("SystemRoot", &windows_directory);
        command.env("WINDIR", &windows_directory);
        command.env("PROGRAMDATA", &program_data);
        command.env("UNIONC_AGENT_STATE_DIR", &state_root);
        // Deliberately do not inherit UNIONC_AGENT_*, proxy variables, PATH,
        // TEMP/TMP, certificate overrides, or user profile variables into the
        // privileged child. PROGRAMDATA and UNIONC_AGENT_STATE_DIR above are
        // independently derived from a Windows Known Folder, never inherited.
        Ok(())
    }

    fn ensure_process_is_elevated() -> anyhow::Result<()> {
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
            .context("failed to inspect the elevated process token")?;
        let _token = KernelHandle(token);
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0_u32;
        unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                Some((&mut elevation as *mut TOKEN_ELEVATION).cast::<c_void>()),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            )
        }
        .context("failed to read process elevation state")?;
        ensure!(
            returned as usize >= size_of::<TOKEN_ELEVATION>() && elevation.TokenIsElevated != 0,
            "this internal operation requires Windows administrator elevation"
        );
        Ok(())
    }

    fn create_single_instance_mutex(
        name: &str,
        privileged: bool,
    ) -> anyhow::Result<Option<KernelHandle>> {
        let name = wide(name);
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
            .context("failed to create the operation mutex")?;
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if already_exists {
            unsafe { CloseHandle(handle) }.ok();
            return Ok(None);
        }
        if privileged {
            ensure_process_is_elevated()?;
        }
        Ok(Some(KernelHandle(handle)))
    }

    struct KernelHandle(HANDLE);

    impl Drop for KernelHandle {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// `windows::HANDLE` intentionally is not `Send`. Transfer only the
    /// integer handle value between threads, then reconstruct ownership in the
    /// destination thread. This wrapper owns and closes the handle if a thread
    /// fails to start before the transfer is consumed.
    struct TransferHandle(Option<isize>);

    impl TransferHandle {
        fn new(handle: KernelHandle) -> Self {
            let raw = handle.0.0 as isize;
            std::mem::forget(handle);
            Self(Some(raw))
        }

        fn into_kernel(mut self) -> KernelHandle {
            let raw = self
                .0
                .take()
                .expect("transferred handle was already consumed");
            KernelHandle(HANDLE(raw as *mut c_void))
        }
    }

    impl Drop for TransferHandle {
        fn drop(&mut self) {
            if let Some(raw) = self.0.take() {
                let _ = unsafe { CloseHandle(HANDLE(raw as *mut c_void)) };
            }
        }
    }

    struct PairChildGuard {
        child: Option<Child>,
        cancel_event_raw: isize,
    }

    impl PairChildGuard {
        fn new(child: Child, cancel_event: HANDLE) -> Self {
            Self {
                child: Some(child),
                cancel_event_raw: cancel_event.0 as isize,
            }
        }

        fn child_mut(&mut self) -> &mut Child {
            self.child.as_mut().expect("pairing child was disarmed")
        }

        fn disarm(&mut self) {
            self.child.take();
        }
    }

    impl Drop for PairChildGuard {
        fn drop(&mut self) {
            let Some(child) = self.child.as_mut() else {
                return;
            };
            let event = HANDLE(self.cancel_event_raw as *mut c_void);
            let _ = unsafe { SetEvent(event) };
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(100));
                    }
                    _ => break,
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ServiceState {
        Stopped,
        StartPending,
        StopPending,
        Running,
        Other(u32),
    }

    impl ServiceState {
        fn label(self) -> &'static str {
            match self {
                Self::Stopped => "已停止",
                Self::StartPending => "正在启动",
                Self::StopPending => "正在停止",
                Self::Running => "正在运行",
                Self::Other(_) => "其他状态",
            }
        }

        fn code(self) -> &'static str {
            match self {
                Self::Stopped => "stopped",
                Self::StartPending => "starting",
                Self::StopPending => "stopping",
                Self::Running => "running",
                Self::Other(_) => "unknown",
            }
        }
    }

    struct ScmHandle(SC_HANDLE);

    impl Drop for ScmHandle {
        fn drop(&mut self) {
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }

    fn open_service(access: u32) -> anyhow::Result<(ScmHandle, ScmHandle)> {
        let manager = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }
            .context("failed to open the Windows Service Control Manager")?;
        let manager = ScmHandle(manager);
        let service_name = wide(WINDOWS_SERVICE_NAME);
        let service = unsafe { OpenServiceW(manager.0, PCWSTR(service_name.as_ptr()), access) }
            .context("UnionC Agent Windows service is not installed or cannot be accessed")?;
        Ok((manager, ScmHandle(service)))
    }

    fn query_service_state() -> anyhow::Result<ServiceState> {
        let (_manager, service) = open_service(SERVICE_QUERY_STATUS)?;
        query_open_service_state(service.0)
    }

    fn query_open_service_state(service: SC_HANDLE) -> anyhow::Result<ServiceState> {
        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0_u32;
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
                size_of::<SERVICE_STATUS_PROCESS>(),
            )
        };
        unsafe { QueryServiceStatusEx(service, SC_STATUS_PROCESS_INFO, Some(bytes), &mut needed) }
            .context("failed to query the UnionC Agent service")?;
        Ok(match status.dwCurrentState {
            value if value == SERVICE_STOPPED => ServiceState::Stopped,
            value if value == SERVICE_START_PENDING => ServiceState::StartPending,
            value if value == SERVICE_STOP_PENDING => ServiceState::StopPending,
            value if value == SERVICE_RUNNING => ServiceState::Running,
            value => ServiceState::Other(value.0),
        })
    }

    fn wait_for_stable_service_state(timeout: Duration) -> anyhow::Result<ServiceState> {
        let deadline = Instant::now() + timeout;
        loop {
            let state = query_service_state()?;
            if !matches!(
                state,
                ServiceState::StartPending | ServiceState::StopPending
            ) {
                return Ok(state);
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for the Agent service to become stable"
            );
            thread::sleep(Duration::from_millis(250));
        }
    }

    fn start_service() -> anyhow::Result<()> {
        let (_manager, service) = open_service(SERVICE_QUERY_STATUS | SERVICE_START)?;
        match query_open_service_state(service.0)? {
            ServiceState::Running => return Ok(()),
            ServiceState::StartPending => {}
            ServiceState::Stopped => unsafe { StartServiceW(service.0, None) }
                .context("failed to start the UnionC Agent service")?,
            state => bail!(
                "cannot start the Agent service from state {}",
                state.label()
            ),
        }
        wait_open_service_for(service.0, ServiceState::Running, Duration::from_secs(30))
    }

    fn stop_service() -> anyhow::Result<()> {
        let (_manager, service) = open_service(SERVICE_QUERY_STATUS | SERVICE_STOP)?;
        match query_open_service_state(service.0)? {
            ServiceState::Stopped => return Ok(()),
            ServiceState::StopPending => {}
            ServiceState::Running => {
                let mut status = SERVICE_STATUS::default();
                unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) }
                    .context("failed to stop the UnionC Agent service")?;
            }
            state => bail!("cannot stop the Agent service from state {}", state.label()),
        }
        wait_open_service_for(service.0, ServiceState::Stopped, Duration::from_secs(30))
    }

    fn restart_service() -> anyhow::Result<()> {
        stop_service().context("pairing succeeded but the Agent service could not be stopped")?;
        start_service().context("pairing succeeded but the Agent service could not be restarted")
    }

    fn wait_open_service_for(
        service: SC_HANDLE,
        expected: ServiceState,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let state = query_open_service_state(service)?;
            if state == expected {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for the Agent service to reach {} (currently {})",
                expected.label(),
                state.label()
            );
            thread::sleep(Duration::from_millis(250));
        }
    }

    fn installed_program_root() -> anyhow::Result<PathBuf> {
        Ok(known_folder(&FOLDERID_ProgramFiles)?.join(CONFIG_DIRECTORY))
    }

    fn installed_state_root() -> anyhow::Result<PathBuf> {
        Ok(known_folder(&FOLDERID_ProgramData)?.join(CONFIG_DIRECTORY))
    }

    fn local_preferences_path() -> anyhow::Result<PathBuf> {
        Ok(known_folder(&FOLDERID_LocalAppData)?
            .join(CONFIG_DIRECTORY)
            .join("tray.json"))
    }

    fn known_folder(identifier: &windows::core::GUID) -> anyhow::Result<PathBuf> {
        let raw = unsafe { SHGetKnownFolderPath(identifier, KF_FLAG_DEFAULT, None) }
            .context("failed to resolve a Windows Known Folder")?;
        let result = (|| {
            let mut length = 0_usize;
            while unsafe { *raw.0.add(length) } != 0 {
                length += 1;
                ensure!(length < 32 * 1024, "Known Folder path is too long");
            }
            let value = unsafe { std::slice::from_raw_parts(raw.0, length) };
            let path = PathBuf::from(OsString::from_wide(value));
            ensure!(
                path.is_absolute(),
                "Known Folder did not return an absolute path"
            );
            Ok(path)
        })();
        unsafe { CoTaskMemFree(Some(raw.0.cast())) };
        result
    }

    fn windows_directory() -> anyhow::Result<PathBuf> {
        let mut buffer = vec![0_u16; 260];
        loop {
            let length = unsafe { GetWindowsDirectoryW(Some(&mut buffer)) } as usize;
            ensure!(length != 0, "GetWindowsDirectoryW failed");
            if length < buffer.len() {
                buffer.truncate(length);
                break;
            }
            ensure!(length < 32 * 1024, "Windows directory path is too long");
            buffer.resize(length + 1, 0);
        }
        let path = PathBuf::from(OsString::from_wide(&buffer));
        ensure!(path.is_absolute() && path.is_dir(), "SystemRoot is invalid");
        Ok(path)
    }

    fn ensure_fixed_regular_file(path: &Path, label: &str) -> anyhow::Result<()> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("{label} is missing: {}", path.display()))?;
        ensure!(metadata.is_file(), "{label} is not a regular file");
        Ok(())
    }

    fn load_preferences(path: &Path) -> anyhow::Result<TrayPreferences> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(TrayPreferences::default());
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(
            bytes.len() <= 16 * 1024,
            "tray preferences file is too large"
        );
        let mut preferences: TrayPreferences =
            serde_json::from_slice(&bytes).context("tray preferences are invalid")?;
        if !preferences.server.is_empty() {
            preferences.server = validate_server_base(&preferences.server)?;
        }
        preferences.name = preferences
            .name
            .as_deref()
            .map(validate_host_name)
            .transpose()?
            .flatten();
        Ok(preferences)
    }

    fn save_preferences(path: &Path, preferences: &TrayPreferences) -> anyhow::Result<()> {
        let parent = path.parent().context("tray preferences have no parent")?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".tray-{}.tmp", random_secret()));
        let result = (|| -> anyhow::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            serde_json::to_writer_pretty(&mut file, preferences)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            let temporary_wide = wide_os(temporary.as_os_str());
            let path_wide = wide_os(path.as_os_str());
            unsafe {
                MoveFileExW(
                    PCWSTR(temporary_wide.as_ptr()),
                    PCWSTR(path_wide.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
            .context("failed to atomically replace tray preferences")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.context("failed to save per-user tray preferences")
    }

    fn random_secret() -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let bytes = rand::random::<[u8; 32]>();
        let mut value = String::with_capacity(64);
        for byte in bytes {
            value.push(HEX[(byte >> 4) as usize] as char);
            value.push(HEX[(byte & 0x0f) as usize] as char);
        }
        value
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn wide_os(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn copy_wide_fixed<const LENGTH: usize>(destination: &mut [u16; LENGTH], value: &str) {
        for (destination, source) in destination
            .iter_mut()
            .zip(value.encode_utf16().take(LENGTH.saturating_sub(1)))
        {
            *destination = source;
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn probe_health_body(body: &str) -> ServerConnectionStatus {
            probe_health_response("200 OK", Some("application/json"), body)
        }

        fn probe_health_response(
            status: &str,
            content_type: Option<&str>,
            body: &str,
        ) -> ServerConnectionStatus {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let address = listener.local_addr().unwrap();
            let content_type = content_type
                .map(|value| format!("Content-Type: {value}\r\n"))
                .unwrap_or_default();
            let response = format!(
                "HTTP/1.1 {status}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let worker = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request).unwrap();
                stream.write_all(response.as_bytes()).unwrap();
            });
            let result = probe_server_connection(&format!("http://{address}"));
            worker.join().unwrap();
            result
        }

        #[test]
        fn serde_dtos_accept_only_the_current_wire_shape() {
            let current_preferences = format!(
                r#"{{"application_version":"{}","server":"","name":null}}"#,
                env!("CARGO_PKG_VERSION")
            );
            assert!(serde_json::from_str::<TrayPreferences>(&current_preferences).is_ok());
            assert!(
                serde_json::from_str::<TrayPreferences>(r#"{"server":"","name":null}"#).is_err()
            );
            assert!(
                serde_json::from_str::<TrayPreferences>(
                    r#"{"application_version":"0.3.1","server":"","name":null}"#
                )
                .is_err()
            );
            assert!(
                serde_json::from_str::<TrayPreferences>(
                    r#"{"application_version":"0.3.2","server":"","name":null,"legacy":true}"#
                )
                .is_err()
            );

            assert!(
                serde_json::from_str::<PairRequest>(
                    r#"{"server":"https://server.example","name":"host","activation_code":"secret"}"#
                )
                .is_ok()
            );
            assert!(
                serde_json::from_str::<PairRequest>(
                    r#"{"server":"https://server.example","activation_code":"secret"}"#
                )
                .is_err()
            );
            assert!(serde_json::from_str::<ConnectionRequest>(r#"{"server":""}"#).is_ok());
            assert!(serde_json::from_str::<ConnectionRequest>(r#"{}"#).is_err());
            assert!(serde_json::from_str::<StateRequest>(r#"{}"#).is_ok());
            assert!(serde_json::from_str::<StateRequest>(r#"{"legacy":true}"#).is_err());
            assert!(
                serde_json::from_str::<ServiceRequest>(r#"{"action":"start","extra":1}"#).is_err()
            );
            assert!(serde_json::from_str::<OperationRequest>(r#"{"id":"id","extra":1}"#).is_err());

            assert!(
                serde_json::from_str::<PairIpcMessage>(
                    r#"{"generation":"generation","request_id":"request","activation_url":"https://server.example/agent/activate/request","pairing_endpoint":"https://server.example/api/agent/v2/pairing-requests"}"#
                )
                .is_ok()
            );
            assert!(
                serde_json::from_str::<PairIpcMessage>(
                    r#"{"generation":"generation","request_id":"request","activation_url":"https://server.example/agent/activate/request","pairing_endpoint":"https://server.example/api/agent/v2/pairing-requests","legacy":true}"#
                )
                .is_err()
            );

            assert!(
                serde_json::from_str::<PairEvent>(
                    r#"{"event":"pairing_waiting","version":"0.3.2","request_id":"request","generation":"generation","activation_url":"https://server.example/agent/activate/request","pairing_endpoint":"https://server.example/api/agent/v2/pairing-requests","expires_at":"2026-08-20T00:00:00Z","poll_interval":2}"#
                )
                .is_ok()
            );
            assert!(
                serde_json::from_str::<PairEvent>(
                    r#"{"event":"pairing_waiting","version":"0.3.2","request_id":"request","generation":"generation","activation_url":"https://server.example/agent/activate/request","pairing_endpoint":"https://server.example/api/agent/v2/pairing-requests","poll_interval":2}"#
                )
                .is_err()
            );
            assert!(
                serde_json::from_str::<PairEvent>(
                    r#"{"event":"paired","version":"0.3.2","request_id":"request","instance_id":"instance","endpoint":"https://server.example/api/agent/v1/report","legacy":true}"#
                )
                .is_err()
            );
            assert!(
                serde_json::from_str::<PairEvent>(r#"{"event":"pairing_cancelled","version":1}"#)
                    .is_err()
            );
            assert!(
                serde_json::from_str::<PairEvent>(
                    r#"{"event":"pairing_cancelled","version":"0.3.1"}"#
                )
                .is_err()
            );

            assert!(
                serde_json::from_str::<ServerHealthResponse>(
                    r#"{"status":"ok","version":"0.3.2","uptime_seconds":1}"#
                )
                .is_ok()
            );
            assert!(
                serde_json::from_str::<ServerHealthResponse>(
                    r#"{"status":"ok","uptime_seconds":1}"#
                )
                .is_err()
            );
            assert!(
                serde_json::from_str::<ServerHealthResponse>(
                    r#"{"status":"ok","version":"0.3.2","uptime_seconds":1,"legacy":true}"#
                )
                .is_err()
            );
        }

        #[test]
        fn existing_preferences_can_be_atomically_replaced() {
            let directory =
                std::env::temp_dir().join(format!("unionc-tray-preferences-{}", random_secret()));
            let path = directory.join("tray.json");
            let first = TrayPreferences {
                application_version: CurrentPackageVersion,
                server: "https://first.example".into(),
                name: Some("first".into()),
            };
            let second = TrayPreferences {
                application_version: CurrentPackageVersion,
                server: "https://second.example".into(),
                name: Some("second".into()),
            };
            save_preferences(&path, &first).unwrap();
            save_preferences(&path, &second).unwrap();
            let loaded = load_preferences(&path).unwrap();
            assert_eq!(loaded.server, second.server);
            assert_eq!(loaded.name, second.name);
            fs::remove_dir_all(directory).unwrap();
        }

        #[test]
        fn pairing_ipc_requires_canonical_uuid_text() {
            let generation = uuid::Uuid::new_v4().to_string();
            let request_id = uuid::Uuid::new_v4().to_string();
            let server = "https://server.example";
            let message = PairIpcMessage {
                generation: generation.clone(),
                request_id: request_id.clone(),
                activation_url: format!("{server}/agent/activate/{request_id}"),
                pairing_endpoint: format!("{server}/api/agent/v2/pairing-requests"),
            };
            validate_pair_ipc_message(&message, server).unwrap();

            let uppercase = PairIpcMessage {
                generation: generation.to_uppercase(),
                ..message
            };
            assert!(validate_pair_ipc_message(&uppercase, server).is_err());
        }

        #[test]
        fn bounded_line_reader_rejects_before_growing_past_the_limit() {
            let input = vec![b'x'; MAX_NDJSON_LINE_BYTES + 1];
            let mut reader = BufReader::with_capacity(1024, input.as_slice());
            let mut line = Vec::new();
            assert!(read_bounded_line(&mut reader, &mut line, MAX_NDJSON_LINE_BYTES).is_err());
            assert!(line.len() <= MAX_NDJSON_LINE_BYTES);
        }

        #[test]
        fn pairing_slot_stays_exclusive_for_the_full_worker_lifetime() {
            let state = Arc::new(LocalControlState {
                bootstrap_tokens: Mutex::new(Vec::new()),
                sessions: Mutex::new(Vec::new()),
                operations: Mutex::new(Vec::new()),
                active_pairings: AtomicUsize::new(0),
                active_service_operations: AtomicUsize::new(0),
                preferences_path: PathBuf::from("unused-test-preferences.json"),
            });
            let first = claim_pairing_slot(&state).unwrap();
            let (release, released) = std::sync::mpsc::channel();
            let worker = thread::spawn(move || {
                released.recv().unwrap();
                drop(first);
            });
            assert!(claim_pairing_slot(&state).is_err());
            release.send(()).unwrap();
            worker.join().unwrap();
            assert!(claim_pairing_slot(&state).is_ok());
        }

        #[test]
        fn connection_probe_distinguishes_unconfigured_and_healthy_server() {
            let unconfigured = probe_server_connection("");
            assert_eq!(unconfigured.status, "unconfigured");
            assert!(unconfigured.version.is_none());

            let healthy = probe_health_body(&format!(
                r#"{{"status":"ok","version":"{}","uptime_seconds":1}}"#,
                env!("CARGO_PKG_VERSION")
            ));
            assert_eq!(healthy.status, "online");
            assert_eq!(healthy.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
            assert!(healthy.latency_ms.is_some());
        }

        #[test]
        fn connection_probe_rejects_missing_or_mismatched_server_version() {
            let missing = probe_health_body(r#"{"status":"ok","uptime_seconds":1}"#);
            assert_eq!(missing.status, "offline");
            assert!(missing.version.is_none());
            assert_eq!(
                missing.message,
                "Server 未返回可用的 UnionC 健康状态（格式或版本信息无效）"
            );

            let mismatched = probe_health_body(
                r#"{"status":"ok","version":"incompatible-test-version","uptime_seconds":1}"#,
            );
            assert_eq!(mismatched.status, "offline");
            assert_eq!(
                mismatched.version.as_deref(),
                Some("incompatible-test-version")
            );
            assert!(mismatched.message.contains("版本不匹配"));
            assert!(mismatched.message.contains(env!("CARGO_PKG_VERSION")));
        }

        #[test]
        fn connection_probe_rejects_non_unionc_success_response() {
            let invalid = probe_health_body("<html>not UnionC</html>");
            assert_eq!(invalid.status, "offline");
            assert!(invalid.message.contains("UnionC"));

            let current_body = format!(
                r#"{{"status":"ok","version":"{}","uptime_seconds":1}}"#,
                env!("CARGO_PKG_VERSION")
            );
            let wrong_status =
                probe_health_response("204 No Content", Some("application/json"), &current_body);
            assert_eq!(wrong_status.status, "offline");
            assert!(wrong_status.message.contains("204"));

            for content_type in [
                None,
                Some("text/plain"),
                Some("application/vnd.unionc+json"),
            ] {
                let wrong_type = probe_health_response("200 OK", content_type, &current_body);
                assert_eq!(wrong_type.status, "offline");
                assert!(wrong_type.message.contains("Content-Type"));
            }
        }
    }
}
