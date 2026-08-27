//! 主机监控与实例生命周期。
//!
//! Agent 注册并周期上报硬件快照，控制台查询遥测、编辑名称并永久删除实例。
//! **不存在任何向 Agent 下发命令的路径**。

pub mod http;
pub mod model;
mod state;
pub mod store;

pub use state::{MonitoringState, REPORT_BUCKET_CAPACITY, REPORT_BUCKET_REFILL_PER_SECOND};

pub use model::*;
