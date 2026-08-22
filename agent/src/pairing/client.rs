use anyhow::bail;
use reqwest::{StatusCode, header};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

const MAX_PAIRING_RESPONSE_BYTES: usize = 64 * 1024;

pub(super) fn random_secret() -> String {
    hex(&rand::random::<[u8; 32]>())
}

pub(super) fn sha256_hex(secret: &str) -> String {
    hex(&Sha256::digest(secret.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(DIGITS[(byte >> 4) as usize] as char);
        value.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    value
}

pub(super) async fn read_limited(
    response: reqwest::Response,
    target: &str,
) -> anyhow::Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PAIRING_RESPONSE_BYTES as u64)
    {
        bail!("{target} response exceeds the 64 KiB limit");
    }
    let mut response = response;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_PAIRING_RESPONSE_BYTES {
            bail!("{target} response exceeds the 64 KiB limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(super) fn pairing_response_content_type(response: &reqwest::Response) -> String {
    let raw = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("<missing>");
    pairing_content_type_for_diagnostics(raw)
}

pub(super) fn pairing_content_type_for_diagnostics(content_type: &str) -> String {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match media_type.as_str() {
        "application/json" | "text/html" | "text/plain" | "application/octet-stream" => media_type,
        "<missing>" => media_type,
        value if value.starts_with("application/") && value.ends_with("+json") => {
            "application/*+json".to_string()
        }
        _ => "<unexpected>".to_string(),
    }
}

pub(super) fn pairing_origin_for_diagnostics(endpoint: &str) -> String {
    reqwest::Url::parse(endpoint)
        .ok()
        .map(|url| url.origin().ascii_serialization())
        .filter(|origin| origin != "null")
        .unwrap_or_else(|| "<invalid Server origin>".to_string())
}

pub(super) fn parse_pairing_json<T: DeserializeOwned>(
    body: &[u8],
    content_type: &str,
    endpoint: &str,
    response_kind: &str,
) -> anyhow::Result<T> {
    let invalid_response = || {
        let origin = pairing_origin_for_diagnostics(endpoint);
        let content_type = pairing_content_type_for_diagnostics(content_type);
        anyhow::anyhow!(
            "UnionC returned an unexpected or malformed {response_kind} from Server origin {origin} (HTTP 2xx Content-Type: {content_type}); the configured Server address or port may be wrong. Use the complete UnionC management-console origin, including its port"
        )
    };
    if pairing_content_type_for_diagnostics(content_type) != "application/json" {
        return Err(invalid_response());
    }
    serde_json::from_slice(body).map_err(|_| invalid_response())
}

pub(super) fn ensure_pairing_status(
    status: StatusCode,
    allowed: &[StatusCode],
    body: &[u8],
    operation: &str,
) -> anyhow::Result<()> {
    if allowed.contains(&status) {
        return Ok(());
    }
    let detail: String = String::from_utf8_lossy(body).chars().take(512).collect();
    if status.is_success() {
        bail!("UnionC returned an unexpected HTTP {status} while attempting to {operation}");
    }
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        bail!(
            "UnionC refused to {operation}: HTTP {status}; start a new browser pairing ({detail})"
        );
    }
    bail!("UnionC failed to {operation}: HTTP {status}: {detail}")
}
