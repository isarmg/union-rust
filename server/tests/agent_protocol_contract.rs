//! Cross-crate contract: an Agent report is the Server's report type, not a look-alike DTO.

use std::any::TypeId;

use unionc::monitoring::AgentReportExt;
use unionc_agent::{
    AGENT_REPORT_SCHEMA_VERSION, AgentHealth, AgentReport, Capability, CpuSnapshot, HostIdentity,
    MemorySnapshot, NetworkSnapshot, SystemSnapshot,
};
use uuid::Uuid;

fn agent_report() -> AgentReport {
    AgentReport {
        schema_version: AGENT_REPORT_SCHEMA_VERSION,
        report_id: Uuid::new_v4().to_string(),
        collected_at: chrono::Utc::now(),
        host: HostIdentity {
            id: Uuid::new_v4().to_string(),
            name: "agent-contract-host".into(),
            os: "linux".into(),
            os_version: Some("test".into()),
            kernel_version: None,
            arch: "x86_64".into(),
            agent_version: env!("CARGO_PKG_VERSION").into(),
        },
        interval_seconds: 0.5,
        system: SystemSnapshot {
            uptime_seconds: 60,
            cpu: CpuSnapshot {
                usage_percent: 25.0,
                logical_count: 4,
                physical_count: Some(2),
                per_core_percent: vec![10.0, 20.0, 30.0, 40.0],
            },
            memory: MemorySnapshot {
                total_bytes: 1_000,
                used_bytes: 400,
                available_bytes: 600,
                swap_total_bytes: 0,
                swap_used_bytes: 0,
            },
            networks: vec![NetworkSnapshot {
                name: "eth0".into(),
                received_bytes_total: 1_000,
                transmitted_bytes_total: 2_000,
                received_bytes_per_second: 2.5,
                transmitted_bytes_per_second: 5.0,
                packets_received_total: 10,
                packets_transmitted_total: 20,
                receive_errors_total: 0,
                transmit_errors_total: 0,
            }],
            disks: vec![],
            temperatures: vec![],
            gpus: vec![],
        },
        capabilities: vec![Capability::available("system.cpu", "sysinfo")],
        agent: AgentHealth {
            spool_pending_batches: 0,
            collector_errors: 0,
        },
    }
}

#[test]
fn agent_json_is_read_by_the_server_without_a_translation_layer() {
    assert_eq!(
        TypeId::of::<unionc_agent::AgentReport>(),
        TypeId::of::<unionc::monitoring::AgentReport>(),
        "both crates must re-export the same protocol type"
    );

    let emitted = agent_report();
    let bytes = serde_json::to_vec(&emitted).expect("Agent report must serialize");
    let received: unionc::monitoring::AgentReport =
        serde_json::from_slice(&bytes).expect("Server report type must deserialize Agent JSON");

    assert_eq!(received, emitted);
    received
        .validate()
        .expect("Agent-produced shared DTO must satisfy Server validation");
}
