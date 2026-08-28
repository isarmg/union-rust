//! Pairing and report acknowledgement wire types.
//!
//! These DTOs intentionally contain no trust-boundary validation beyond strict JSON shape and
//! canonical UUID decoding. The Server still owns policy checks such as hash format, supported
//! Agent version, activation-code limits, and pairing state transitions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{HostIdentity, report::deserialize_canonical_uuid};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPairingRequest {
    pub host: HostIdentity,
    pub token_hash: String,
    pub polling_secret_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPairingResponse {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub request_id: String,
    pub activation_url: String,
    pub expires_in: u64,
    pub poll_interval: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPairingStatusResponse {
    pub status: PairingStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_canonical_uuid"
    )]
    pub instance_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingStatus {
    Waiting,
    Active,
    Denied,
    Expired,
}

impl PairingStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Active => "active",
            Self::Denied => "denied",
            Self::Expired => "expired",
        }
    }
}

impl TryFrom<&str> for PairingStatus {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "waiting" => Ok(Self::Waiting),
            "active" => Ok(Self::Active),
            "denied" => Ok(Self::Denied),
            "expired" => Ok(Self::Expired),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivateAgentRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub request_id: String,
    pub activation_code: String,
}

/// Borrowed serialization view used by Agents so the one-time activation code
/// is not copied into an additional heap allocation before transmission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ActivateAgentRequestRef<'a> {
    pub request_id: &'a str,
    pub activation_code: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivateAgentResponse {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub instance_id: String,
    pub status: ActivatePairingStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivatePairingStatus {
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentReportAck {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub host_id: String,
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub report_id: String,
    pub accepted: bool,
    pub received_at: DateTime<Utc>,
}

fn deserialize_optional_canonical_uuid<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(|value| {
            let deserializer = serde::de::value::StringDeserializer::<D::Error>::new(value);
            deserialize_canonical_uuid(deserializer)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;

    fn host() -> HostIdentity {
        HostIdentity {
            id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
            os: "linux".into(),
            os_version: None,
            kernel_version: None,
            arch: "x86_64".into(),
            agent_version: "0.3.6".into(),
        }
    }

    #[test]
    fn pairing_request_round_trips_without_loss() {
        let request = AgentPairingRequest {
            host: host(),
            token_hash: "a".repeat(64),
            polling_secret_hash: "b".repeat(64),
        };
        let encoded = serde_json::to_vec(&request).unwrap();
        assert_eq!(
            serde_json::from_slice::<AgentPairingRequest>(&encoded).unwrap(),
            request
        );
    }

    #[test]
    fn pairing_responses_reject_unknown_statuses_and_noncanonical_uuids() {
        for value in [
            serde_json::json!({
                "status": "future",
                "instance_id": null
            }),
            serde_json::json!({
                "status": "active",
                "instance_id": "BBBBBBBB-BBBB-4BBB-8BBB-BBBBBBBBBBBB"
            }),
        ] {
            assert!(serde_json::from_value::<AgentPairingStatusResponse>(value).is_err());
        }
    }

    #[test]
    fn report_acknowledgement_rejects_unknown_fields() {
        let value = serde_json::json!({
            "host_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "report_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "accepted": true,
            "received_at": "2026-01-01T00:00:00Z",
            "legacy_status": "ok"
        });
        assert!(serde_json::from_value::<AgentReportAck>(value).is_err());
    }

    fn assert_rejects_server_control_fields<T: DeserializeOwned>(value: serde_json::Value) {
        for field in ["command", "configuration", "script"] {
            let mut candidate = value.clone();
            candidate
                .as_object_mut()
                .expect("response fixture must be an object")
                .insert(field.into(), serde_json::json!("forbidden"));
            assert!(
                serde_json::from_value::<T>(candidate).is_err(),
                "server-to-Agent response unexpectedly accepted {field}"
            );
        }
    }

    #[test]
    fn server_to_agent_contract_has_no_control_payload() {
        assert_rejects_server_control_fields::<AgentPairingResponse>(serde_json::json!({
            "request_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "activation_url": "/modules/host-monitoring/activate/bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "expires_in": 900,
            "poll_interval": 2
        }));
        assert_rejects_server_control_fields::<AgentPairingStatusResponse>(serde_json::json!({
            "status": "waiting"
        }));
        assert_rejects_server_control_fields::<ActivateAgentResponse>(serde_json::json!({
            "instance_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "status": "active"
        }));
        assert_rejects_server_control_fields::<AgentReportAck>(serde_json::json!({
            "host_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "report_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "accepted": true,
            "received_at": "2026-01-01T00:00:00Z"
        }));
    }

    #[test]
    fn borrowed_activation_request_matches_owned_wire_shape() {
        let request_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        let activation_code = "uci_example";
        let owned = ActivateAgentRequest {
            request_id: request_id.into(),
            activation_code: activation_code.into(),
        };
        let borrowed = ActivateAgentRequestRef {
            request_id,
            activation_code,
        };

        assert_eq!(
            serde_json::to_value(owned).unwrap(),
            serde_json::to_value(borrowed).unwrap()
        );
    }
}
