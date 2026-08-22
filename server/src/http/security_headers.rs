//! 全局安全响应头。
//!
//! # 为什么 API 也需要这些头
//!
//! UnionC 本身只返回 JSON，看起来"没有 HTML 就没有 XSS"。但它同时是一个**代理**：
//! `/api/services/sunshine/hosts/{id}/covers/{index}` 会把上游主机的字节原样转发出来。
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
//! | `X-Frame-Options: DENY` | 老浏览器的 clickjacking 兜底 |
//! | `Referrer-Policy: no-referrer` | 防止 SSE ticket 之类的 URL 参数经 Referer 外泄 |
//! | `Cross-Origin-Resource-Policy: same-origin` | 阻止其他站点直接嵌入本站资源 |
//! | `Strict-Transport-Security` | 仅生产环境；本服务强制经 HTTPS 反代访问 |

use axum::{
    extract::{Request, State},
    http::{HeaderName, HeaderValue, header},
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
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; base-uri 'none'"),
    ),
    (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
    (
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    ),
    (
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    ),
];

/// 一年，含子域并允许预加载。仅在生产环境下发——开发环境走明文 HTTP，
/// 发了反而会把开发者的浏览器锁死在 https 上。
const HSTS: HeaderValue = HeaderValue::from_static("max-age=31536000; includeSubDomains; preload");

pub(super) async fn apply(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in BASELINE {
        // 用 entry 语义：handler 若已明确设置过（例如某个头需要更宽松的策略），不覆盖它。
        if !headers.contains_key(&name) {
            headers.insert(name, value);
        }
    }
    if state.settings.production {
        headers.insert(header::STRICT_TRANSPORT_SECURITY, HSTS);
    }
    response
}
