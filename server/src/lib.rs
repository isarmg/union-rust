//! UnionC 控制台后端。
//!
//! # 模块组织
//!
//! 代码按**功能**而非技术分层组织：一个功能就是一个目录。按技术分层切分会让
//! Sunshine 这样一个功能散落在路由、领域、客户端、工具等多处，改一个行为要在
//! 几个目录之间来回跳。
//!
//! | 目录 | 内容 |
//! |---|---|
//! | `platform`   | 编译期模块目录、私有 worker supervisor 与固定反向网关 |
//! | `auth`       | 管理员认证、会话、改密 |
//! | `system`     | 本机资源采样与健康探针 |
//! | `http`       | 路由装配与全局中间件（鉴权、CSRF、安全响应头） |
//! | `infra`      | 与业务无关的基础设施：数据库、密钥、路径、HTTP 客户端 |
//! | `config`     | 运行配置模型与环境覆盖 |
//!
//! 每个功能目录内部统一用同一组文件名，便于定位：
//!
//! - `model.rs`  —— 请求/响应类型与领域校验
//! - `http.rs`   —— HTTP handler（Sunshine 因体量大而拆成 `http/` 子目录）
//! - `store.rs`  —— 持久化
//!
//! 跨功能共享的东西只有三样，放在 crate 根：`state`（共享状态）、`error`（统一错误）、
//! `startup`（启动编排）。

#[cfg(not(target_os = "linux"))]
compile_error!("unionc server supports Linux only");

pub mod auth;
pub mod config;
pub mod error;
pub mod http;
pub mod infra;
pub mod platform;
pub mod startup;
pub mod state;
pub mod system;
