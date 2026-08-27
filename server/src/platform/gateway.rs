#![cfg_attr(
    not(any(
        feature = "module-sentinel-monitor",
        feature = "module-photo-backup",
        feature = "module-dufs",
        feature = "module-sunshine",
        feature = "module-host-monitoring"
    )),
    allow(dead_code, unused_imports)
)]

use axum::{
    Json, Router,
    body::Body,
    extract::{OriginalUri, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::any,
};
use serde_json::json;

use super::ExternalService;
#[cfg(test)]
use super::spec::compiled_specs;
use crate::state::AppState;

pub(super) const PROTOCOL_VERSION: &str = "gateway-v1";
pub(super) const PROTOCOL_HEADER: &str = "x-union-module-protocol";
pub(super) const AUDIENCE_HEADER: &str = "x-union-module-audience";
pub(super) const TOKEN_HEADER: &str = "x-union-module-token";
pub(super) const PREFIX_HEADER: &str = "x-forwarded-prefix";

pub(crate) fn gateway_router() -> Router<AppState> {
    let router = Router::new();
    #[cfg(feature = "module-sentinel-monitor")]
    let router = router
        .route(
            "/modules/sentinel-monitor",
            any(|| async { Redirect::permanent("/modules/sentinel-monitor/") }),
        )
        .route("/modules/sentinel-monitor/{*path}", any(sentinel_monitor));
    #[cfg(feature = "module-photo-backup")]
    let router = router
        .route(
            "/modules/photo-backup",
            any(|| async { Redirect::permanent("/modules/photo-backup/") }),
        )
        .route("/modules/photo-backup/{*path}", any(photo_backup));
    #[cfg(feature = "module-dufs")]
    let router = router
        .route(
            "/modules/dufs",
            any(|| async { Redirect::permanent("/modules/dufs/") }),
        )
        .route("/modules/dufs/{*path}", any(dufs));
    router
}

/// Browser-console module APIs live at their historical paths so the web UI does not need a
/// migration flag. This router is deliberately mounted *inside* Union's session + CSRF layer.
pub(crate) fn console_gateway_router() -> Router<AppState> {
    let router = Router::new();
    #[cfg(feature = "module-sunshine")]
    let router = router
        .route("/api/services/sunshine", any(sunshine_console))
        .route("/api/services/sunshine/{*path}", any(sunshine_console));
    #[cfg(feature = "module-host-monitoring")]
    let router = router
        .route("/api/monitoring", any(host_monitoring_console))
        .route("/api/monitoring/{*path}", any(host_monitoring_console));
    router
}

/// Agent endpoints use their protocol credentials and must remain reachable before a browser
/// administrator session exists. Only these fixed paths bypass the console middleware.
pub(crate) fn public_worker_router() -> Router<AppState> {
    let router = Router::new();
    #[cfg(feature = "module-host-monitoring")]
    let router = router
        .route("/api/agent", any(host_monitoring_agent))
        .route("/api/agent/{*path}", any(host_monitoring_agent));
    router
}

#[cfg(feature = "module-sunshine")]
async fn sunshine_console(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    request: Request,
) -> Response {
    proxy(state, "sunshine", uri, request, ProxyPath::Original, None).await
}

#[cfg(feature = "module-host-monitoring")]
async fn host_monitoring_console(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    request: Request,
) -> Response {
    proxy(
        state,
        "host-monitoring",
        uri,
        request,
        ProxyPath::Original,
        None,
    )
    .await
}

#[cfg(feature = "module-host-monitoring")]
async fn host_monitoring_agent(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    request: Request,
) -> Response {
    proxy(
        state,
        "host-monitoring",
        uri,
        request,
        ProxyPath::Original,
        None,
    )
    .await
}

#[cfg(feature = "module-sentinel-monitor")]
async fn sentinel_monitor(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    request: Request,
) -> Response {
    proxy(
        state,
        "sentinel-monitor",
        uri,
        request,
        ProxyPath::StripCompiledPrefix,
        Some("/modules/sentinel-monitor"),
    )
    .await
}

#[cfg(feature = "module-photo-backup")]
async fn photo_backup(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    request: Request,
) -> Response {
    proxy(
        state,
        "photo-backup",
        uri,
        request,
        ProxyPath::StripCompiledPrefix,
        Some("/modules/photo-backup"),
    )
    .await
}

#[cfg(feature = "module-dufs")]
async fn dufs(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    request: Request,
) -> Response {
    proxy(
        state,
        "dufs",
        uri,
        request,
        ProxyPath::StripCompiledPrefix,
        Some("/modules/dufs"),
    )
    .await
}

#[derive(Debug, Clone, Copy)]
enum ProxyPath {
    #[cfg(any(feature = "module-sunshine", feature = "module-host-monitoring"))]
    Original,
    StripCompiledPrefix,
}

async fn proxy(
    state: AppState,
    module: &'static str,
    original_uri: axum::http::Uri,
    request: Request,
    path_mode: ProxyPath,
    public_prefix: Option<&'static str>,
) -> Response {
    let client = match crate::auth::http::require_reverse_proxy_contract(
        &state,
        request.headers(),
        "模块网关",
    ) {
        Ok(client) => client,
        Err(error) => return error.into_response(),
    };
    let Some(service) = state.platform.service_for_gateway(module).await else {
        return gateway_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "module_gateway_unavailable",
            "模块尚未通过 worker 存活、就绪和 gateway-v1 内部凭据门禁",
        );
    };
    let suffix = match path_mode {
        #[cfg(any(feature = "module-sunshine", feature = "module-host-monitoring"))]
        ProxyPath::Original => original_uri.path(),
        ProxyPath::StripCompiledPrefix => {
            let Some(suffix) = original_uri
                .path()
                .strip_prefix(service.spec.gateway_prefix)
            else {
                return gateway_error(
                    StatusCode::NOT_FOUND,
                    "module_gateway_path_invalid",
                    "请求不属于已编译模块的固定网关前缀",
                );
            };
            suffix
        }
    };
    if !suffix.starts_with('/') {
        return gateway_error(
            StatusCode::NOT_FOUND,
            "module_gateway_path_invalid",
            "模块网关路径必须位于固定前缀之下",
        );
    }
    let mut upstream = format!("http://{}{}", service.spec.bind, suffix);
    if let Some(query) = original_uri.query() {
        upstream.push('?');
        upstream.push_str(query);
    }

    let (parts, body) = request.into_parts();
    let arrived_over_https = parts
        .headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        == Some("https");
    let mut headers = sanitize_request_headers(parts.headers);
    set_trusted_header(&mut headers, PROTOCOL_HEADER, PROTOCOL_VERSION);
    set_trusted_header(&mut headers, AUDIENCE_HEADER, service.spec.id);
    set_trusted_header(
        &mut headers,
        TOKEN_HEADER,
        service.credential.token.as_ref(),
    );
    set_trusted_header(&mut headers, PREFIX_HEADER, service.spec.gateway_prefix);
    let forwarded_proto = if state.settings.production || arrived_over_https {
        "https"
    } else {
        "http"
    };
    set_trusted_header(&mut headers, "x-forwarded-proto", forwarded_proto);
    if let Some(client) = client {
        set_trusted_header(&mut headers, "x-forwarded-for", &client.to_string());
    }

    let response = state
        .platform
        .gateway_client
        .request(parts.method, upstream)
        .headers(headers)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()))
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) if error.is_connect() => {
            return gateway_error(
                StatusCode::BAD_GATEWAY,
                "module_worker_connection_failed",
                "私有 worker 连接失败；supervisor 将重新探测或重启",
            );
        }
        Err(_) => {
            return gateway_error(
                StatusCode::BAD_GATEWAY,
                "module_worker_request_failed",
                "私有 worker 请求失败",
            );
        }
    };

    proxy_response(response, &service, public_prefix)
}

fn proxy_response(
    response: reqwest::Response,
    service: &ExternalService,
    public_prefix: Option<&str>,
) -> Response {
    let status = response.status();
    let mut headers = response.headers().clone();
    sanitize_response_headers(&mut headers);
    if let Some(prefix) = public_prefix {
        debug_assert_eq!(prefix, service.spec.gateway_prefix);
        rewrite_location(&mut headers, prefix);
        rewrite_set_cookie_paths(&mut headers, prefix);
    }
    let body = Body::from_stream(response.bytes_stream());
    let mut proxied = Response::new(body);
    *proxied.status_mut() = status;
    *proxied.headers_mut() = headers;
    proxied
}

fn sanitize_request_headers(mut headers: HeaderMap) -> HeaderMap {
    remove_hop_by_hop_headers(&mut headers);
    for name in [
        header::HOST.as_str(),
        header::PROXY_AUTHORIZATION.as_str(),
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-port",
        "x-forwarded-proto",
        PREFIX_HEADER,
        PROTOCOL_HEADER,
        AUDIENCE_HEADER,
        TOKEN_HEADER,
        "x-unionc-proxy-secret",
        "x-csrf-token",
    ] {
        headers.remove(name);
    }
    filter_union_cookies(&mut headers);
    headers
}

fn sanitize_response_headers(headers: &mut HeaderMap) {
    remove_hop_by_hop_headers(headers);
    for name in [
        PROTOCOL_HEADER,
        AUDIENCE_HEADER,
        TOKEN_HEADER,
        PREFIX_HEADER,
    ] {
        headers.remove(name);
    }
}

fn remove_hop_by_hop_headers(headers: &mut HeaderMap) {
    let connection_tokens = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|value| HeaderName::from_bytes(value.trim().as_bytes()).ok())
        .collect::<Vec<_>>();
    for name in connection_tokens {
        headers.remove(name);
    }
    for name in [
        header::CONNECTION.as_str(),
        "proxy-connection",
        "keep-alive",
        header::TRANSFER_ENCODING.as_str(),
        header::UPGRADE.as_str(),
        header::TRAILER.as_str(),
        header::TE.as_str(),
    ] {
        headers.remove(name);
    }
}

fn filter_union_cookies(headers: &mut HeaderMap) {
    let cookies = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .map(str::trim)
        .filter(|cookie| !cookie.is_empty())
        .filter(|cookie| {
            let name = cookie
                .split_once('=')
                .map(|(name, _)| name.trim())
                .unwrap_or_default();
            !matches!(name, "session" | "__Host-session" | "csrf" | "__Host-csrf")
        })
        .collect::<Vec<_>>()
        .join("; ");
    headers.remove(header::COOKIE);
    if !cookies.is_empty()
        && let Ok(value) = HeaderValue::from_str(&cookies)
    {
        headers.insert(header::COOKIE, value);
    }
}

fn rewrite_location(headers: &mut HeaderMap, prefix: &str) {
    let Some(location) = headers
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    if location.starts_with('/') && !location.starts_with(prefix) {
        let rewritten = format!("{prefix}{location}");
        if let Ok(value) = HeaderValue::from_str(&rewritten) {
            headers.insert(header::LOCATION, value);
        }
    }
}

fn rewrite_set_cookie_paths(headers: &mut HeaderMap, prefix: &str) {
    let cookies = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|cookie| rewrite_cookie_path(cookie, prefix))
        .collect::<Vec<_>>();
    if cookies.is_empty() {
        return;
    }
    headers.remove(header::SET_COOKIE);
    for cookie in cookies {
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            headers.append(header::SET_COOKIE, value);
        }
    }
}

fn rewrite_cookie_path(cookie: &str, prefix: &str) -> String {
    let name = cookie.split('=').next().unwrap_or_default().trim();
    // The __Host- contract requires Path=/ exactly. These module cookies have distinct names and
    // the gateway filters Union's own __Host-session before forwarding to any worker.
    if name.starts_with("__Host-") {
        return cookie.to_string();
    }
    cookie
        .split(';')
        .map(|attribute| {
            let trimmed = attribute.trim();
            let Some(path) = trimmed
                .strip_prefix("Path=")
                .or_else(|| trimmed.strip_prefix("path="))
            else {
                return trimmed.to_string();
            };
            if path.starts_with(prefix) {
                trimmed.to_string()
            } else if path.starts_with('/') {
                format!("Path={prefix}{path}")
            } else {
                trimmed.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn set_trusted_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    let value = HeaderValue::from_str(value).expect("compiled gateway values are header-safe");
    headers.insert(HeaderName::from_static(name), value);
}

fn gateway_error(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (status, Json(json!({ "code": code, "message": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_sanitization_removes_union_identity_but_keeps_module_identity() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static(
                "session=union; csrf=union-csrf; monitor_session=worker; __Host-dufs-session=dufs",
            ),
        );
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer module-user-token"),
        );
        headers.insert(TOKEN_HEADER, HeaderValue::from_static("attacker"));
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.8"));
        let sanitized = sanitize_request_headers(headers);
        assert_eq!(
            sanitized[header::COOKIE],
            "monitor_session=worker; __Host-dufs-session=dufs"
        );
        assert_eq!(sanitized[header::AUTHORIZATION], "Bearer module-user-token");
        assert!(!sanitized.contains_key(TOKEN_HEADER));
        assert!(!sanitized.contains_key("x-forwarded-for"));
    }

    #[test]
    fn connection_named_headers_are_not_forwarded() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, x-private-hop"),
        );
        headers.insert("x-private-hop", HeaderValue::from_static("secret"));
        headers.insert("x-end-to-end", HeaderValue::from_static("kept"));
        let sanitized = sanitize_request_headers(headers);
        assert!(!sanitized.contains_key(header::CONNECTION));
        assert!(!sanitized.contains_key("x-private-hop"));
        assert_eq!(sanitized["x-end-to-end"], "kept");
    }

    #[test]
    fn cookies_and_redirects_are_scoped_to_the_compiled_prefix() {
        assert_eq!(
            rewrite_cookie_path(
                "photo_backup_admin=value; Path=/admin; HttpOnly",
                "/modules/photo-backup"
            ),
            "photo_backup_admin=value; Path=/modules/photo-backup/admin; HttpOnly"
        );
        assert_eq!(
            rewrite_cookie_path("__Host-dufs-session=value; Path=/; Secure", "/modules/dufs"),
            "__Host-dufs-session=value; Path=/; Secure"
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::LOCATION, HeaderValue::from_static("/admin"));
        rewrite_location(&mut headers, "/modules/photo-backup");
        assert_eq!(headers[header::LOCATION], "/modules/photo-backup/admin");
    }

    #[test]
    fn compiled_gateway_specs_match_protocol_prefixes() {
        for spec in compiled_specs() {
            assert!(spec.gateway_prefix.ends_with(spec.id));
        }
    }
}
