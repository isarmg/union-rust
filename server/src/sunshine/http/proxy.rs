//! Sunshine Web API 代理端点。

use super::{common::*, *};

// ─── Sunshine API 代理 ────────────────────────────────────────────────────────
// 以下 handler 都是简单的代理：找到主机 → 调用 sunshine 模块的对应函数 → 返回结果。
// 所有实际的 HTTP 通信逻辑都封装在 `sunshine.rs` 中。

pub(crate) async fn apps_list(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(
        client::apps_list(&find_host(&state, &id).await?).await?,
    ))
}

pub(crate) async fn apps_save(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> AppResult<Json<Value>> {
    validate_proxy_json_object("Sunshine app payload", &body, 256 * 1024)?;
    let detail = body
        .get("name")
        .and_then(Value::as_str)
        .map(|name| format!("name={}", name.trim()))
        .unwrap_or_else(|| "app payload saved".to_string());
    let host = find_host(&state, &id).await?;
    let response = finish_sunshine_upstream_mutation(
        &state,
        "sunshine.app.save",
        id,
        Some(detail),
        async move { client::apps_save(&host, body).await },
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn apps_close(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let host = find_host(&state, &id).await?;
    let response =
        finish_sunshine_upstream_mutation(&state, "sunshine.app.close", id, None, async move {
            client::apps_close(&host).await
        })
        .await?;
    Ok(Json(response))
}

pub(crate) async fn apps_delete(
    State(state): State<AppState>,
    Path((id, index)): Path<(String, u32)>,
) -> AppResult<Json<Value>> {
    validate_index(index)?;
    // `Path<(String, u32)>` 提取两个路径参数 `/hosts/{id}/apps/{index}`
    let host = find_host(&state, &id).await?;
    let response = finish_sunshine_upstream_mutation(
        &state,
        "sunshine.app.delete",
        id,
        Some(format!("index={index}")),
        async move { client::apps_delete(&host, index).await },
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn clients_list(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(
        client::clients_list(&find_host(&state, &id).await?).await?,
    ))
}

pub(crate) async fn clients_unpair(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(p): Json<SunshineUnpairRequest>,
) -> AppResult<Json<Value>> {
    let client_id = validate_client_id(&p.uuid)?.to_string();
    let detail = format!("client={client_id}");
    let host = find_host(&state, &id).await?;
    let response = finish_sunshine_upstream_mutation(
        &state,
        "sunshine.client.unpair",
        id,
        Some(detail),
        async move { client::clients_unpair(&host, &client_id).await },
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn clients_unpair_all(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let host = find_host(&state, &id).await?;
    let response = finish_sunshine_upstream_mutation(
        &state,
        "sunshine.client.unpair_all",
        id,
        None,
        async move { client::clients_unpair_all(&host).await },
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn clients_update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(p): Json<SunshineClientUpdateRequest>,
) -> AppResult<Json<Value>> {
    let client_id = validate_client_id(&p.uuid)?.to_string();
    let detail = format!("client={client_id} enabled={}", p.enabled);
    let host = find_host(&state, &id).await?;
    let response = finish_sunshine_upstream_mutation(
        &state,
        "sunshine.client.update",
        id,
        Some(detail),
        async move { client::clients_update(&host, &client_id, p.enabled).await },
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn config_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(
        client::config_get(&find_host(&state, &id).await?).await?,
    ))
}

pub(crate) async fn config_save(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> AppResult<Json<Value>> {
    validate_proxy_json_object("Sunshine config payload", &body, 1024 * 1024)?;
    let host = find_host(&state, &id).await?;
    let response = finish_sunshine_upstream_mutation(
        &state,
        "sunshine.config.save",
        id,
        Some("config updated".to_string()),
        async move { client::config_save(&host, body).await },
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn config_locale(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(
        client::config_locale(&find_host(&state, &id).await?).await?,
    ))
}

pub(crate) async fn api_logs(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(
        client::api_logs(&find_host(&state, &id).await?).await?,
    ))
}

pub(crate) async fn pin(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(p): Json<SunshinePinRequest>,
) -> AppResult<Json<Value>> {
    let (pin, name) = validate_pin_request(&p.pin, &p.name)?;
    let detail = format!("name={name}");
    let host = find_host(&state, &id).await?;
    let response = finish_sunshine_upstream_mutation(
        &state,
        "sunshine.client.pair",
        id,
        Some(detail),
        async move { client::pin_pair(&host, &pin, &name).await },
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn restart(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let host = find_host(&state, &id).await?;
    let response = finish_sunshine_upstream_mutation(
        &state,
        "sunshine.system.restart",
        id,
        None,
        async move { client::restart(&host).await },
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn reset_display(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let host = find_host(&state, &id).await?;
    let response =
        finish_sunshine_upstream_mutation(&state, "sunshine.display.reset", id, None, async move {
            client::reset_display_device(&host).await
        })
        .await?;
    Ok(Json(response))
}

/// 允许原样转发的封面 MIME 类型。
///
/// 白名单之外的一律降级为 `application/octet-stream`，浏览器只会下载、不会渲染。
const ALLOWED_COVER_TYPES: [&str; 5] = [
    "image/jpeg",
    "image/png",
    "image/webp",
    "image/gif",
    "image/avif",
];

/// 获取并转发游戏封面图片（二进制响应，需要特殊处理）。
///
/// # 为什么必须对 Content-Type 做白名单
///
/// 把上游的 `Content-Type` **原样**写进响应，等于让一台恶意或已被攻陷的 Sunshine
/// 主机只要回 `Content-Type: text/html` 加一段 HTML，UnionC 就会以**自己的源**提供
/// 这段 HTML——即存储型 XSS。而 CSRF cookie 出于双提交模式的要求刻意不是 HttpOnly 的，
/// 注入脚本能直接读到它并发起已认证的状态变更请求。非生产环境还允许关闭 `verify_tls`，
/// 此时中间人也能构造该响应。
///
/// 因此走白名单：不在名单内就降级成 `application/octet-stream`，配合全局的
/// `X-Content-Type-Options: nosniff`，浏览器不会把它当作可执行文档渲染。
pub(crate) async fn cover_get(
    State(state): State<AppState>,
    Path((id, index)): Path<(String, u32)>,
) -> Result<Response, AppError> {
    validate_index(index)?;
    let host = find_host(&state, &id).await?;
    let (content_type, bytes) = client::cover_get(&host, index).await?;
    let mut resp = bytes.into_response(); // Vec<u8> 转为 HTTP 响应（自动设置 Content-Length）
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, safe_cover_content_type(&content_type));
    // 封面是纯资源，明确禁止作为文档打开。
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    Ok(resp)
}

/// 把上游 Content-Type 收敛到图片白名单。
fn safe_cover_content_type(upstream: &str) -> HeaderValue {
    // 去掉 `; charset=...` 之类的参数再比对，避免用参数绕过白名单。
    let essence = upstream
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    ALLOWED_COVER_TYPES
        .into_iter()
        .find(|allowed| *allowed == essence.as_str())
        .map(HeaderValue::from_static)
        .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream"))
}

/// 上传游戏封面图片（通过 URL，让 Sunshine 服务器端下载）。
pub(crate) async fn cover_upload(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(p): Json<SunshineCoverUploadRequest>,
) -> AppResult<Json<Value>> {
    let (key, url) = validate_cover_upload(&p.key, &p.url)?;
    let detail = format!("key={key}");
    let host = find_host(&state, &id).await?;
    let response = finish_sunshine_upstream_mutation(
        &state,
        "sunshine.cover.upload",
        id,
        Some(detail),
        async move { client::cover_upload(&host, &key, &url).await },
    )
    .await?;
    Ok(Json(response))
}

fn validate_index(index: u32) -> AppResult<()> {
    if index > 10_000 {
        return Err(AppError::BadRequest(
            "Sunshine app index is out of range".to_string(),
        ));
    }
    Ok(())
}
