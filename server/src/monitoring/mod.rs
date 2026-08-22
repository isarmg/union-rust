//! 只读主机监控。
//!
//! Agent 注册并周期上报硬件快照，控制台只读查询。**不存在任何下发命令的路径**。

pub mod http;
pub mod model;
pub mod store;

pub use model::*;
