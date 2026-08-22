# UnionC 文档中心

这里是项目文档的唯一导航入口。根目录 `README.md` 只负责快速介绍；需要理解、开发、
部署或排查项目时，从本页选择对应资料。

## 先选择阅读目标

| 目标 | 从这里开始 |
|---|---|
| 第一次接触项目 | [零基础教学](../beginner-guide/README.md) |
| 快速建立完整认识 | [完整项目手册](../DOCUMENTATION.md) |
| 判断功能范围和明确非目标 | [功能与边界说明](reference/capabilities.md) |
| 配置本地开发环境 | [本地开发运行手册](runbooks/development.md) |
| 开发或部署 Server | [Server 说明](server.md) |
| 开发或部署 Agent | [Agent 说明](agent.md) |
| 理解配对协议 | [Agent 配对协议](agent-pairing.md) |
| 安装、重装或退役 Agent | [Agent 生命周期手册](runbooks/agent-lifecycle.md) |
| 开发或部署 Web | [Web 说明](web.md) |
| 接入 OTLP | [OTLP 指标导出](monitoring.md) |
| 报告安全问题 | [安全策略](../SECURITY.md) |
| 提交代码 | [贡献指南](../CONTRIBUTING.md) |

## 文档目录

```text
docs/
├── README.md                 本索引
├── server.md                 Server 配置、部署和运维约束
├── agent.md                  Agent 配置、采集和投递语义
├── agent-pairing.md          配对状态机与 HTTP 契约
├── web.md                    前端开发与静态部署
├── monitoring.md             可选 OTLP 导出
├── reference/                当前参考资料
│   └── capabilities.md       产品能力、边界与不变量
├── runbooks/                 可直接执行的开发/运维流程
│   ├── development.md        跨平台本地开发入口
│   └── agent-lifecycle.md    Agent 安装、卸载与退役
├── examples/                 可复制后按环境修改的示例
│   └── caddy/                Caddy 反向代理示例
└── history/                  带时间边界的历史记录，不作为当前规范
    └── 2026-08-project-audit.md
```

## 权威来源

为避免同一信息在多份文档中相互覆盖，按主题维护唯一来源：

| 信息 | 权威来源 |
|---|---|
| 线上 DTO | `protocol/src/` 及相应合同测试 |
| API 路径、鉴权和输入校验 | Server 路由/模型源码及测试 |
| SQLite 当前 schema | `server/schema/sqlite.sql` |
| Agent 配置默认值和优先级 | `agent/src/config.rs` 及测试 |
| Server 环境变量 | `server/src/config/runtime.rs` 与 [Server 说明](server.md) |
| 产品支持范围 | [功能与边界说明](reference/capabilities.md) |
| 完整架构和接口导览 | [完整项目手册](../DOCUMENTATION.md) |
| 安装和操作步骤 | `runbooks/` 下对应手册及 packaging 自动化测试 |

源码和可执行测试最终决定程序行为。文档与源码不一致时，不要悄悄选择其中一个：先确认
预期行为，再在同一改动中同步修正文档或实现。`beginner-guide/` 是学习路线，`history/`
是历史证据，它们都不取代当前主题文档。

## 示例与历史资料

Caddy 示例按入口职责分开：

- [管理台、静态前端和 API](examples/caddy/Caddyfile.console.example)
- [独立 Agent 数据入口与 mTLS](examples/caddy/Caddyfile.agent-api.example)
- [OTLP 遥测入口与 mTLS](examples/caddy/Caddyfile.telemetry.example)

示例包含占位域名、路径和密钥环境变量，不能未经检查直接用于生产。

[项目审查与整改报告](history/2026-08-project-audit.md)记录特定日期的审查结果，适合追溯
设计动机；其中的行号、测试状态和问题清单不代表当前工作树状态。

## 维护规则

- 新增主题文档后在本页登记，不再向根 `README.md` 堆叠平行索引。
- 操作步骤放在 `runbooks/`，长期契约放在 `reference/` 或对应组件文档。
- 配置样例放在 `examples/`，并从说明其安全边界的文档链接过去。
- 一次性审查、迁移记录和过期设计放在 `history/`，文件名包含日期或版本。
- 移动文档后使用仓库级搜索检查 Markdown 链接和代码注释中的路径。
