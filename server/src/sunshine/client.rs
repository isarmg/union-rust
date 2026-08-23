//! Sunshine API 客户端。
//!
//! 所有函数接收 `&SunshineHostConfig` 而非 `&AppState`，使调用方可以按主机 ID
//! 自由选择目标主机，天然支持多主机管理场景。
//!
//! # 代理模式说明
//!
//! 这个模块充当一个 HTTP 代理：前端把请求发给本应用（union），
//! 本应用再用 `reqwest`（Rust 的 HTTP 客户端库）把请求转发给 Sunshine 的 Web API。
//! 这样做的好处是：
//! 1. 前端只需要和 union 通信，不需要直接访问 Sunshine（避免跨域问题）
//! 2. 认证凭据（用户名/密码）保存在服务器端，不暴露给前端
//! 3. 可以统一做错误处理、日志记录等

use std::time::Duration;

use serde_json::Value;

use crate::{
    config::SunshineHostConfig,
    error::{AppError, AppResult},
    infra::http_client,
    infra::network,
};

// ─── 响应体大小限制 ───────────────────────────────────────────────────────────
//
// Sunshine 主机是管理员配置的上游，但"管理员配置"不等于"可以无条件信任"：主机可能
// 被攻陷，非生产环境还允许关闭 TLS 校验（此时中间人也能构造响应）。用
// `resp.text()` / `resp.bytes()` 把**整个**响应读进内存是没有上限的——一个超大响应
// 就能把服务端 OOM。Agent 上报侧有 512 KiB 的 `DefaultBodyLimit`，上游方向同样需要设限。
//
// 这里对两类响应各设一个上限，并且是**流式累计**判断：超限即中断连接，而不是先收完
// 再检查大小（那样限制就没有意义了）。

/// API 文本/JSON 响应上限。Sunshine 的配置与日志通常都在几十 KiB 量级。
const MAX_JSON_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// 封面图片上限。
const MAX_COVER_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Upstream diagnostics are copied into logs, health snapshots and API errors;
/// keep that long-lived representation small even when the response body is not.
const MAX_UPSTREAM_ERROR_DETAIL_CHARS: usize = 200;

/// 流式读取响应体，累计超过 `limit` 立即中断。
async fn read_limited(response: reqwest::Response, limit: usize, what: &str) -> AppResult<Vec<u8>> {
    // Content-Length 可信时先做一次快速拒绝，省掉整段传输。
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return Err(AppError::Upstream(format!(
            "Sunshine {what} 响应超过 {} KiB 上限",
            limit / 1024
        )));
    }
    // 逐块读取。`chunk()` 是 reqwest 自带的增量读取接口，不需要额外的 stream feature，
    // 且在这里中断即关闭连接——不会像"先收完再检查大小"那样白读一遍。
    let mut buffer = Vec::new();
    let mut response = response;
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|e| AppError::Upstream(format!("读取 Sunshine {what} 响应失败: {e}")))?;
        let Some(chunk) = chunk else { break };
        // Content-Length 可能撒谎或缺失（分块编码），因此逐块累计校验。
        if buffer.len() + chunk.len() > limit {
            return Err(AppError::Upstream(format!(
                "Sunshine {what} 响应超过 {} KiB 上限",
                limit / 1024
            )));
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer)
}

async fn read_limited_text(response: reqwest::Response, what: &str) -> AppResult<String> {
    let bytes = read_limited(response, MAX_JSON_RESPONSE_BYTES, what).await?;
    String::from_utf8(bytes)
        .map_err(|_| AppError::Upstream(format!("Sunshine {what} 响应不是有效 UTF-8")))
}

// ─── 内部工具 ─────────────────────────────────────────────────────────────────

pub fn web_url(host: &SunshineHostConfig) -> String {
    format!("https://{}", network::authority(&host.host, host.web_port))
}

fn api_url(host: &SunshineHostConfig, path: &str) -> String {
    format!("{}{path}", web_url(host))
}

/// 统一处理 Sunshine API 的响应：检查状态码，提取错误信息，解析 JSON。
///
/// # 错误提取逻辑
///
/// 当 HTTP 状态码不是 2xx 时，尝试从响应体提取人类可读的错误描述：
/// 1. 先尝试把响应体解析为 JSON
/// 2. 从 JSON 中找 `"status"` 或 `"error"` 字段（Sunshine 的常见错误格式）
/// 3. 如果找不到，就截取响应体前 200 个字符作为错误信息
///
async fn handle_response(resp: reqwest::Response) -> AppResult<Value> {
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = read_limited_text(resp, "API").await?;

    if !status.is_success() {
        return Err(sunshine_status_error(status, &text));
    }
    parse_json_success(&content_type, &text)
}

fn parse_json_success(content_type: &str, text: &str) -> AppResult<Value> {
    let media_type = content_type.split(';').next().unwrap_or("").trim();
    if !media_type.eq_ignore_ascii_case("application/json") {
        return Err(AppError::Upstream(
            "Sunshine API success response must use application/json".to_string(),
        ));
    }
    serde_json::from_str::<Value>(text)
        .map_err(|error| AppError::Upstream(format!("Sunshine API 响应不是有效 JSON: {error}")))
}

fn sunshine_status_error(status: reqwest::StatusCode, text: &str) -> AppError {
    let parsed = serde_json::from_str::<Value>(text).ok();
    let source = parsed
        .as_ref()
        .and_then(|value| {
            value
                .get("status")
                .and_then(Value::as_str)
                .or_else(|| value.get("error").and_then(Value::as_str))
        })
        .unwrap_or(text);
    let mut characters = source.chars();
    let mut detail = characters
        .by_ref()
        .take(MAX_UPSTREAM_ERROR_DETAIL_CHARS)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if characters.next().is_some() {
        detail.pop();
        detail.push('…');
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        AppError::Forbidden(format!(
            "Sunshine 认证失败，请检查主机用户名和密码（HTTP {status}: {detail}）"
        ))
    } else {
        AppError::Upstream(format!("Sunshine API 返回 HTTP {status}: {detail}"))
    }
}

/// 向 Sunshine API 发送 GET 请求。
///
/// `basic_auth` 是 HTTP Basic 认证的标准方式：
/// 将用户名和密码以 `username:password` 格式用 Base64 编码，
/// 放在请求头 `Authorization: Basic <编码后的字符串>` 中。
/// Sunshine 的 Web UI 就是用这种方式保护 API 的。
async fn sunshine_get(host: &SunshineHostConfig, path: &str) -> AppResult<Value> {
    let resp = http_client::for_tls(host.verify_tls)?
        .get(api_url(host, path))
        .basic_auth(&host.username, Some(&host.password)) // HTTP Basic 认证：用户名 + 密码
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("连接 Sunshine {path} 失败: {e}")))?;
    handle_response(resp).await
}

/// 向 Sunshine API 发送带 JSON 请求体的 POST 请求。
///
/// `.json(body)` 会自动把 `Value` 序列化为 JSON 字符串，
/// 并设置 `Content-Type: application/json` 请求头。
async fn sunshine_post_json(
    host: &SunshineHostConfig,
    path: &str,
    body: &Value,
) -> AppResult<Value> {
    let resp = http_client::for_tls(host.verify_tls)?
        .post(api_url(host, path))
        .basic_auth(&host.username, Some(&host.password))
        .json(body)
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("请求 Sunshine {path} 失败: {e}")))?;
    handle_response(resp).await
}

/// 向 Sunshine API 发送无请求体的 POST 请求。
///
/// 为什么需要手动设置 `CONTENT_LENGTH: 0`？
///
/// 某些服务器（包括部分版本的 Sunshine）对 POST 请求有严格要求：
/// 必须明确声明 Content-Length 为 0，否则服务器可能认为请求不完整而挂起等待请求体。
/// reqwest 在没有请求体时不会自动添加这个头，所以需要手动加上。
async fn sunshine_post_empty(host: &SunshineHostConfig, path: &str) -> AppResult<Value> {
    let resp = http_client::for_tls(host.verify_tls)?
        .post(api_url(host, path))
        .basic_auth(&host.username, Some(&host.password))
        .header(reqwest::header::CONTENT_LENGTH, "0") // 明确告知服务器请求体为空，避免服务器等待
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("请求 Sunshine {path} 失败: {e}")))?;
    handle_response(resp).await
}

/// 向 Sunshine API 发送 DELETE 请求。
async fn sunshine_delete(host: &SunshineHostConfig, path: &str) -> AppResult<Value> {
    let resp = http_client::for_tls(host.verify_tls)?
        .delete(api_url(host, path))
        .basic_auth(&host.username, Some(&host.password))
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("请求 Sunshine {path} 失败: {e}")))?;
    handle_response(resp).await
}

// ─── TCP 可达性检测（不需要认证）────────────────────────────────────────────────

/// 检测 Sunshine 主机是否可通过 TCP 连接访问（不涉及认证）。
///
/// # 超时机制
///
/// `tokio::time::timeout` 包裹异步操作，如果在指定时间内没有完成就取消并返回超时错误。
/// 这里设置 500ms 超时，原因：
/// - 局域网内 TCP 连接通常 <50ms，500ms 已经足够宽裕
/// - 如果超时，说明主机已关机或网络不通，不需要等更长时间
///
/// `.is_ok_and(|r| r.is_ok())` 的双重 `is_ok` 含义：
/// - 外层 `is_ok()`：检查 `timeout` 是否没有超时（`Ok` 表示在时间内得到结果）
/// - 内层 `|r| r.is_ok()`：检查 TCP 连接本身是否成功
pub async fn check_reachable(host: &SunshineHostConfig) -> bool {
    use tokio::{net::TcpStream, time::timeout};
    let address = network::normalize_host(&host.host);
    timeout(
        Duration::from_millis(500), // 500 毫秒超时：主机离线时不等太久
        TcpStream::connect((address.as_str(), host.web_port)),
    )
    .await
    .is_ok_and(|r| r.is_ok()) // 超时返回 false，连接失败也返回 false，只有连接成功才返回 true
}

/// 验证 Sunshine Web API 及管理凭据，而不只是检查端口是否打开。
pub async fn check_connection(host: &SunshineHostConfig) -> Result<(), String> {
    apps_list(host)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

// ─── 应用管理 ──────────────────────────────────────────────────────────────────

/// 获取 Sunshine 管理的游戏/应用列表。
pub async fn apps_list(host: &SunshineHostConfig) -> AppResult<Value> {
    let response = sunshine_get(host, "/api/apps").await?;
    normalize_apps_response(response)
}

fn normalize_apps_response(response: Value) -> AppResult<Value> {
    let Some(apps) = response.get("apps").filter(|value| value.is_array()) else {
        return Err(AppError::Upstream(
            "Sunshine /api/apps response must contain an apps array".to_string(),
        ));
    };
    Ok(serde_json::json!({ "apps": apps }))
}

/// 保存（新增或修改）一个游戏/应用配置。
pub async fn apps_save(host: &SunshineHostConfig, app: Value) -> AppResult<Value> {
    sunshine_post_json(host, "/api/apps", &app).await
}

/// 关闭当前正在运行的游戏/应用。
pub async fn apps_close(host: &SunshineHostConfig) -> AppResult<Value> {
    sunshine_post_empty(host, "/api/apps/close").await
}

/// 删除指定索引的游戏/应用。
pub async fn apps_delete(host: &SunshineHostConfig, index: u32) -> AppResult<Value> {
    sunshine_delete(host, &format!("/api/apps/{index}")).await
}

// ─── 客户端管理 ────────────────────────────────────────────────────────────────

/// 列出已配对的 Moonlight 客户端。
pub async fn clients_list(host: &SunshineHostConfig) -> AppResult<Value> {
    let response = sunshine_get(host, "/api/clients/list").await?;
    normalize_clients_response(response)
}

fn normalize_clients_response(response: Value) -> AppResult<Value> {
    let Some(status) = response.get("status").filter(|value| value.is_boolean()) else {
        return Err(AppError::Upstream(
            "Sunshine /api/clients/list response must contain boolean status and named_certs array"
                .to_string(),
        ));
    };
    let Some(named_certs) = response.get("named_certs").filter(|value| value.is_array()) else {
        return Err(AppError::Upstream(
            "Sunshine /api/clients/list response must contain boolean status and named_certs array"
                .to_string(),
        ));
    };
    Ok(serde_json::json!({
        "status": status,
        "named_certs": named_certs
    }))
}

/// 取消与指定 UUID 客户端的配对。
pub async fn clients_unpair(host: &SunshineHostConfig, uuid: &str) -> AppResult<Value> {
    sunshine_post_json(
        host,
        "/api/clients/unpair",
        &serde_json::json!({ "uuid": uuid }),
    )
    .await
}

/// 取消所有已配对客户端。
pub async fn clients_unpair_all(host: &SunshineHostConfig) -> AppResult<Value> {
    sunshine_post_empty(host, "/api/clients/unpair-all").await
}

/// 更新指定客户端的启用/禁用状态。
pub async fn clients_update(
    host: &SunshineHostConfig,
    uuid: &str,
    enabled: bool,
) -> AppResult<Value> {
    sunshine_post_json(
        host,
        "/api/clients/update",
        &serde_json::json!({ "uuid": uuid, "enabled": enabled }),
    )
    .await
}

// ─── 配置管理 ──────────────────────────────────────────────────────────────────

/// 获取 Sunshine 当前配置。
pub async fn config_get(host: &SunshineHostConfig) -> AppResult<Value> {
    sunshine_get(host, "/api/config").await
}

/// 保存 Sunshine 配置。
pub async fn config_save(host: &SunshineHostConfig, config: Value) -> AppResult<Value> {
    sunshine_post_json(host, "/api/config", &config).await
}

/// 获取 Sunshine 的本地化配置（不需要认证，所以单独实现）。
pub async fn config_locale(host: &SunshineHostConfig) -> AppResult<Value> {
    // 注意：这个接口不需要 basic_auth，所以没有使用通用的 sunshine_get
    let resp = http_client::for_tls(host.verify_tls)?
        .get(api_url(host, "/api/configLocale"))
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("连接 Sunshine /api/configLocale 失败: {e}")))?;
    handle_response(resp).await
}

// ─── 日志 ──────────────────────────────────────────────────────────────────────

/// 获取 Sunshine 的运行日志。
pub async fn api_logs(host: &SunshineHostConfig) -> AppResult<Value> {
    let response = http_client::for_tls(host.verify_tls)?
        .get(api_url(host, "/api/logs"))
        .basic_auth(&host.username, Some(&host.password))
        .send()
        .await
        .map_err(|error| AppError::Upstream(format!("连接 Sunshine /api/logs 失败: {error}")))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = read_limited_text(response, "日志").await?;
    if !status.is_success() {
        return Err(sunshine_status_error(status, &text));
    }
    logs_payload(&content_type, text)
}

fn logs_payload(content_type: &str, content: String) -> AppResult<Value> {
    let media_type = content_type.split(';').next().unwrap_or("").trim();
    if !media_type.eq_ignore_ascii_case("text/plain") {
        return Err(AppError::Upstream(
            "Sunshine /api/logs response must use text/plain".to_string(),
        ));
    }
    Ok(serde_json::json!({ "content": content }))
}

// ─── 配对 ──────────────────────────────────────────────────────────────────────

/// 使用 PIN 码与 Moonlight 客户端完成配对。
///
/// Moonlight 配对流程：客户端显示一个 PIN 码，用户在 Sunshine 管理界面输入此 PIN 完成配对。
pub async fn pin_pair(host: &SunshineHostConfig, pin: &str, name: &str) -> AppResult<Value> {
    sunshine_post_json(
        host,
        "/api/pin",
        &serde_json::json!({ "pin": pin, "name": name }),
    )
    .await
}

// ─── 系统操作 ──────────────────────────────────────────────────────────────────

/// 重启 Sunshine 服务进程。
pub async fn restart(host: &SunshineHostConfig) -> AppResult<Value> {
    sunshine_post_empty(host, "/api/restart").await
}

/// 重置显示设备持久化配置（用于解决虚拟显示器配置异常问题）。
pub async fn reset_display_device(host: &SunshineHostConfig) -> AppResult<Value> {
    sunshine_post_empty(host, "/api/reset-display-device-persistence").await
}

// ─── 封面图片 ──────────────────────────────────────────────────────────────────

/// 下载指定应用的封面图片，返回 (Content-Type, 图片字节数据) 元组。
///
/// 这里不使用通用的 `handle_response`，因为需要返回二进制数据（图片字节），
/// 而不是 JSON。所以单独处理响应，读取 Content-Type 头和原始字节流。
pub async fn cover_get(host: &SunshineHostConfig, index: u32) -> AppResult<(String, Vec<u8>)> {
    let resp = http_client::for_tls(host.verify_tls)?
        .get(api_url(host, &format!("/api/covers/{index}")))
        .basic_auth(&host.username, Some(&host.password))
        .send()
        .await
        .map_err(|e| AppError::Process(format!("Sunshine cover GET failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(AppError::Process(format!(
            "Sunshine cover endpoint returned HTTP {}",
            resp.status()
        )));
    }

    // 从响应头中提取 Content-Type（如 "image/jpeg" 或 "image/png"）
    // 如果响应头不存在或不是有效字符串，默认使用 "image/jpeg"
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();

    // 流式读取并在超过上限时中断，避免被超大响应撑爆内存。
    let bytes = read_limited(resp, MAX_COVER_RESPONSE_BYTES, "封面").await?;

    Ok((content_type, bytes))
}

/// 上传游戏封面图片（通过 URL 方式，让 Sunshine 自己去下载图片）。
pub async fn cover_upload(host: &SunshineHostConfig, key: &str, url: &str) -> AppResult<Value> {
    sunshine_post_json(
        host,
        "/api/covers/upload",
        &serde_json::json!({ "key": key, "url": url }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{
        logs_payload, normalize_apps_response, normalize_clients_response, parse_json_success,
        sunshine_status_error,
    };
    use crate::error::AppError;

    #[test]
    fn common_api_success_requires_current_json_contract() {
        assert_eq!(
            parse_json_success("application/json; charset=utf-8", r#"{"status":true}"#).unwrap(),
            serde_json::json!({"status": true})
        );
        assert!(parse_json_success("text/plain", r#"{"status":true}"#).is_err());
        assert!(parse_json_success("application/json", "").is_err());
        assert!(parse_json_success("application/json", "ok").is_err());
    }

    #[test]
    fn collection_responses_use_only_the_current_envelopes() {
        assert_eq!(
            normalize_apps_response(serde_json::json!({"apps": [], "removed": true})).unwrap(),
            serde_json::json!({"apps": []})
        );
        assert!(normalize_apps_response(serde_json::json!([])).is_err());
        assert_eq!(
            normalize_clients_response(serde_json::json!({
                "status": true,
                "named_certs": [],
                "removed": true
            }))
            .unwrap(),
            serde_json::json!({"status": true, "named_certs": []})
        );
        assert!(
            normalize_clients_response(serde_json::json!({"status": true, "clients": []})).is_err()
        );
    }

    #[test]
    fn api_logs_requires_current_text_plain_upstream() {
        assert_eq!(
            logs_payload("text/plain; charset=utf-8", "line".to_string()).unwrap(),
            serde_json::json!({"content": "line"})
        );
        assert!(logs_payload("application/json", r#""line""#.to_string()).is_err());
        assert!(logs_payload("", "line".to_string()).is_err());
    }

    #[test]
    fn upstream_json_error_fields_are_always_bounded_and_single_line() {
        let oversized = format!("first line\n{}", "界".repeat(1_000));
        let body = serde_json::json!({ "status": true, "error": oversized }).to_string();
        let error = sunshine_status_error(reqwest::StatusCode::BAD_GATEWAY, &body);
        let AppError::Upstream(message) = error else {
            panic!("non-authentication upstream status must remain an upstream error");
        };
        let detail = message
            .strip_prefix("Sunshine API 返回 HTTP 502 Bad Gateway: ")
            .expect("stable error prefix");
        assert_eq!(detail.chars().count(), 200);
        assert!(detail.ends_with('…'));
        assert!(!detail.chars().any(char::is_control));
        assert!(detail.starts_with("first line "));
    }
}
