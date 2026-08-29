# Union 文档中心

当前规范以 Union 唯一公网产品、Builder 纳入的标准模块包和五个私有业务进程为基线。Builder
决定发行包含，Plugin Runtime 只配置、启停和监管当前发行内模块。历史文档只解释动机，不能
覆盖当前架构。Union 源码仓库本身只维护 Core 和 Web Shell；业务 worker 的独立仓库是
源码治理边界，不是独立公网产品或绕过 Union 的部署入口。

| 目标 | 文档 |
|---|---|
| 需求、边界和发行模型 | [功能与边界](reference/capabilities.md) |
| 模块包、运行期管理、gateway 与数据所有权 | [发行模块](modules.md) |
| 环境、安装、数据迁移和故障处理 | [Union 服务端](server.md) |
| 四库 PostgreSQL provision、隔离验收、备份与凭据轮换 | [PostgreSQL 隔离运行手册](runbooks/postgresql-isolation.md) |
| 本地开发 | [开发运行手册](runbooks/development.md) |
| 管理前端 | [Web](web.md) |
| Agent 协议和生命周期 | [Agent](agent.md)、[配对](agent-pairing.md)、[生命周期](runbooks/agent-lifecycle.md) |
| 主机遥测和可选 OTLP | [监控](monitoring.md) |
| 零基础阅读路线 | [beginner-guide](../beginner-guide/README.md) |

## 权威来源

| 信息 | 权威来源 |
|---|---|
| 包发现、配置与启停状态 | `server/src/platform/package_store.rs`, `configuration.rs`, `runtime.rs` |
| 动态 gateway / 生命周期行为 | `server/src/platform/gateway.rs`, `runtime.rs` 及测试 |
| 模块 Manifest 契约 | `sarmg-platform` Manifest schema/parser 与各模块根 `manifest.json` |
| Host worker、Agent 与双方 DTO | `host-monitoring` 仓库及其合同测试 |
| 模块数据库 | 各 worker migration；禁止以旧 Union SQLite schema 作为当前规范 |
| 发行包含与安装 | `union-builder` 2.1 官方 profiles、`union-release.json` 与 `SHA256SUMS` |
| 服务器平台 | 仅 Linux amd64/arm64；CI 使用原生固定 runner，Release 分别发布两份完整 `full` 包 |

源码和自动化测试决定程序行为。文档中的命令是受支持流程，不等于某台生产主机已经通过
验收；验收结论必须附目标平台、release id、profile、测试输出和时间。

## 维护规则

- 新模块必须提供完整标准包契约、Manifest/Schema/权限一致性测试，并由 Builder profile 锁定
  revision；不得向 Core/Web 添加业务 feature 或静态模块表。
- Sunshine 源码权威仓库是 [`sunshine-worker`](https://github.com/isarmg/sunshine-worker)；Host
  worker、`unionc-protocol` 与 `host-m-agent` 的源码权威仓库是
  [`host-monitoring`](https://github.com/isarmg/host-monitoring)。Union 仓库不复制其源码。
- 不记录可变 worker URL，不给 worker 增加独立公网部署/Release 教程，也不增加运行时代码安装
  入口。
- 数据变更必须写清 schema owner、备份单元、离线导入和不可逆点。
- Agent/手机文档明确其为远端 companion。Builder `full` 服务器发行包含 Host worker，但 Agent
  和 Photo 客户端由各模块仓库维护、由 Builder Release 集中产出，不能把它们列入
  Union Server distribution 或 supervisor 图。
- `docs/history/` 只保留历史证据；旧 Cargo-feature 组合、静态 spec、SQLite 共享设计和旧
  Actions 不能作为当前操作手册。
