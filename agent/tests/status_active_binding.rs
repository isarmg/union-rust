use std::{collections::BTreeMap, fs, path::Path, process::Command};

use unionc_agent::AgentConfig;
use uuid::Uuid;

struct Fixture {
    root: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    config_path: std::path::PathBuf,
    report_endpoint: String,
}

impl Fixture {
    fn new(mismatched_binding: bool) -> Self {
        let root = std::env::temp_dir().join(format!("unionc-status-binding-{}", Uuid::new_v4()));
        let state_dir = root.join("state");
        let config_path = root.join("config.json");
        fs::create_dir_all(&state_dir).unwrap();
        let mut config = AgentConfig::default();
        config.endpoint = "https://old.example/api/modules/host-monitoring/agent/v1/report".into();
        config.state_dir = state_dir.clone();
        fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let report_endpoint =
            "https://new.example/api/modules/host-monitoring/agent/v1/report".to_string();
        fs::write(state_dir.join("host-id"), instance_id.to_string()).unwrap();
        fs::write(state_dir.join("agent-token"), "a".repeat(64)).unwrap();
        fs::write(
            state_dir.join("auth-state.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "status": "authorized",
                "reason": "browser pairing completed",
                "changed_at": chrono::Utc::now(),
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            state_dir.join("pairing-state.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "phase": "active",
                "version": env!("CARGO_PKG_VERSION"),
                "generation": generation,
                "request_id": request_id,
                "activation_url": "https://new.example/modules/host-monitoring/activate/test",
                "instance_id": instance_id,
                "report_endpoint": report_endpoint.clone(),
                "completed_at": chrono::Utc::now(),
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            state_dir.join("active-binding.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "generation": if mismatched_binding { Uuid::new_v4() } else { generation },
                "request_id": request_id,
                "instance_id": instance_id,
                "report_endpoint": report_endpoint.clone(),
            }))
            .unwrap(),
        )
        .unwrap();
        Self {
            root,
            state_dir,
            config_path,
            report_endpoint,
        }
    }

    fn status(&self) -> serde_json::Value {
        let output = Command::new(env!("CARGO_BIN_EXE_unionc-agent"))
            .args([
                "status",
                "--output",
                "json",
                "--config",
                self.config_path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn state_files(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn status_uses_the_active_binding_endpoint_without_mutating_state() {
    let fixture = Fixture::new(false);
    let before = state_files(&fixture.state_dir);

    let status = fixture.status();

    assert_eq!(status["status"], "configured");
    assert_eq!(
        status["endpoint"].as_str(),
        Some(fixture.report_endpoint.as_str())
    );
    assert_eq!(status["checks"]["active_binding"]["status"], "ok");
    assert_eq!(state_files(&fixture.state_dir), before);
}

#[test]
fn status_fails_closed_on_a_mismatched_binding_without_mutating_state() {
    let fixture = Fixture::new(true);
    let before = state_files(&fixture.state_dir);

    let status = fixture.status();

    assert_eq!(status["status"], "degraded");
    assert!(status["endpoint"].is_null());
    assert_eq!(status["checks"]["active_binding"]["status"], "error");
    assert_eq!(state_files(&fixture.state_dir), before);
}
