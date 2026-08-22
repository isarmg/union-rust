# 06. Agent 详解

Agent 是 UnionC 的数据源，也是跨平台和故障恢复最集中的部分。理解它时，应把“采样节拍”和“网络投递”视为两个并行子系统。

## 1. Agent 的职责与非职责

Agent 负责：

- 读取本机资源；
- 构造当前协议报告；
- 管理本机身份、credential 和配对状态；
- 可靠排队并主动 HTTPS 上报；
- 输出本地诊断；
- 可选 OTLP 导出。

Agent 不负责：

- 监听 Server 的入站连接；
- 执行远程命令；
- 修改主机配置；
- 远程进程控制或文件传输；
- 自己下载新版本。

这让 Agent 可以使用低权限服务账户和严格文件权限。

## 2. 可执行入口

主要入口 `agent/src/main.rs` 同时支持前台 CLI 和服务运行。Windows 还有：

- `bin/unionc-agent-tray.rs`：普通用户通知区、本机随机回环配置页、服务控制入口；
- `bin/unionc-agent-maintenance.rs`：安装器调用的固定维护动作。

核心库模块在 `agent/src/lib.rs` 导出，使集成测试与多个 binary 复用同一实现。

## 3. 六个日常命令

| 命令 | 是否联网 | 是否写状态/投递 | 用途 |
|---|---:|---:|---|
| `run` | 是 | 是 | 默认，持续采样和上报 |
| `once` | 是 | 是 | 排空历史并投递一个新快照 |
| `probe` | 否 | 否 | 查看本机能采到什么 |
| `pair` | 是 | 是 | 创建/恢复浏览器授权配对 |
| `doctor` | 默认否 | 默认只读 | 检查配置、采集、身份、spool |
| `status` | 否 | 只读 | 即使配置有问题也尽量输出本地状态 |

只有显式 `doctor --delivery` 才执行真实投递，它可能排空 spool。因此排错时先运行只读 `status` 和 `doctor`。

身份准备发生在采样循环之前。已有 pending 配对时，只有 `run` 会持续轮询等待浏览器授权；
`once` 与 `doctor --delivery` 会立即报错并给出 activation URL。完全未配对时，`run` 只会
退避等待并提示执行 `pair`，不会开始无身份采样；两个一次性投递命令则立即失败。

## 4. 配置来源与优先级

`agent/src/config.rs` 组合：

```text
内置默认值
  → 配置文件
  → UNIONC_AGENT_* 环境变量
  → CLI 覆盖（例如 --server、--endpoint、--name）
  → 按命令做完整校验
```

默认值：

| 配置 | 默认 |
|---|---|
| report endpoint | `http://127.0.0.1:8081/api/agent/v1/report` |
| 采样间隔 | 10 秒 |
| 慢指标间隔 | 30 秒 |
| HTTP 超时 | 10 秒 |
| jitter | ±10% |
| spool 上限 | 64 MiB |
| OTLP | 关闭 |
| 非回环 HTTP | 禁止 |

默认状态/配置位置：

| 平台 | 状态目录 | 默认配置 |
|---|---|---|
| Linux | `/var/lib/unionc-agent` | `/etc/unionc-agent/config.json` |
| Windows | `%PROGRAMDATA%\UnionC Agent` | 状态目录中的 `config.json` |
| macOS | `/Library/Application Support/UnionC Agent` | 状态目录中的 `config.json` |

教程和测试必须同时把 `UNIONC_AGENT_CONFIG` 指向隔离的 `config.json`，并把 `UNIONC_AGENT_STATE_DIR` 指向隔离状态目录，不能复用系统 Agent 的真实配置或状态。

## 5. 配置为何严格校验

启动前会检查：

- interval > 0，且加上最坏 jitter 后不超过 Server 3600 秒契约；
- slow interval 不小于普通 interval；
- timeout 在 1 到 300 秒；
- spool 至少 1 MiB；
- 若配置 host name，则必须为 1–255 字节、去首尾后非空且无控制字符；
- 非回环 endpoint 默认必须 HTTPS；
- TLS identity 格式不能同时配置 PEM 与 PKCS#12；
- 未编译 `otlp` feature 时不允许留下 OTLP 配置。

在启动时拒绝错误配置，比让每一份报告进入 spool 后持续收到永久 400 更可诊断。

配置 JSON 使用 `deny_unknown_fields`，并要求 `application_version` 精确等于当前 Agent 包
版本。配置文件会先被完整反序列化，再叠加环境变量，所以环境变量不能替旧版本、缺字段或
多字段的 JSON “补齐”结构。命令还有刻意保留的诊断例外：

- `status` 跳过常规校验，把缺失、不可读或格式错误的配置作为只读诊断输出；
- `probe` 不检查投递 endpoint、TLS 或 OTLP，因为它不联网；
- 默认的 `doctor` 不因这些投递设置直接中止，但会把未来 `run` 的配置问题列为诊断项；
- `doctor --delivery` 与真正投递命令一样执行相关网络配置校验。

## 6. 采样器

`collectors/mod.rs` 的 `SystemSampler` 管理跨周期基线。sysinfo 维护刷新基线；`received()` / `transmitted()` 与 `DiskUsage.read_bytes` / `written_bytes` 已是两次 refresh 间的本轮增量，Agent 再除以实测 `interval_seconds` 得到速率。`*_total` 字段则直接取 sysinfo 的累计值。

采集器同时返回 metrics 和 capabilities。采集失败通常被转化为某项 capability 不可用，而不是让整个 Agent 因一块 GPU 读不到而退出。`collector_errors` 只统计 `transient` 和 `invalid_data` 一类真正异常；硬件不存在或平台不支持不算运行错误。

平台实现：

- 通用 CPU/内存/网络/磁盘基于 sysinfo 与平台辅助；
- Linux 温度读取 hwmon；
- Linux AMD/Intel GPU 读取 DRM sysfs；
- Windows GPU 使用 WDDM 性能计数器；
- NVIDIA 由默认 `nvidia` feature 和 NVML 提供；
- macOS 或不支持能力会明确报告原因。

当前温度采集按 `slow_interval_seconds` 降频缓存；GPU 仍在每个正常采样周期查询。修改“慢指标”范围时要同步检查缓存、capability 与测试，不能仅改配置名。

## 7. 采样节拍与投递解耦

`run_loop` 创建：

- 一个采样循环；
- 一个容量很小的投递唤醒通道；
- 一个独立 `DeliveryWorker`；
- shutdown 和 host identity 的 watch 通道。

采样时刻只做：

1. 读取 spool 个数；
2. 本地采样；
3. 原子写入 spool；
4. 通知 delivery worker；
5. 按基础周期与 jitter 安排下一次 deadline。

所有网络 I/O、退避和最多 32 份补传都在 delivery worker 中。否则一个 10 秒 HTTP 超时就可能把“每 10 秒采样”变成“每 20 秒甚至更慢”。

节拍锚定前一次 deadline；如果进程暂停太久，则从当前时间重新排下一次，不进行密集追赶采样。

上述“原子写入”描述的是写成功时不会留下半个 JSON，不代表磁盘故障时报告必然保住。
常驻 `run` 为 spool 读取、写入和补传分别统计健康度：单次写失败会记录并丢弃本次采样后
继续运行，同类操作连续失败 100 次才退出交给服务管理器。`once` / `doctor --delivery`
没有这层守护进程降级封装，本地 spool I/O 出错会立即失败。

## 8. spool 的文件模型

`Spool::open(state_dir, max_bytes)` 创建私有 `spool/`。正常文件名：

```text
00000001766000000000-<report-uuid>.json
```

固定宽度时间戳让字典序就是采样顺序。关键操作：

- `enqueue`：JSON 序列化，临时文件写入、同步、原子 rename；
- `oldest`：O(n) 找最小文件，不分配并排序整个队列；
- `acknowledge`：成功 ACK 后删除；
- 损坏 JSON：改名为 `.invalid` 隔离；
- 容量超限：先删最老 `.invalid`，再淘汰最老待发报告。

`.invalid` 也计入同一预算，否则磁盘损坏持续产生的隔离文件会无限占盘。

spool 使用短时本地 mutex 序列化 enqueue/淘汰/ack，防止容量核算与并发删除互相误判。

## 9. 投递与错误分类

`transport.rs::SendError` 按“要让同一份报告成功，需要改变什么”分类：

| 类别 | 典型状态 | 需要改变 | 行为 |
|---|---|---|---|
| Permanent | 400/409/413/422 | 报告内容 | 记录并丢弃队首 |
| Unauthorized | 401 | credential 未知/失效，或主机仍 active 但该 credential 已被重配替换/撤销 | 当前常驻 `run` 持久写 `reauth_required`、停止投递并继续采样到有界 spool |
| Revoked | 403 | 主机生命周期已撤销，或有效 credential 与报告 `host_id` 绑定不匹配 | 当前常驻 `run` 持久写 `reauth_required`、停止投递并继续采样到有界 spool |
| Transient | 网络、421、429、5xx 及未归为永久的其他响应 | 时间或部署状态 | 保留并退避重试 |

将永久失败留在 FIFO 队首会永远挡住所有后续有效报告，因此必须出队。将暂时失败误删则会造成数据丢失，因此分类与 Server 状态码语义必须同步测试。

这张表的 401/403 状态落盘行为专指当前正在运行的 delivery worker。Web 撤销不会主动推送
到本机；Agent 在下一次报告被拒时才得知。`once` / `doctor --delivery` 只把当前可重试
报告入队并返回错误，不写 `reauth_required`。常驻进程写入该状态后仍能继续采样；但一旦
重启，`run` 会因没有 authorized reporter 而在采样循环前退出，由服务管理器重试，直到
管理员为同一实例完成重新配对。

## 10. 每轮补传为什么最多 32 份

`flush_spool` 每批最多发送 32 个报告，然后主动让出调度。下一批无需等待新采样 tick，但批次边界能避免长时间断线后一个 Agent 持续独占网络和 runtime。

Server 的每主机令牌桶容量 64，正好允许正常的 32 份补传突发并保留余量，同时限制长期平均写入速率。

## 11. ACK 校验

Reporter 不只看 `response.is_success()`。它限制响应体大小，并检查：

- 状态必须正好是 202；
- media type 必须是 JSON；
- JSON 不允许未知字段；
- ACK host UUID 与本地实例一致；
- ACK report UUID 与当前队首一致。

常驻 `run` 的确认顺序是：UnionC ACK → 删除 spool → 可选放入 OTLP 队列。一次性的
`once` / `doctor --delivery` 排空旧 spool 时不导出这些旧报告；其当前报告得到 UnionC ACK
后会同步尝试 OTLP，可能等待到请求超时。OTLP 始终是尽力而为旁路，不能阻止权威 UnionC
报告成功。

## 12. 退避与 jitter

- 采样 jitter 把大量主机的写入摊开，避免所有 Agent 整秒同时上报；
- 网络错误使用带随机性的指数退避，避免 Server 恢复时形成同步重试风暴；
- 成功投递后重置失败退避；
- 身份撤销不是等待能解决的问题，因此不会无限自动重试或偷偷生成新 credential。

## 13. 可恢复配对状态机

`pairing.rs` 的本地状态包括：

```text
Creating → Pending → Activating → Active
                 ├→ Denied
                 └→ Expired
```

- Creating 先持久保存两个秘密和目标 endpoint，再发网络请求；
- Pending 保存 request ID、激活 URL、到期时间和轮询间隔；
- Activating 是本地提交日志，一旦进入不再做网络 I/O；token、host ID、config、auth state
  逐个用原子文件替换幂等写入，`Active` 最后写，因此整体 crash-safe，但不是多文件原子事务；
- Active 最后写入，代表本地状态完全一致。

网络中断后 `pair` 或 `run` 可以继续当前请求，不生成会让浏览器批准对象错位的新 secret。并发配对通过状态目录锁与 generation 防护。

## 14. 本地私有文件

| 文件/目录 | 内容 |
|---|---|
| `host-id` | Server 分配的稳定 instance UUID |
| `agent-token` | 长期 Agent secret 明文，仅本机 |
| `pairing-state.json` | 可恢复配对状态与临时秘密 |
| `auth-state.json` | 例如 `reauth_required` 的持久授权状态 |
| `.credential-state.lock` | 服务、CLI、托盘之间的状态事务锁 |
| `spool/` | 未确认报告与隔离文件 |
| config | endpoint、周期、TLS 等当前版本配置 |

Unix 文件使用私有目录与 0600 一类权限，写入采用临时文件和原子替换。Windows 安装器建立 ACL，macOS/Linux 包建立专用服务身份和目录。

## 15. OTLP 可选旁路

Agent 默认 feature 是 `nvidia`，OTLP 需显式编译。保留默认 NVIDIA 能力并增加 OTLP：

```bash
cargo build -p unionc-agent --features otlp
```

只编译 OTLP、同时关闭 NVIDIA：

```bash
cargo build -p unionc-agent --no-default-features --features otlp
```

生成发布制品时再加 `--release`。

启用后：

- 使用手写的 OTLP Metrics protobuf 子集；
- gzip + `application/x-protobuf` 发送；
- 常驻 `run` 使用容量 128 的独立有界队列；队列满时丢弃并告警，不阻塞主采样/上报；
- `once` / `doctor --delivery` 不用该队列：旧 spool 不做 OTLP，当前报告在 UnionC ACK 后
  同步尝试导出；
- 不导出完整 capability 列表或 UnionC 全量资产快照，但会携带识别时间序列所需的主机、
  服务、网卡、磁盘、传感器和 GPU 等资源/数据点属性；
- UnionC JSON/SQLite 仍是管理台权威数据源。

协议编码由官方 proto 生成类型的测试与真实 Collector 测试双重验证。

## 16. 三平台差异

| 方面 | Linux | Windows | macOS |
|---|---|---|---|
| 服务管理 | systemd | Windows SCM | launchd |
| TLS 后端 | rustls | native-tls | native-tls |
| 客户端身份 | PEM | PKCS#12 | PKCS#12 |
| 交互入口 | CLI | CLI + 通知区托盘 | CLI |
| 状态位置 | `/var/lib` | ProgramData | `/Library/Application Support` |

跨平台代码必须在目标 CI 上编译。只在 Linux 上通过单测，不能证明 Windows API、feature 或安装器仍正确。

## 17. 排错顺序

```bash
unionc-agent status
unionc-agent doctor
unionc-agent probe --json
unionc-agent doctor --delivery   # 最后且明确接受真实投递时
```

关注：有效配置、state_dir、身份、credential、`reauth_required`、pending/invalid spool 数、capability 原因、endpoint 和 TLS。

## 18. 本章自检

1. 为什么 `doctor` 默认不联网？
2. 采样循环为何先落 spool 再通知投递？
3. `.invalid` 文件为什么也占容量预算？
4. 400 与 503 对同一份队首报告应分别怎么处理？
5. 配对为什么需要 Creating/Activating 两个本地过渡阶段？
6. OTLP 故障为什么不能让 UnionC 主上报失败？

下一章：[07. Web 前端详解](07-Web前端详解.md)。
