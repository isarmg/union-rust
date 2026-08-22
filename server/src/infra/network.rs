//! 主机地址规范化与 URL authority 格式化。

use std::net::IpAddr;

pub fn normalize_host(value: &str) -> String {
    value.trim().trim_matches(['[', ']']).to_string()
}

pub fn is_valid_host(value: &str) -> bool {
    let host = normalize_host(value);
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    let name = host.trim_end_matches('.');
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

pub fn url_host(value: &str) -> String {
    let host = normalize_host(value);
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host
    }
}

pub fn authority(host: &str, port: u16) -> String {
    format!("{}:{port}", url_host(host))
}
