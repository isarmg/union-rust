//! Telemetry report wire types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, de};

/// Current schema emitted by the Agent and accepted by the Server.
pub const AGENT_REPORT_SCHEMA_VERSION: u16 = 1;

/// Maximum compact JSON request body accepted by the report endpoint.
pub const AGENT_REPORT_MAX_BODY_BYTES: usize = 512 * 1024;
pub const AGENT_REPORT_MIN_INTERVAL_SECONDS: f64 = 0.1;
pub const AGENT_REPORT_MAX_INTERVAL_SECONDS: u64 = 3600;
/// Collection bounds are wire-contract limits, not merely Server implementation details.
pub const AGENT_REPORT_MAX_CAPABILITIES: usize = 256;
pub const AGENT_REPORT_MAX_CPU_CORES: usize = 4096;
pub const AGENT_REPORT_MAX_NETWORKS: usize = 1024;
pub const AGENT_REPORT_MAX_DISKS: usize = 1024;
pub const AGENT_REPORT_MAX_TEMPERATURES: usize = 4096;
pub const AGENT_REPORT_MAX_GPUS: usize = 128;

/// Text limits use UTF-8 bytes, matching JSON trust-boundary validation.
pub const AGENT_REPORT_MAX_HOST_OS_BYTES: usize = 64;
pub const AGENT_REPORT_MAX_HOST_VERSION_BYTES: usize = 128;
pub const AGENT_REPORT_MAX_HOST_ARCH_BYTES: usize = 64;
pub const AGENT_REPORT_MAX_AGENT_VERSION_BYTES: usize = 128;
pub const AGENT_REPORT_MAX_CAPABILITY_NAME_BYTES: usize = 128;
pub const AGENT_REPORT_MAX_CAPABILITY_SOURCE_BYTES: usize = 128;
pub const AGENT_REPORT_MAX_CAPABILITY_MESSAGE_BYTES: usize = 1024;
pub const AGENT_REPORT_MAX_NETWORK_NAME_BYTES: usize = 255;
pub const AGENT_REPORT_MAX_DISK_NAME_BYTES: usize = 1024;
pub const AGENT_REPORT_MAX_MOUNT_POINT_BYTES: usize = 4096;
pub const AGENT_REPORT_MAX_FILE_SYSTEM_BYTES: usize = 128;
pub const AGENT_REPORT_MAX_TEMPERATURE_ID_BYTES: usize = 255;
pub const AGENT_REPORT_MAX_TEMPERATURE_LABEL_BYTES: usize = 255;
pub const AGENT_REPORT_MAX_TEMPERATURE_SOURCE_BYTES: usize = 64;
pub const AGENT_REPORT_MAX_GPU_ID_BYTES: usize = 255;
pub const AGENT_REPORT_MAX_GPU_VENDOR_BYTES: usize = 64;
pub const AGENT_REPORT_MAX_GPU_NAME_BYTES: usize = 255;
pub const AGENT_REPORT_MAX_GPU_SOURCE_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentReport {
    pub schema_version: u16,
    /// Canonical lowercase, hyphenated UUID text.
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub report_id: String,
    pub collected_at: DateTime<Utc>,
    pub host: HostIdentity,
    pub interval_seconds: f64,
    pub system: SystemSnapshot,
    pub capabilities: Vec<Capability>,
    pub agent: AgentHealth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostIdentity {
    /// Canonical lowercase, hyphenated UUID text.
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub id: String,
    pub os: String,
    pub os_version: Option<String>,
    pub kernel_version: Option<String>,
    pub arch: String,
    pub agent_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemSnapshot {
    #[serde(with = "crate::json_u64")]
    pub uptime_seconds: u64,
    pub cpu: CpuSnapshot,
    pub memory: MemorySnapshot,
    pub networks: Vec<NetworkSnapshot>,
    pub disks: Vec<DiskSnapshot>,
    pub temperatures: Vec<TemperatureSnapshot>,
    pub gpus: Vec<GpuSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuSnapshot {
    pub usage_percent: f64,
    /// Fixed-width on the wire: `usize` would make the protocol depend on Agent architecture.
    pub logical_count: u32,
    pub physical_count: Option<u32>,
    pub per_core_percent: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySnapshot {
    #[serde(with = "crate::json_u64")]
    pub total_bytes: u64,
    #[serde(with = "crate::json_u64")]
    pub used_bytes: u64,
    #[serde(with = "crate::json_u64")]
    pub available_bytes: u64,
    #[serde(with = "crate::json_u64")]
    pub swap_total_bytes: u64,
    #[serde(with = "crate::json_u64")]
    pub swap_used_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSnapshot {
    pub name: String,
    #[serde(with = "crate::json_u64")]
    pub received_bytes_total: u64,
    #[serde(with = "crate::json_u64")]
    pub transmitted_bytes_total: u64,
    /// Rates are floating point so sub-unit sampling intervals do not silently truncate them.
    pub received_bytes_per_second: f64,
    pub transmitted_bytes_per_second: f64,
    #[serde(with = "crate::json_u64")]
    pub packets_received_total: u64,
    #[serde(with = "crate::json_u64")]
    pub packets_transmitted_total: u64,
    #[serde(with = "crate::json_u64")]
    pub receive_errors_total: u64,
    #[serde(with = "crate::json_u64")]
    pub transmit_errors_total: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskSnapshot {
    pub name: String,
    pub mount_point: String,
    pub file_system: String,
    #[serde(with = "crate::json_u64")]
    pub total_bytes: u64,
    #[serde(with = "crate::json_u64")]
    pub available_bytes: u64,
    #[serde(with = "crate::json_u64")]
    pub read_bytes_total: u64,
    #[serde(with = "crate::json_u64")]
    pub written_bytes_total: u64,
    pub read_bytes_per_second: f64,
    pub written_bytes_per_second: f64,
    pub is_read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemperatureSnapshot {
    pub id: String,
    pub label: String,
    pub celsius: Option<f64>,
    pub max_celsius: Option<f64>,
    pub critical_celsius: Option<f64>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpuSnapshot {
    pub id: String,
    pub vendor: String,
    pub name: String,
    pub utilization_percent: Option<f64>,
    #[serde(default, with = "crate::json_u64::option")]
    pub memory_total_bytes: Option<u64>,
    #[serde(default, with = "crate::json_u64::option")]
    pub memory_used_bytes: Option<u64>,
    pub temperature_celsius: Option<f64>,
    pub power_watts: Option<f64>,
    pub core_clock_mhz: Option<f64>,
    pub memory_clock_mhz: Option<f64>,
    pub pcie_rx_bytes_per_second: Option<f64>,
    pub pcie_tx_bytes_per_second: Option<f64>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub name: String,
    pub available: bool,
    pub source: String,
    pub error_kind: Option<CapabilityErrorKind>,
    pub message: Option<String>,
}

/// Capability failures supported by the current Agent and Server release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityErrorKind {
    Unsupported,
    NotPresent,
    DriverMissing,
    PermissionDenied,
    Transient,
    InvalidData,
}

impl CapabilityErrorKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Unsupported => "unsupported",
            Self::NotPresent => "not_present",
            Self::DriverMissing => "driver_missing",
            Self::PermissionDenied => "permission_denied",
            Self::Transient => "transient",
            Self::InvalidData => "invalid_data",
        }
    }
}

impl Capability {
    pub fn available(name: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            available: true,
            source: source.into(),
            error_kind: None,
            message: None,
        }
    }

    pub fn unavailable(
        name: impl Into<String>,
        source: impl Into<String>,
        error_kind: CapabilityErrorKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            available: false,
            source: source.into(),
            error_kind: Some(error_kind),
            message: Some(message.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentHealth {
    #[serde(with = "crate::json_u64")]
    pub spool_pending_batches: u64,
    #[serde(with = "crate::json_u64")]
    pub collector_errors: u64,
}

pub(crate) fn deserialize_canonical_uuid<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    let parsed = uuid::Uuid::parse_str(&value)
        .map_err(|_| de::Error::custom("expected a canonical lowercase, hyphenated UUID"))?;
    if parsed.to_string() != value {
        return Err(de::Error::custom(
            "expected a canonical lowercase, hyphenated UUID",
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> AgentReport {
        AgentReport {
            schema_version: AGENT_REPORT_SCHEMA_VERSION,
            report_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            collected_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            host: HostIdentity {
                id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
                os: "linux".into(),
                os_version: Some("1".into()),
                kernel_version: None,
                arch: "x86_64".into(),
                agent_version: "0.3.6".into(),
            },
            interval_seconds: 0.5,
            system: SystemSnapshot {
                uptime_seconds: 1,
                cpu: CpuSnapshot {
                    usage_percent: 12.5,
                    logical_count: 4,
                    physical_count: Some(2),
                    per_core_percent: vec![10.0, 15.0],
                },
                memory: MemorySnapshot {
                    total_bytes: 10,
                    used_bytes: 4,
                    available_bytes: 6,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                },
                networks: vec![NetworkSnapshot {
                    name: "eth0".into(),
                    received_bytes_total: 10,
                    transmitted_bytes_total: 20,
                    received_bytes_per_second: 2.5,
                    transmitted_bytes_per_second: 5.0,
                    packets_received_total: 1,
                    packets_transmitted_total: 2,
                    receive_errors_total: 0,
                    transmit_errors_total: 0,
                }],
                disks: vec![],
                temperatures: vec![],
                gpus: vec![],
            },
            capabilities: vec![Capability::unavailable(
                "gpu.test",
                "fixture",
                CapabilityErrorKind::NotPresent,
                "not installed",
            )],
            agent: AgentHealth {
                spool_pending_batches: 1,
                collector_errors: 0,
            },
        }
    }

    #[test]
    fn report_json_round_trips_without_loss() {
        let report = fixture();
        let json = serde_json::to_vec(&report).unwrap();
        assert_eq!(
            serde_json::from_slice::<AgentReport>(&json).unwrap(),
            report
        );
    }

    #[test]
    fn u64_fields_switch_to_decimal_strings_past_the_javascript_boundary() {
        let mut report = fixture();
        report.system.uptime_seconds = crate::json_u64::MAX_SAFE_INTEGER;
        report.system.networks[0].received_bytes_total = crate::json_u64::MAX_SAFE_INTEGER + 1;
        report.system.networks[0].packets_received_total = u64::MAX;
        report.system.gpus.push(GpuSnapshot {
            id: "gpu0".into(),
            vendor: "test".into(),
            name: "test".into(),
            utilization_percent: None,
            memory_total_bytes: Some(u64::MAX),
            memory_used_bytes: None,
            temperature_celsius: None,
            power_watts: None,
            core_clock_mhz: None,
            memory_clock_mhz: None,
            pcie_rx_bytes_per_second: None,
            pcie_tx_bytes_per_second: None,
            source: "test".into(),
        });

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(
            json["system"]["uptime_seconds"],
            serde_json::json!(9_007_199_254_740_991_u64)
        );
        assert_eq!(
            json["system"]["networks"][0]["received_bytes_total"],
            "9007199254740992"
        );
        assert_eq!(
            json["system"]["networks"][0]["packets_received_total"],
            "18446744073709551615"
        );
        assert_eq!(
            json["system"]["gpus"][0]["memory_total_bytes"],
            "18446744073709551615"
        );
        assert!(json["system"]["gpus"][0]["memory_used_bytes"].is_null());
        assert_eq!(serde_json::from_value::<AgentReport>(json).unwrap(), report);
    }

    #[test]
    fn legacy_large_json_integers_remain_accepted_without_loss() {
        let mut json = serde_json::to_value(fixture()).unwrap();
        json["system"]["networks"][0]["received_bytes_total"] = serde_json::json!(u64::MAX);
        let report = serde_json::from_value::<AgentReport>(json).unwrap();
        assert_eq!(report.system.networks[0].received_bytes_total, u64::MAX);
    }

    #[test]
    fn malformed_decimal_u64_strings_are_rejected() {
        for invalid in ["", "01", "+1", "-1", "1.0", "18446744073709551616"] {
            let mut json = serde_json::to_value(fixture()).unwrap();
            json["system"]["networks"][0]["received_bytes_total"] = serde_json::json!(invalid);
            assert!(
                serde_json::from_value::<AgentReport>(json).is_err(),
                "accepted malformed u64 string {invalid:?}"
            );
        }
    }

    #[test]
    fn unsupported_capability_error_kind_is_rejected() {
        let json = r#"{"name":"gpu.future","available":false,"source":"driver","error_kind":"firmware_mismatch","message":"unavailable"}"#;
        assert!(serde_json::from_str::<Capability>(json).is_err());
    }

    #[test]
    fn unknown_report_fields_are_rejected_at_every_level() {
        let mut values = Vec::new();
        let mut top_level = serde_json::to_value(fixture()).unwrap();
        top_level["removed_top_level"] = serde_json::json!(true);
        values.push(top_level);
        let mut host = serde_json::to_value(fixture()).unwrap();
        host["host"]["legacy_id"] = serde_json::json!(true);
        values.push(host);
        let mut cpu = serde_json::to_value(fixture()).unwrap();
        cpu["system"]["cpu"]["old_usage"] = serde_json::json!(true);
        values.push(cpu);
        for value in values {
            assert!(serde_json::from_value::<AgentReport>(value).is_err());
        }
    }

    #[test]
    fn noncanonical_report_uuids_are_rejected() {
        for (field, value) in [
            ("report_id", "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA"),
            ("report_id", "aaaaaaaaaaaa4aaa8aaaaaaaaaaaaaaa"),
            ("host.id", "BBBBBBBB-BBBB-4BBB-8BBB-BBBBBBBBBBBB"),
            ("host.id", "bbbbbbbbbbbb4bbb8bbbbbbbbbbbbbbb"),
        ] {
            let mut report = serde_json::to_value(fixture()).unwrap();
            match field {
                "report_id" => report["report_id"] = serde_json::json!(value),
                "host.id" => report["host"]["id"] = serde_json::json!(value),
                _ => unreachable!(),
            }
            assert!(
                serde_json::from_value::<AgentReport>(report).is_err(),
                "{field} accepted noncanonical UUID {value}"
            );
        }
    }
}
