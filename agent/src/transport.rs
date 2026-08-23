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
    report_contract,
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
        let (bounded, body) = report_contract::encode_report_body(report)
            .map_err(|error| SendError::Permanent(format!("invalid Agent report: {error}")))?;
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
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
        validate_unionc_ack(status, content_type.as_deref(), &body, &bounded)
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
        Ok(ensure_success(status, &body, "OTLP")?)
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
    let mut builder = Client::builder()
        .timeout(config.request_timeout())
        .user_agent(format!("unionc-agent/{}", env!("CARGO_PKG_VERSION")))
        // Every Agent endpoint is an exact API address. Following a 307/308
        // can replay report or pairing JSON to an unvalidated origin even if
        // reqwest strips the Authorization header on the cross-origin hop.
        .redirect(reqwest::redirect::Policy::none());
    if config.tls_identity_password.is_some() && config.tls_identity_pkcs12.is_none() {
        bail!("tls_identity_password requires tls_identity_pkcs12");
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        if config.tls_identity_pkcs12.is_some() {
            bail!(
                "tls_identity_pkcs12 is supported only on Windows and macOS; use \
                 tls_identity_pem on this platform"
            );
        }
        if let Some(path) = &config.tls_identity_pem {
            let bytes = fs::read(path)
                .with_context(|| format!("failed to read TLS identity {}", path.display()))?;
            builder = builder.identity(Identity::from_pem(&bytes)?);
        }
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
/// | `Unauthorized` | 服务端稳定 `unauthorized` 机器码确认凭据失效 | 需要创建新实例并再次配对 |
/// | `Transient`  | 只需等待 | 退避重试 |
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// 服务端拒绝了报文内容本身（400/409/413）。重试必然再次失败，
    /// 继续入队只会让 spool 被必失败的数据占满并挤掉后续有效报文。
    #[error("{0}")]
    Permanent(String),
    /// UnionC 以 401 和稳定 `unauthorized` 机器码确认凭据不被接受。主机进入
    /// `reauth_required`，只能通过创建新实例并执行当前 v2 配对流程恢复；Agent 不会自动生成
    /// 或替换凭据。代理/WAF 生成的未知 401 不得使用此变体。
    #[error("{0}")]
    Unauthorized(String),
    /// 网络故障或服务端暂时不可用。重试有意义。
    #[error("{0}")]
    Transient(String),
}

impl SendError {
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }

    /// 凭据已失效，需要创建新实例并再次配对后才可能成功。
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Unauthorized(_))
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
        return ensure_success(status, body, "UnionC");
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

fn ensure_success(status: StatusCode, body: &[u8], target: &str) -> Result<(), SendError> {
    if status.is_success() {
        return Ok(());
    }
    let error_code = std::str::from_utf8(body)
        .ok()
        .and_then(|body| serde_json::from_str::<ServerErrorCode>(body).ok())
        .map(|error| error.code);
    let detail: String = String::from_utf8_lossy(body).chars().take(512).collect();
    let message = format!("{target} rejected telemetry with HTTP {status}: {detail}");
    // 404/408/421/429 与 5xx 留作可重试：服务端重启、反代修复、限流退避之后，
    // 同一份报文仍可能被接受。
    match status {
        StatusCode::BAD_REQUEST | StatusCode::CONFLICT | StatusCode::PAYLOAD_TOO_LARGE => {
            Err(SendError::Permanent(message))
        }
        // 421 = 请求没走对链路（反向代理未透传 X-Forwarded-*），**不是**凭据问题。
        // 必须早于下面这一支匹配，否则会误判为需要创建新实例并再次配对。
        StatusCode::MISDIRECTED_REQUEST => Err(SendError::Transient(format!(
            "{message}（这是部署配置问题，不是凭据失效：请检查反向代理是否透传 \
             X-Forwarded-Proto 与 X-Forwarded-For）"
        ))),
        StatusCode::UNAUTHORIZED => match error_code.as_deref() {
            Some("unauthorized") => Err(SendError::Unauthorized(message)),
            // A reverse proxy, WAF, or temporary upstream auth layer may generate its own 401.
            // Only UnionC's stable machine code proves that the host credential is invalid;
            // otherwise keep the report queued and retry after the deployment recovers.
            _ => Err(SendError::Transient(message)),
        },
        StatusCode::FORBIDDEN => match error_code.as_deref() {
            // A valid credential accompanied by another host identity can never make this exact
            // report valid. This is the expected fate of old queued reports after pairing to a
            // different server/instance, so discard only that report and continue the FIFO.
            Some("agent_host_mismatch") => Err(SendError::Permanent(message)),
            // A proxy or WAF may generate an unrelated 403. Retrying is safer than permanently
            // deauthorizing a valid credential or deleting telemetry.
            _ => Err(SendError::Transient(message)),
        },
        _ => Err(SendError::Transient(message)),
    }
}

#[derive(serde::Deserialize)]
struct ServerErrorCode {
    code: String,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::model::{AgentHealth, CpuSnapshot, HostIdentity, MemorySnapshot, SystemSnapshot};

    #[test]
    fn agent_api_client_never_follows_redirects() {
        let client = build_client(&AgentConfig::default()).expect("build Agent API client");
        let configuration = format!("{client:?}");
        assert!(
            configuration.contains("Policy(None)"),
            "Agent API client unexpectedly permits redirects: {configuration}"
        );
    }

    fn report() -> AgentReport {
        AgentReport {
            schema_version: 1,
            report_id: Uuid::new_v4().to_string(),
            collected_at: Utc::now(),
            host: HostIdentity {
                id: Uuid::new_v4().to_string(),
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
    fn client_builder_rejects_unbound_identity_password() {
        let config = AgentConfig {
            tls_identity_password: Some("secret".into()),
            ..AgentConfig::default()
        };
        let error = build_client(&config)
            .expect_err("an otherwise unused TLS identity password must not be ignored");
        assert!(error.to_string().contains("tls_identity_pkcs12"));
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    #[test]
    fn client_builder_rejects_pkcs12_on_non_native_tls_backend() {
        let config = AgentConfig {
            tls_identity_pkcs12: Some("missing-client-identity.p12".into()),
            ..AgentConfig::default()
        };
        let error = build_client(&config)
            .expect_err("an unsupported PKCS#12 identity must not be silently ignored");
        assert!(error.to_string().contains("tls_identity_pem"));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn client_builder_rejects_pem_on_native_tls_backend() {
        let config = AgentConfig {
            tls_identity_pem: Some("missing-client-identity.pem".into()),
            ..AgentConfig::default()
        };
        let error = build_client(&config)
            .expect_err("an unsupported PEM identity must not reach request construction");
        assert!(error.to_string().contains("tls_identity_pkcs12"));
    }

    #[test]
    fn report_id_conflicts_are_permanent() {
        let error = ensure_success(
            StatusCode::CONFLICT,
            b"report_id already belongs to another host",
            "UnionC",
        )
        .expect_err("409 cannot become successful by retrying the same report");
        assert!(error.is_permanent());
    }

    #[test]
    fn non_contract_422_is_not_treated_as_a_current_permanent_rejection() {
        let error = ensure_success(
            StatusCode::UNPROCESSABLE_ENTITY,
            b"unexpected response",
            "UnionC",
        )
        .expect_err("422 is not part of the current Server report contract");
        assert!(matches!(error, SendError::Transient(_)));
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
    fn stable_unauthorized_code_requires_new_pairing() {
        let error = ensure_success(
            StatusCode::UNAUTHORIZED,
            br#"{"code":"unauthorized","message":"unauthorized"}"#,
            "UnionC",
        )
        .expect_err("UnionC's stable unauthorized code must require a newly authorized pairing");
        assert!(error.is_unauthorized());
    }

    #[test]
    fn unrecognized_unauthorized_response_keeps_the_credential_retryable() {
        let responses: &[&[u8]] = &[
            b"<html><body>temporary proxy authentication</body></html>",
            br#"{"code":"upstream_auth_required","message":"try again"}"#,
            br#"{"message":"missing machine code"}"#,
            br#"{"code":"Unauthorized","message":"machine codes are case-sensitive"}"#,
            b"{\"code\":\"unauthorized\",\"message\":\"invalid UTF-8: \xff\"}",
        ];
        for body in responses {
            let error = ensure_success(StatusCode::UNAUTHORIZED, body, "UnionC")
                .expect_err("an unknown 401 must not be accepted");
            assert!(matches!(error, SendError::Transient(_)));
            assert!(!error.is_unauthorized());
        }
    }

    #[test]
    fn forbidden_host_identity_mismatch_is_permanent_for_that_report() {
        let error = ensure_success(
            StatusCode::FORBIDDEN,
            br#"{"code":"agent_host_mismatch","message":"token does not belong to host"}"#,
            "UnionC",
        )
        .expect_err("a queued report for another host can never match the current credential");
        assert!(error.is_permanent());
    }

    #[test]
    fn unrecognized_forbidden_response_keeps_the_credential_retryable() {
        for body in [
            b"temporary policy rejection".as_slice(),
            br#"{"code":"forbidden","message":"unrelated access policy"}"#,
        ] {
            let error = ensure_success(StatusCode::FORBIDDEN, body, "UnionC")
                .expect_err("an unknown 403 must not be accepted");
            assert!(matches!(error, SendError::Transient(_)));
            assert!(!error.is_permanent());
        }
    }
}
