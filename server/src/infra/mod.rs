//! 与业务无关的基础设施。
//!
//! 这里的模块不认识具体业务概念，只提供平台能力：连接数据库、加解密和解析数据目录。

pub mod database;
pub mod paths;
pub mod secrets;
