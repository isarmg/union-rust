//! 与业务无关的基础设施。
//!
//! 这里的模块不认识"主机""Sunshine"这些概念，只提供能力：连数据库、加解密、
//! 解析数据目录、发 HTTP 请求。功能模块依赖它们，反之不成立。

pub mod database;
pub mod http_client;
pub mod network;
pub mod paths;
pub mod secrets;
