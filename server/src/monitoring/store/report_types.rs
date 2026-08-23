/// Report-storage failures whose semantics must remain visible to HTTP callers.
#[derive(Debug, thiserror::Error)]
pub enum StoreReportError {
    #[error("report_id already belongs to another host")]
    ReportIdBelongsToAnotherHost,
    #[error("monitoring host no longer exists")]
    HostNotFound,
    #[error("monitoring credential no longer exists")]
    CredentialNotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitoringTokenAuthentication {
    Active(String),
    Unknown,
}
