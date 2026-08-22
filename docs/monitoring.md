# 可选的 OTLP 指标导出

UnionC 自身保存每台主机的最新快照与有限历史（默认 30 天，见
`UNIONC_TELEMETRY_RETENTION_DAYS`），足以支撑管理台的列表、详情与历史曲线。

如果需要更长的保留期、PromQL 查询或看板，Agent 可以在 UnionC 接受报告后，把时序数值
继续旁路到任意 OTLP/HTTP 端点。这个依赖方向不是对称的：Collector 不可用不会让已经成功
的 UnionC 主上报失败；UnionC 尚未确认的报告则不会先送往 OTLP。

```
Agent ──JSON──→ UnionC /api/agent/v1/report（权威）──→ 内嵌 SQLite
Agent ←────────────── 严格 ACK ───────────────────── UnionC
  │
  └──ACK 后可选──→ OTLP /v1/metrics ──→ 你自备的 Collector / 后端
                    （尽力而为）
```

**权威数据源始终是 UnionC 经每主机 token 验证的 JSON 报告**，OTLP 只承载时序数值。
它不包含完整资产快照或完整 capability 列表，但会携带识别时间序列所需的主机、服务和设备
属性；管理台展示的数据不受 OTLP 侧影响。

> 本仓库**不提供**观测栈的部署物（compose 文件、Collector 配置、看板等）。
> Agent 只需要一个能收 OTLP/HTTP protobuf 的端点，用什么部署、部署在哪由你决定。

## 接入

在 Agent 配置或环境变量中指定 OTLP 端点：

```bash
UNIONC_AGENT_OTLP_ENDPOINT=https://telemetry.example.com/v1/metrics
UNIONC_AGENT_OTLP_TOKEN=<可选的 bearer token>
```

Agent 发送 **gzip 压缩的 OTLP/HTTP protobuf**（`content-type: application/x-protobuf`、
`content-encoding: gzip`），配置了 token 时附带 `Authorization: Bearer`。
任何兼容 OTLP/HTTP 的接收端都可以，常见选择是
[OpenTelemetry Collector](https://opentelemetry.io/docs/collector/) 的 `otlp` receiver，
再经 `otlphttp` exporter 转发到你的时序库。

常驻 `run` 的导出是**异步且有界**的：报告经容量 128 的队列交给独立任务发送，慢速的
Collector 不会拖慢采样与主上报；队列满时直接丢弃并告警。补传成功后**先出队再导出**——
反过来，`acknowledge` 失败会让同一份报告下轮重复导出。

`once` / `doctor --delivery` 不创建这个队列。它们清理旧 spool 时不会把旧报告补发到
OTLP；当前报告得到 UnionC ACK 后才同步尝试 OTLP，因而可能等待到请求超时。OTLP 失败仍
只记告警，不把已经成功的 UnionC 上报改判为失败。

反向代理配置（含 mTLS 客户端证书校验）见
[Caddyfile.telemetry.example](examples/caddy/Caddyfile.telemetry.example)。

> **注意**：OTLP 导出由 `otlp` feature 门控，默认构建不启用。需要显式使用
> `--features otlp` 构建；未编译该 feature 却为投递命令配置 `otlp_endpoint` 或
> `otlp_token` 时，Agent 会在启动校验阶段明确失败。接收端运行异常则只记录
> `optional OTLP export failed` 一类告警，不会让 UnionC 主上报失败。
> 当前 release workflow 的 Linux/Windows 制品使用默认 feature（NVIDIA、无 OTLP），
> macOS 制品则显式使用 `--no-default-features --features otlp`。

## 导出的指标

资源属性：`host.id`、`host.name`、`os.type`、`host.arch`、`service.name`、
`service.version`。注意 `os.type` / `host.arch` 按 OTLP 语义约定转写
（`macos`→`darwin`、`x86_64`→`amd64`、`aarch64`→`arm64`）。

| Metric | 类型 | 单位 | 数据点属性 |
|---|---|---|---|
| `system.cpu.utilization` | Gauge | `1` | — |
| `system.memory.usage` / `.limit` | Gauge | `By` | `system.memory.state` |
| `system.uptime` | Gauge | `s` | — |
| `system.network.io` | Sum（单调、Cumulative） | `By` | `network.interface.name`、`network.io.direction` |
| `system.disk.io` | Sum（单调、Cumulative） | `By` | `system.device`、`disk.io.direction` |
| `system.filesystem.usage` | Gauge | `By` | `system.device`、`system.filesystem.mountpoint` |
| `hw.temperature` | Gauge | `Cel` | `sensor.id`、`sensor.label`、`telemetry.source` |
| `hw.gpu.utilization` | Gauge | `1` | `gpu.id`、`gpu.vendor`、`gpu.name` |
| `hw.gpu.memory.usage` / `.limit` | Gauge | `By` | 同上 |
| `hw.gpu.temperature` | Gauge | `Cel` | 同上 |
| `hw.gpu.power` | Gauge | `W` | 同上 |

## 编码细节

OTLP protobuf 由 `agent/src/otlp.rs` 手写实现（仅 Metrics 子集，字段编号对齐官方
proto），不引入完整的 opentelemetry SDK。

同一类设备（多网卡、多磁盘、多传感器）会**收敛到一个 metric 下的多个数据点**，
以数据点属性区分。OTLP 数据模型要求同一 scope 内一个 metric 名只能出现一次；
为每个设备各建一个同名 `Metric` 虽然 Collector 通常照收不误并返回 200，
但下游按名聚合时会互相覆盖或被当成冲突的时间序列。

字段编号是手抄的，抄错一个 tag 号编出来的字节流就是错的，而模块内单测发现不了：
它们编解码用的是**同一份**定义，抄错的编号在自洽的两侧同样自洽。因此有两道独立防线：

| 测试 | 回答的问题 | 依赖 |
|---|---|---|
| `agent/tests/otlp_encoding.rs` | 编码是否正确？ | `opentelemetry-proto`（**dev-dependency**，官方 proto 生成的类型；运行时依赖一个字节没变） |
| `agent/tests/otlp_live.rs` | 对端是否真的接受？ | CI 的独立 `otlp` job，跑真实的 `otel/opentelemetry-collector-contrib` |

后者由 `UNIONC_AGENT_TEST_REQUIRE_OTLP=1` 守护，把"静默跳过"升级为"失败"。

## 生产注意事项

- 可为每台 Agent 签发各自的客户端证书；但当前配置中的 TLS CA 与客户端身份由 UnionC、
  pairing 和 OTLP 共用，若 OTLP 必须使用另一套证书，需要认证网关隔离或修改代码；
- 按 `主机数 × 每主机序列数 ÷ 上报间隔` 实测调整保留期与资源；
- 在入口限制请求体、证书身份、时间戳偏移与速率。

### 这不是多租户安全边界

该 OTLP 入口是**同一信任域内**的长时序通道。客户端证书能证明设备属于这套部署，
但标准 Collector **不会**自动验证证书 SAN 是否等于 OTLP 资源属性里的 `host.id`。
因此：

- 不要跨租户共享 Agent CA；
- 需要强主机隔离时，应在入口增加能把证书身份绑定到资源属性的认证网关，
  并限制速率、标签数与时序基数。
