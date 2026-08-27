use std::{collections::HashSet, time::Duration};

use reqwest::{Method, Response, StatusCode, header};
use serde_json::Value;

use crate::{
    error::{AppError, AppResult},
    model::{Host, web_url},
};

const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;
const MAX_COVER_BYTES: usize = 8 * 1024 * 1024;
const MAX_ITEMS: usize = 512;

#[derive(Clone)]
pub struct UpstreamClient {
    verified: reqwest::Client,
    insecure: reqwest::Client,
}

impl UpstreamClient {
    pub fn new() -> anyhow::Result<Self> {
        let common = || {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
        };
        Ok(Self {
            verified: common().build()?,
            insecure: common().danger_accept_invalid_certs(true).build()?,
        })
    }

    fn for_host(&self, host: &Host) -> &reqwest::Client {
        if host.verify_tls {
            &self.verified
        } else {
            &self.insecure
        }
    }

    async fn json(
        &self,
        host: &Host,
        method: Method,
        path: &str,
        body: Option<&Value>,
        authenticated: bool,
    ) -> AppResult<Value> {
        let mut request = self
            .for_host(host)
            .request(method, format!("{}{path}", web_url(host)));
        if authenticated {
            request = request.basic_auth(&host.username, Some(&host.password));
        }
        if let Some(body) = body {
            request = request.json(body);
        } else if request.try_clone().is_some() {
            request = request.header(header::CONTENT_LENGTH, "0");
        }
        let response = request
            .send()
            .await
            .map_err(|error| AppError::Upstream(format!("Sunshine request failed: {error}")))?;
        json_response(response).await
    }

    pub async fn apps_list(&self, host: &Host) -> AppResult<Value> {
        let value = self
            .json(host, Method::GET, "/api/apps", None, true)
            .await?;
        let apps = value
            .get("apps")
            .and_then(Value::as_array)
            .ok_or_else(|| AppError::Upstream("Sunshine apps response is invalid".into()))?;
        if apps.len() > MAX_ITEMS || apps.iter().any(|item| !item.is_object()) {
            return Err(AppError::Upstream(
                "Sunshine apps response exceeds its safe shape".into(),
            ));
        }
        Ok(serde_json::json!({ "apps": apps }))
    }

    pub async fn apps_save(&self, host: &Host, body: &Value) -> AppResult<Value> {
        self.json(host, Method::POST, "/api/apps", Some(body), true)
            .await
    }

    pub async fn apps_close(&self, host: &Host) -> AppResult<Value> {
        self.json(host, Method::POST, "/api/apps/close", None, true)
            .await
    }

    pub async fn apps_delete(&self, host: &Host, index: u32) -> AppResult<Value> {
        self.json(
            host,
            Method::DELETE,
            &format!("/api/apps/{index}"),
            None,
            true,
        )
        .await
    }

    pub async fn clients_list(&self, host: &Host) -> AppResult<Value> {
        let value = self
            .json(host, Method::GET, "/api/clients/list", None, true)
            .await?;
        let status = value
            .get("status")
            .and_then(Value::as_bool)
            .ok_or_else(|| AppError::Upstream("Sunshine clients response is invalid".into()))?;
        let clients = value
            .get("named_certs")
            .and_then(Value::as_array)
            .ok_or_else(|| AppError::Upstream("Sunshine clients response is invalid".into()))?;
        if clients.len() > MAX_ITEMS {
            return Err(AppError::Upstream(
                "Sunshine clients response has too many items".into(),
            ));
        }
        let mut ids = HashSet::with_capacity(clients.len());
        for client in clients {
            let id = client
                .get("uuid")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty() && id.len() <= 128)
                .ok_or_else(|| AppError::Upstream("Sunshine client UUID is invalid".into()))?;
            if !client.get("enabled").is_some_and(Value::is_boolean) || !ids.insert(id) {
                return Err(AppError::Upstream(
                    "Sunshine clients response has an invalid item".into(),
                ));
            }
        }
        Ok(serde_json::json!({ "status": status, "named_certs": clients }))
    }

    pub async fn clients_unpair(&self, host: &Host, uuid: &str) -> AppResult<Value> {
        self.json(
            host,
            Method::POST,
            "/api/clients/unpair",
            Some(&serde_json::json!({ "uuid": uuid })),
            true,
        )
        .await
    }

    pub async fn clients_unpair_all(&self, host: &Host) -> AppResult<Value> {
        self.json(host, Method::POST, "/api/clients/unpair-all", None, true)
            .await
    }

    pub async fn clients_update(&self, host: &Host, uuid: &str, enabled: bool) -> AppResult<Value> {
        self.json(
            host,
            Method::POST,
            "/api/clients/update",
            Some(&serde_json::json!({ "uuid": uuid, "enabled": enabled })),
            true,
        )
        .await
    }

    pub async fn config_get(&self, host: &Host) -> AppResult<Value> {
        self.json(host, Method::GET, "/api/config", None, true)
            .await
    }

    pub async fn config_save(&self, host: &Host, body: &Value) -> AppResult<Value> {
        self.json(host, Method::POST, "/api/config", Some(body), true)
            .await
    }

    pub async fn config_locale(&self, host: &Host) -> AppResult<Value> {
        self.json(host, Method::GET, "/api/configLocale", None, false)
            .await
    }

    pub async fn logs(&self, host: &Host) -> AppResult<Value> {
        let response = self
            .for_host(host)
            .get(format!("{}/api/logs", web_url(host)))
            .basic_auth(&host.username, Some(&host.password))
            .send()
            .await
            .map_err(|error| AppError::Upstream(format!("Sunshine logs failed: {error}")))?;
        let status = response.status();
        let content_type = content_type(&response);
        let text = String::from_utf8(read_limited(response, MAX_JSON_BYTES).await?)
            .map_err(|_| AppError::Upstream("Sunshine logs are not UTF-8".into()))?;
        ensure_status(status, &text)?;
        if !content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("text/plain")
        {
            return Err(AppError::Upstream(
                "Sunshine logs must use text/plain".into(),
            ));
        }
        Ok(serde_json::json!({ "content": text }))
    }

    pub async fn pin(&self, host: &Host, pin: &str, name: &str) -> AppResult<Value> {
        self.json(
            host,
            Method::POST,
            "/api/pin",
            Some(&serde_json::json!({ "pin": pin, "name": name })),
            true,
        )
        .await
    }

    pub async fn restart(&self, host: &Host) -> AppResult<Value> {
        self.json(host, Method::POST, "/api/restart", None, true)
            .await
    }

    pub async fn reset_display(&self, host: &Host) -> AppResult<Value> {
        self.json(
            host,
            Method::POST,
            "/api/reset-display-device-persistence",
            None,
            true,
        )
        .await
    }

    pub async fn cover(&self, host: &Host, index: u32) -> AppResult<(String, Vec<u8>)> {
        let response = self
            .for_host(host)
            .get(format!("{}/api/covers/{index}", web_url(host)))
            .basic_auth(&host.username, Some(&host.password))
            .send()
            .await
            .map_err(|error| AppError::Upstream(format!("Sunshine cover failed: {error}")))?;
        let status = response.status();
        let content_type = content_type(&response);
        if !status.is_success() {
            return Err(status_error(status, "cover request failed"));
        }
        Ok((content_type, read_limited(response, MAX_COVER_BYTES).await?))
    }

    pub async fn cover_upload(&self, host: &Host, key: &str, url: &str) -> AppResult<Value> {
        self.json(
            host,
            Method::POST,
            "/api/covers/upload",
            Some(&serde_json::json!({ "key": key, "url": url })),
            true,
        )
        .await
    }

    pub async fn check_reachable(&self, host: &Host) -> bool {
        tokio::time::timeout(
            Duration::from_millis(500),
            tokio::net::TcpStream::connect((host.host.as_str(), host.web_port)),
        )
        .await
        .is_ok_and(|result| result.is_ok())
    }
}

async fn json_response(response: Response) -> AppResult<Value> {
    let status = response.status();
    let content_type = content_type(&response);
    let text = String::from_utf8(read_limited(response, MAX_JSON_BYTES).await?)
        .map_err(|_| AppError::Upstream("Sunshine response is not UTF-8".into()))?;
    ensure_status(status, &text)?;
    if !content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("application/json")
    {
        return Err(AppError::Upstream(
            "Sunshine success response must use application/json".into(),
        ));
    }
    serde_json::from_str(&text)
        .map_err(|error| AppError::Upstream(format!("invalid Sunshine JSON: {error}")))
}

async fn read_limited(mut response: Response, limit: usize) -> AppResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(AppError::Upstream("Sunshine response is too large".into()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| AppError::Upstream(format!("reading Sunshine response failed: {error}")))?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(AppError::Upstream("Sunshine response is too large".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn content_type(response: &Response) -> String {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn ensure_status(status: StatusCode, body: &str) -> AppResult<()> {
    if status.is_success() {
        Ok(())
    } else {
        Err(status_error(status, body))
    }
}

fn status_error(status: StatusCode, body: &str) -> AppError {
    let detail = body
        .chars()
        .take(200)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        AppError::Forbidden(format!(
            "Sunshine authentication failed (HTTP {status}: {detail})"
        ))
    } else {
        AppError::Upstream(format!("Sunshine returned HTTP {status}: {detail}"))
    }
}
