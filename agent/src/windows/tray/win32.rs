fn canonical_uuid(value: &str, label: &str) -> anyhow::Result<uuid::Uuid> {
    let parsed = uuid::Uuid::parse_str(value).with_context(|| format!("{label} is not a UUID"))?;
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
