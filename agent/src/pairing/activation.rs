use std::net::IpAddr;

use anyhow::{Context, bail};
use uuid::Uuid;

pub(super) fn activation_endpoint(pairing_endpoint: &str) -> anyhow::Result<reqwest::Url> {
    crate::config::validate_pairing_endpoint(pairing_endpoint)
        .context("stored pairing endpoint is unsafe")?;
    let mut endpoint = reqwest::Url::parse(pairing_endpoint)
        .context("stored pairing endpoint is not a valid URL")?;
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        bail!("stored pairing endpoint unexpectedly contains credentials");
    }
    let path = endpoint.path().trim_end_matches('/');
    let base = path
        .strip_suffix("/pairing-requests")
        .context("stored pairing endpoint does not end in /pairing-requests")?;
    endpoint.set_path(&format!("{base}/activate"));
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    Ok(endpoint)
}

pub(super) fn validate_activation_url_request(
    activation_url: &str,
    pairing_endpoint: &str,
    request_id: Uuid,
) -> anyhow::Result<()> {
    let url =
        reqwest::Url::parse(activation_url).context("stored pairing activation URL is invalid")?;
    let pairing =
        reqwest::Url::parse(pairing_endpoint).context("stored pairing endpoint is invalid")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.scheme() != pairing.scheme()
        || url.host_str().map(str::to_ascii_lowercase)
            != pairing.host_str().map(str::to_ascii_lowercase)
        || url.port_or_known_default() != pairing.port_or_known_default()
        || url.path() != format!("/agent/activate/{request_id}")
    {
        bail!("stored activation URL does not match the pending pairing request");
    }
    Ok(())
}

pub(super) fn resolve_activation_url(
    pairing_endpoint: &str,
    activation_url: &str,
) -> anyhow::Result<String> {
    let base = reqwest::Url::parse(pairing_endpoint).context("invalid pairing endpoint URL")?;
    let url = match reqwest::Url::parse(activation_url) {
        Ok(url) => url,
        Err(_) => base
            .join(activation_url)
            .context("invalid activation URL returned by UnionC")?,
    };
    if !url.username().is_empty() || url.password().is_some() {
        bail!("UnionC returned an activation URL containing credentials");
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(url.host_str()) => {}
        "http" => bail!("UnionC returned an insecure non-loopback activation URL"),
        scheme => bail!("UnionC returned an unsupported activation URL scheme: {scheme}"),
    }
    Ok(url.to_string())
}

fn is_loopback_host(host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}
