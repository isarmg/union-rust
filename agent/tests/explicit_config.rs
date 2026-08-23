use std::{fs, path::Path, process::Command};

use uuid::Uuid;

struct Fixture {
    root: std::path::PathBuf,
    state_dir: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "unionc-agent-explicit-config-integration-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let state_dir = root.join("state");
        Self { root, state_dir }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_unionc-agent"));
        command
            .env("UNIONC_AGENT_STATE_DIR", &self.state_dir)
            .env_remove("UNIONC_AGENT_CONFIG");
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_pair_rejects_config_before_state_changes(config_path: &Path) {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args([
            "pair",
            "--server",
            "http://127.0.0.1:1",
            "--config",
            config_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to load config")
            && stderr.contains(&config_path.display().to_string()),
        "unexpected pairing error: {stderr}"
    );
    assert!(
        !fixture.state_dir.exists(),
        "pairing touched state before rejecting the explicit config"
    );
}

#[test]
fn pair_rejects_missing_and_directory_configs_before_state_changes() {
    let fixture = Fixture::new();
    let missing = fixture.root.join("missing.json");
    let directory = fixture.root.join("directory-config");
    fs::create_dir(&directory).unwrap();

    assert_pair_rejects_config_before_state_changes(&missing);
    assert_pair_rejects_config_before_state_changes(&directory);
}

#[test]
fn status_reports_a_missing_explicit_config_without_creating_state() {
    let fixture = Fixture::new();
    let missing = fixture.root.join("missing.json");
    let output = fixture
        .command()
        .args([
            "status",
            "--output",
            "json",
            "--config",
            missing.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshot["checks"]["configuration"]["status"], "error");
    assert_eq!(
        snapshot["checks"]["configuration"]["code"],
        "config_invalid"
    );
    assert!(
        snapshot["checks"]["configuration"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(&missing.display().to_string()))
    );
    assert!(!fixture.state_dir.exists());
}
