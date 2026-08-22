//! UnionC 的跨平台只读遥测 Agent。
//!
//! Agent 不监听端口、不执行服务端命令，也不包含自更新器。所有平台差异都通过
//! capability 明确表达；缺失数据使用 `None`，不会用 0 冒充。

mod atomic_file;
pub mod collectors;
pub mod config;
pub mod model;
#[cfg(feature = "otlp")]
pub mod otlp;
pub mod pairing;
mod private_fs;
mod report_contract;
pub mod service;
pub mod spool;
mod state_lock;
pub mod transport;
pub mod tray_support;

pub use collectors::SystemSampler;
pub use config::{AgentCommand, AgentConfig, OutputMode};
pub use model::*;
