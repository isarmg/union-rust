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
fn manifest_routes_match_the_worker_http_contract() {
    let manifest: Value = serde_json::from_str(include_str!("../manifest.json")).unwrap();

    let create = route(&manifest, "hosts-create");
    assert_eq!(create["path"], "/hosts");
    assert_eq!(create["upstream_path"], "/api/services/sunshine/hosts");
    assert_eq!(create["methods"], serde_json::json!(["POST"]));

    let write = route(&manifest, "hosts-write");
    assert_eq!(write["path"], "/hosts/{*path}");
    assert_eq!(
        write["upstream_path"],
        "/api/services/sunshine/hosts/{*path}"
    );
    assert_eq!(
        write["methods"],
        serde_json::json!(["POST", "PATCH", "DELETE"])
    );

    // PUT is not implemented by the worker. PATCH is used by host updates and POST by actions.
    assert!(
        !manifest["backend"]["routes"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|route| route["methods"].as_array().unwrap())
            .any(|method| method == "PUT")
    );
}
