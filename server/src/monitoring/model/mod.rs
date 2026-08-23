use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
pub use unionc_protocol::{
    AGENT_REPORT_MAX_AGENT_VERSION_BYTES, AGENT_REPORT_MAX_BODY_BYTES,
    AGENT_REPORT_MAX_CAPABILITIES, AGENT_REPORT_MAX_CAPABILITY_MESSAGE_BYTES,
    AGENT_REPORT_MAX_CAPABILITY_NAME_BYTES, AGENT_REPORT_MAX_CAPABILITY_SOURCE_BYTES,
    AGENT_REPORT_MAX_CPU_CORES, AGENT_REPORT_MAX_DISK_NAME_BYTES, AGENT_REPORT_MAX_DISKS,
    AGENT_REPORT_MAX_FILE_SYSTEM_BYTES, AGENT_REPORT_MAX_GPU_ID_BYTES,
    AGENT_REPORT_MAX_GPU_NAME_BYTES, AGENT_REPORT_MAX_GPU_SOURCE_BYTES,
    AGENT_REPORT_MAX_GPU_VENDOR_BYTES, AGENT_REPORT_MAX_GPUS, AGENT_REPORT_MAX_HOST_ARCH_BYTES,
    AGENT_REPORT_MAX_HOST_OS_BYTES, AGENT_REPORT_MAX_HOST_VERSION_BYTES,
    AGENT_REPORT_MAX_INTERVAL_SECONDS, AGENT_REPORT_MAX_MOUNT_POINT_BYTES,
    AGENT_REPORT_MAX_NETWORK_NAME_BYTES, AGENT_REPORT_MAX_NETWORKS,
    AGENT_REPORT_MAX_TEMPERATURE_ID_BYTES, AGENT_REPORT_MAX_TEMPERATURE_LABEL_BYTES,
    AGENT_REPORT_MAX_TEMPERATURE_SOURCE_BYTES, AGENT_REPORT_MAX_TEMPERATURES,
    AGENT_REPORT_MIN_INTERVAL_SECONDS, AGENT_REPORT_SCHEMA_VERSION, ActivateAgentRequest,
    ActivateAgentResponse, ActivatePairingStatus, AgentHealth, AgentPairingRequest,
    AgentPairingResponse, AgentPairingStatusResponse, AgentReport, AgentReportAck, Capability,
    CapabilityErrorKind, CpuSnapshot, DiskSnapshot, GpuSnapshot, HostIdentity, MemorySnapshot,
    NetworkSnapshot, PairingStatus, SystemSnapshot, TemperatureSnapshot,
};

use crate::error::{AppError, AppResult};

mod validation;

use validation::*;

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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateMonitoringRemarkRequest {
    pub remark: String,
}

impl UpdateMonitoringRemarkRequest {
    pub fn validated_remark(&self) -> AppResult<String> {
        let remark = self.remark.trim().to_string();
        validate_text("monitoring instance remark", &remark, 255)?;
        Ok(remark)
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

/// Server-side policy validation for the shared pairing request DTO.
pub trait AgentPairingRequestExt {
    fn validate(&self) -> AppResult<()>;
}

impl AgentPairingRequestExt for AgentPairingRequest {
    fn validate(&self) -> AppResult<()> {
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
pub struct AgentPairingPublicSummary {
    pub request_id: String,
    pub os: String,
    pub arch: String,
    pub agent_version: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostSummary {
    pub id: String,
    /// Server-owned operator remark; it is not part of `HostIdentity`.
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
        validate_text("host.os", &self.os, AGENT_REPORT_MAX_HOST_OS_BYTES)?;
        validate_text("host.arch", &self.arch, AGENT_REPORT_MAX_HOST_ARCH_BYTES)?;
        validate_text(
            "host.agent_version",
            &self.agent_version,
            AGENT_REPORT_MAX_AGENT_VERSION_BYTES,
        )?;
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
        validate_optional_text(
            "host.os_version",
            self.os_version.as_deref(),
            AGENT_REPORT_MAX_HOST_VERSION_BYTES,
        )?;
        validate_optional_text(
            "host.kernel_version",
            self.kernel_version.as_deref(),
            AGENT_REPORT_MAX_HOST_VERSION_BYTES,
        )
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
        // Agent 与 Server 从 protocol crate 读取同一份区间；修改共享常量时仍须同步
        // schema/sqlite.sql 的粗粒度 CHECK 约束。
        if !self.interval_seconds.is_finite()
            || !(AGENT_REPORT_MIN_INTERVAL_SECONDS..=AGENT_REPORT_MAX_INTERVAL_SECONDS as f64)
                .contains(&self.interval_seconds)
        {
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
        if self.capabilities.len() > AGENT_REPORT_MAX_CAPABILITIES
            || self.system.cpu.per_core_percent.len() > AGENT_REPORT_MAX_CPU_CORES
            || self.system.networks.len() > AGENT_REPORT_MAX_NETWORKS
            || self.system.disks.len() > AGENT_REPORT_MAX_DISKS
            || self.system.temperatures.len() > AGENT_REPORT_MAX_TEMPERATURES
            || self.system.gpus.len() > AGENT_REPORT_MAX_GPUS
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
            validate_text(
                "capability.name",
                &capability.name,
                AGENT_REPORT_MAX_CAPABILITY_NAME_BYTES,
            )?;
            validate_text(
                "capability.source",
                &capability.source,
                AGENT_REPORT_MAX_CAPABILITY_SOURCE_BYTES,
            )?;
            if let Some(error_kind) = &capability.error_kind {
                validate_text(
                    "capability.error_kind",
                    error_kind.as_str(),
                    AGENT_REPORT_MAX_CAPABILITY_NAME_BYTES,
                )?;
            }
            // message 是人类可读的诊断信息，放宽到 1 KiB，但仍必须有界。
            if let Some(message) = &capability.message {
                validate_text(
                    "capability.message",
                    message,
                    AGENT_REPORT_MAX_CAPABILITY_MESSAGE_BYTES,
                )?;
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
            validate_text(
                "network.name",
                &network.name,
                AGENT_REPORT_MAX_NETWORK_NAME_BYTES,
            )?;
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
            validate_optional_text(
                "disk.name",
                Some(&disk.name),
                AGENT_REPORT_MAX_DISK_NAME_BYTES,
            )?;
            validate_text(
                "disk.mount_point",
                &disk.mount_point,
                AGENT_REPORT_MAX_MOUNT_POINT_BYTES,
            )?;
            // 伪文件系统与未识别设备的 file_system 可能是空串，故用宽松校验。
            validate_optional_text(
                "disk.file_system",
                Some(&disk.file_system),
                AGENT_REPORT_MAX_FILE_SYSTEM_BYTES,
            )?;
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
            validate_optional_text(
                "temperature.id",
                Some(&sensor.id),
                AGENT_REPORT_MAX_TEMPERATURE_ID_BYTES,
            )?;
            validate_optional_text(
                "temperature.label",
                Some(&sensor.label),
                AGENT_REPORT_MAX_TEMPERATURE_LABEL_BYTES,
            )?;
            validate_optional_text(
                "temperature.source",
                Some(&sensor.source),
                AGENT_REPORT_MAX_TEMPERATURE_SOURCE_BYTES,
            )?;
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
            validate_optional_text("gpu.id", Some(&gpu.id), AGENT_REPORT_MAX_GPU_ID_BYTES)?;
            validate_optional_text(
                "gpu.vendor",
                Some(&gpu.vendor),
                AGENT_REPORT_MAX_GPU_VENDOR_BYTES,
            )?;
            validate_optional_text("gpu.name", Some(&gpu.name), AGENT_REPORT_MAX_GPU_NAME_BYTES)?;
            validate_optional_text(
                "gpu.source",
                Some(&gpu.source),
                AGENT_REPORT_MAX_GPU_SOURCE_BYTES,
            )?;
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

// Keep trust-boundary regression tests beside the private extension traits
// without widening the production model API.
include!("tests.rs");
