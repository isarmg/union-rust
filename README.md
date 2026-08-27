# UnionC

Union 是自托管管理平台和唯一发行单元。主机监控、Sunshine、Sentinel Monitor、Photo Backup
与 Dufs 按发行 profile 在构建期选择；目标运行模型是 Union 公共 gateway 加私有独立模块
进程。当前 Sunshine/主机仍在核心进程内；Sentinel、Photo 与 Dufs 已能由 Cargo feature
选择 catalog 并统一组装，但静态网关与 supervisor 尚未完成，不能把目标文档误读为已完成
实现。模块需求、边界和迁移状态见 [`docs/modules.md`](docs/modules.md)。

跨平台 Agent 采集 CPU / 内存 / 磁盘 / 网络 / 温度 / GPU 指标，上报到中心服务端；
管理台提供实时状态、历史曲线与主机生命周期管理。

当前 Windows x64 MSI 还安装一个普通用户权限的通知区托盘伴侣，可打开本机浏览器
配置、配对、查看或启停服务。本地页一次填写 Server 地址和管理台生成的
一次性授权密钥即可配对；用户选择“退出”时，托盘会先通过 UAC 停止后台采集服务。

**服务端不向被监控主机下发任何命令、配置或脚本**——这是设计约束，不是待实现的功能。
代码中不存在远程执行、进程控制、文件传输或 Agent 自更新端点。

## 组成

| 目录 | 内容 | 平台 |
|---|---|---|
| `server` | 服务端（Rust + axum + 内嵌 SQLite） | 仅 Linux |
| `agent` | 采集 Agent（Rust） | Linux / Windows / macOS |
| `protocol` | Server 与 Agent 共用的线上 DTO（Rust library） | 跨平台 |
| `web` | 管理台前端（React + TypeScript） | 浏览器 |

远端 Agent 与 Photo Android/iOS 客户端必须物理上单独安装，但只随 Union Release 版本化；
模块仓库不再提供独立 Release。组合构建由独立的
[`union-builder`](https://github.com/isarmg/union-builder) 执行。
当前可复现组合工具版本为
[`v0.2.0`](https://github.com/isarmg/union-builder/releases/tag/v0.2.0)。

## 快速开始

```bash
# 服务端（Linux / WSL；入口固定工作目录和 .runtime/server）
./tools/dev-server.sh

# 前端
cd web && npm ci && npm run dev

# 查看某台机器能采到什么（不联网）
cargo run -p unionc-agent --bin unionc-agent -- probe
```

默认监听 `127.0.0.1:8081`，首次启动会打印开发管理员口令。

## 测试

```bash
cargo test --workspace
```

Server 测试各自创建临时 SQLite，不需要外部数据库。项目只支持当前版本新建的数据、
协议、配置与安装布局；旧 Server 数据库、旧 Agent 配置/身份及旧安装方式均不读取、
不升级，也不提供转换或导入桥。部署新版本前请导出仍需保留的数据，并全新部署、
创建新实例并配对 Agent。

## 文档

- **[文档中心](docs/README.md)**：项目参考、开发、部署、运维、安全和历史资料的唯一索引。
- **[零基础教学路线](beginner-guide/README.md)**：从第一次运行到能沿真实代码解释系统。
- **[更新日志](CHANGELOG.md)**：各版本具备的能力。

## 许可

[Apache License 2.0](LICENSE-APACHE)
