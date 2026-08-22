use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_seconds: i64,
}

#[derive(Debug, Serialize)]
pub struct ReadinessResponse {
    pub status: String,
    pub database: bool,
    pub data_directory: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatus {
    pub name: String,
    pub kind: String,
    pub runtime_state: String,
    pub healthy: bool,
    pub address: Option<String>,
    pub pid: Option<u32>,
    pub message: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SystemResources {
    pub cpu_usage_percent: f32,
    pub memory_total_kib: u64,
    pub memory_used_kib: u64,
    pub network: NetworkThroughput,
    pub disk_throughput: DiskThroughput,
    pub disks: Vec<DiskInfo>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NetworkThroughput {
    pub received_bytes_per_second: u64,
    pub transmitted_bytes_per_second: u64,
    pub total_bytes_per_second: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DiskThroughput {
    pub read_bytes_per_second: u64,
    pub write_bytes_per_second: u64,
    pub total_bytes_per_second: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct EventPayload {
    pub kind: String,
    pub generated_at: String,
    pub services: Vec<ServiceStatus>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    /// 稳定的机器可读错误码；客户端不应解析自然语言 message。
    pub code: String,
    pub message: String,
}
