//! UnionC 控制台后端。
//!
//! # 模块组织
//!
//! Core 只包含平台级能力；具体业务通过发行内模块的 Manifest、私有进程和网关边界
//! 接入。Core 内部按平台功能组织，避免认证、运行时、HTTP 与基础设施职责互相渗透。
//!
//! | 目录 | 内容 |
//! |---|---|
//! | `platform`   | 发行内模块目录、私有 worker supervisor 与 Manifest 驱动网关 |
//! | `auth`       | 管理员认证、会话、改密 |
//! | `system`     | Core 健康、审计查询与平台事件流 HTTP 接口 |
//! | `http`       | 路由装配与全局中间件（鉴权、CSRF、安全响应头） |
//! | `infra`      | 与业务无关的基础设施：Core SQLite、密钥与路径 |
//! | `config`     | 运行配置模型与环境覆盖 |
//!
//! 平台功能目录内部统一用同一组文件名，便于定位：
//!
//! - `model.rs`  —— 请求/响应类型与领域校验
//! - `http.rs`   —— HTTP handler 与路由装配
//! - `store.rs`  —— 持久化
//!
//! 跨功能共享的东西只有三样，放在 crate 根：`state`（共享状态）、`error`（统一错误）、
//! `startup`（启动编排）。

// Keep every module visible to rust-analyzer while making an unsupported Core build fail at the
// crate boundary. Remote Agents have a wider platform matrix, but they live in host-monitoring.
#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
compile_error!("unionc Core supports only Linux amd64 (x86_64) and arm64 (aarch64)");

pub mod auth;
pub mod config;
pub mod error;
pub mod http;
pub mod infra;
pub mod platform;
pub mod startup;
pub mod state;
pub mod system;
