#![cfg_attr(
    not(any(
        feature = "module-sentinel-monitor",
        feature = "module-photo-backup",
        feature = "module-dufs",
        feature = "module-sunshine",
        feature = "module-host-monitoring"
    )),
    allow(dead_code)
)]

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkerKind {
    #[cfg(feature = "module-sunshine")]
    Sunshine,
    #[cfg(feature = "module-host-monitoring")]
    HostMonitoring,
    #[cfg(feature = "module-sentinel-monitor")]
    SentinelMonitor,
    #[cfg(feature = "module-photo-backup")]
    PhotoBackup,
    #[cfg(feature = "module-dufs")]
    Dufs,
}

/// The complete runtime identity of a worker is part of the Union binary.
///
/// Adding a future module is one table entry plus one compile-time feature. No URL, port or
/// executable path comes from a database or an administrator-controlled web request.
#[derive(Debug, Clone, Copy)]
pub(super) struct ModuleSpec {
    pub id: &'static str,
    pub bind: SocketAddr,
    pub gateway_prefix: &'static str,
    pub liveness_path: &'static str,
    pub readiness_path: Option<&'static str>,
    pub kind: WorkerKind,
}

const fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

pub(super) fn compiled_specs() -> Vec<ModuleSpec> {
    vec![
        #[cfg(feature = "module-sunshine")]
        ModuleSpec {
            id: "sunshine",
            bind: loopback(18104),
            gateway_prefix: "/modules/sunshine",
            liveness_path: "/health/live",
            readiness_path: Some("/health/ready"),
            kind: WorkerKind::Sunshine,
        },
        #[cfg(feature = "module-host-monitoring")]
        ModuleSpec {
            id: "host-monitoring",
            bind: loopback(18105),
            gateway_prefix: "/modules/host-monitoring",
            liveness_path: "/health/live",
            readiness_path: Some("/health/ready"),
            kind: WorkerKind::HostMonitoring,
        },
        #[cfg(feature = "module-sentinel-monitor")]
        ModuleSpec {
            id: "sentinel-monitor",
            bind: loopback(18101),
            gateway_prefix: "/modules/sentinel-monitor",
            liveness_path: "/health/live",
            readiness_path: Some("/health/ready"),
            kind: WorkerKind::SentinelMonitor,
        },
        #[cfg(feature = "module-photo-backup")]
        ModuleSpec {
            id: "photo-backup",
            bind: loopback(18102),
            gateway_prefix: "/modules/photo-backup",
            liveness_path: "/health/live",
            readiness_path: Some("/health/ready"),
            kind: WorkerKind::PhotoBackup,
        },
        #[cfg(feature = "module-dufs")]
        ModuleSpec {
            id: "dufs",
            bind: loopback(18103),
            gateway_prefix: "/modules/dufs",
            liveness_path: "/__dufs__/health",
            readiness_path: Some("/__dufs__/ready"),
            kind: WorkerKind::Dufs,
        },
    ]
}

#[derive(Clone)]
pub(super) struct InternalCredential {
    pub audience: &'static str,
    pub token: Arc<str>,
}

impl InternalCredential {
    pub fn new(audience: &'static str) -> Self {
        // `Uuid::new_v4` is backed by the operating-system random source. Two UUIDs retain well
        // over 200 bits of entropy after their version/variant bits and avoid persisting a worker
        // credential anywhere. Restarting Union rotates every audience independently.
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        Self {
            audience,
            token: Arc::from(token),
        }
    }
}

impl fmt::Debug for InternalCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InternalCredential")
            .field("audience", &self.audience)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_process_scoped_unique_and_redacted() {
        let first = InternalCredential::new("dufs");
        let second = InternalCredential::new("dufs");
        assert_eq!(first.audience, "dufs");
        assert_eq!(first.token.len(), 64);
        assert_ne!(first.token, second.token);
        assert!(!format!("{first:?}").contains(first.token.as_ref()));
    }

    #[test]
    fn bindings_and_prefixes_are_unique() {
        let specs = compiled_specs();
        for (index, left) in specs.iter().enumerate() {
            for right in specs.iter().skip(index + 1) {
                assert_ne!(left.id, right.id);
                assert_ne!(left.bind, right.bind);
                assert_ne!(left.gateway_prefix, right.gateway_prefix);
            }
        }
    }
}
