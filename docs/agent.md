# Union Agent

`unionc-agent` 是安装在远端 Linux/Windows/macOS 主机上的只读遥测 companion。它采集 CPU、
内存、网络、磁盘、温度和可用 GPU 指标，主动通过 HTTPS 向 Union 上报。

Agent 不是服务端模块：

- 不属于五个 Cargo module feature；
- 不由 Union supervisor 启动、停止或更新；
- 随 Union Release 的 compatibility matrix 记录兼容版本；
- 可以由包管理、MDM 或组织软件中心分发，但没有独立服务端产品/模块 Release。

Agent 不监听远端端口，不实现命令执行、脚本/配置下发、文件传输或自更新。长期设备 secret
在 Agent 本地生成，Host worker 只持有验证所需数据。首次配对使用短时一次性授权值；完整
状态机见 [Agent 配对](agent-pairing.md)。

数据路径固定为 Agent → Union `/api/agent/...` → 私有 Host worker。Agent 不知道 worker 的
18105 端口或 gateway token。Host 数据存储在 PostgreSQL `host_monitoring` schema；旧
Union SQLite 只可能作为一次性离线导入源。

开发验证：

```bash
cargo test -p unionc-agent
cargo run -p unionc-agent -- probe
```

这不构成目标平台安装验收。正式 Release 必须声明 Agent 版本/协议兼容范围，并分别验证目标
操作系统的安装、配对、上报、离线 spool、重装和退役。
