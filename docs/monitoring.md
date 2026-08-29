# 主机监控与可选 OTLP

Host Monitoring 是 Builder 可纳入发行的标准模块包，Backend 以 Union Runtime 监管的本地私有
进程运行。模块必须先按 Schema 配置其专用 PostgreSQL database/role，再由管理员在运行期启用；
当前发行未包含该包时，不能靠配置 URL 或启停操作补入它。

Host worker、`unionc-protocol` 和跨平台 `host-m-agent` 的源码权威均为
[`host-monitoring`](https://github.com/isarmg/host-monitoring)。Builder `full` 服务器发行只纳入并
固定其中的 Host worker；Agent 是同仓构建、在目标主机独立安装的 companion artifact。

Agent 只连接 Union 的公共模块数据面：

- 配对：`/api/modules/host-monitoring/agent/v2/*`
- 上报：`/api/modules/host-monitoring/agent/v1/report`

Union 根据 Manifest 将允许的请求转给当前 Host worker，注入 `gateway-v1` 身份并清洗外部伪造
内部头。worker 的 loopback 地址和端口属于 Runtime 内部状态，不是 Agent 配置或公共兼容契约。
配对、设备凭据验证、最新报告和历史数据都由 Host 模块及其专用 PostgreSQL database 持有，
Core SQLite 不保存这些业务数据。

```text
远端 Agent --HTTPS/设备凭据--> Union --Manifest Gateway--> Host worker --> Host PostgreSQL
       \--主上报 ACK 后可选、尽力而为--> OTLP/HTTP Collector
```

管理页面和管理 API 使用 Core 会话/RBAC；Agent 配对与上报是 Manifest 声明的模块领域认证路由，
由 Host worker 校验一次性授权值、polling secret 和设备 credential。两者都只能经 Union 到达，
`gateway-v1` token 永远不会交给 Agent 或浏览器。

OTLP 不是权威存储，不影响主上报 ACK，也不替代 Host PostgreSQL。Agent 可用自身的 `otlp` Cargo
feature 构建并配置 `HOST_M_AGENT_OTLP_ENDPOINT`；这是远端 companion 的构建选项，不是 Union
Core 的业务模块 feature。Collector、看板和长期时序库由部署者提供。

Agent 不属于 Union 服务器 distribution，也不由 Plugin Runtime 启动、更新或卸载。它不是公网
服务或 Core 私有 worker；服务端不向 Agent 下发命令、配置、脚本或更新。完整配对和退役边界见
[Agent](agent.md)、[Agent 配对](agent-pairing.md) 和
[生命周期 runbook](runbooks/agent-lifecycle.md)。

本地协议测试、模块 readiness 或 Collector live test 不构成生产验收；发布候选仍需记录目标系统
上的安装、配对、上报、离线 spool、数据库备份恢复、进程故障和版本兼容结果。
