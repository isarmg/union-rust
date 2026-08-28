# Host Monitoring Agent

`unionc-agent` 是安装在远端 Linux/Windows/macOS 主机上的只读遥测 companion。它采集 CPU、
内存、网络、磁盘、温度和可用 GPU 指标，主动通过 HTTPS 向 Union 上报。

Agent 不是服务端模块：

- 不属于五个服务器模块包；
- 不由 Union supervisor 启动、停止或更新；
- 不进入 Union `full` 服务器 distribution，必须在目标主机独立安装；
- 随 Union Release 的 compatibility matrix 记录兼容版本；
- 由 Host 仓库维护，由 Union Builder Release 集中构建和发布 companion artifact，可经
  包管理、MDM 或组织软件中心分发，但不是独立公网
  服务或服务器模块 Release。

Agent 不监听远端端口，不实现命令执行、脚本/配置下发、文件传输或自更新。长期设备 secret
在 Agent 本地生成，Host worker 只持有验证所需数据。首次配对使用短时一次性授权值；完整
状态机见 [Agent 配对](agent-pairing.md)。

数据路径固定为 Agent → Union `/api/modules/host-monitoring/agent/...` → 私有 Host worker。
Agent 不知道 worker 的 loopback 地址或 gateway token。Host 数据存储在模块专用 PostgreSQL
database/role（库内 `host_monitoring` schema）；旧 Union SQLite 只可能作为一次性离线导入源。

Host worker、双方共用的 `unionc-protocol` 与 `unionc-agent` 均由独立
[`host-monitoring`](https://github.com/isarmg/host-monitoring) 仓库权威维护。Builder `full` profile
以不可变 revision 纳入其中的 Host worker；Builder Release 从同一锁定 Host revision
另行构建 Agent，但绝不把 Agent 混入服务器 worker 包。当前集中产物为 Linux
amd64/arm64 DEB/RPM、Windows amd64 未签名 MSI、macOS arm64 未签名 PKG；
Android/iOS/iPadOS 只有宿主应用可嵌入的 Rust 源码 SDK，尚无可声称的 APK/IPA。

Agent 开发验证必须在 `host-monitoring` 仓库根目录执行，而不是在 Union 仓库执行：

```bash
cargo test -p unionc-agent
cargo run -p unionc-agent -- probe
```

这不构成目标平台安装验收。正式 Release 必须声明 Agent 版本/协议兼容范围，并分别验证目标
操作系统的安装、配对、上报、离线 spool、重装和退役。
