//! 共享上游 HTTP 客户端。
//!
//! `reqwest::Client` 内部维护连接池，应该长期复用。严格 TLS 与显式关闭验证的
//! 主机使用不同客户端，避免安全策略在请求之间混淆。Sunshine API 使用固定端点，
//! 所以上游重定向一律作为响应返回，不能自动转发到未校验的新目标。

use std::{sync::LazyLock, time::Duration};

use crate::error::{AppError, AppResult};

struct Clients {
    strict: reqwest::Client,
    insecure: reqwest::Client,
}

fn build_client(accept_invalid_certs: bool) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(accept_invalid_certs)
        // An administered Sunshine host can still be compromised. Following
        // its Location header would let it make this server contact an
        // arbitrary second target, and 307/308 can replay mutation bodies.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| error.to_string())
}

static CLIENTS: LazyLock<Result<Clients, String>> = LazyLock::new(|| {
    Ok(Clients {
        strict: build_client(false)?,
        insecure: build_client(true)?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_and_insecure_clients_both_disable_redirects() {
        let strict = for_tls(true).expect("build strict upstream HTTP client");
        let insecure = for_tls(false).expect("build insecure upstream HTTP client");
        assert!(
            !std::ptr::eq(strict, insecure),
            "the two TLS modes must not share one client"
        );

        for client in [strict, insecure] {
            let configuration = format!("{client:?}");

            // reqwest includes every non-default redirect policy in Client's
            // debug configuration. This verifies the real builder without
            // binding a port, which is forbidden in some CI sandboxes.
            assert!(
                configuration.contains("Policy(None)"),
                "upstream client unexpectedly permits redirects: {configuration}"
            );
        }
    }
}
