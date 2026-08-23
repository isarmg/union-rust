#[cfg(test)]
mod tests {
    use super::*;

    fn report_with_disk_name(name: &str) -> AgentReport {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "report_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "collected_at": "2026-01-01T00:00:00Z",
            "host": {
                "id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "os": "windows", "os_version": null,
                "kernel_version": null, "arch": "x86_64", "agent_version": "0.3.3"
            },
            "interval_seconds": 10.0,
            "system": {
                "uptime_seconds": 1,
                "cpu": {"usage_percent": 5.0, "logical_count": 1, "physical_count": null, "per_core_percent": [5.0]},
                "memory": {"total_bytes": 100, "used_bytes": 50, "available_bytes": 50, "swap_total_bytes": 0, "swap_used_bytes": 0},
                "networks": [],
                "disks": [
                    {"name":name,"mount_point":"F:\\\\","file_system":"NTFS","total_bytes":1,"available_bytes":1,"read_bytes_total":1,"written_bytes_total":1,"read_bytes_per_second":0.0,"written_bytes_per_second":0.0,"is_read_only":false}
                ],
                "temperatures": [], "gpus": []
            },
            "capabilities": [],
            "agent": {"spool_pending_batches": 0, "collector_errors": 0}
        }))
        .expect("valid report fixture")
    }

    #[test]
    fn windows_volume_without_a_label_is_a_valid_disk() {
        report_with_disk_name("").validate().unwrap();

        let error = report_with_disk_name("bad\nname").validate().unwrap_err();
        assert!(error.to_string().contains("disk.name"));
        assert!(error.to_string().contains("control characters"));
    }

    #[test]
    fn summary_uses_largest_device_rate_instead_of_double_counting() {
        let report: AgentReport = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "report_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "collected_at": "2026-01-01T00:00:00Z",
            "host": {
                "id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "os": "linux", "os_version": null,
                "kernel_version": null, "arch": "x86_64", "agent_version": "0.3.3"
            },
            "interval_seconds": 10.0,
            "system": {
                "uptime_seconds": 1,
                "cpu": {"usage_percent": 5.0, "logical_count": 1, "physical_count": null, "per_core_percent": [5.0]},
                "memory": {"total_bytes": 100, "used_bytes": 50, "available_bytes": 50, "swap_total_bytes": 0, "swap_used_bytes": 0},
                "networks": [
                    {"name":"eth0","received_bytes_total":1,"transmitted_bytes_total":1,"received_bytes_per_second":100.0,"transmitted_bytes_per_second":40.0,"packets_received_total":1,"packets_transmitted_total":1,"receive_errors_total":0,"transmit_errors_total":0},
                    {"name":"bridge0","received_bytes_total":1,"transmitted_bytes_total":1,"received_bytes_per_second":80.0,"transmitted_bytes_per_second":70.0,"packets_received_total":1,"packets_transmitted_total":1,"receive_errors_total":0,"transmit_errors_total":0}
                ],
                "disks": [
                    {"name":"sda","mount_point":"/","file_system":"ext4","total_bytes":1,"available_bytes":1,"read_bytes_total":1,"written_bytes_total":1,"read_bytes_per_second":30.0,"written_bytes_per_second":60.0,"is_read_only":false},
                    {"name":"bind","mount_point":"/bind","file_system":"ext4","total_bytes":1,"available_bytes":1,"read_bytes_total":1,"written_bytes_total":1,"read_bytes_per_second":20.0,"written_bytes_per_second":50.0,"is_read_only":false}
                ],
                "temperatures": [], "gpus": []
            },
            "capabilities": [],
            "agent": {"spool_pending_batches": 0, "collector_errors": 0}
        }))
        .expect("valid report");

        let summary = report.metric_summary();
        assert_eq!(summary.network_received_bytes_per_second, Some(100.0));
        assert_eq!(summary.network_transmitted_bytes_per_second, Some(70.0));
        assert_eq!(summary.disk_read_bytes_per_second, Some(30.0));
        assert_eq!(summary.disk_written_bytes_per_second, Some(60.0));
    }

    #[test]
    fn management_requests_reject_removed_fields() {
        assert!(
            serde_json::from_value::<CreateAgentInstanceRequest>(serde_json::json!({
                "display_name": "agent",
                "enrollment_code": "removed"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<AgentPairingRequest>(serde_json::json!({
                "host": {
                    "id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                    "os": "linux",
                    "os_version": null,
                    "kernel_version": null,
                    "arch": "x86_64",
                    "agent_version": "0.3.3"
                },
                "token_hash": "a".repeat(64),
                "polling_secret_hash": "b".repeat(64),
                "legacy_token": "removed"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivateAgentRequest>(serde_json::json!({
                "request_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "activation_code": "uci_current",
                "host_id": "removed"
            }))
            .is_err()
        );
    }

    #[test]
    fn management_instance_id_requires_canonical_uuid() {
        for instance_id in [
            "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
            "aaaaaaaaaaaa4aaa8aaaaaaaaaaaaaaa",
        ] {
            let request = CreateAgentInstanceRequest {
                display_name: None,
                expires_in_minutes: None,
                instance_id: Some(instance_id.to_string()),
            };
            assert!(request.validated().is_err(), "accepted {instance_id}");
        }
    }
}
