//! 全局安全响应头。
//!
//! # 为什么 API 也需要这些头
//!
//! UnionC 本身只返回 JSON，看起来"没有 HTML 就没有 XSS"。但它同时是一个**代理**：
//! `/api/modules/<id>/*` 会把模块进程的响应字节流式转发出来。
//! 只要上游能影响响应的类型或内容，浏览器就有可能把它当作文档渲染——而且是在 UnionC
//! 自己的源上执行，能读到刻意非 HttpOnly 的 CSRF cookie。
//!
//! 因此这里对**所有**响应统一施加一组保守的头：即使将来新增了别的转发端点、或者某处
//! 忘了收敛 Content-Type，浏览器侧仍有一道兜底。
//!
//! | 头 | 作用 |
//! |---|---|
//! | `X-Content-Type-Options: nosniff` | 禁止 MIME 嗅探，`application/octet-stream` 不会被猜成 HTML |
//! | `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'` | 即便真渲染了 HTML，也加载不了任何资源、执行不了脚本 |
//! | `X-Frame-Options` | Shell/API 使用 `DENY`；固定网关内的模块 HTML 由 Core 统一使用 `SAMEORIGIN` |
//! | `Referrer-Policy: no-referrer` | 防止 SSE ticket 之类的 URL 参数经 Referer 外泄 |
//! | `Cross-Origin-Resource-Policy: same-origin` | 阻止其他站点直接嵌入本站资源 |
//! | `Cross-Origin-Opener-Policy: same-origin` | 隔离跨源顶层浏览上下文 |
//! | `Permissions-Policy` | 默认关闭模块不需要的高权限浏览器能力 |
//! | `Strict-Transport-Security` | 仅生产环境；本服务强制经 HTTPS 反代访问 |

use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, header},
    middleware::Next,
    response::Response,
};

use crate::state::AppState;

/// 与传输方式无关的固定头，所有环境一致。
const BASELINE: [(HeaderName, HeaderValue); 5] = [
    (
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    ),
    (
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    ),
    (
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    ),
    (
        HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin"),
    ),
    (
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=(), usb=()"),
    ),
];

const STRICT_CSP: HeaderValue = HeaderValue::from_static(
    "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'",
);
const SHELL_CSP: HeaderValue = HeaderValue::from_static(
    "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
     img-src 'self' data:; font-src 'self'; frame-src 'self'; frame-ancestors 'none'; \
     base-uri 'none'; object-src 'none'",
);
const MODULE_DOCUMENT_CSP: HeaderValue = HeaderValue::from_static(
    "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
     img-src 'self' data:; font-src 'self'; form-action 'self'; frame-ancestors 'self'; \
     base-uri 'none'; object-src 'none'",
);

/// 一年，含子域并允许预加载。仅在生产环境下发——开发环境走明文 HTTP，
/// 发了反而会把开发者的浏览器锁死在 https 上。
const HSTS: HeaderValue = HeaderValue::from_static("max-age=31536000; includeSubDomains; preload");

pub(super) async fn apply(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let shell_document = is_shell_document_path(request.uri().path());
    let module_document = request.uri().path().starts_with("/api/modules/");
    let mut response = next.run(request).await;
    enforce(
        response.headers_mut(),
        shell_document,
        module_document,
        state.settings.production,
    );
    response
}

fn enforce(headers: &mut HeaderMap, shell_document: bool, module_document: bool, production: bool) {
    for (name, value) in BASELINE {
        headers.insert(name, value);
    }
    let is_html = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    let module_html = module_document && is_html;
    headers.insert(
        header::X_FRAME_OPTIONS,
        if module_html {
            HeaderValue::from_static("SAMEORIGIN")
        } else {
            HeaderValue::from_static("DENY")
        },
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        if shell_document && is_html {
            SHELL_CSP
        } else if module_html {
            MODULE_DOCUMENT_CSP
        } else {
            STRICT_CSP
        },
    );
    if production {
        headers.insert(header::STRICT_TRANSPORT_SECURITY, HSTS);
    } else {
        headers.remove(header::STRICT_TRANSPORT_SECURITY);
    }
}

fn is_shell_document_path(path: &str) -> bool {
    matches!(path, "/" | "/overview" | "/settings")
        || (path.starts_with("/modules/") && !path.contains("/assets/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_documents_and_module_assets_are_distinguished() {
        assert!(is_shell_document_path("/"));
        assert!(is_shell_document_path("/modules/dufs/browse"));
        assert!(!is_shell_document_path(
            "/modules/dufs/assets/frontend/entry.js"
        ));
        assert!(!is_shell_document_path("/api/modules/dufs/files"));
    }

    #[test]
    fn core_overwrites_handler_security_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src *"),
        );
        headers.insert(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("ALLOWALL"),
        );
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("off"),
        );
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=0"),
        );
        headers.insert(
            HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("camera=*, microphone=*"),
        );

        enforce(&mut headers, false, true, false);

        assert_eq!(headers[header::X_FRAME_OPTIONS], "SAMEORIGIN");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(
            headers[HeaderName::from_static("permissions-policy")],
            "camera=(), microphone=(), geolocation=(), payment=(), usb=()"
        );
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            MODULE_DOCUMENT_CSP
        );
        let policy = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(policy.contains("script-src 'self'"));
        assert!(!policy.contains("'sha256-"));
        assert!(!policy.contains("'unsafe-inline'"));
        assert!(!headers.contains_key(header::STRICT_TRANSPORT_SECURITY));
    }

    #[test]
    fn production_hsts_and_shell_policy_are_forced() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
        enforce(&mut headers, true, false, true);
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(headers[header::CONTENT_SECURITY_POLICY], SHELL_CSP);
        assert_eq!(headers[header::STRICT_TRANSPORT_SECURITY], HSTS);
    }
}
