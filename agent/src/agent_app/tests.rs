use super::*;

#[tokio::test]
async fn tray_pairing_cancel_wakes_without_terminating_the_process() {
    let (controller, signal) = unionc_agent::service::shutdown_channel();
    controller.request_shutdown();
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        wait_for_pairing_abort(Some(&signal), None),
    )
    .await
    .expect("graceful pairing cancellation should wake")
    .unwrap();
    assert_eq!(outcome, PairingWait::Cancelled);
}

#[tokio::test]
async fn tray_pairing_deadline_is_observed_at_network_select_boundaries() {
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        wait_for_pairing_abort(None, Some(Instant::now())),
    )
    .await
    .expect("expired pairing deadline should wake")
    .unwrap();
    assert_eq!(outcome, PairingWait::Deadline);
}

#[test]
fn pairing_activation_loads_the_server_assigned_identity() {
    let directory = std::env::temp_dir().join(format!("unionc-active-host-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let instance_id = Uuid::new_v4();
    fs::write(directory.join("host-id"), instance_id.to_string()).unwrap();
    let stale_id = Uuid::new_v4();
    let mut config = AgentConfig::default();
    config.state_dir = directory.clone();
    config.config_path = Some(directory.join("config.json"));
    config.host_name = Some("paired-name".into());
    let mut host = unionc_agent::HostIdentity {
        id: stale_id.to_string(),
        name: "stale".into(),
        os: "test".into(),
        os_version: None,
        kernel_version: None,
        arch: "test".into(),
        agent_version: "test".into(),
    };

    let generation = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    fs::write(directory.join("agent-token"), "paired-token").unwrap();
    fs::write(directory.join("host-id"), instance_id.to_string()).unwrap();
    fs::write(
        directory.join("auth-state.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "status": "authorized",
            "reason": "current pairing completed",
            "changed_at": chrono::Utc::now()
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        directory.join("pairing-state.json"),
        serde_json::to_vec(&serde_json::json!({
            "phase": "active",
            "version": env!("CARGO_PKG_VERSION"),
            "generation": generation,
            "request_id": request_id,
            "activation_url": "https://unionc.example/agent/activate/test",
            "instance_id": instance_id,
            "report_endpoint": "https://unionc.example/api/agent/v1/report",
            "host_name": "paired-name",
            "completed_at": chrono::Utc::now()
        }))
        .unwrap(),
    )
    .unwrap();

    let _reporter = pairing::activate_reporter_snapshot(
        &mut config,
        &mut host,
        generation,
        request_id,
        instance_id,
        "https://unionc.example/api/agent/v1/report",
    )
    .unwrap();

    assert_eq!(host.id, instance_id.to_string());
    assert_eq!(host.name, "paired-name");
    assert_eq!(
        config.endpoint,
        "https://unionc.example/api/agent/v1/report"
    );
    fs::remove_dir_all(directory).unwrap();
}
