use std::collections::BTreeSet;

use serde_json::Value;

fn route<'a>(manifest: &'a Value, id: &str) -> &'a Value {
    manifest["backend"]["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|route| route["id"] == id)
        .unwrap_or_else(|| panic!("missing manifest route {id}"))
}

#[test]
fn browser_activation_uses_platform_rbac_and_the_dynamic_module_route() {
    let manifest: Value = serde_json::from_str(include_str!("../manifest.json")).unwrap();
    let activation = route(&manifest, "agent-activate-admin");
    assert_eq!(activation["path"], "/agent/v2/activate-admin");
    assert_eq!(activation["upstream_path"], "/api/agent/v2/activate-admin");
    assert_eq!(activation["methods"], serde_json::json!(["POST"]));
    assert_eq!(activation["auth"], "platform");
    assert_eq!(activation["permission"], "host-monitoring.agents.write");

    assert!(
        manifest["frontend"]["components"]
            .as_array()
            .unwrap()
            .iter()
            .any(|component| component == "HostActivationView")
    );
    let frontend_route = manifest["frontend"]["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|route| route["component"] == "HostActivationView")
        .unwrap();
    assert_eq!(
        frontend_route["path"],
        "/modules/host-monitoring/activate/:requestId"
    );
    assert_eq!(frontend_route["permission"], "host-monitoring.agents.write");
}

#[test]
fn only_agent_device_capabilities_keep_module_authentication() {
    let manifest: Value = serde_json::from_str(include_str!("../manifest.json")).unwrap();
    let actual = manifest["backend"]["routes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|route| route["auth"] == "module")
        .map(|route| route["id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual,
        BTreeSet::from([
            "agent-report",
            "agent-activate",
            "pairing-create",
            "pairing-read",
            "pairing-status",
        ])
    );
}
