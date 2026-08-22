/// Report-storage failures whose semantics must remain visible to HTTP callers.
#[derive(Debug, thiserror::Error)]
pub enum StoreReportError {
    #[error("report_id already belongs to another host")]
    ReportIdBelongsToAnotherHost,
    #[error("monitoring host is not active")]
    HostNotActive,
    #[error("monitoring credential is no longer active")]
    CredentialNotActive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitoringTokenAuthentication {
    Active(String),
    Revoked,
    Unknown,
}
