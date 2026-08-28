use axum::{
    Json, Router,
    body::Body,
    extract::{OriginalUri, Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use sarmg_platform_gateway::{PRINCIPAL_HEADER, insert_principal};
use serde_json::json;

use super::{AuthenticatedPrincipal, PluginBackend};
use crate::state::AppState;

const PROTOCOL_VERSION: &str = "gateway-v1";
const PROTOCOL_HEADER: &str = "x-union-module-protocol";
const AUDIENCE_HEADER: &str = "x-union-module-audience";
const TOKEN_HEADER: &str = "x-union-module-token";
const PREFIX_HEADER: &str = "x-forwarded-prefix";
const MAX_FRONTEND_ASSET_BYTES: u64 = 32 * 1024 * 1024;

pub(crate) fn module_api_router() -> Router<AppState> {
    Router::new()
        .route("/api/modules/{module}", any(api_root))
        .route("/api/modules/{module}/{*path}", any(api_path))
}

pub(crate) fn module_asset_router() -> Router<AppState> {
    Router::new().route("/modules/{module}/assets/{*path}", any(asset))
}

async fn api_root(
    State(state): State<AppState>,
    principal: Option<axum::Extension<AuthenticatedPrincipal>>,
    Path(module): Path<String>,
    OriginalUri(uri): OriginalUri,
    request: Request,
) -> Response {
    proxy(
        state,
        principal.map(|value| value.0),
        module,
        "/",
        uri,
        request,
    )
    .await
}

async fn api_path(
    State(state): State<AppState>,
    principal: Option<axum::Extension<AuthenticatedPrincipal>>,
    Path((module, path)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    request: Request,
) -> Response {
    proxy(
        state,
        principal.map(|value| value.0),
        module,
        &format!("/{path}"),
        uri,
        request,
    )
    .await
}

async fn proxy(
    state: AppState,
    principal: Option<AuthenticatedPrincipal>,
    module: String,
    suffix: &str,
    original_uri: axum::http::Uri,
    mut request: Request,
) -> Response {
    let client = match crate::auth::http::require_reverse_proxy_contract(
        &state,
        request.headers(),
        "plugin gateway",
    ) {
        Ok(client) => client,
        Err(error) => return error.into_response(),
    };
    let arrived_over_https = request
        .headers()
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        == Some("https");
    let Some(backend) = state.platform.backend(&module).await else {
        return gateway_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "plugin_unavailable",
            "module is disabled, unhealthy or absent from this distribution",
        );
    };
    let Some(route) = backend.route_for(request.method(), suffix) else {
        return gateway_error(
            StatusCode::NOT_FOUND,
            "plugin_route_not_registered",
            "the plugin manifest did not register this method and path",
        );
    };
    let route_auth = route.auth;
    let route_id = route.id.to_owned();
    let permission = route.permission.map(str::to_owned);
    let request_max_bytes = route.request_body.max_bytes;
    let upstream_path = route.upstream_path;
    if route_auth == sarmg_platform_core::RouteAuth::Platform {
        let Some(principal) = principal.as_ref() else {
            return gateway_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "a platform session is required for this plugin route",
            );
        };
        let Some(permission) = permission.as_deref() else {
            return gateway_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "plugin_route_invalid",
                "platform-authenticated route has no permission",
            );
        };
        if !state
            .platform
            .permissions
            .allows(&principal.username, permission)
            .await
        {
            return gateway_error(
                StatusCode::FORBIDDEN,
                "plugin_permission_denied",
                "the authenticated principal lacks the route permission",
            );
        }
    }

    let query = original_uri
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let rewritten = match format!("{upstream_path}{query}").parse() {
        Ok(uri) => uri,
        Err(_) => {
            return gateway_error(
                StatusCode::BAD_REQUEST,
                "plugin_path_invalid",
                "the plugin path could not be represented as an upstream URI",
            );
        }
    };
    *request.uri_mut() = rewritten;
    let mut headers = sanitize_request_headers(std::mem::take(request.headers_mut()));
    set_trusted_header(&mut headers, PROTOCOL_HEADER, PROTOCOL_VERSION);
    set_trusted_header(&mut headers, AUDIENCE_HEADER, &module);
    set_trusted_header(
        &mut headers,
        PREFIX_HEADER,
        &format!("/api/modules/{module}"),
    );
    if let Some(principal) = principal.as_ref()
        && insert_principal(&mut headers, &principal.username).is_err()
    {
        return gateway_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "platform_principal_invalid",
            "the authenticated platform principal cannot be forwarded",
        );
    }
    set_trusted_header(
        &mut headers,
        "x-forwarded-proto",
        if state.settings.production || arrived_over_https {
            "https"
        } else {
            "http"
        },
    );
    if let Some(client) = client {
        set_trusted_header(&mut headers, "x-forwarded-for", &client.to_string());
    }
    *request.headers_mut() = headers;

    if backend.endpoint().is_none() {
        let actor = if let Some(principal) = principal.as_ref() {
            let correlation_id = request
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            Some(super::sdk_bridge::actor(
                principal.username.clone(),
                state
                    .platform
                    .permissions
                    .permissions_for(&principal.username)
                    .await,
                correlation_id,
            ))
        } else {
            None
        };
        let mut response = backend
            .call(request, route_id, request_max_bytes, actor)
            .await;
        sanitize_response_headers(response.headers_mut(), &module);
        return response;
    }
    if backend.protocol() != sarmg_platform_core::ServiceProtocol::Http {
        return gateway_error(
            StatusCode::NOT_IMPLEMENTED,
            "plugin_protocol_not_http",
            "gRPC services are available to platform adapters but cannot be proxied to browsers",
        );
    }
    proxy_http(state, backend, &module, &upstream_path, query, request).await
}

async fn proxy_http(
    state: AppState,
    backend: PluginBackend,
    module: &str,
    suffix: &str,
    query: String,
    request: Request,
) -> Response {
    let endpoint = backend
        .endpoint()
        .expect("external backend has an endpoint");
    let mut upstream = match endpoint.base_url.join(suffix.trim_start_matches('/')) {
        Ok(url) => url,
        Err(_) => {
            return gateway_error(
                StatusCode::BAD_GATEWAY,
                "plugin_endpoint_invalid",
                "the trusted service endpoint could not resolve the registered route",
            );
        }
    };
    upstream.set_query(query.strip_prefix('?'));
    let (parts, body) = request.into_parts();
    let mut headers = parts.headers;
    for (name, value) in &endpoint.headers {
        set_trusted_header_dynamic(&mut headers, name, value);
        if name.eq_ignore_ascii_case("x-union-plugin-token") {
            set_trusted_header(&mut headers, TOKEN_HEADER, value);
        }
    }
    let response = state
        .platform
        .gateway_client()
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
                "plugin_connection_failed",
                "the registered plugin service refused the connection",
            );
        }
        Err(_) => {
            return gateway_error(
                StatusCode::BAD_GATEWAY,
                "plugin_request_failed",
                "the registered plugin service request failed",
            );
        }
    };
    let status = response.status();
    let mut headers = response.headers().clone();
    sanitize_response_headers(&mut headers, module);
    let mut proxied = Response::new(Body::from_stream(response.bytes_stream()));
    *proxied.status_mut() = status;
    *proxied.headers_mut() = headers;
    proxied
}

async fn asset(
    State(state): State<AppState>,
    Path((module, path)): Path<(String, String)>,
) -> Response {
    let Some(path) = (match state.platform.asset(&module, &path).await {
        Ok(path) => path,
        Err(_) => {
            return gateway_error(
                StatusCode::NOT_FOUND,
                "plugin_asset_not_found",
                "plugin asset path is invalid or unavailable",
            );
        }
    }) else {
        return gateway_error(
            StatusCode::NOT_FOUND,
            "plugin_asset_not_found",
            "plugin asset is unavailable",
        );
    };
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_FRONTEND_ASSET_BYTES => {
            metadata
        }
        _ => {
            return gateway_error(
                StatusCode::NOT_FOUND,
                "plugin_asset_not_found",
                "plugin asset is unavailable",
            );
        }
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) if bytes.len() as u64 == metadata.len() => bytes,
        _ => {
            return gateway_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "plugin_asset_changed",
                "plugin asset changed while it was being read",
            );
        }
    };
    let content_type = match path.extension().and_then(|extension| extension.to_str()) {
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-cache"),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        bytes,
    )
        .into_response()
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
        PRINCIPAL_HEADER,
        "x-union-plugin-token",
        "x-unionc-proxy-secret",
        "x-csrf-token",
    ] {
        headers.remove(name);
    }
    filter_union_cookies(&mut headers);
    headers
}

fn sanitize_response_headers(headers: &mut HeaderMap, module: &str) {
    remove_hop_by_hop_headers(headers);
    for name in [
        PROTOCOL_HEADER,
        AUDIENCE_HEADER,
        TOKEN_HEADER,
        PREFIX_HEADER,
        PRINCIPAL_HEADER,
        "x-union-plugin-token",
        header::CONTENT_SECURITY_POLICY.as_str(),
        header::X_FRAME_OPTIONS.as_str(),
        header::X_CONTENT_TYPE_OPTIONS.as_str(),
        header::REFERRER_POLICY.as_str(),
        header::STRICT_TRANSPORT_SECURITY.as_str(),
        "cross-origin-resource-policy",
        "cross-origin-opener-policy",
        "cross-origin-embedder-policy",
        "content-security-policy-report-only",
        "permissions-policy",
        "origin-agent-cluster",
        "x-permitted-cross-domain-policies",
        "access-control-allow-origin",
        "access-control-allow-credentials",
        "access-control-allow-headers",
        "access-control-allow-methods",
        "access-control-expose-headers",
        "access-control-max-age",
        "clear-site-data",
    ] {
        headers.remove(name);
    }
    constrain_module_location(headers, module);
    filter_module_response_cookies(headers, module);
}

fn constrain_module_location(headers: &mut HeaderMap, module: &str) {
    let Some(value) = headers
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
    else {
        headers.remove(header::LOCATION);
        return;
    };
    let base = match url::Url::parse(&format!("https://union.invalid/api/modules/{module}/")) {
        Ok(base) => base,
        Err(_) => {
            headers.remove(header::LOCATION);
            return;
        }
    };
    let Ok(resolved) = base.join(value) else {
        headers.remove(header::LOCATION);
        return;
    };
    let prefix = format!("/api/modules/{module}");
    if resolved.scheme() != "https"
        || resolved.host_str() != Some("union.invalid")
        || !(resolved.path() == prefix || resolved.path().starts_with(&format!("{prefix}/")))
    {
        headers.remove(header::LOCATION);
        return;
    }
    let mut scoped = resolved.path().to_owned();
    if let Some(query) = resolved.query() {
        scoped.push('?');
        scoped.push_str(query);
    }
    if let Some(fragment) = resolved.fragment() {
        scoped.push('#');
        scoped.push_str(fragment);
    }
    match HeaderValue::from_str(&scoped) {
        Ok(value) => {
            headers.insert(header::LOCATION, value);
        }
        Err(_) => {
            headers.remove(header::LOCATION);
        }
    }
}

/// Modules share Union's public origin, so they may only set a cookie whose name is namespaced to
/// the module and whose scope cannot reach Core or another module. Clear-Site-Data is stripped
/// above because it cannot be scoped to a module path.
fn filter_module_response_cookies(headers: &mut HeaderMap, module: &str) {
    let retained = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter(|value| module_cookie_is_scoped(value, module))
        .cloned()
        .collect::<Vec<_>>();
    headers.remove(header::SET_COOKIE);
    for value in retained {
        headers.append(header::SET_COOKIE, value);
    }
}

fn module_cookie_is_scoped(value: &HeaderValue, module: &str) -> bool {
    let Ok(value) = value.to_str() else {
        return false;
    };
    if value.len() > 4096 {
        return false;
    }
    let mut parts = value.split(';').map(str::trim);
    let Some((name, _)) = parts.next().and_then(|pair| pair.split_once('=')) else {
        return false;
    };
    if !name.starts_with(&format!("__Secure-{module}-")) && !name.starts_with(&format!("{module}-"))
    {
        return false;
    }

    let mut secure = false;
    let mut http_only = false;
    let mut same_site_strict = false;
    let mut scoped_path = false;
    let mut same_site_seen = false;
    let mut path_seen = false;
    for attribute in parts {
        let (name, value) = attribute
            .split_once('=')
            .map_or((attribute, None), |(name, value)| (name, Some(value)));
        if name.eq_ignore_ascii_case("domain") {
            return false;
        }
        if name.eq_ignore_ascii_case("secure") && value.is_none() {
            secure = true;
        } else if name.eq_ignore_ascii_case("httponly") && value.is_none() {
            http_only = true;
        } else if name.eq_ignore_ascii_case("samesite") {
            if same_site_seen {
                return false;
            }
            same_site_seen = true;
            same_site_strict = value.is_some_and(|value| value.eq_ignore_ascii_case("strict"));
        } else if name.eq_ignore_ascii_case("path") {
            if path_seen {
                return false;
            }
            path_seen = true;
            let expected = format!("/api/modules/{module}");
            scoped_path =
                value.is_some_and(|value| value == expected || value == format!("{expected}/"));
        }
    }
    secure && http_only && same_site_strict && scoped_path
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

fn set_trusted_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    let Ok(value) = HeaderValue::from_str(value) else {
        return;
    };
    headers.insert(HeaderName::from_static(name), value);
}

fn set_trusted_header_dynamic(headers: &mut HeaderMap, name: &str, value: &str) {
    let (Ok(name), Ok(value)) = (
        HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) else {
        return;
    };
    headers.insert(name, value);
}

fn gateway_error(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (status, Json(json!({ "code": code, "message": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_sanitization_removes_platform_identity_and_keeps_plugin_identity() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("session=union; module_session=plugin"),
        );
        headers.insert(TOKEN_HEADER, HeaderValue::from_static("attacker"));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer plugin"),
        );
        let sanitized = sanitize_request_headers(headers);
        assert_eq!(sanitized[header::COOKIE], "module_session=plugin");
        assert_eq!(sanitized[header::AUTHORIZATION], "Bearer plugin");
        assert!(!sanitized.contains_key(TOKEN_HEADER));
    }

    #[test]
    fn response_sanitization_removes_platform_headers_and_unsafe_cookies() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::SET_COOKIE,
            HeaderValue::from_static(
                "__Secure-dufs-session=safe; Path=/api/modules/dufs/; HttpOnly; Secure; SameSite=Strict",
            ),
        );
        headers.append(
            header::SET_COOKIE,
            HeaderValue::from_static("session=stolen; Path=/; HttpOnly; Secure; SameSite=Strict"),
        );
        headers.append(
            header::SET_COOKIE,
            HeaderValue::from_static("dufs-wide=unsafe; Path=/; HttpOnly; Secure; SameSite=Strict"),
        );
        for (name, value) in [
            ("clear-site-data", "\"cookies\""),
            ("content-security-policy", "default-src *"),
            ("x-frame-options", "ALLOWALL"),
            ("x-content-type-options", "off"),
            ("referrer-policy", "unsafe-url"),
            ("cross-origin-resource-policy", "cross-origin"),
            ("cross-origin-opener-policy", "unsafe-none"),
            ("permissions-policy", "camera=*, microphone=*"),
            ("access-control-allow-origin", "*"),
            ("strict-transport-security", "max-age=0"),
        ] {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_static(value),
            );
        }
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));

        sanitize_response_headers(&mut headers, "dufs");

        let cookies = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(cookies.len(), 1);
        assert!(cookies[0].starts_with("__Secure-dufs-session="));
        assert_eq!(headers[header::CONTENT_TYPE], "text/html");
        for name in [
            "clear-site-data",
            "content-security-policy",
            "x-frame-options",
            "x-content-type-options",
            "referrer-policy",
            "cross-origin-resource-policy",
            "cross-origin-opener-policy",
            "permissions-policy",
            "access-control-allow-origin",
            "strict-transport-security",
        ] {
            assert!(!headers.contains_key(name), "{name} survived sanitization");
        }
    }

    #[test]
    fn module_redirects_are_resolved_only_inside_their_gateway_prefix() {
        for (location, expected) in [
            (
                "child?download=1",
                Some("/api/modules/dufs/child?download=1"),
            ),
            ("/api/modules/dufs/", Some("/api/modules/dufs/")),
            ("../photo-backup", None),
            ("/api/settings", None),
            ("https://attacker.invalid/", None),
            ("//attacker.invalid/", None),
            ("%2e%2e/settings", None),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::LOCATION, HeaderValue::from_str(location).unwrap());
            sanitize_response_headers(&mut headers, "dufs");
            assert_eq!(
                headers
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok()),
                expected,
                "location={location}"
            );
        }
    }
}
