use std::{fs, process::Command};

use unionc_agent::AgentConfig;
use uuid::Uuid;

struct Fixture {
    root: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    config_path: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "unionc-agent-pairing-transport-policy-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let state_dir = root.join("state");
        let config_path = root.join("config.json");
        let mut config = AgentConfig::default();
        config.endpoint = "http://192.0.2.10:1/api/modules/host-monitoring/agent/v1/report".into();
        config.pairing_endpoint = Some(
            "https://192.0.2.10:1/api/modules/host-monitoring/agent/v2/pairing-requests".into(),
        );
        config.request_timeout_seconds = 1;
        config.state_dir = state_dir.clone();
        config.allow_insecure_http = false;
        fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        Self {
            root,
            state_dir,
            config_path,
        }
    }

    fn pair(&self, cli_override: bool, environment_override: bool) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_unionc-agent"));
        command.args(["pair", "--config", self.config_path.to_str().unwrap()]);
        if cli_override {
            command.arg("--allow-insecure-http");
        }
        command.env_remove("UNIONC_AGENT_ALLOW_INSECURE_HTTP");
        if environment_override {
            command.env("UNIONC_AGENT_ALLOW_INSECURE_HTTP", "true");
        }
        command.output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_temporary_override_is_rejected(cli_override: bool, environment_override: bool) {
    let fixture = Fixture::new();
    let output = fixture.pair(cli_override, environment_override);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requires allow_insecure_http=true in the existing persistent config"),
        "unexpected pairing error: {stderr}"
    );
    assert!(
        !fixture.state_dir.exists(),
        "pairing persisted state before rejecting its temporary transport policy"
    );
}

#[test]
fn cli_override_cannot_authorize_a_durable_remote_http_binding() {
    assert_temporary_override_is_rejected(true, false);
}

#[test]
fn environment_override_cannot_authorize_a_durable_remote_http_binding() {
    assert_temporary_override_is_rejected(false, true);
}
