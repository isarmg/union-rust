use chrono::{DateTime, Utc};

use crate::monitoring::{AgentReport, Capability, HostIdentity, MetricSummary};

#[derive(Debug)]
pub struct StoredHost {
    /// Server-owned operator remark; the Agent wire protocol carries no device name.
    pub name: String,
    pub identity: HostIdentity,
    pub capabilities: Vec<Capability>,
    pub registered_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub latest_collected_at: Option<DateTime<Utc>>,
    pub latest_interval_seconds: Option<f64>,
    /// Latest-report scalar summaries, read without decoding the JSON payload.
    pub metrics: MetricSummary,
    /// The complete report is loaded only by the host-detail query.
    pub latest: Option<AgentReport>,
}

#[derive(Debug)]
pub struct StoredHistoryPoint {
    pub report_id: String,
    pub collected_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub metrics: MetricSummary,
}
