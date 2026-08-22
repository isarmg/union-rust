use std::path::PathBuf;

use crate::config::AgentConfig;

pub(super) fn apply_active_config(
    config: &mut AgentConfig,
    report_endpoint: &str,
    host_name: &Option<String>,
) {
    config.endpoint = report_endpoint.to_string();
    config.pairing_endpoint = None;
    config.host_name.clone_from(host_name);
}

pub(super) fn persist_active_config_unlocked(
    config: &AgentConfig,
    report_endpoint: &str,
    host_name: &Option<String>,
) -> anyhow::Result<PathBuf> {
    let mut active = config.clone();
    apply_active_config(&mut active, report_endpoint, host_name);
    active.persist_after_pairing()
}
