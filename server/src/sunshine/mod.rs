//! Sunshine 串流主机管理。
//!
//! 服务端作为反向代理转发到各主机的 Web API，凭据加密保存在服务端，不下发给浏览器。

pub mod client;
pub mod http;
pub mod model;
pub mod status;

pub use model::*;
