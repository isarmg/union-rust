//! 共享上游 HTTP 客户端。
//!
//! `reqwest::Client` 内部维护连接池，应该长期复用。严格 TLS 与显式关闭验证的
//! 主机使用不同客户端，避免安全策略在请求之间混淆。

use std::{sync::LazyLock, time::Duration};

use crate::error::{AppError, AppResult};

struct Clients {
    strict: reqwest::Client,
    insecure: reqwest::Client,
}

static CLIENTS: LazyLock<Result<Clients, String>> = LazyLock::new(|| {
    let build = |accept_invalid_certs| {
        reqwest::Client::builder()
            .danger_accept_invalid_certs(accept_invalid_certs)
            .timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .map_err(|error| error.to_string())
    };
    Ok(Clients {
        strict: build(false)?,
        insecure: build(true)?,
    })
});

pub fn for_tls(verify_tls: bool) -> AppResult<&'static reqwest::Client> {
    let clients = CLIENTS
        .as_ref()
        .map_err(|error| AppError::Upstream(format!("无法创建 HTTP 客户端: {error}")))?;
    Ok(if verify_tls {
        &clients.strict
    } else {
        &clients.insecure
    })
}
