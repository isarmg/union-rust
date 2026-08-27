use std::time::{Duration, Instant};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

pub(super) const REPORT_AUTH_WINDOW: Duration = Duration::from_secs(60);
pub(super) const MAX_REPORT_AUTH_PER_IP: usize = 15_000;
pub(super) const MAX_REPORT_AUTH_GLOBAL: usize = 18_000;
pub(super) const PAIRING_WINDOW: Duration = Duration::from_secs(60);
pub(super) const MAX_PAIRING_PER_IP: usize = 120;
pub(super) const MAX_PAIRING_GLOBAL: usize = 6_000;

pub(super) async fn check_pairing_rate(
    state: &AppState,
    client: Option<std::net::IpAddr>,
) -> AppResult<()> {
    let now = Instant::now();
    let mut global = state.monitoring.pairing_attempts.lock().await;
    prune_pairing_attempts(&mut global, now);
    if global.len() >= MAX_PAIRING_GLOBAL {
        return Err(AppError::TooManyRequests(
            "global agent pairing rate limit exceeded".to_string(),
        ));
    }
    if let Some(address) = client {
        let mut by_ip = state.monitoring.pairing_attempts_by_ip.lock().await;
        if by_ip.len() >= MAX_PAIRING_GLOBAL * 2 {
            by_ip.retain(|_, attempts| {
                prune_pairing_attempts(attempts, now);
                !attempts.is_empty()
            });
        }
        let attempts = by_ip.entry(address).or_default();
        prune_pairing_attempts(attempts, now);
        if attempts.len() >= MAX_PAIRING_PER_IP {
            return Err(AppError::TooManyRequests(
                "agent pairing rate limit exceeded".to_string(),
            ));
        }
        attempts.push_back(now);
    }
    global.push_back(now);
    Ok(())
}

pub(super) fn prune_pairing_attempts(
    attempts: &mut std::collections::VecDeque<Instant>,
    now: Instant,
) {
    while attempts
        .front()
        .is_some_and(|attempt| now.duration_since(*attempt) >= PAIRING_WINDOW)
    {
        attempts.pop_front();
    }
}

/// Anonymous report-authentication quota, applied before the token lookup.
pub(super) async fn check_report_auth_rate(
    state: &AppState,
    client: Option<std::net::IpAddr>,
) -> AppResult<()> {
    let now = Instant::now();
    let mut global = state.monitoring.report_auth_attempts.lock().await;
    prune_report_auth_attempts(&mut global, now);
    if global.len() >= MAX_REPORT_AUTH_GLOBAL {
        return Err(AppError::TooManyRequests(
            "global agent report authentication rate limit exceeded".to_string(),
        ));
    }
    if let Some(address) = client {
        let mut by_ip = state.monitoring.report_auth_attempts_by_ip.lock().await;
        if by_ip.len() >= MAX_REPORT_AUTH_GLOBAL * 2 {
            by_ip.retain(|_, attempts| {
                prune_report_auth_attempts(attempts, now);
                !attempts.is_empty()
            });
        }
        let attempts = by_ip.entry(address).or_default();
        prune_report_auth_attempts(attempts, now);
        if attempts.len() >= MAX_REPORT_AUTH_PER_IP {
            return Err(AppError::TooManyRequests(
                "agent report authentication rate limit exceeded".to_string(),
            ));
        }
        attempts.push_back(now);
    }
    global.push_back(now);
    Ok(())
}

pub(super) fn prune_report_auth_attempts(
    attempts: &mut std::collections::VecDeque<Instant>,
    now: Instant,
) {
    while attempts
        .front()
        .is_some_and(|attempt| now.duration_since(*attempt) >= REPORT_AUTH_WINDOW)
    {
        attempts.pop_front();
    }
}

/// Authenticated report quota, keyed by host identity.
pub(super) async fn check_report_rate(state: &AppState, host_id: &str) -> AppResult<()> {
    if state.monitoring.allow_report(host_id, Instant::now()).await {
        return Ok(());
    }
    Err(AppError::TooManyRequests(
        "agent report rate limit exceeded for this host".to_string(),
    ))
}
