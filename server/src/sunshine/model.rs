use serde::{Deserialize, Serialize};

/// 当前主机配置的后台连接探测状态。
///
/// `pending` 与“已确认不可达”必须是两个不同状态：新增/修改配置后如果暂时用
/// `reachable: false` 占位，控制台会向用户错误报告故障。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SunshineProbeStatus {
    Pending,
    Complete,
}

#[derive(Debug, Serialize)]
pub struct SunshineStatus {
    pub host: String,
    pub web_port: u16,
    pub web_url: String,
    pub reachable: bool,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct SunshineHostInfo {
    pub id: String,
    pub name: String,
    pub host: String,
    pub web_port: u16,
    pub username: String,
    pub password_set: bool,
    pub verify_tls: bool,
    pub web_url: String,
    pub probe_status: SunshineProbeStatus,
    /// `None` 表示当前配置尚在探测，不能解释为不可达。
    pub reachable: Option<bool>,
    /// `None` 表示当前配置尚在探测；探测完成后一定为 `Some`。
    pub connected: Option<bool>,
    pub connection_error: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SunshineHostSaveRequest {
    pub name: String,
    pub host: String,
    pub web_port: u16,
    pub username: String,
    /// `None` 表示保留旧密码，空字符串表示清空密码。
    pub password: Option<String>,
    pub verify_tls: bool,
}

/// Partial host update. Omitted fields retain their current value; an explicit
/// empty password clears it. This prevents independent editor panels from
/// overwriting unrelated fields with a stale full-object snapshot.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SunshineHostPatchRequest {
    pub name: Option<String>,
    pub host: Option<String>,
    pub web_port: Option<u16>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub verify_tls: Option<bool>,
}

impl SunshineHostPatchRequest {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.host.is_none()
            && self.web_port.is_none()
            && self.username.is_none()
            && self.password.is_none()
            && self.verify_tls.is_none()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SunshineUnpairRequest {
    pub uuid: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SunshineClientUpdateRequest {
    pub uuid: String,
    pub enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SunshinePinRequest {
    pub pin: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SunshineCoverUploadRequest {
    pub key: String,
    pub url: String,
}
