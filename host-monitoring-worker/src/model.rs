use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use unionc_protocol::{
    AGENT_REPORT_MAX_AGENT_VERSION_BYTES, AGENT_REPORT_MAX_CAPABILITIES,
    AGENT_REPORT_MAX_CAPABILITY_MESSAGE_BYTES, AGENT_REPORT_MAX_CAPABILITY_NAME_BYTES,
    AGENT_REPORT_MAX_CAPABILITY_SOURCE_BYTES, AGENT_REPORT_MAX_CPU_CORES,
    AGENT_REPORT_MAX_DISK_NAME_BYTES, AGENT_REPORT_MAX_DISKS, AGENT_REPORT_MAX_FILE_SYSTEM_BYTES,
    AGENT_REPORT_MAX_GPU_ID_BYTES, AGENT_REPORT_MAX_GPU_NAME_BYTES,
    AGENT_REPORT_MAX_GPU_SOURCE_BYTES, AGENT_REPORT_MAX_GPU_VENDOR_BYTES, AGENT_REPORT_MAX_GPUS,
    AGENT_REPORT_MAX_HOST_ARCH_BYTES, AGENT_REPORT_MAX_HOST_OS_BYTES,
    AGENT_REPORT_MAX_HOST_VERSION_BYTES, AGENT_REPORT_MAX_INTERVAL_SECONDS,
    AGENT_REPORT_MAX_MOUNT_POINT_BYTES, AGENT_REPORT_MAX_NETWORK_NAME_BYTES,
    AGENT_REPORT_MAX_NETWORKS, AGENT_REPORT_MAX_TEMPERATURE_ID_BYTES,
    AGENT_REPORT_MAX_TEMPERATURE_LABEL_BYTES, AGENT_REPORT_MAX_TEMPERATURE_SOURCE_BYTES,
    AGENT_REPORT_MAX_TEMPERATURES, AGENT_REPORT_MIN_INTERVAL_SECONDS, AGENT_REPORT_SCHEMA_VERSION,
    AgentPairingRequest, AgentReport, Capability, HostIdentity,
};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAgentInstanceRequest {
    pub display_name: Option<String>,
    pub expires_in_minutes: Option<i64>,
}

impl CreateAgentInstanceRequest {
    pub fn validated(self) -> Result<(String, i64)> {
        let display_name = self
            .display_name
            .as_deref()
            .unwrap_or("概览")
            .trim()
            .to_owned();
        validate_required("display_name", &display_name, 255)?;
        let expires = self.expires_in_minutes.unwrap_or(15);
        if !(5..=1440).contains(&expires) {
            return Err(Error::BadRequest(
                "expires_in_minutes must be between 5 and 1440".into(),
            ));
        }
        Ok((display_name, expires))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateMonitoringRemarkRequest {
    pub remark: String,
}

impl UpdateMonitoringRemarkRequest {
    pub fn validated(self) -> Result<String> {
        let value = self.remark.trim().to_owned();
        validate_required("remark", &value, 255)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentInstanceSummary {
    pub request_id: String,
    pub instance_id: String,
    pub display_name: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct CreatedAgentInstance {
    #[serde(flatten)]
    pub summary: AgentInstanceSummary,
    pub activation_code: String,
}

#[derive(Debug, Serialize)]
pub struct AgentPairingPublicSummary {
    pub request_id: String,
    pub os: String,
    pub arch: String,
    pub agent_version: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricSummary {
    pub cpu_usage_percent: Option<f64>,
    pub memory_usage_percent: Option<f64>,
    pub network_received_bytes_per_second: Option<f64>,
    pub network_transmitted_bytes_per_second: Option<f64>,
    pub disk_read_bytes_per_second: Option<f64>,
    pub disk_written_bytes_per_second: Option<f64>,
    pub max_temperature_celsius: Option<f64>,
    pub gpu_utilization_percent: Option<f64>,
    pub gpu_memory_usage_percent: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct HostSummary {
    pub id: String,
    pub name: String,
    pub os: String,
    pub os_version: Option<String>,
    pub kernel_version: Option<String>,
    pub arch: String,
    pub agent_version: String,
    pub registered_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub latest_collected_at: Option<DateTime<Utc>>,
    pub status: String,
    pub capabilities: Vec<Capability>,
    #[serde(flatten)]
    pub metrics: MetricSummary,
}

#[derive(Debug, Serialize)]
pub struct HostListResponse {
    pub hosts: Vec<HostSummary>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct HostDetailResponse {
    pub host: HostSummary,
    pub latest: Option<AgentReport>,
}

#[derive(Debug, Serialize)]
pub struct HistoryPoint {
    pub report_id: String,
    pub collected_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    #[serde(flatten)]
    pub metrics: MetricSummary,
}

#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub host_id: String,
    pub points: Vec<HistoryPoint>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
}

pub fn validate_pairing(request: &AgentPairingRequest) -> Result<()> {
    validate_host(&request.host)?;
    validate_hash("token_hash", &request.token_hash)?;
    validate_hash("polling_secret_hash", &request.polling_secret_hash)?;
    if request.token_hash == request.polling_secret_hash {
        return Err(Error::BadRequest(
            "token_hash and polling_secret_hash must differ".into(),
        ));
    }
    Ok(())
}

pub fn validate_report(report: &AgentReport) -> Result<MetricSummary> {
    validate_host(&report.host)?;
    canonical_uuid(&report.report_id, "report_id")?;
    if report.schema_version != AGENT_REPORT_SCHEMA_VERSION {
        return Err(Error::BadRequest(
            "unsupported agent report schema_version".into(),
        ));
    }
    if !report.interval_seconds.is_finite()
        || !(AGENT_REPORT_MIN_INTERVAL_SECONDS..=AGENT_REPORT_MAX_INTERVAL_SECONDS as f64)
            .contains(&report.interval_seconds)
    {
        return Err(Error::BadRequest(
            "interval_seconds is outside the supported range".into(),
        ));
    }
    if report.collected_at > Utc::now() + chrono::Duration::minutes(5) {
        return Err(Error::BadRequest(
            "collected_at is too far in the future".into(),
        ));
    }
    if report.capabilities.len() > AGENT_REPORT_MAX_CAPABILITIES
        || report.system.cpu.per_core_percent.len() > AGENT_REPORT_MAX_CPU_CORES
        || report.system.networks.len() > AGENT_REPORT_MAX_NETWORKS
        || report.system.disks.len() > AGENT_REPORT_MAX_DISKS
        || report.system.temperatures.len() > AGENT_REPORT_MAX_TEMPERATURES
        || report.system.gpus.len() > AGENT_REPORT_MAX_GPUS
    {
        return Err(Error::BadRequest("report contains too many devices".into()));
    }
    if report.system.cpu.logical_count == 0
        || report.system.cpu.per_core_percent.len() != report.system.cpu.logical_count as usize
    {
        return Err(Error::BadRequest(
            "cpu core count and per-core values disagree".into(),
        ));
    }
    percent("cpu.usage_percent", report.system.cpu.usage_percent)?;
    for value in &report.system.cpu.per_core_percent {
        percent("cpu.per_core_percent", *value)?;
    }
    if report.system.memory.used_bytes > report.system.memory.total_bytes
        || report.system.memory.available_bytes > report.system.memory.total_bytes
        || report.system.memory.swap_used_bytes > report.system.memory.swap_total_bytes
    {
        return Err(Error::BadRequest("memory counters exceed totals".into()));
    }
    for capability in &report.capabilities {
        validate_required(
            "capability.name",
            &capability.name,
            AGENT_REPORT_MAX_CAPABILITY_NAME_BYTES,
        )?;
        validate_required(
            "capability.source",
            &capability.source,
            AGENT_REPORT_MAX_CAPABILITY_SOURCE_BYTES,
        )?;
        if let Some(message) = &capability.message {
            validate_optional(
                "capability.message",
                message,
                AGENT_REPORT_MAX_CAPABILITY_MESSAGE_BYTES,
            )?;
        }
    }
    for network in &report.system.networks {
        validate_required(
            "network.name",
            &network.name,
            AGENT_REPORT_MAX_NETWORK_NAME_BYTES,
        )?;
        nonnegative(
            "network.received_bytes_per_second",
            network.received_bytes_per_second,
        )?;
        nonnegative(
            "network.transmitted_bytes_per_second",
            network.transmitted_bytes_per_second,
        )?;
    }
    for disk in &report.system.disks {
        validate_optional("disk.name", &disk.name, AGENT_REPORT_MAX_DISK_NAME_BYTES)?;
        validate_required(
            "disk.mount_point",
            &disk.mount_point,
            AGENT_REPORT_MAX_MOUNT_POINT_BYTES,
        )?;
        validate_optional(
            "disk.file_system",
            &disk.file_system,
            AGENT_REPORT_MAX_FILE_SYSTEM_BYTES,
        )?;
        if disk.available_bytes > disk.total_bytes {
            return Err(Error::BadRequest(
                "disk available bytes exceed total".into(),
            ));
        }
        nonnegative("disk.read_bytes_per_second", disk.read_bytes_per_second)?;
        nonnegative(
            "disk.written_bytes_per_second",
            disk.written_bytes_per_second,
        )?;
    }
    for sensor in &report.system.temperatures {
        validate_optional(
            "temperature.id",
            &sensor.id,
            AGENT_REPORT_MAX_TEMPERATURE_ID_BYTES,
        )?;
        validate_optional(
            "temperature.label",
            &sensor.label,
            AGENT_REPORT_MAX_TEMPERATURE_LABEL_BYTES,
        )?;
        validate_optional(
            "temperature.source",
            &sensor.source,
            AGENT_REPORT_MAX_TEMPERATURE_SOURCE_BYTES,
        )?;
        for value in [sensor.celsius, sensor.max_celsius, sensor.critical_celsius]
            .into_iter()
            .flatten()
        {
            if !value.is_finite() || !(-273.15..=1000.0).contains(&value) {
                return Err(Error::BadRequest("invalid temperature".into()));
            }
        }
    }
    for gpu in &report.system.gpus {
        validate_optional("gpu.id", &gpu.id, AGENT_REPORT_MAX_GPU_ID_BYTES)?;
        validate_optional("gpu.vendor", &gpu.vendor, AGENT_REPORT_MAX_GPU_VENDOR_BYTES)?;
        validate_optional("gpu.name", &gpu.name, AGENT_REPORT_MAX_GPU_NAME_BYTES)?;
        validate_optional("gpu.source", &gpu.source, AGENT_REPORT_MAX_GPU_SOURCE_BYTES)?;
        if let Some(value) = gpu.utilization_percent {
            percent("gpu.utilization_percent", value)?;
        }
        if gpu
            .memory_used_bytes
            .zip(gpu.memory_total_bytes)
            .is_some_and(|(used, total)| used > total)
        {
            return Err(Error::BadRequest("GPU memory usage exceeds total".into()));
        }
        for value in [
            gpu.power_watts,
            gpu.core_clock_mhz,
            gpu.memory_clock_mhz,
            gpu.pcie_rx_bytes_per_second,
            gpu.pcie_tx_bytes_per_second,
        ]
        .into_iter()
        .flatten()
        {
            nonnegative("gpu metric", value)?;
        }
    }
    Ok(metric_summary(report))
}

pub fn validate_host(host: &HostIdentity) -> Result<()> {
    canonical_uuid(&host.id, "host.id")?;
    validate_required("host.os", &host.os, AGENT_REPORT_MAX_HOST_OS_BYTES)?;
    validate_required("host.arch", &host.arch, AGENT_REPORT_MAX_HOST_ARCH_BYTES)?;
    validate_required(
        "host.agent_version",
        &host.agent_version,
        AGENT_REPORT_MAX_AGENT_VERSION_BYTES,
    )?;
    validate_optional(
        "host.os_version",
        host.os_version.as_deref().unwrap_or(""),
        AGENT_REPORT_MAX_HOST_VERSION_BYTES,
    )?;
    validate_optional(
        "host.kernel_version",
        host.kernel_version.as_deref().unwrap_or(""),
        AGENT_REPORT_MAX_HOST_VERSION_BYTES,
    )?;
    if host.agent_version != env!("CARGO_PKG_VERSION") {
        return Err(Error::BadRequest(format!(
            "unsupported host.agent_version; expected {}",
            env!("CARGO_PKG_VERSION")
        )));
    }
    Ok(())
}

pub fn canonical_uuid(value: &str, field: &str) -> Result<uuid::Uuid> {
    let parsed = uuid::Uuid::parse_str(value)
        .map_err(|_| Error::BadRequest(format!("{field} must be a canonical UUID")))?;
    if parsed.to_string() != value {
        return Err(Error::BadRequest(format!(
            "{field} must be lowercase and hyphenated"
        )));
    }
    Ok(parsed)
}

fn validate_hash(field: &str, value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::BadRequest(format!(
            "{field} must be lowercase SHA-256 hex"
        )));
    }
    Ok(())
}

fn validate_required(field: &str, value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::BadRequest(format!("{field} must not be empty")));
    }
    validate_optional(field, value, max)
}

fn validate_optional(field: &str, value: &str, max: usize) -> Result<()> {
    if value.len() > max || value.chars().any(char::is_control) {
        return Err(Error::BadRequest(format!("invalid {field}")));
    }
    Ok(())
}

fn percent(field: &str, value: f64) -> Result<()> {
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        return Err(Error::BadRequest(format!("invalid {field}")));
    }
    Ok(())
}

fn nonnegative(field: &str, value: f64) -> Result<()> {
    if !value.is_finite() || value < 0.0 {
        return Err(Error::BadRequest(format!("invalid {field}")));
    }
    Ok(())
}

fn metric_summary(report: &AgentReport) -> MetricSummary {
    let gpu_memory = report
        .system
        .gpus
        .iter()
        .filter_map(|gpu| gpu.memory_used_bytes.zip(gpu.memory_total_bytes))
        .fold((0_u64, 0_u64), |sum, value| {
            (sum.0.saturating_add(value.0), sum.1.saturating_add(value.1))
        });
    MetricSummary {
        cpu_usage_percent: Some(report.system.cpu.usage_percent),
        memory_usage_percent: (report.system.memory.total_bytes > 0).then(|| {
            report.system.memory.used_bytes as f64 * 100.0 / report.system.memory.total_bytes as f64
        }),
        network_received_bytes_per_second: report
            .system
            .networks
            .iter()
            .map(|v| v.received_bytes_per_second)
            .reduce(f64::max),
        network_transmitted_bytes_per_second: report
            .system
            .networks
            .iter()
            .map(|v| v.transmitted_bytes_per_second)
            .reduce(f64::max),
        disk_read_bytes_per_second: report
            .system
            .disks
            .iter()
            .map(|v| v.read_bytes_per_second)
            .reduce(f64::max),
        disk_written_bytes_per_second: report
            .system
            .disks
            .iter()
            .map(|v| v.written_bytes_per_second)
            .reduce(f64::max),
        max_temperature_celsius: report
            .system
            .temperatures
            .iter()
            .filter_map(|v| v.celsius)
            .chain(
                report
                    .system
                    .gpus
                    .iter()
                    .filter_map(|v| v.temperature_celsius),
            )
            .reduce(f64::max),
        gpu_utilization_percent: report
            .system
            .gpus
            .iter()
            .filter_map(|v| v.utilization_percent)
            .reduce(f64::max),
        gpu_memory_usage_percent: (gpu_memory.1 > 0)
            .then(|| gpu_memory.0 as f64 * 100.0 / gpu_memory.1 as f64),
    }
}

pub fn host_status(last_seen: DateTime<Utc>, interval: Option<f64>) -> String {
    let age = (Utc::now() - last_seen).num_seconds().max(0) as f64;
    let interval = interval.unwrap_or(10.0).clamp(1.0, 3600.0);
    if age <= (interval * 3.0).max(30.0) {
        "online"
    } else if age <= (interval * 12.0).max(300.0) {
        "stale"
    } else {
        "offline"
    }
    .into()
}
