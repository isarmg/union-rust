use std::{collections::HashMap, sync::Arc};

use tokio::sync::{Mutex, RwLock};

use crate::config::SunshineHostConfig;

/// Runtime state owned exclusively by the Sunshine module.
#[derive(Clone)]
pub struct SunshineState {
    pub hosts: Arc<RwLock<Vec<SunshineHostConfig>>>,
    /// Latest background reachability/authentication snapshot for each host.
    pub health: Arc<RwLock<HashMap<String, SunshineHostHealth>>>,
    /// Wakes the single background probe after a configuration mutation.
    pub health_refresh: Arc<tokio::sync::Notify>,
    /// Serializes persistent host configuration changes.
    pub settings_lock: Arc<Mutex<()>>,
}

impl SunshineState {
    pub(crate) fn new(hosts: Vec<SunshineHostConfig>) -> Self {
        Self {
            hosts: Arc::new(RwLock::new(hosts)),
            health: Arc::new(RwLock::new(HashMap::new())),
            health_refresh: Arc::new(tokio::sync::Notify::new()),
            settings_lock: Arc::new(Mutex::new(())),
        }
    }
}

/// Health snapshot for one configured Sunshine host.
#[derive(Clone)]
pub struct SunshineHostHealth {
    pub reachable: Option<bool>,
    pub connected: Option<bool>,
    pub connection_error: Option<String>,
}

impl SunshineHostHealth {
    pub fn pending() -> Self {
        Self {
            reachable: None,
            connected: None,
            connection_error: Some("连接状态正在后台检测".to_string()),
        }
    }

    pub fn completed(reachable: bool, connection: &Result<(), String>) -> Self {
        Self {
            reachable: Some(reachable),
            connected: Some(reachable && connection.is_ok()),
            connection_error: connection.as_ref().err().cloned(),
        }
    }
}
