# UnionC

只读的多主机状态监控系统，附带一组 Sunshine 串流主机管理能力。

跨平台 Agent 采集 CPU / 内存 / 磁盘 / 网络 / 温度 / GPU 指标，上报到中心服务端；
管理台提供实时状态、历史曲线与主机生命周期管理。

当前 Windows x64 MSI 还安装一个普通用户权限的通知区托盘伴侣，可打开本机浏览器
配置、配对/重新配对、查看或启停服务。本地页一次填写 Server 地址和管理台生成的
一次性授权密钥即可配对；用户选择“退出”时，托盘会先通过 UAC 停止后台采集服务。

**服务端不向被监控主机下发任何命令、配置或脚本**——这是设计约束，不是待实现的功能。
代码中不存在远程执行、进程控制、文件传输或 Agent 自更新端点。

## 组成

| 目录 | 内容 | 平台 |
|---|---|---|
| `server` | 服务端（Rust + axum + 内嵌 SQLite） | 仅 Linux |
| `agent` | 采集 Agent（Rust） | Linux / Windows / macOS |
| `web` | 管理台前端（React + TypeScript） | 浏览器 |

## 快速开始

```bash
# 服务端（数据目录默认为 ./unionc/data，可用 UNIONC_DATA_DIR 覆盖）
cargo run -p unionc

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
重新配对 Agent。

## 文档

**[零基础教学路线 → beginner-guide/README.md](beginner-guide/README.md)**
从第一次运行开始，按课程顺序讲解技术基础、总体架构、Server、Agent、共享协议、Web、
SQLite、安全、测试、部署和实战练习。

**[完整项目文档 → DOCUMENTATION.md](DOCUMENTATION.md)**
功能详解、系统架构、接口契约、数据模型、安全模型、部署与运维手册、设计决策记录。

**[功能与边界清单 → PROJECT_CAPABILITIES.md](PROJECT_CAPABILITIES.md)**
逐项标明哪些是产品核心、哪些是可靠性与安全保障、哪些可以关闭，以及软件分发等外部职责。

其他：

- [服务端说明](docs/server.md) — 环境变量、密钥轮换、监控接口、反代契约
- [Agent 说明](docs/agent.md) — 采集能力、平台差异、投递与失败处理
- [Agent 浏览器配对协议](docs/agent-pairing.md) — 首次激活、恢复、撤销与重新配对
- [Agent 安装与退役](docs/agent-lifecycle.md) — 三平台全新安装、同版本重装、保留卸载、purge 与签名发布
- [前端说明](docs/web.md) — 开发、构建与静态部署
- [OTLP 导出](docs/monitoring.md) — 可选的时序导出（需自备 Collector）
- [安全策略](SECURITY.md) — 信任边界、漏洞报告、已权衡的取舍
- [贡献指南](CONTRIBUTING.md) — 环境要求、代码约定、提交前检查
- [更新日志](CHANGELOG.md) — 各版本具备的能力

## 许可

MIT OR Apache-2.0
