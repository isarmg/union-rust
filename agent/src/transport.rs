use std::{fs, path::Path};

#[cfg(feature = "otlp")]
use std::io::Write;

use anyhow::{Context, bail};
#[cfg(feature = "otlp")]
use flate2::{Compression, write::GzEncoder};
use reqwest::{Certificate, Client, Identity, StatusCode};

use unionc_protocol::AgentReportAck;

use crate::{
    config::AgentConfig,
    model::AgentReport,
    private_fs::{self, OwnerPolicy},
};

const MAX_ERROR_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct Reporter {
    client: Client,
    endpoint: String,
    token: String,
    // 仅 otlp feature 下读取；无该 feature 时保留字段以维持构造逻辑一致。
    #[cfg_attr(not(feature = "otlp"), allow(dead_code))]
    otlp_endpoint: Option<String>,
    #[cfg_attr(not(feature = "otlp"), allow(dead_code))]
    otlp_token: Option<String>,
}

impl Reporter {
    pub fn new(config: &AgentConfig) -> anyhow::Result<Self> {
        crate::pairing::reporter_for_current_active_state(config)?.context(
            "a complete current-version Active pairing state is required before creating the reporter",
        )
    }

    /// Build a reporter only from an already-issued long-lived credential.
    /// This never performs pairing or network I/O and is used while the
    /// pairing state lock protects the token/config snapshot from an
    /// overlapping browser-pairing commit.
    pub(crate) fn for_existing_credential(config: &AgentConfig) -> anyhow::Result<Option<Self>> {
        let token_path = config.state_dir.join("agent-token");
        if !token_path.is_file() {
            return Ok(None);
        }
        let token = read_secret(&token_path, "host token").with_context(|| {
            format!(
                "the stored host credential is unreadable or invalid; run `unionc-agent pair \
                 --server <url>` to authorize this host again ({})",
                token_path.display()
            )
        })?;
        Self::with_token(config, token).map(Some)
    }

    fn with_token(config: &AgentConfig, token: String) -> anyhow::Result<Self> {
        let client = build_client(config)?;
        Self::with_client_and_token(config, client, token)
    }

    fn with_client_and_token(
        config: &AgentConfig,
        client: Client,
        token: String,
    ) -> anyhow::Result<Self> {
        if token.trim().is_empty() {
            bail!("the per-host token is empty");
        }
        Ok(Self {
            client,
            endpoint: config.endpoint.clone(),
            token,
            otlp_endpoint: config.otlp_endpoint.clone(),
            otlp_token: config.otlp_token.clone(),
        })
    }

    pub async fn send_unionc(&self, report: &AgentReport) -> Result<(), SendError> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(report)
            .send()
            .await
            .map_err(|error| SendError::Transient(format!("UnionC request failed: {error}")))?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = read_limited(response, MAX_ERROR_RESPONSE_BYTES, "UnionC")
            .await
            .map_err(SendError::Transient)?;
        validate_unionc_ack(status, content_type.as_deref(), &body, report)
    }

    #[cfg(feature = "otlp")]
    pub async fn send_otlp(&self, report: &AgentReport) -> anyhow::Result<()> {
        use prost::Message;

        let Some(endpoint) = &self.otlp_endpoint else {
            return Ok(());
        };
        let request = crate::otlp::encode_report(report);
        let mut protobuf = Vec::with_capacity(request.encoded_len());
        request.encode(&mut protobuf)?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&protobuf)?;
        let body = encoder.finish()?;
        let mut request = self
            .client
            .post(endpoint)
            .header("content-type", "application/x-protobuf")
            .header("content-encoding", "gzip")
            .body(body);
        if let Some(token) = &self.otlp_token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await?;
        let status = response.status();
        let body = read_limited(response, MAX_ERROR_RESPONSE_BYTES, "OTLP")
            .await
            .map_err(anyhow::Error::msg)?;
        // OTLP 是可选的次要输出，调用方只做告警，不区分永久/暂时失败。
        Ok(ensure_success(
            status,
            String::from_utf8_lossy(&body).into_owned(),
            "OTLP",
        )?)
    }

    #[cfg(not(feature = "otlp"))]
    pub async fn send_otlp(&self, _report: &AgentReport) -> anyhow::Result<()> {
        Ok(())
    }
}

/// 有界读取远端响应。超限时立即停止，不先把整个响应收进内存。
async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
    target: &str,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!(
            "{target} response exceeds the {} KiB limit",
            limit / 1024
        ));
    }
    let mut body = Vec::new();
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|error| format!("failed to read {target} response: {error}"))?;
        let Some(chunk) = chunk else { break };
        if body.len() + chunk.len() > limit {
            return Err(format!(
                "{target} response exceeds the {} KiB limit",
                limit / 1024
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) fn build_client(config: &AgentConfig) -> anyhow::Result<Client> {
    build_client_with_redirects(config, true)
}

/// Build the client used for the one-time authorization-key exchange. A
/// redirect could replay the JSON body (and its key) to another origin, so the
/// activation path always disables redirects while retaining the Agent's
/// configured CA, client identity, timeout, and TLS backend.
pub(crate) fn build_activation_client(config: &AgentConfig) -> anyhow::Result<Client> {
    build_client_with_redirects(config, false)
}

fn build_client_with_redirects(
    config: &AgentConfig,
    follow_redirects: bool,
) -> anyhow::Result<Client> {
    let mut builder = Client::builder()
        .timeout(config.request_timeout())
        .user_agent(format!("unionc-agent/{}", env!("CARGO_PKG_VERSION")));
    if !follow_redirects {
        builder = builder.redirect(reqwest::redirect::Policy::none());
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    if let Some(path) = &config.tls_identity_pem {
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read TLS identity {}", path.display()))?;
        builder = builder.identity(Identity::from_pem(&bytes)?);
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        if config.tls_identity_pem.is_some() {
            bail!(
                "the native TLS backend requires tls_identity_pkcs12 instead of tls_identity_pem"
            );
        }
        if let Some(path) = &config.tls_identity_pkcs12 {
            let bytes = fs::read(path)
                .with_context(|| format!("failed to read TLS identity {}", path.display()))?;
            builder = builder.identity(Identity::from_pkcs12_der(
                &bytes,
                config.tls_identity_password.as_deref().unwrap_or(""),
            )?);
        }
    }
    if let Some(path) = &config.tls_ca_pem {
        let bytes =
            fs::read(path).with_context(|| format!("failed to read TLS CA {}", path.display()))?;
        builder = builder.add_root_certificate(Certificate::from_pem(&bytes)?);
    }
    Ok(builder.build()?)
}

pub(crate) fn read_secret(path: &Path, kind: &str) -> anyhow::Result<String> {
    let token = fs::read_to_string(path)
        .with_context(|| format!("failed to read {kind} {}", path.display()))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("{kind} {} is empty", path.display());
    }
    Ok(token)
}

pub(crate) fn persist_private_value(path: &Path, token: &str, kind: &str) -> anyhow::Result<()> {
    if token.trim().is_empty() {
        bail!("refusing to persist an empty {kind}");
    }
    let parent = path
        .parent()
        .context("token path has no parent directory")?;
    private_fs::ensure_private_directory(parent)?;
    private_fs::write_atomic(path, token.trim().as_bytes(), OwnerPolicy::Parent(parent))
        .with_context(|| format!("failed to persist {kind} {}", path.display()))
}

/// 上报失败的性质。判据是**要让同一份报文最终被接受，需要改变什么**：
///
/// | 变体 | 需要改变的东西 | 处置 |
/// |---|---|---|
/// | `Permanent`  | 报文内容本身（改不了） | 丢弃 |
/// | `Unauthorized` | 凭据 | 需要浏览器重新授权 |
/// | `Transient`  | 只需等待 | 退避重试 |
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// 服务端拒绝了报文内容本身（400/409/413/422）。重试必然再次失败，
    /// 继续入队只会让 spool 被必失败的数据占满并挤掉后续有效报文。
    #[error("{0}")]
    Permanent(String),
    /// 凭据不被接受（401）。主机进入 `reauth_required`，只能通过
    /// 当前 v2 浏览器配对流程恢复；Agent 不会自动生成或替换凭据。
    #[error("{0}")]
    Unauthorized(String),
    /// The host credential was deliberately revoked or is bound to another
    /// instance (403). Replacing it automatically would defeat host
    /// decommissioning, so browser authorization is required.
    #[error("{0}")]
    Revoked(String),
    /// 网络故障或服务端暂时不可用。重试有意义。
    #[error("{0}")]
    Transient(String),
}

impl SendError {
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }

    /// 凭据已失效，需要浏览器重新授权后才可能成功。
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Unauthorized(_))
    }

    pub fn is_revoked(&self) -> bool {
        matches!(self, Self::Revoked(_))
    }
}

fn validate_unionc_ack(
    status: StatusCode,
    content_type: Option<&str>,
    body: &[u8],
    report: &AgentReport,
) -> Result<(), SendError> {
    if status != StatusCode::ACCEPTED {
        if status.is_success() {
            return Err(SendError::Transient(format!(
                "UnionC returned unexpected HTTP {status}; report acknowledgements require HTTP 202 Accepted"
            )));
        }
        return ensure_success(status, String::from_utf8_lossy(body).into_owned(), "UnionC");
    }
    if !content_type.is_some_and(is_application_json) {
        return Err(SendError::Transient(format!(
            "UnionC returned HTTP {status} without Content-Type application/json"
        )));
    }
    let ack: AgentReportAck = serde_json::from_slice(body).map_err(|error| {
        SendError::Transient(format!(
            "UnionC returned HTTP {status} without a valid report acknowledgement: {error}"
        ))
    })?;
    if ack.host_id != report.host.id || ack.report_id != report.report_id {
        return Err(SendError::Transient(format!(
            "UnionC acknowledgement identity mismatch: expected host {} report {}, got host {} \
             report {}",
            report.host.id, report.report_id, ack.host_id, ack.report_id
        )));
    }
    Ok(())
}

fn is_application_json(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

fn ensure_success(status: StatusCode, body: String, target: &str) -> Result<(), SendError> {
    if status.is_success() {
        return Ok(());
    }
    let detail: String = body.chars().take(512).collect();
    let message = format!("{target} rejected telemetry with HTTP {status}: {detail}");
    // 404/408/421/429 与 5xx 留作可重试：服务端重启、反代修复、限流退避之后，
    // 同一份报文仍可能被接受。
    match status {
        StatusCode::BAD_REQUEST
        | StatusCode::CONFLICT
        | StatusCode::PAYLOAD_TOO_LARGE
        | StatusCode::UNPROCESSABLE_ENTITY => Err(SendError::Permanent(message)),
        // 421 = 请求没走对链路（反向代理未透传 X-Forwarded-*），**不是**凭据问题。
        // 必须早于下面这一支匹配，否则会误判为需要浏览器重新授权。
        StatusCode::MISDIRECTED_REQUEST => Err(SendError::Transient(format!(
            "{message}（这是部署配置问题，不是凭据失效：请检查反向代理是否透传 \
             X-Forwarded-Proto 与 X-Forwarded-For）"
        ))),
        StatusCode::UNAUTHORIZED => Err(SendError::Unauthorized(message)),
        StatusCode::FORBIDDEN => Err(SendError::Revoked(format!(
            "{message}; this credential will not be replaced automatically—run `unionc-agent \
             pair --server <url>` to authorize the host again"
        ))),
        _ => Err(SendError::Transient(message)),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::model::{AgentHealth, CpuSnapshot, HostIdentity, MemorySnapshot, SystemSnapshot};

    fn report() -> AgentReport {
        AgentReport {
            schema_version: 1,
            report_id: Uuid::new_v4().to_string(),
            collected_at: Utc::now(),
            host: HostIdentity {
                id: Uuid::new_v4().to_string(),
                name: "ack-test".into(),
                os: "test".into(),
                os_version: None,
                kernel_version: None,
                arch: "test".into(),
                agent_version: "test".into(),
            },
            interval_seconds: 10.0,
            system: SystemSnapshot {
                uptime_seconds: 1,
                cpu: CpuSnapshot {
                    usage_percent: 0.0,
                    logical_count: 1,
                    physical_count: Some(1),
                    per_core_percent: vec![0.0],
                },
                memory: MemorySnapshot {
                    total_bytes: 1,
                    used_bytes: 0,
                    available_bytes: 1,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                },
                networks: Vec::new(),
                disks: Vec::new(),
                temperatures: Vec::new(),
                gpus: Vec::new(),
            },
            capabilities: Vec::new(),
            agent: AgentHealth {
                spool_pending_batches: 0,
                collector_errors: 0,
            },
        }
    }

    #[test]
    fn persists_trimmed_host_token() {
        let directory = std::env::temp_dir().join(format!("unionc-agent-token-{}", Uuid::new_v4()));
        let path = directory.join("agent-token");
        persist_private_value(&path, " secret-token\n", "host token").unwrap();
        assert_eq!(read_secret(&path, "host token").unwrap(), "secret-token");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn report_id_conflicts_are_permanent() {
        let error = ensure_success(
            StatusCode::CONFLICT,
            "report_id already belongs to another host".to_string(),
            "UnionC",
        )
        .expect_err("409 cannot become successful by retrying the same report");
        assert!(error.is_permanent());
    }

    #[test]
    fn a_successful_report_requires_a_matching_acknowledgement() {
        let report = report();
        let body = serde_json::to_vec(&serde_json::json!({
            "host_id": report.host.id,
            "report_id": report.report_id,
            "accepted": false,
            "received_at": Utc::now()
        }))
        .unwrap();
        validate_unionc_ack(
            StatusCode::ACCEPTED,
            Some("application/json; charset=utf-8"),
            &body,
            &report,
        )
        .unwrap();

        let error = validate_unionc_ack(StatusCode::OK, Some("application/json"), &body, &report)
            .expect_err("a structurally valid HTTP 200 must not acknowledge a report");
        assert!(matches!(error, SendError::Transient(_)));

        assert!(matches!(
            validate_unionc_ack(StatusCode::ACCEPTED, Some("text/plain"), &body, &report),
            Err(SendError::Transient(_))
        ));
        assert!(matches!(
            validate_unionc_ack(StatusCode::ACCEPTED, None, &body, &report),
            Err(SendError::Transient(_))
        ));
    }

    #[test]
    fn an_acknowledgement_for_another_report_is_not_accepted() {
        let report = report();
        let body = serde_json::to_vec(&serde_json::json!({
            "host_id": report.host.id,
            "report_id": Uuid::new_v4(),
            "accepted": true,
            "received_at": Utc::now()
        }))
        .unwrap();
        assert!(matches!(
            validate_unionc_ack(
                StatusCode::ACCEPTED,
                Some("application/json"),
                &body,
                &report
            ),
            Err(SendError::Transient(_))
        ));
    }

    #[test]
    fn acknowledgement_rejects_missing_or_unknown_current_contract_fields() {
        let report = report();
        let without_accepted = serde_json::to_vec(&serde_json::json!({
            "host_id": report.host.id,
            "report_id": report.report_id,
            "received_at": Utc::now()
        }))
        .unwrap();
        assert!(matches!(
            validate_unionc_ack(
                StatusCode::ACCEPTED,
                Some("application/json"),
                &without_accepted,
                &report
            ),
            Err(SendError::Transient(_))
        ));

        let with_obsolete_field = serde_json::to_vec(&serde_json::json!({
            "host_id": report.host.id,
            "report_id": report.report_id,
            "accepted": true,
            "received_at": Utc::now(),
            "legacy_status": "ok"
        }))
        .unwrap();
        assert!(matches!(
            validate_unionc_ack(
                StatusCode::ACCEPTED,
                Some("application/json"),
                &with_obsolete_field,
                &report
            ),
            Err(SendError::Transient(_))
        ));

        let noncanonical_uuid = serde_json::to_vec(&serde_json::json!({
            "host_id": report.host.id.to_uppercase(),
            "report_id": report.report_id,
            "accepted": true,
            "received_at": Utc::now()
        }))
        .unwrap();
        assert!(matches!(
            validate_unionc_ack(
                StatusCode::ACCEPTED,
                Some("application/json"),
                &noncanonical_uuid,
                &report
            ),
            Err(SendError::Transient(_))
        ));
    }

    #[test]
    fn forbidden_requires_browser_reauthorization() {
        let error = ensure_success(StatusCode::FORBIDDEN, "revoked".into(), "UnionC")
            .expect_err("403 must require browser reauthorization");
        assert!(error.is_revoked());
        assert!(!error.is_unauthorized());
    }
}
