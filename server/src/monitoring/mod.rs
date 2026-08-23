//! 主机监控与实例生命周期。
//!
//! Agent 注册并周期上报硬件快照，控制台查询遥测并管理 Server 备注、凭据和数据生命周期。
//! **不存在任何向 Agent 下发命令的路径**。

pub mod http;
pub mod model;
pub mod store;

pub use model::*;
