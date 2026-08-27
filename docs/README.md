# Union 文档中心

当前规范以 Union 唯一产品、五个编译期模块和私有 worker 为基线。历史文档只解释动机，不能
覆盖当前架构。

| 目标 | 文档 |
|---|---|
| 需求、边界和发行模型 | [功能与边界](reference/capabilities.md) |
| feature、端口、gateway 与数据所有权 | [编译期模块](modules.md) |
| 环境、安装、数据迁移和故障处理 | [Union 服务端](server.md) |
| 本地开发 | [开发运行手册](runbooks/development.md) |
| 管理前端 | [Web](web.md) |
| Agent 协议和生命周期 | [Agent](agent.md)、[配对](agent-pairing.md)、[生命周期](runbooks/agent-lifecycle.md) |
| 主机遥测和可选 OTLP | [监控](monitoring.md) |
| 零基础阅读路线 | [beginner-guide](../beginner-guide/README.md) |

## 权威来源

| 信息 | 权威来源 |
|---|---|
| 编译 feature、worker 地址与前缀 | `server/Cargo.toml`, `server/src/platform/spec.rs` |
| gateway / supervisor 行为 | `server/src/platform/gateway.rs`, `supervisor.rs` 及测试 |
| 模块 manifest | `sarmg-platform/modules/*.json` 固定 revision |
| Host/Agent DTO | `protocol/src/` 及合同测试 |
| 模块数据库 | 各 worker migration；禁止以旧 Union SQLite schema 作为当前规范 |
| 发行图与安装 | `union-builder` v1.0.0 官方 profiles 和 release manifest |

源码和自动化测试决定程序行为。文档中的命令是受支持流程，不等于某台生产主机已经通过
验收；验收结论必须附目标平台、release id、profile、测试输出和时间。

## 维护规则

- 新模块必须同时有 feature、固定 spec、manifest、Builder profile 条目和缺席测试。
- 不记录可变 `SARMG_*` URL，不给 worker 增加独立部署/Release 教程。
- 数据变更必须写清 schema owner、备份单元、离线导入和不可逆点。
- Agent/手机文档明确其为远端 companion，不能把它们列入 server supervisor 图。
- `docs/history/` 只保留历史证据，过时版本、SQLite 设计和旧 Actions 不能作为当前操作手册。
