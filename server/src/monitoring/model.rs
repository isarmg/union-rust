use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
pub use unionc_protocol::{
    AGENT_REPORT_SCHEMA_VERSION, AgentHealth, AgentReport, Capability, CapabilityErrorKind,
    CpuSnapshot, DiskSnapshot, GpuSnapshot, HostIdentity, MemorySnapshot, NetworkSnapshot,
    SystemSnapshot, TemperatureSnapshot,
};

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAgentInstanceRequest {
    pub display_name: Option<String>,
    pub expires_in_minutes: Option<i64>,
    /// Re-pair an existing instance while preserving its report history.
    pub instance_id: Option<String>,
}

impl CreateAgentInstanceRequest {
    pub fn validated(&self) -> AppResult<(String, i64, Option<String>)> {
        let display_name = self
            .display_name
            .as_deref()
            .unwrap_or("新 Agent")
            .trim()
            .to_string();
        validate_text("agent instance display_name", &display_name, 255)?;
        let expires_in_minutes = self.expires_in_minutes.unwrap_or(15);
        if !(5..=1440).contains(&expires_in_minutes) {
            return Err(AppError::BadRequest(
                "expires_in_minutes must be between 5 and 1440".to_string(),
            ));
        }
        let instance_id = self
            .instance_id
            .as_deref()
            .map(|value| {
                validate_canonical_uuid("instance_id", value)?;
                Ok::<String, AppError>(value.to_string())
            })
            .transpose()?;
        Ok((display_name, expires_in_minutes, instance_id))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentInstanceSummary {
    /// Invite identifier used by the management DELETE endpoint.
    pub request_id: String,
    /// Authoritative host identifier reserved before the Agent pairs.
    pub instance_id: String,
    pub display_name: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct CreatedAgentInstance {
    #[serde(flatten)]
    pub summary: AgentInstanceSummary,
    /// Plaintext is returned only in the creation response.
    pub activation_code: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPairingRequest {
    pub host: HostIdentity,
    pub token_hash: String,
    pub polling_secret_hash: String,
}

impl AgentPairingRequest {
    pub fn validate(&self) -> AppResult<()> {
        self.host.validate()?;
        validate_sha256_hex("token_hash", &self.token_hash)?;
        validate_sha256_hex("polling_secret_hash", &self.polling_secret_hash)?;
        if self.token_hash == self.polling_secret_hash {
            return Err(AppError::BadRequest(
                "token_hash and polling_secret_hash must be different".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct AgentPairingResponse {
    pub request_id: String,
    pub activation_url: String,
    pub expires_in: u64,
    pub poll_interval: u64,
}

#[derive(Debug, Serialize)]
pub struct AgentPairingStatusResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AgentPairingPublicSummary {
    pub request_id: String,
    pub name: String,
    pub os: String,
    pub arch: String,
    pub agent_version: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivateAgentRequest {
    pub request_id: String,
    pub activation_code: String,
}

#[derive(Debug, Serialize)]
pub struct ActivateAgentResponse {
    pub instance_id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct AgentReportResponse {
    pub host_id: String,
    pub report_id: String,
    pub accepted: bool,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostSummary {
    pub id: String,
    pub name: String,
    pub os: String,
    pub os_version: Option<String>,
    pub kernel_version: Option<String>,
    pub arch: String,
    pub agent_version: String,
    pub lifecycle_status: String,
    pub registered_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub latest_collected_at: Option<DateTime<Utc>>,
    pub status: String,
    pub capabilities: Vec<Capability>,
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
pub struct HostListResponse {
    pub hosts: Vec<HostSummary>,
    /// 库中主机总数。与 `hosts.len()` 不等时说明本页之外还有——
    /// 前端据此提示用户，而不是让截断悄无声息地发生。
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

#[derive(Debug, Clone, Default)]
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

/// Server-side trust-boundary checks for the shared host DTO.
pub trait HostIdentityExt {
    fn validate(&self) -> AppResult<()>;
}

impl HostIdentityExt for HostIdentity {
    fn validate(&self) -> AppResult<()> {
        validate_canonical_uuid("host.id", &self.id)?;
        validate_text("host.name", &self.name, 255)?;
        validate_text("host.os", &self.os, 64)?;
        validate_text("host.arch", &self.arch, 64)?;
        validate_text("host.agent_version", &self.agent_version, 128)?;
        if self.agent_version != env!("CARGO_PKG_VERSION") {
            return Err(AppError::BadRequest(format!(
                "unsupported host.agent_version; expected {}",
                env!("CARGO_PKG_VERSION")
            )));
        }
        // 这两个是**可选的描述性字段**，与上面的身份字段不同：`System::os_version()`
        // 在部分平台返回 None 或空串，因此走宽松校验（可为空，但仍限长、禁控制字符）。
        //
        // 它们必须有界的理由比其他字段更强：二者直接写入 `monitored_hosts` 的
        // 无长度约束 TEXT 列，并随 `HostSummary` 在**主机列表**接口返回——
        // 一台被攻陷的 Agent 注入一次，此后每一次列表查询都要带上这份文本。
        validate_optional_text("host.os_version", self.os_version.as_deref(), 128)?;
        validate_optional_text("host.kernel_version", self.kernel_version.as_deref(), 128)
    }
}

/// Server-only validation and derived metrics for the shared report DTO.
pub trait AgentReportExt {
    fn validate(&self) -> AppResult<()>;
    fn metric_summary(&self) -> MetricSummary;
}

impl AgentReportExt for AgentReport {
    fn validate(&self) -> AppResult<()> {
        self.host.validate()?;
        validate_canonical_uuid("report_id", &self.report_id)?;
        if self.schema_version != AGENT_REPORT_SCHEMA_VERSION {
            return Err(AppError::BadRequest(
                "unsupported agent report schema_version".to_string(),
            ));
        }
        // 上限 3600 是 Agent 与服务端之间的契约，三处必须同步修改：
        // 此处、schema/sqlite.sql 的 CHECK 约束，以及
        // agent/src/config.rs 的 MAX_REPORT_INTERVAL_SECONDS。
        if !self.interval_seconds.is_finite() || !(0.1..=3600.0).contains(&self.interval_seconds) {
            return Err(AppError::BadRequest(
                "interval_seconds is outside the supported range".to_string(),
            ));
        }
        if self.collected_at > Utc::now() + chrono::Duration::minutes(5) {
            return Err(AppError::BadRequest(
                "collected_at is too far in the future".to_string(),
            ));
        }
        // 数量上限必须覆盖报文里**每一个**可变长集合。
        //
        // `per_core_percent` 曾是这份清单里唯一的缺口：它不含文本，因此逃过了下面
        // 那轮逐字段的文本校验，又不在这里的设备计数里。512 KiB 的 body 上限之内
        // 可以塞进约 10 万个浮点数，它们会完整落进 `payload` JSON 文本，并由详情接口
        // 原样回传给控制台。目前最大的商用 CPU 也就几百个逻辑核，4096 留足余量。
        if self.capabilities.len() > 256
            || self.system.cpu.per_core_percent.len() > 4096
            || self.system.networks.len() > 1024
            || self.system.disks.len() > 1024
            || self.system.temperatures.len() > 4096
            || self.system.gpus.len() > 128
        {
            return Err(AppError::BadRequest(
                "report contains too many devices".to_string(),
            ));
        }
        // 报文里的**每一个**文本字段都必须有界。
        //
        // 数量约束（上面那段）管不住内容长度：512 KiB 的 body 上限之内，一台被攻陷的
        // Agent 可以把配额全部塞进任意一个不限长的字符串。这些文本会落库并原样回传给
        // 控制台，因此校验覆盖必须是**穷尽**的，而不是逐个字段临时判断。
        //
        // 两类校验的分工：
        // * `validate_text`          —— 身份类字段，必须非空；
        // * `validate_optional_text` —— 描述性字段，允许为空（采集侧确实会产出空串，
        //   例如 Windows 无卷标磁盘的 `name`、伪文件系统的 `file_system`、无标签传感器
        //   的 `label`），但仍限长、禁控制字符。
        //
        // 下面**逐字段列全**，不写"其余字段同理"之类的概括。概括无法被核查，新增字段
        // 时也不会有任何东西提醒你补上——列全虽然啰嗦，但漏掉一个是看得见的。
        for capability in &self.capabilities {
            validate_text("capability.name", &capability.name, 128)?;
            validate_text("capability.source", &capability.source, 128)?;
            if let Some(error_kind) = &capability.error_kind {
                validate_text("capability.error_kind", error_kind.as_str(), 128)?;
            }
            // message 是人类可读的诊断信息，放宽到 1 KiB，但仍必须有界。
            if let Some(message) = &capability.message {
                validate_text("capability.message", message, 1024)?;
            }
        }
        validate_percent("cpu.usage_percent", self.system.cpu.usage_percent)?;
        if self.system.cpu.logical_count == 0 {
            return Err(AppError::BadRequest(
                "cpu.logical_count must be positive".to_string(),
            ));
        }
        if self.system.cpu.per_core_percent.len() != self.system.cpu.logical_count as usize {
            return Err(AppError::BadRequest(
                "cpu.per_core_percent length must equal cpu.logical_count".to_string(),
            ));
        }
        if self
            .system
            .cpu
            .physical_count
            .is_some_and(|count| count == 0 || count > self.system.cpu.logical_count)
        {
            return Err(AppError::BadRequest(
                "cpu.physical_count must be between 1 and cpu.logical_count".to_string(),
            ));
        }
        for value in &self.system.cpu.per_core_percent {
            validate_percent("cpu.per_core_percent", *value)?;
        }
        if self.system.memory.used_bytes > self.system.memory.total_bytes
            || self.system.memory.available_bytes > self.system.memory.total_bytes
            || self.system.memory.swap_used_bytes > self.system.memory.swap_total_bytes
        {
            return Err(AppError::BadRequest(
                "memory counters exceed their reported totals".to_string(),
            ));
        }
        for network in &self.system.networks {
            validate_text("network.name", &network.name, 255)?;
            validate_nonnegative_rate(
                "network.received_bytes_per_second",
                network.received_bytes_per_second,
            )?;
            validate_nonnegative_rate(
                "network.transmitted_bytes_per_second",
                network.transmitted_bytes_per_second,
            )?;
        }
        for disk in &self.system.disks {
            // Windows 对无卷标卷返回空 `disk.name`，而 mount_point 仍能唯一、可读地
            // 标识该卷（例如 `F:\\`）。前端本来就会回退显示 mount_point；拒绝空名称
            // 只会让整份合法遥测永久 400，导致已配对主机一直显示离线。
            validate_optional_text("disk.name", Some(&disk.name), 1024)?;
            validate_text("disk.mount_point", &disk.mount_point, 4096)?;
            // 伪文件系统与未识别设备的 file_system 可能是空串，故用宽松校验。
            validate_optional_text("disk.file_system", Some(&disk.file_system), 128)?;
            if disk.available_bytes > disk.total_bytes {
                return Err(AppError::BadRequest(
                    "disk available bytes exceed total bytes".to_string(),
                ));
            }
            validate_nonnegative_rate("disk.read_bytes_per_second", disk.read_bytes_per_second)?;
            validate_nonnegative_rate(
                "disk.written_bytes_per_second",
                disk.written_bytes_per_second,
            )?;
        }
        for sensor in &self.system.temperatures {
            // 传感器三个文本字段都可能为空：sysinfo 的 `label()` 对无标签组件返回空串，
            // 而 `id` 在拿不到设备 id 时正是回退到该 label。
            validate_optional_text("temperature.id", Some(&sensor.id), 255)?;
            validate_optional_text("temperature.label", Some(&sensor.label), 255)?;
            validate_optional_text("temperature.source", Some(&sensor.source), 64)?;
            for (field, value) in [
                ("temperature.celsius", sensor.celsius),
                ("temperature.max_celsius", sensor.max_celsius),
                ("temperature.critical_celsius", sensor.critical_celsius),
            ] {
                if value
                    .is_some_and(|value| !value.is_finite() || !(-273.15..=1000.0).contains(&value))
                {
                    return Err(AppError::BadRequest(format!("invalid {field}")));
                }
            }
        }
        for gpu in &self.system.gpus {
            // GPU 的四个文本字段全部来自厂商 API 或 sysfs 字符串，长度不可预期。
            validate_optional_text("gpu.id", Some(&gpu.id), 255)?;
            validate_optional_text("gpu.vendor", Some(&gpu.vendor), 64)?;
            validate_optional_text("gpu.name", Some(&gpu.name), 255)?;
            validate_optional_text("gpu.source", Some(&gpu.source), 64)?;
            if let Some(value) = gpu.utilization_percent {
                validate_percent("gpu.utilization_percent", value)?;
            }
            if gpu
                .memory_used_bytes
                .zip(gpu.memory_total_bytes)
                .is_some_and(|(used, total)| used > total)
            {
                return Err(AppError::BadRequest(
                    "GPU memory usage exceeds total memory".to_string(),
                ));
            }
            if gpu
                .temperature_celsius
                .is_some_and(|value| !value.is_finite() || !(-273.15..=1000.0).contains(&value))
            {
                return Err(AppError::BadRequest(
                    "invalid gpu.temperature_celsius".to_string(),
                ));
            }
            for (field, value) in [
                ("gpu.power_watts", gpu.power_watts),
                ("gpu.core_clock_mhz", gpu.core_clock_mhz),
                ("gpu.memory_clock_mhz", gpu.memory_clock_mhz),
                ("gpu.pcie_rx_bytes_per_second", gpu.pcie_rx_bytes_per_second),
                ("gpu.pcie_tx_bytes_per_second", gpu.pcie_tx_bytes_per_second),
            ] {
                if let Some(value) = value {
                    validate_nonnegative_rate(field, value)?;
                }
            }
        }
        Ok(())
    }

    fn metric_summary(&self) -> MetricSummary {
        let memory_usage_percent = (self.system.memory.total_bytes > 0).then(|| {
            self.system.memory.used_bytes as f64 * 100.0 / self.system.memory.total_bytes as f64
        });
        let max_sensor_temperature = self
            .system
            .temperatures
            .iter()
            .filter_map(|sensor| sensor.celsius)
            .chain(
                self.system
                    .gpus
                    .iter()
                    .filter_map(|gpu| gpu.temperature_celsius),
            )
            .reduce(f64::max);
        let gpu_memory = self
            .system
            .gpus
            .iter()
            .filter_map(|gpu| gpu.memory_used_bytes.zip(gpu.memory_total_bytes))
            .fold((0_u64, 0_u64), |sum, (used, total)| {
                (sum.0.saturating_add(used), sum.1.saturating_add(total))
            });
        MetricSummary {
            cpu_usage_percent: Some(self.system.cpu.usage_percent),
            memory_usage_percent,
            network_received_bytes_per_second: self
                .system
                .networks
                .iter()
                .map(|item| item.received_bytes_per_second)
                .reduce(f64::max),
            network_transmitted_bytes_per_second: self
                .system
                .networks
                .iter()
                .map(|item| item.transmitted_bytes_per_second)
                .reduce(f64::max),
            disk_read_bytes_per_second: self
                .system
                .disks
                .iter()
                .map(|item| item.read_bytes_per_second)
                .reduce(f64::max),
            disk_written_bytes_per_second: self
                .system
                .disks
                .iter()
                .map(|item| item.written_bytes_per_second)
                .reduce(f64::max),
            max_temperature_celsius: max_sensor_temperature,
            gpu_utilization_percent: self
                .system
                .gpus
                .iter()
                .filter_map(|gpu| gpu.utilization_percent)
                .reduce(f64::max),
            gpu_memory_usage_percent: (gpu_memory.1 > 0)
                .then(|| gpu_memory.0 as f64 * 100.0 / gpu_memory.1 as f64),
        }
    }
}

/// 身份类文本字段：必须非空、限长、无控制字符。
fn validate_text(field: &str, value: &str, max: usize) -> AppResult<()> {
    if value.trim().is_empty() {
        return Err(AppError::BadRequest(format!("invalid {field}")));
    }
    validate_bounded_text(field, value, max)
}

/// 描述性文本字段：**允许为空或缺失**，但同样限长、无控制字符。
///
/// 存在这个变体是因为采集侧确实会产出空串——Windows 无卷标磁盘的 `name`、伪文件
/// 系统的 `file_system`、无标签传感器的 `label`、拿不到版本号时的 `os_version`。
/// 对这些字段套用"必须非空"会让一份完全正常的报文被整体拒绝，即用可用性换一个并不存在
/// 的安全收益。
/// 真正需要守住的是**上界**，两个变体在这一点上完全一致。
fn validate_optional_text(field: &str, value: Option<&str>, max: usize) -> AppResult<()> {
    match value {
        Some(value) => validate_bounded_text(field, value, max),
        None => Ok(()),
    }
}

fn validate_bounded_text(field: &str, value: &str, max: usize) -> AppResult<()> {
    if value.len() > max {
        return Err(AppError::BadRequest(format!(
            "{field} exceeds {max} bytes (got {})",
            value.len()
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(AppError::BadRequest(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(())
}

fn validate_percent(field: &str, value: f64) -> AppResult<()> {
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        return Err(AppError::BadRequest(format!("invalid {field}")));
    }
    Ok(())
}

fn validate_nonnegative_rate(field: &str, value: f64) -> AppResult<()> {
    if !value.is_finite() || value < 0.0 {
        return Err(AppError::BadRequest(format!("invalid {field}")));
    }
    Ok(())
}

fn validate_sha256_hex(field: &str, value: &str) -> AppResult<()> {
    if value.len() != 64
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(AppError::BadRequest(format!(
            "{field} must be a lowercase SHA-256 hex digest"
        )));
    }
    Ok(())
}

fn validate_canonical_uuid(field: &str, value: &str) -> AppResult<()> {
    let parsed = uuid::Uuid::parse_str(value).map_err(|_| {
        AppError::BadRequest(format!(
            "{field} must be a canonical lowercase, hyphenated UUID"
        ))
    })?;
    if parsed.to_string() != value {
        return Err(AppError::BadRequest(format!(
            "{field} must be a canonical lowercase, hyphenated UUID"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_with_disk_name(name: &str) -> AgentReport {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "report_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "collected_at": "2026-01-01T00:00:00Z",
            "host": {
                "id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "name": "host", "os": "windows", "os_version": null,
                "kernel_version": null, "arch": "x86_64", "agent_version": "0.3.2"
            },
            "interval_seconds": 10.0,
            "system": {
                "uptime_seconds": 1,
                "cpu": {"usage_percent": 5.0, "logical_count": 1, "physical_count": null, "per_core_percent": [5.0]},
                "memory": {"total_bytes": 100, "used_bytes": 50, "available_bytes": 50, "swap_total_bytes": 0, "swap_used_bytes": 0},
                "networks": [],
                "disks": [
                    {"name":name,"mount_point":"F:\\\\","file_system":"NTFS","total_bytes":1,"available_bytes":1,"read_bytes_total":1,"written_bytes_total":1,"read_bytes_per_second":0.0,"written_bytes_per_second":0.0,"is_read_only":false}
                ],
                "temperatures": [], "gpus": []
            },
            "capabilities": [],
            "agent": {"spool_pending_batches": 0, "collector_errors": 0}
        }))
        .expect("valid report fixture")
    }

    #[test]
    fn windows_volume_without_a_label_is_a_valid_disk() {
        report_with_disk_name("").validate().unwrap();

        let error = report_with_disk_name("bad\nname").validate().unwrap_err();
        assert!(error.to_string().contains("disk.name"));
        assert!(error.to_string().contains("control characters"));
    }

    #[test]
    fn summary_uses_largest_device_rate_instead_of_double_counting() {
        let report: AgentReport = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "report_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "collected_at": "2026-01-01T00:00:00Z",
            "host": {
                "id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "name": "host", "os": "linux", "os_version": null,
                "kernel_version": null, "arch": "x86_64", "agent_version": "0.3.2"
            },
            "interval_seconds": 10.0,
            "system": {
                "uptime_seconds": 1,
                "cpu": {"usage_percent": 5.0, "logical_count": 1, "physical_count": null, "per_core_percent": [5.0]},
                "memory": {"total_bytes": 100, "used_bytes": 50, "available_bytes": 50, "swap_total_bytes": 0, "swap_used_bytes": 0},
                "networks": [
                    {"name":"eth0","received_bytes_total":1,"transmitted_bytes_total":1,"received_bytes_per_second":100.0,"transmitted_bytes_per_second":40.0,"packets_received_total":1,"packets_transmitted_total":1,"receive_errors_total":0,"transmit_errors_total":0},
                    {"name":"bridge0","received_bytes_total":1,"transmitted_bytes_total":1,"received_bytes_per_second":80.0,"transmitted_bytes_per_second":70.0,"packets_received_total":1,"packets_transmitted_total":1,"receive_errors_total":0,"transmit_errors_total":0}
                ],
                "disks": [
                    {"name":"sda","mount_point":"/","file_system":"ext4","total_bytes":1,"available_bytes":1,"read_bytes_total":1,"written_bytes_total":1,"read_bytes_per_second":30.0,"written_bytes_per_second":60.0,"is_read_only":false},
                    {"name":"bind","mount_point":"/bind","file_system":"ext4","total_bytes":1,"available_bytes":1,"read_bytes_total":1,"written_bytes_total":1,"read_bytes_per_second":20.0,"written_bytes_per_second":50.0,"is_read_only":false}
                ],
                "temperatures": [], "gpus": []
            },
            "capabilities": [],
            "agent": {"spool_pending_batches": 0, "collector_errors": 0}
        }))
        .expect("valid report");

        let summary = report.metric_summary();
        assert_eq!(summary.network_received_bytes_per_second, Some(100.0));
        assert_eq!(summary.network_transmitted_bytes_per_second, Some(70.0));
        assert_eq!(summary.disk_read_bytes_per_second, Some(30.0));
        assert_eq!(summary.disk_written_bytes_per_second, Some(60.0));
    }

    #[test]
    fn management_requests_reject_removed_fields() {
        assert!(
            serde_json::from_value::<CreateAgentInstanceRequest>(serde_json::json!({
                "display_name": "agent",
                "enrollment_code": "removed"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<AgentPairingRequest>(serde_json::json!({
                "host": {
                    "id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                    "name": "host",
                    "os": "linux",
                    "os_version": null,
                    "kernel_version": null,
                    "arch": "x86_64",
                    "agent_version": "0.3.2"
                },
                "token_hash": "a".repeat(64),
                "polling_secret_hash": "b".repeat(64),
                "legacy_token": "removed"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivateAgentRequest>(serde_json::json!({
                "request_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "activation_code": "uci_current",
                "host_id": "removed"
            }))
            .is_err()
        );
    }

    #[test]
    fn management_instance_id_requires_canonical_uuid() {
        for instance_id in [
            "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
            "aaaaaaaaaaaa4aaa8aaaaaaaaaaaaaaa",
        ] {
            let request = CreateAgentInstanceRequest {
                display_name: None,
                expires_in_minutes: None,
                instance_id: Some(instance_id.to_string()),
            };
            assert!(request.validated().is_err(), "accepted {instance_id}");
        }
    }
}
