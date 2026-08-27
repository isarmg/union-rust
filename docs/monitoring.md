# 主机监控与可选 OTLP

Host monitoring 是 `module-host-monitoring` feature 对应的私有 worker。Agent 通过 Union 的
固定 `/api/agent/...` 数据面上报；Union 注入 gateway identity 后转给回环
`127.0.0.1:18105`，worker 用自己的 PostgreSQL schema `host_monitoring` 保存配对、凭据、
最新报告和历史。

```text
远端 Agent --HTTPS/设备凭据--> Union --gateway-v1--> Host worker --> PostgreSQL
       \--主上报 ACK 后可选、尽力而为--> OTLP/HTTP Collector
```

OTLP 不是权威库，不影响主上报 ACK，也不替代 Host PostgreSQL。Agent 可用 `otlp` feature
编译并配置 `UNIONC_AGENT_OTLP_ENDPOINT`；接收端、看板和长期时序库由部署者提供。

Agent 是远端 companion，不由 Union supervisor 启动。服务端不向 Agent 下发命令、配置、
脚本或更新。禁用 Host feature 的 profile 不包含 Agent 数据面，不能靠设置 URL 在运行时补回。
