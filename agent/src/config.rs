use std::{
    env, fs,
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::private_fs::{self, OwnerPolicy};
use anyhow::{Context, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8081/api/agent/v1/report";

/// 上报报文中“实测间隔”的**服务端契约上限**。
///
/// `server/src/monitoring/model/validation.rs` 是 HTTP 契约的权威实现，本常量必须与其
/// 上限同步。
/// SQLite 只用 `0 < interval_seconds <= 3600` 做粗粒度存储防线；Agent 的配置值则是
/// 整数秒（最小 1），并按 jitter 后最坏实测周期校验，因此三层边界相关但并不完全相同。
///
/// 报文中的 `interval_seconds` 是实测经过时间，因此落到区间之外会让**每一次**上报
/// 都被服务端以 400 永久拒绝；投递 worker 会把这类必失败报文从 spool 确认丢弃。
/// 在启动时拒绝，好过让用户只在日志里追查周期性数据缺口。
pub const MAX_REPORT_INTERVAL_SECONDS: u64 = 3600;

/// 契约下限。
///
/// 下限与上限同样需要显式常量。采集侧对实测间隔的兜底若只写成"防除零"的 `.max(0.001)`，
/// 就比服务端要求的 0.1 **低两个数量级**：当前主循环最小 sleep 0.5 秒触及不到，
/// 但那是巧合而非约束，一旦调整 jitter 或引入更短的周期，报文就会被判为 400
/// （永久拒绝）**直接丢弃**。采集侧引用本常量，使这条边界有守卫而不是靠巧合成立。
pub const MIN_REPORT_INTERVAL_SECONDS: f64 = 0.1;

/// 编译期守卫：契约区间必须自洽。写成 `const _` 而非测试，是因为这两个常量的关系
/// 属于"不可能为真就不该编译"的性质，没有必要等到跑测试才发现。
const _: () = assert!(MIN_REPORT_INTERVAL_SECONDS > 0.0);
const _: () = assert!(MIN_REPORT_INTERVAL_SECONDS < MAX_REPORT_INTERVAL_SECONDS as f64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentCommand {
    Run,
    Once,
    Probe,
    Pair,
    Doctor,
    Status,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputMode {
    Json,
    #[default]
    Human,
}

#[derive(Clone, Copy)]
pub(crate) struct CurrentPackageVersion;

impl Serialize for CurrentPackageVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(env!("CARGO_PKG_VERSION"))
    }
}

impl<'de> Deserialize<'de> for CurrentPackageVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = String::deserialize(deserializer)?;
        if version == env!("CARGO_PKG_VERSION") {
            Ok(Self)
        } else {
            Err(D::Error::custom(format!(
                "configuration belongs to Agent {version}, expected {}",
                env!("CARGO_PKG_VERSION")
            )))
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub(crate) application_version: CurrentPackageVersion,
    pub endpoint: String,
    /// Browser-authorized v2 pairing endpoint. When omitted it is derived from
    /// the current report endpoint.
    pub pairing_endpoint: Option<String>,
    pub otlp_endpoint: Option<String>,
    pub otlp_token: Option<String>,
    /// 可选显示名称；不改变稳定 host-id。
    pub host_name: Option<String>,
    pub interval_seconds: u64,
    pub slow_interval_seconds: u64,
    pub request_timeout_seconds: u64,
    pub jitter_percent: u8,
    pub state_dir: PathBuf,
    pub spool_max_bytes: u64,
    pub tls_identity_pem: Option<PathBuf>,
    pub tls_identity_pkcs12: Option<PathBuf>,
    pub tls_identity_password: Option<String>,
    pub tls_ca_pem: Option<PathBuf>,
    pub allow_insecure_http: bool,
    #[serde(skip)]
    pub config_path: Option<PathBuf>,
    #[serde(skip)]
    pub server_override: Option<String>,
    #[serde(skip)]
    pub endpoint_override: Option<String>,
    /// Internal machine-readable event stream used by the signed Windows tray
    /// broker. This flag is never read from or persisted to configuration.
    #[serde(skip)]
    pub tray_events: bool,
    /// Internal graceful-cancellation event for the elevated Windows tray
    /// broker. It is accepted only together with `pair --tray-events`.
    #[serde(skip)]
    pub tray_cancel_event: Option<String>,
    /// Hard upper bound for a tray-initiated pairing operation.
    #[serde(skip)]
    pub tray_deadline_seconds: Option<u64>,
    /// Internal one-shot authorization-key channel. The signed Windows tray
    /// broker supplies the key over the pair child's anonymous stdin; it is
    /// never accepted as an argument, environment variable, or config value.
    #[serde(skip)]
    pub tray_activation_stdin: bool,
    /// Explicit user-confirmed replacement of an incomplete request targeting
    /// another server. Ordinary CLI pairing remains resumable/fail-closed.
    #[serde(skip)]
    pub replace_pending_pairing: bool,
    /// Presentation is process-local and is never persisted.
    #[serde(skip)]
    pub output_mode: OutputMode,
    /// `doctor` is read-only by default. This explicit opt-in performs an
    /// end-to-end delivery probe for administrators who need it.
    #[serde(skip)]
    pub doctor_delivery: bool,
    /// A lenient `status` load records configuration trouble instead of
    /// preventing the one command intended to diagnose it.
    #[serde(skip)]
    pub config_issue: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            application_version: CurrentPackageVersion,
            endpoint: DEFAULT_ENDPOINT.to_string(),
            pairing_endpoint: None,
            otlp_endpoint: None,
            otlp_token: None,
            host_name: None,
            interval_seconds: 10,
            slow_interval_seconds: 30,
            request_timeout_seconds: 10,
            jitter_percent: 10,
            state_dir: default_state_dir(),
            spool_max_bytes: 64 * 1024 * 1024,
            tls_identity_pem: None,
            tls_identity_pkcs12: None,
            tls_identity_password: None,
            tls_ca_pem: None,
            allow_insecure_http: false,
            config_path: None,
            server_override: None,
            endpoint_override: None,
            tray_events: false,
            tray_cancel_event: None,
            tray_deadline_seconds: None,
            tray_activation_stdin: false,
            replace_pending_pairing: false,
            output_mode: OutputMode::Human,
            doctor_delivery: false,
            config_issue: None,
        }
    }
}

impl AgentConfig {
    pub fn load_from_args() -> anyhow::Result<(Self, AgentCommand)> {
        let mut command = None;
        let mut windows_service = false;
        let mut config_path = env::var_os("UNIONC_AGENT_CONFIG").map(PathBuf::from);
        let mut server_override = None;
        let mut host_name_override = None;
        let mut endpoint_override = None;
        let mut allow_insecure_http = false;
        let mut tray_events = false;
        let mut tray_cancel_event = None;
        let mut tray_deadline_seconds = None;
        let mut tray_activation_stdin = false;
        let mut replace_pending_pairing = false;
        let mut output_mode = OutputMode::Human;
        let mut output_mode_selected = false;
        let mut doctor_delivery = false;
        let mut args = env::args().skip(1);
        let mut argument_position = 0usize;
        while let Some(arg) = args.next() {
            argument_position += 1;
            match arg.as_str() {
                "run" => select_command(&mut command, AgentCommand::Run, "run")?,
                "once" => select_command(&mut command, AgentCommand::Once, "once")?,
                "probe" => select_command(&mut command, AgentCommand::Probe, "probe")?,
                "pair" => select_command(&mut command, AgentCommand::Pair, "pair")?,
                "doctor" => select_command(&mut command, AgentCommand::Doctor, "doctor")?,
                "status" => select_command(&mut command, AgentCommand::Status, "status")?,
                crate::service::WINDOWS_SERVICE_ARGUMENT => {
                    validate_windows_service_position(argument_position)?;
                    if windows_service {
                        bail!("--windows-service may be specified only once");
                    }
                    windows_service = true;
                }
                "--config" => {
                    let value = args.next().context("--config requires a file path")?;
                    config_path = Some(PathBuf::from(value));
                }
                "--server" => {
                    server_override = Some(args.next().context("--server requires a URL")?);
                }
                "--endpoint" => {
                    endpoint_override =
                        Some(args.next().context("--endpoint requires a report URL")?);
                }
                "--name" => {
                    host_name_override = Some(args.next().context("--name requires a host name")?);
                }
                "--allow-insecure-http" => allow_insecure_http = true,
                "--tray-events" => {
                    if tray_events {
                        bail!("--tray-events may be specified only once");
                    }
                    tray_events = true;
                }
                "--tray-cancel-event" => {
                    if tray_cancel_event.is_some() {
                        bail!("--tray-cancel-event may be specified only once");
                    }
                    tray_cancel_event = Some(
                        args.next()
                            .context("--tray-cancel-event requires an event name")?,
                    );
                }
                "--tray-deadline-seconds" => {
                    if tray_deadline_seconds.is_some() {
                        bail!("--tray-deadline-seconds may be specified only once");
                    }
                    tray_deadline_seconds = Some(
                        args.next()
                            .context("--tray-deadline-seconds requires a value")?
                            .parse::<u64>()
                            .context("invalid --tray-deadline-seconds value")?,
                    );
                }
                "--tray-activation-stdin" => {
                    if tray_activation_stdin {
                        bail!("--tray-activation-stdin may be specified only once");
                    }
                    tray_activation_stdin = true;
                }
                "--replace-pending-pairing" => {
                    if replace_pending_pairing {
                        bail!("--replace-pending-pairing may be specified only once");
                    }
                    replace_pending_pairing = true;
                }
                "--output" => {
                    if output_mode_selected {
                        bail!("--output may be specified only once");
                    }
                    output_mode = match args
                        .next()
                        .context("--output requires human or json")?
                        .as_str()
                    {
                        "human" => OutputMode::Human,
                        "json" => OutputMode::Json,
                        other => bail!("unsupported output format {other}; use human or json"),
                    };
                    output_mode_selected = true;
                }
                "--json" => {
                    if output_mode_selected {
                        bail!("--json conflicts with an earlier output option");
                    }
                    output_mode = OutputMode::Json;
                    output_mode_selected = true;
                }
                "--delivery" => {
                    if doctor_delivery {
                        bail!("--delivery may be specified only once");
                    }
                    doctor_delivery = true;
                }
                "-V" | "--version" => {
                    println!("unionc-agent {}", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                "-h" | "--help" => {
                    if windows_service {
                        bail!("--help is not available in Windows service mode");
                    }
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other}"),
            }
        }
        let command = command.unwrap_or(AgentCommand::Run);
        if doctor_delivery && command != AgentCommand::Doctor {
            bail!("--delivery may be used only with doctor");
        }
        validate_windows_service_invocation(windows_service, command)?;
        if tray_events && command != AgentCommand::Pair {
            bail!("--tray-events may be used only with the pair command");
        }
        if (tray_cancel_event.is_some()
            || tray_deadline_seconds.is_some()
            || tray_activation_stdin
            || replace_pending_pairing)
            && (!tray_events || command != AgentCommand::Pair)
        {
            bail!("internal tray pairing controls require pair --tray-events");
        }
        if let Some(seconds) = tray_deadline_seconds
            && !(60..=3600).contains(&seconds)
        {
            bail!("--tray-deadline-seconds must be between 60 and 3600");
        }
        if let Some(name) = &tray_cancel_event {
            validate_tray_cancel_event(name)?;
        }

        if config_path.is_none() {
            let default = default_config_path();
            if default.is_file() {
                config_path = Some(default);
            }
        }
        let mut config_issue = None;
        let mut config = if let Some(path) = config_path.as_ref().filter(|path| path.is_file()) {
            match fs::read(path).and_then(|bytes| {
                serde_json::from_slice(&bytes)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            }) {
                Ok(config) => config,
                Err(error) if command == AgentCommand::Status => {
                    config_issue = Some(format!("failed to load {}: {error}", path.display()));
                    Self::default()
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to load config {}", path.display()));
                }
            }
        } else {
            if command == AgentCommand::Status
                && let Some(path) = config_path.as_ref()
            {
                config_issue = Some(format!("configuration file {} is missing", path.display()));
            }
            Self::default()
        };
        config.apply_environment()?;
        config.config_path = config_path;
        config.server_override = server_override;
        config.endpoint_override = endpoint_override;
        config.tray_events = tray_events;
        config.tray_cancel_event = tray_cancel_event;
        config.tray_deadline_seconds = tray_deadline_seconds;
        config.tray_activation_stdin = tray_activation_stdin;
        config.replace_pending_pairing = replace_pending_pairing;
        config.output_mode = output_mode;
        config.doctor_delivery = doctor_delivery;
        config.config_issue = config_issue;
        if let Some(name) = host_name_override {
            config.host_name = non_empty(name);
        }
        if allow_insecure_http {
            config.allow_insecure_http = true;
        }
        if command == AgentCommand::Pair {
            config.apply_pair_options()?;
        } else if let Some(endpoint) = config.endpoint_override.take() {
            config.endpoint = endpoint;
        }
        config.validate(command)?;
        Ok((config, command))
    }

    fn apply_pair_options(&mut self) -> anyhow::Result<()> {
        if self.server_override.is_some() && self.endpoint_override.is_some() {
            bail!("pair accepts either --server or --endpoint, not both");
        }
        if let Some(server) = self.server_override.as_deref() {
            let server = crate::tray_support::validate_server_base(server)
                .context("invalid --server URL")?;
            self.endpoint = format!("{}/api/agent/v1/report", server.trim_end_matches('/'));
            self.pairing_endpoint = Some(format!(
                "{}/api/agent/v2/pairing-requests",
                server.trim_end_matches('/')
            ));
        } else if let Some(endpoint) = self.endpoint_override.take() {
            self.endpoint = endpoint;
            self.pairing_endpoint = None;
        }

        Ok(())
    }

    fn apply_environment(&mut self) -> anyhow::Result<()> {
        if let Ok(value) = env::var("UNIONC_AGENT_ENDPOINT") {
            self.endpoint = value;
        }
        if let Ok(value) = env::var("UNIONC_AGENT_PAIRING_ENDPOINT") {
            self.pairing_endpoint = non_empty(value);
        }
        if let Ok(value) = env::var("UNIONC_AGENT_OTLP_ENDPOINT") {
            self.otlp_endpoint = non_empty(value);
        }
        if let Ok(value) = env::var("UNIONC_AGENT_OTLP_TOKEN") {
            self.otlp_token = non_empty(value);
        }
        if let Ok(value) = env::var("UNIONC_AGENT_HOST_NAME") {
            self.host_name = non_empty(value);
        }
        if let Ok(value) = env::var("UNIONC_AGENT_STATE_DIR") {
            self.state_dir = PathBuf::from(value);
        }
        if let Ok(value) = env::var("UNIONC_AGENT_INTERVAL_SECONDS") {
            self.interval_seconds = value.parse().context("invalid interval")?;
        }
        if let Ok(value) = env::var("UNIONC_AGENT_SLOW_INTERVAL_SECONDS") {
            self.slow_interval_seconds = value.parse().context("invalid slow interval")?;
        }
        if let Ok(value) = env::var("UNIONC_AGENT_TLS_IDENTITY_PEM") {
            self.tls_identity_pem = Some(PathBuf::from(value));
        }
        if let Ok(value) = env::var("UNIONC_AGENT_TLS_IDENTITY_PKCS12") {
            self.tls_identity_pkcs12 = Some(PathBuf::from(value));
        }
        if let Ok(value) = env::var("UNIONC_AGENT_TLS_IDENTITY_PASSWORD") {
            self.tls_identity_password = Some(value);
        }
        if let Ok(value) = env::var("UNIONC_AGENT_TLS_CA_PEM") {
            self.tls_ca_pem = Some(PathBuf::from(value));
        }
        if let Ok(value) = env::var("UNIONC_AGENT_ALLOW_INSECURE_HTTP") {
            self.allow_insecure_http =
                parse_bool(&value).context("invalid UNIONC_AGENT_ALLOW_INSECURE_HTTP boolean")?;
        }
        Ok(())
    }

    fn validate(&self, command: AgentCommand) -> anyhow::Result<()> {
        // Status must be available precisely when configuration is missing or
        // malformed. It reports those conditions in its snapshot instead of
        // failing before any diagnostics can be rendered.
        if command == AgentCommand::Status {
            return Ok(());
        }
        if self.interval_seconds == 0 {
            bail!("interval_seconds must be greater than zero");
        }
        if self.jitter_percent > 50 {
            bail!("jitter_percent must not exceed 50");
        }
        // 校验的是**最坏情况下的采样 cadence**，而不是配置值本身。
        //
        // run 模式的网络投递由独立 worker 完成，不再计入两次采样之间的间隔；正常
        // 周期只由 ticker 的 jitter 决定。`interval_seconds = 3600` 若只比较配置值，
        // 仍会被 10% 默认 jitter 推到 3960 秒并被服务端判为 400。
        //
        // 那种失败尤其难查：报文入 spool 后仍会因永久拒绝被确认丢弃，现象只是
        // "数据没了"加上日志里周期性的 400。因此在启动时按最坏 jitter
        // 拒绝，并把可用上限直接算给用户。
        let worst_case_cycle = self.worst_case_cycle_seconds();
        if worst_case_cycle > MAX_REPORT_INTERVAL_SECONDS as f64 {
            bail!(
                "interval_seconds ({}) with jitter_percent ({}) can produce a measured interval \
                 of up to {worst_case_cycle:.0}s, which exceeds the server contract limit of \
                 {MAX_REPORT_INTERVAL_SECONDS}s; such reports are rejected with HTTP 400 and \
                 discarded from the spool. Use interval_seconds <= {} at this jitter, \
                 or lower jitter_percent",
                self.interval_seconds,
                self.jitter_percent,
                self.max_interval_seconds_at_current_jitter()
            );
        }
        if self.slow_interval_seconds < self.interval_seconds {
            bail!("slow_interval_seconds must be at least interval_seconds");
        }
        if self.request_timeout_seconds == 0 {
            bail!("request_timeout_seconds must be greater than zero");
        }
        if self.request_timeout_seconds > 300 {
            bail!("request_timeout_seconds must not exceed 300 seconds");
        }
        if self.spool_max_bytes < 1024 * 1024 {
            bail!("spool_max_bytes must be at least 1 MiB");
        }
        let validates_delivery = match command {
            AgentCommand::Probe => false,
            AgentCommand::Doctor => self.doctor_delivery,
            _ => true,
        };
        if validates_delivery {
            validate_endpoint(&self.endpoint, self.allow_insecure_http)?;
            validate_pairing_endpoint(&self.pairing_endpoint(), self.allow_insecure_http)?;
        }
        #[cfg(not(feature = "otlp"))]
        if validates_delivery && (self.otlp_endpoint.is_some() || self.otlp_token.is_some()) {
            bail!(
                "OTLP export is configured but this Agent was built without the optional `otlp` \
                 feature; rebuild with `--features otlp` or remove the OTLP settings"
            );
        }
        #[cfg(feature = "otlp")]
        if validates_delivery && let Some(endpoint) = &self.otlp_endpoint {
            validate_endpoint(endpoint, self.allow_insecure_http)?;
        }
        if validates_delivery
            && self.tls_identity_pem.is_some()
            && self.tls_identity_pkcs12.is_some()
        {
            bail!("configure only one TLS client identity format");
        }
        if validates_delivery
            && self.tls_identity_password.is_some()
            && self.tls_identity_pkcs12.is_none()
        {
            bail!("tls_identity_password requires tls_identity_pkcs12");
        }
        #[cfg(all(not(windows), not(target_os = "macos")))]
        if validates_delivery && self.tls_identity_pkcs12.is_some() {
            bail!(
                "tls_identity_pkcs12 is supported only on Windows and macOS; use \
                 tls_identity_pem on this platform"
            );
        }
        #[cfg(any(windows, target_os = "macos"))]
        if validates_delivery && self.tls_identity_pem.is_some() {
            bail!(
                "the native TLS backend requires tls_identity_pkcs12 instead of \
                 tls_identity_pem"
            );
        }
        if let Some(name) = &self.host_name
            && (name.len() > 255 || name.trim().is_empty() || name.chars().any(char::is_control))
        {
            bail!("host_name must be 1 to 255 bytes without control characters");
        }
        Ok(())
    }

    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_seconds)
    }

    pub fn diagnostic_config_issue(&self) -> Option<&str> {
        self.config_issue.as_deref()
    }

    /// Validate the effective configuration for a future `run` without
    /// requiring or changing credentials. Diagnostic renderers use this to
    /// report every problem instead of aborting before producing a result.
    pub fn validate_for_diagnostics(&self) -> anyhow::Result<()> {
        if let Some(issue) = &self.config_issue {
            bail!("{issue}");
        }
        self.validate(AgentCommand::Run)
    }

    /// jitter 能把一个采集周期拉长到的最大秒数。
    ///
    /// 与 `main.rs` 的 `jitter()` 共用同一个上界公式：`base * (1 + percent/100)`。
    /// 两处若各写一遍就会漂移，因此校验侧引用本函数，而不是重新推导一遍系数。
    pub fn worst_case_cycle_seconds(&self) -> f64 {
        self.interval_seconds as f64 * (1.0 + self.jitter_percent as f64 / 100.0)
    }

    /// 当前 jitter 设置下，仍能满足服务端契约的最大 `interval_seconds`。
    ///
    /// 向下取整：取整后的值代入 `worst_case_cycle_seconds()` 必定 <= 契约上限。
    pub fn max_interval_seconds_at_current_jitter(&self) -> u64 {
        (MAX_REPORT_INTERVAL_SECONDS as f64 / (1.0 + self.jitter_percent as f64 / 100.0)) as u64
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_seconds)
    }

    pub fn pairing_endpoint(&self) -> String {
        self.pairing_endpoint.clone().unwrap_or_else(|| {
            if let Some(base) = self.endpoint.strip_suffix("/api/agent/v1/report") {
                return format!("{base}/api/agent/v2/pairing-requests");
            }
            let mut url = reqwest::Url::parse(&self.endpoint)
                .expect("endpoint was validated before pairing_endpoint is used");
            url.set_path("/api/agent/v2/pairing-requests");
            url.set_query(None);
            url.set_fragment(None);
            url.to_string().trim_end_matches('/').to_string()
        })
    }

    /// Save the browser-pairing endpoint only after the pending request has
    /// been durably recorded. This makes an interrupted `pair` resumable.
    pub fn persist_after_pairing(&self) -> anyhow::Result<PathBuf> {
        self.persist_durable_config()
    }

    fn persist_durable_config(&self) -> anyhow::Result<PathBuf> {
        let path = self.config_path.clone().unwrap_or_else(default_config_path);
        let mut persisted = self.clone();
        persisted.config_path = None;
        persisted.server_override = None;
        persisted.endpoint_override = None;
        persisted.tray_events = false;
        persisted.tray_cancel_event = None;
        persisted.tray_deadline_seconds = None;
        persisted.tray_activation_stdin = false;
        persisted.replace_pending_pairing = false;
        let bytes = serde_json::to_vec_pretty(&persisted)?;
        persist_private_config(&path, &bytes)?;
        Ok(path)
    }
}

fn select_command(
    selected: &mut Option<AgentCommand>,
    command: AgentCommand,
    spelling: &str,
) -> anyhow::Result<()> {
    if let Some(previous) = selected {
        bail!("multiple commands are not allowed (selected {previous:?}, then {spelling})");
    }
    *selected = Some(command);
    Ok(())
}

fn validate_windows_service_invocation(
    requested: bool,
    command: AgentCommand,
) -> anyhow::Result<()> {
    if !requested {
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = command;
        bail!("--windows-service is available only on Windows");
    }
    #[cfg(windows)]
    {
        if command != AgentCommand::Run {
            bail!("--windows-service may be used only with the run command");
        }
        Ok(())
    }
}

fn validate_windows_service_position(argument_position: usize) -> anyhow::Result<()> {
    if argument_position != 1 {
        bail!("--windows-service must be the first argument");
    }
    Ok(())
}

fn validate_tray_cancel_event(name: &str) -> anyhow::Result<()> {
    #[cfg(not(windows))]
    {
        let _ = name;
        bail!("--tray-cancel-event is available only on Windows");
    }
    #[cfg(windows)]
    {
        const PREFIX: &str = "Local\\UnionCAgentPairCancel-";
        let suffix = name
            .strip_prefix(PREFIX)
            .context("invalid tray cancellation event name")?;
        if suffix.len() != 64 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("invalid tray cancellation event name");
        }
        Ok(())
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn parse_bool(value: &str) -> anyhow::Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("expected true/false, 1/0, yes/no, or on/off"),
    }
}

fn validate_endpoint(endpoint: &str, allow_insecure_http: bool) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(endpoint)
        .with_context(|| format!("invalid telemetry endpoint {endpoint}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("telemetry endpoint must not embed credentials");
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if allow_insecure_http || is_loopback_host(url.host_str()) => Ok(()),
        "http" => bail!(
            "plain HTTP telemetry is allowed only for loopback; use HTTPS or explicitly set \
             allow_insecure_http for an isolated trusted network"
        ),
        scheme => bail!("unsupported telemetry endpoint scheme: {scheme}"),
    }
}

fn validate_pairing_endpoint(endpoint: &str, allow_insecure_http: bool) -> anyhow::Result<()> {
    validate_endpoint(endpoint, allow_insecure_http)?;
    let url = reqwest::Url::parse(endpoint).expect("validate_endpoint accepted the URL");
    if url.query().is_some() || url.fragment().is_some() {
        bail!(
            "pairing_endpoint must not contain a query or fragment because request-specific paths \
             are appended while polling"
        );
    }
    Ok(())
}

fn is_loopback_host(host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

fn default_state_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let base = env::var_os("PROGRAMDATA").unwrap_or_else(|| "C:\\ProgramData".into());
        PathBuf::from(base).join("UnionC Agent")
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Library/Application Support/UnionC Agent")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        PathBuf::from("/var/lib/unionc-agent")
    }
}

fn default_config_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        default_state_dir().join("config.json")
    }
    #[cfg(target_os = "macos")]
    {
        default_state_dir().join("config.json")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        PathBuf::from("/etc/unionc-agent/config.json")
    }
}

fn persist_private_config(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    path.parent().context("config path has no parent")?;
    let mut content = Vec::with_capacity(bytes.len() + 1);
    content.extend_from_slice(bytes);
    content.push(b'\n');
    // pair 通常由 root 执行，而长期服务使用 unionc-agent/_unioncagent。原配置若
    // 已由安装包设置好属主与权限，原子替换必须把它们复制到新 inode；仅保留 mode
    // 会留下 root:root 0640，服务账户仍然读不到。
    private_fs::write_atomic(path, &content, OwnerPolicy::PreserveTarget)
        .with_context(|| format!("failed to save private config {}", path.display()))
}

fn print_help() {
    println!(
        "unionc-agent [run|once|probe|pair|doctor|status] [options]\n\
         run   continuously collect and report read-only telemetry (default)\n\
         once  collect and report one snapshot\n\
         probe print the local capability report without contacting a server\n\
         pair  authorize this host in a browser and store a host-scoped credential\n\
         doctor inspect configuration, collection, authorization, and spool without writes\n\
         status print local identity, authorization, pairing, and spool state\n\
         Pairing example:\n\
           unionc-agent pair --server https://unionc.example.com\n\n\
         Common options: --config PATH [--endpoint REPORT_URL] [--output human|json]\n\
         Doctor delivery opt-in: --delivery (sends one report and may drain queued reports)\n\
         Pair options: [--server URL | --endpoint REPORT_URL] [--name NAME]\n\
         Remote plaintext HTTP is never accepted by browser pairing.\n\n\
         Browser pairing keeps the long-lived secret local and stores it in the private\n\
         state directory; the browser receives only the public activation status."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selecting_more_than_one_command_is_rejected() {
        let mut selected = None;
        select_command(&mut selected, AgentCommand::Run, "run").unwrap();
        let error = select_command(&mut selected, AgentCommand::Probe, "probe")
            .expect_err("a second command must not override the first one");
        assert!(error.to_string().contains("multiple commands"));
        assert_eq!(selected, Some(AgentCommand::Run));
    }

    #[test]
    fn windows_service_flag_is_platform_gated() {
        assert!(validate_windows_service_invocation(false, AgentCommand::Run).is_ok());
        #[cfg(not(windows))]
        assert!(validate_windows_service_invocation(true, AgentCommand::Run).is_err());
        #[cfg(windows)]
        {
            assert!(validate_windows_service_invocation(true, AgentCommand::Run).is_ok());
            assert!(validate_windows_service_invocation(true, AgentCommand::Probe).is_err());
        }
    }

    #[test]
    fn windows_service_flag_must_be_the_first_user_argument() {
        assert!(validate_windows_service_position(1).is_ok());
        assert!(validate_windows_service_position(2).is_err());
        assert!(validate_windows_service_position(3).is_err());
    }

    #[test]
    fn derives_v2_pairing_endpoint_from_v1_report_endpoint() {
        let config = AgentConfig {
            endpoint: "https://unionc.example/prefix/api/agent/v1/report".into(),
            ..AgentConfig::default()
        };
        assert_eq!(
            config.pairing_endpoint(),
            "https://unionc.example/prefix/api/agent/v2/pairing-requests"
        );
    }

    #[test]
    fn pairing_server_must_be_a_root_management_console_origin() {
        let mut root = AgentConfig {
            server_override: Some("https://unionc.example/".into()),
            ..AgentConfig::default()
        };
        root.apply_pair_options().unwrap();
        assert_eq!(
            root.pairing_endpoint.as_deref(),
            Some("https://unionc.example/api/agent/v2/pairing-requests")
        );

        let mut path = AgentConfig {
            server_override: Some("https://unionc.example/console".into()),
            ..AgentConfig::default()
        };
        let error = path
            .apply_pair_options()
            .expect_err("a path would silently target the wrong pairing API");
        assert!(format!("{error:#}").contains("without a path"));
    }

    #[test]
    fn rejects_remote_plaintext_by_default() {
        assert!(validate_endpoint("http://192.0.2.10/report", false).is_err());
        assert!(validate_endpoint("http://127.0.0.1/report", false).is_ok());
        assert!(validate_endpoint("https://telemetry.example/report", false).is_ok());
    }

    #[test]
    fn pairing_endpoint_rejects_query_and_fragment_without_restricting_telemetry() {
        for endpoint in [
            "https://unionc.example/api/agent/v2/pairing-requests?tenant=one",
            "https://unionc.example/api/agent/v2/pairing-requests#bootstrap",
            "https://unionc.example/api/agent/v2/pairing-requests?#",
        ] {
            let config = AgentConfig {
                pairing_endpoint: Some(endpoint.into()),
                ..AgentConfig::default()
            };
            let error = config
                .validate(AgentCommand::Run)
                .expect_err("pairing request paths cannot be appended after a query or fragment");
            assert!(error.to_string().contains("query or fragment"));
        }

        assert!(
            validate_endpoint("https://telemetry.example/report?tenant=one#client", false).is_ok(),
            "the generic telemetry endpoint keeps its existing query/fragment policy"
        );
    }

    #[test]
    fn local_diagnostics_are_not_blocked_by_a_bad_network_endpoint() {
        let config = AgentConfig {
            endpoint: "not a URL".into(),
            ..AgentConfig::default()
        };
        assert!(config.validate(AgentCommand::Status).is_ok());
        assert!(config.validate(AgentCommand::Probe).is_ok());
        assert!(config.validate(AgentCommand::Doctor).is_ok());
        assert!(config.validate(AgentCommand::Run).is_err());
    }

    #[test]
    fn doctor_delivery_explicitly_restores_network_validation() {
        let config = AgentConfig {
            endpoint: "not a URL".into(),
            doctor_delivery: true,
            ..AgentConfig::default()
        };
        assert!(config.validate(AgentCommand::Doctor).is_err());
    }

    #[test]
    fn tls_identity_password_requires_a_pkcs12_identity() {
        let config = AgentConfig {
            tls_identity_password: Some("secret".into()),
            ..AgentConfig::default()
        };
        let error = config
            .validate(AgentCommand::Run)
            .expect_err("an otherwise unused TLS identity password must not be ignored");
        assert!(error.to_string().contains("tls_identity_pkcs12"));
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    #[test]
    fn non_native_tls_backend_rejects_pkcs12_identity() {
        let config = AgentConfig {
            tls_identity_pkcs12: Some("client-identity.p12".into()),
            ..AgentConfig::default()
        };
        let error = config
            .validate(AgentCommand::Run)
            .expect_err("an unsupported PKCS#12 identity must not be silently ignored");
        assert!(error.to_string().contains("tls_identity_pem"));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn native_tls_backend_rejects_pem_identity() {
        let config = AgentConfig {
            tls_identity_pem: Some("client-identity.pem".into()),
            ..AgentConfig::default()
        };
        let error = config
            .validate(AgentCommand::Run)
            .expect_err("an unsupported PEM identity must not reach request construction");
        assert!(error.to_string().contains("tls_identity_pkcs12"));
    }

    #[test]
    fn removed_pre_pairing_configuration_fields_are_rejected() {
        for field in [
            "registration_endpoint",
            "token",
            "enrollment_token",
            "host_id",
        ] {
            let mut document = serde_json::to_value(AgentConfig::default()).unwrap();
            document
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), serde_json::Value::Null);
            assert!(
                serde_json::from_value::<AgentConfig>(document).is_err(),
                "obsolete field {field} must not be silently accepted"
            );
        }
    }

    #[test]
    fn configuration_requires_the_exact_current_application_version_and_shape() {
        let current = serde_json::to_value(AgentConfig::default()).unwrap();
        assert_eq!(current["application_version"], env!("CARGO_PKG_VERSION"));
        serde_json::from_value::<AgentConfig>(current.clone()).unwrap();

        let mut missing_version = current.clone();
        missing_version
            .as_object_mut()
            .unwrap()
            .remove("application_version");
        assert!(serde_json::from_value::<AgentConfig>(missing_version).is_err());

        let mut different_version = current.clone();
        different_version["application_version"] = serde_json::json!("not-current");
        assert!(serde_json::from_value::<AgentConfig>(different_version).is_err());

        let mut incomplete = current;
        incomplete
            .as_object_mut()
            .unwrap()
            .remove("interval_seconds");
        assert!(serde_json::from_value::<AgentConfig>(incomplete).is_err());
    }

    fn config_with_interval(interval_seconds: u64) -> AgentConfig {
        AgentConfig {
            interval_seconds,
            // slow_interval 必须 >= interval，否则会先撞上另一条校验。
            slow_interval_seconds: interval_seconds,
            ..AgentConfig::default()
        }
    }

    /// 契约上限是对**实测周期**的约束；投递已经解耦，因此正常运行时该周期只由
    /// ticker jitter 决定。默认 jitter 非零时，恰好等于上限仍必然越界。
    ///
    /// 回归：此前这里断言的是 `is_ok()`，于是一个"通过了启动校验"的配置在运行时
    /// 每一份报文都被判 400 并直接丢弃。
    #[test]
    fn rejects_interval_at_the_contract_limit_because_jitter_pushes_it_over() {
        let config = config_with_interval(MAX_REPORT_INTERVAL_SECONDS);
        assert!(config.jitter_percent > 0, "本用例依赖默认 jitter 非零");
        let error = config
            .validate(AgentCommand::Run)
            .expect_err("配置值等于上限时，加上 jitter 必然越界，必须拒绝");
        let message = error.to_string();
        assert!(
            message.contains("3600") && message.contains("400"),
            "错误信息应说明上限与后果，实际为：{message}"
        );
    }

    /// 拒绝之外还要给出**可直接采用**的替代值，否则用户只能靠试。
    #[test]
    fn the_reported_maximum_interval_is_actually_accepted() {
        let rejected = config_with_interval(MAX_REPORT_INTERVAL_SECONDS);
        let suggested = rejected.max_interval_seconds_at_current_jitter();
        let config = config_with_interval(suggested);
        assert!(
            config.validate(AgentCommand::Run).is_ok(),
            "错误信息里建议的 interval_seconds={suggested} 必须真的能通过校验"
        );
        assert!(
            config.worst_case_cycle_seconds() <= MAX_REPORT_INTERVAL_SECONDS as f64,
            "建议值的最坏周期不得越过契约上限"
        );
    }

    /// 零 jitter 时 ticker cadence 就是配置值，因此配置值本身仍可取到上限；机器
    /// 休眠或进程暂停造成的异常越界由采集侧 clamp 兜住。
    #[test]
    fn zero_jitter_allows_the_full_contract_range() {
        let config = AgentConfig {
            jitter_percent: 0,
            ..config_with_interval(MAX_REPORT_INTERVAL_SECONDS)
        };
        assert!(config.validate(AgentCommand::Run).is_ok());
    }

    #[test]
    fn rejects_two_hour_interval_that_would_be_silently_dropped() {
        assert!(
            config_with_interval(7200)
                .validate(AgentCommand::Run)
                .is_err()
        );
    }

    #[test]
    fn request_timeout_is_bounded_for_graceful_tray_cancellation() {
        let mut config = config_with_interval(10);
        config.request_timeout_seconds = 300;
        assert!(config.validate(AgentCommand::Run).is_ok());
        config.request_timeout_seconds = 301;
        let error = config
            .validate(AgentCommand::Run)
            .expect_err("unbounded network waits defeat tray cancellation guarantees");
        assert!(error.to_string().contains("300"));
    }

    #[cfg(not(feature = "otlp"))]
    #[test]
    fn configured_otlp_requires_the_optional_feature() {
        let config = AgentConfig {
            otlp_endpoint: Some("https://collector.example/v1/metrics".into()),
            ..config_with_interval(10)
        };
        let error = config
            .validate(AgentCommand::Run)
            .expect_err("a non-OTLP build must not silently ignore configured export");
        assert!(error.to_string().contains("--features otlp"));
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn optional_otlp_feature_accepts_a_valid_endpoint() {
        let config = AgentConfig {
            otlp_endpoint: Some("https://collector.example/v1/metrics".into()),
            ..config_with_interval(10)
        };
        assert!(config.validate(AgentCommand::Run).is_ok());
    }

    /// 采集侧对实测间隔的兜底必须落在服务端契约内。
    ///
    /// 低于下限的报文会被服务端判为 400（永久拒绝）并从 spool 确认丢弃。这里用
    /// 配置能取到的最极端组合来验证：
    /// 最小合法间隔（1 秒）配合最大 jitter，得到的最短周期仍须高于契约下限。
    #[test]
    fn the_shortest_possible_cycle_stays_inside_the_server_contract() {
        let smallest_interval = 1.0_f64; // validate() 要求 interval_seconds >= 1
        let largest_jitter = 50.0_f64 / 100.0; // validate() 要求 jitter_percent <= 50
        let shortest_cycle = smallest_interval * (1.0 - largest_jitter);

        assert!(
            shortest_cycle > MIN_REPORT_INTERVAL_SECONDS,
            "最短可能周期 {shortest_cycle}s 已逼近契约下限 {MIN_REPORT_INTERVAL_SECONDS}s；\
             调整 jitter 上限或间隔下限时必须重新评估这条边界"
        );
    }
}
