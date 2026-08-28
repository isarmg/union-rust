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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
