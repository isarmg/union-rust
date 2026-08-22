# UnionC 项目文档

> 版本：v0.3.2 ｜ 文档更新：2026-08-20
> 本文是项目的完整说明：功能、架构、各组成部分、接口契约、部署与运维。

## 目录

- [1. 项目概述](#1-项目概述)
- [2. 系统架构](#2-系统架构)
- [3. 组成部分](#3-组成部分)
  - [3.1 服务端 `server`](#31-服务端-server)
  - [3.2 Agent `agent`](#32-agent-agent)
  - [3.3 管理台前端 `web`](#33-管理台前端-web)
  - [3.4 可选的 OTLP 导出](#34-可选的-otlp-导出)
- [4. 功能详解](#4-功能详解)
  - [4.1 主机实例与一次性授权配对](#41-主机实例与一次性授权配对) ｜ [4.2 重新配对、撤销与删除边界](#42-重新配对撤销与删除边界)
  - [4.3 采集与投递](#43-采集与投递) ｜ [4.4 主机状态判定](#44-主机状态判定)
  - [4.5 Sunshine 管理](#45-sunshine-管理) ｜ [4.6 审计日志](#46-审计日志)
- [5. 技术设计](#5-技术设计)
  - [5.1 技术选型](#51-技术选型) ｜ [5.2 三条原则](#52-贯穿全项目的三条原则)
  - [5.3 当前数据库基线](#53-当前数据库基线) ｜ [5.4 密钥环与轮换](#54-密钥环与轮换)
  - [5.5 SQLite 写入与数据保留](#55-sqlite-写入与数据保留)
- [6. HTTP 接口契约](#6-http-接口契约)
  - [6.4 报文契约](#64-报文契约) ｜ [6.5 错误响应](#65-错误响应)
- [7. 数据模型](#7-数据模型)
- [8. 安全模型](#8-安全模型)
- [9. 部署](#9-部署)
- [10. 运维手册](#10-运维手册)
- [11. 开发与测试](#11-开发与测试)
- [12. 设计决策记录](#12-设计决策记录)

---

## 1. 项目概述

UnionC 是一套**只读**的多主机状态监控系统，外加一组 Sunshine 串流主机的管理能力。

### 它做什么

| 能力 | 说明 |
|---|---|
| **多主机监控** | 跨平台 Agent 采集 CPU / 内存 / 磁盘 / 网络 / 温度 / GPU，上报到中心服务端 |
| **实时与历史** | 主机列表、实时详情、历史曲线（最多 1000 个采样点） |
| **主机生命周期** | 管理台预留实例、浏览器一次性码激活、持久撤销与重新配对 |
| **Windows 本机入口**（可选） | 通知区托盘打开本机浏览器配置，配对/重新配对、Server 连接检测、查看或启停服务 |
| **本机资源** | 服务端自身的 CPU / 内存 / 磁盘 / 网络吞吐 |
| **Sunshine 管理** | 多主机配置、应用与配对客户端管理、配置读写、日志 |
| **OTLP 导出**（可选） | Agent 可同时把时序数据推送到 OpenTelemetry Collector |

### 它明确不做什么

这是一条**设计约束**，不是尚未实现的功能：

- 不向被监控主机下发任何命令、配置或脚本
- 不做进程控制、远程执行、文件传输
- 不做 Agent 自更新
- 代码中不存在上述端点

Agent 的权限模型即由此而来：它只需要读权限，systemd unit 里 `CapabilityBoundingSet=` 为空。

### 平台支持

| 组件 | 平台 |
|---|---|
| **服务端** | **仅 Linux**（`lib.rs` 中的 `compile_error!` 固定该约束，CI 只跑 ubuntu） |
| **Agent** | **Linux / Windows / macOS**，CI workflow 配置三平台矩阵 |
| **前端** | 浏览器（构建产物为静态文件，由反向代理提供） |

---

## 2. 系统架构

```
                    ┌──────────────────────────────────────┐
                    │          浏览器（管理台）              │
                    │        web (React)          │
                    └──────────────┬───────────────────────┘
                                   │ HTTPS + Cookie 会话 + CSRF 双提交
                    ┌──────────────▼───────────────────────┐
                    │        反向代理（Caddy / nginx）       │
                    │   TLS 终止，透传 X-Forwarded-Proto    │
                    └──────────────┬───────────────────────┘
                                   │ HTTP，回环地址
                    ┌──────────────▼───────────────────────┐
                    │      UnionC 服务端 (Rust / axum)      │
                    │  ┌────────────┬──────────┬─────────┐ │
                    │  │ 控制台 API │ Agent API │ 后台任务 │ │
                    │  └────────────┴──────────┴─────────┘ │
                    └────┬──────────────┬──────────────┬───┘
                         │              │              │
              ┌──────────▼───┐  ┌───────▼──────┐  ┌───▼─────────────┐
              │ 内嵌 SQLite  │  │ Sunshine 主机 │  │ 本机 /proc /sys │
              │ 配置/审计/遥测│  │  HTTPS 代理   │  │   资源采样      │
              └──────────────┘  └──────────────┘  └─────────────────┘
                         ▲
                         │ Bearer secret 上报（HTTPS）
              ┌──────────┴────────────────────────────┐
              │   unionc-agent × N                    │
              │   Linux / Windows / macOS             │
              │   本地 spool 断线续传                  │
              └───────────────┬───────────────────────┘
                              │ 可选 OTLP/protobuf
                    ┌─────────▼──────────────┐
                    │  自备的 OTLP 接收端     │
                    │  （本仓库不提供部署物） │
                    └────────────────────────┘
```

### 数据流

**上报路径**（Agent → 服务端）
1. Agent 定期采样（默认 10 秒），温度等慢速指标按 `slow_interval_seconds` 降频
2. 常驻 `run` 先把每份采样持久写入本地 spool（默认上限 64 MiB）
3. 独立投递 worker 按 FIFO 发送并在 ACK 后删除，每轮最多 32 批；失败的报文留待重试
4. 服务端校验报文 → 写入 `agent_metric_reports` → 更新 `monitored_hosts` 当前状态

**读取路径**（浏览器 → 服务端）
1. 列表 / 历史只读**摘要数值列**，完全不触碰完整报文 JSON
2. 详情接口通过 `latest_report_id` 主键 JOIN 取完整报文
3. 服务状态与本机资源都读**后台任务维护的快照**，HTTP 不触发采样

### 请求生命周期

中间件的分层是刻意的——安全头必须覆盖鉴权失败与 panic 兜底的响应，因此挂在最外层：

```
请求
 └─ assign_request_id（忽略客户端值，生成服务端 UUID；响应回写同一值）
     └─ TraceLayer（结构化日志）
         └─ 安全响应头（对所有业务响应生效，含 4xx/5xx）
             └─ 全局兜底上限 1 MiB（认证/Agent/监控控制面更小）
                 ├─ 控制台路由 ─ require_auth
                 │    ├─ 公开路径直接放行（health / ready / login）
                 │    ├─ /api/events 走一次性票据
                 │    ├─ 其余：会话 Cookie → 读锁查会话 → 数据库可用性 → CSRF
                 │    └─ 建立审计上下文（操作者 + 服务端 request id）后进入 handler
                 └─ Agent 路由 ─ 不走会话中间件
                      └─ 反代契约 → 匿名限流 → 凭据校验 → 才解析 body
```

数据库可用性检查带 1 秒 TTL 的单航缓存，避免每个请求都往库里打一次 `SELECT 1`；
过期时并发请求只触发一个探测，其余等待并复用结果。它供公开的 `/api/ready` 与确实需要库
的管理面路径（`/api/services`、`/api/monitoring`、`/api/events`、`/api/audit-logs`）共享。

---

## 3. 组成部分

### 3.1 服务端 `server`

Rust + axum + 内嵌 SQLite。约 10,600 行源码（不含测试）。

代码**按功能组织，不按技术分层**：一个功能就是一个目录，改一个行为不必在多个技术分层
之间跳转。

```
server/src/
├─ main.rs          进程入口、日志、优雅关停；数据库维护子命令
├─ startup.rs       启动编排：解析数据目录 → 载配置 → 初始化/校验当前 schema → 建状态 → 拉后台任务
├─ state.rs         跨功能共享状态
├─ error.rs         统一错误类型与 HTTP 映射
│
├─ monitoring/      只读主机监控
│  ├─ model/          报文类型、校验、指标摘要计算
│  ├─ http/           Agent 配对/上报 + 控制台查询/撤销
│  └─ store/          配对、遥测、主机和保留期持久化
│
├─ sunshine/        Sunshine 主机管理
│  ├─ model.rs        请求响应类型
│  ├─ client.rs       上游 HTTP 客户端（带响应体大小限制）
│  ├─ status.rs       并发状态探测
│  └─ http/           主机 CRUD、API 代理（体量大，拆为子目录）
│
├─ auth/            管理员认证
│  ├─ model.rs        登录/改密类型
│  └─ http.rs         登录、会话、限流、改密
│
├─ system/          本机资源与健康探针
│  ├─ model.rs        资源快照类型
│  ├─ resources.rs    后台采样器（唯一采样者 + 快照）
│  └─ http.rs         健康、就绪、资源与 SSE
│
├─ http/            路由装配与全局中间件
│  ├─ mod.rs          路由树组合
│  ├─ access_control.rs  会话认证、SSE 票据、CSRF 双提交
│  └─ security_headers.rs 全局安全响应头
│
├─ infra/           与业务无关的基础设施
│  ├─ database/       当前 schema、配置持久化、审计日志
│  ├─ secrets.rs      AES-256-GCM 密钥环
│  ├─ paths.rs        数据目录解析
│  ├─ http_client.rs  共享上游客户端
│  └─ network.rs      地址规范化
│
└─ config/          运行配置模型与环境覆盖
```

体量较小的功能目录仍常用同一组文件名：`model.rs` 是类型与校验，
`http.rs` 是 handler，`store.rs` 是持久化。`monitoring` 体量较大，已将这三类职责拆成
`model/`、`http/` 和 `store/` 目录，各自由 `mod.rs` 作为模块入口。

**后台任务**（均由 `startup.rs` 拉起）

| 任务 | 周期 | 作用 |
|---|---|---|
| 资源采样 | 2 秒 | 采集本机 CPU/内存/磁盘/网络，写入快照（唯一采样者） |
| Sunshine 探测 | TCP 5 秒、API 30 秒（配置变更时立即唤醒） | 快慢两个 worker（各自并发上限 8）；API single-flight 复用最新 TCP 批次，慢请求不阻塞状态/SSE |
| 内存回收 | 10 分钟 | 清理过期会话、空闲登录桶与上报令牌桶 |
| 数据库保留期 | 24 小时 | 清理超期审计日志与遥测历史（短写事务分批删除，见 [5.5](#55-sqlite-写入与数据保留)） |

SQLite 在 HTTP 监听前必须成功创建或通过当前 schema 精确校验，不存在“未配置数据库”的
空壳运行模式。内存回收
和数据库保留期任务因此每次正常启动都会创建；保留天数从 `UNIONC_RETENTION_DAYS` /
`UNIONC_TELEMETRY_RETENTION_DAYS` 读取。非整数或越界值会拒绝启动，不会静默改成默认值。

**启动顺序**（`startup::initialize`）：解析数据目录（必须最先，否则相对路径会触发
误判的"首次部署"）→ 一次性校验环境配置 → 建目录布局 → 取得数据目录的单 Server 文件锁 →
初始化密钥环 → 读/建管理员配置 → 新建或严格校验 `unionc.db` 当前 schema → 从库中装载 Sunshine 主机 → 建资源采样基线 →
组装 `AppState` → 拉起后台任务。
`unionc rekey` 是离线写操作，同样先取得数据库文件锁且不启动 HTTP；服务仍在运行时会拒绝，
必须短暂停服，重加密完成后再启动。

### 3.2 Agent `agent`

Rust，跨三平台；Windows 另含独立 GUI 托盘和原生维护程序。

| 模块 | 职责 |
|---|---|
| `main.rs` | 极薄的进程入口，转交给 `agent_app/` |
| `agent_app/` | 命令与运行时编排：采集循环、投递、退避、配对衔接与 Windows Service host |
| `config.rs` | 配置文件 + 环境变量 + 命令行；启动期契约校验 |
| `collectors/` | 采样实现；平台差异由 `#[cfg]` 完全隔离 |
| `collectors/linux_hwmon.rs` | Linux 温度（直读 hwmon，不依赖 sysinfo 的聚合） |
| `collectors/linux_gpu.rs` | Linux AMD / Intel GPU（`/sys/class/drm` sysfs） |
| `collectors/windows_gpu.rs` | Windows GPU（WDDM 性能计数器） |
| `collectors/nvidia.rs` | NVIDIA GPU（NVML），可通过 feature 关闭 |
| `spool.rs` | 断线续传队列，原子落盘 + 容量配额 |
| `pairing/` | 可恢复的浏览器配对状态机、本地 secret 与轮询 |
| `tray_support.rs` | Windows 托盘共用的参数、URL、HTTP 与转义校验；跨平台单测覆盖 |
| `transport.rs` | ACK 校验、当前 `/api/agent/v1/report` 上报、OTLP 导出、TLS 客户端构建 |
| `otlp.rs` | OTLP Metrics protobuf 子集（手写，字段编号对齐官方 proto） |
| `bin/unionc-agent-tray.rs` | Windows 托盘的极薄入口，实现位于 `windows/tray/` |
| `windows/tray/` | Windows 通知区、本机随机回环配置页、UAC 配对与 SCM 服务控制 |
| `bin/unionc-agent-maintenance.rs` | WiX MSI 维护助手的极薄入口，实现位于 `windows/maintenance/` |
| `windows/maintenance/` | 执行当前安装校验、ACL、保留卸载与 purge |

#### 3.2.1 共享线上协议 `protocol`

`unionc-protocol` 是 Server 与 Agent 共用的唯一 wire DTO crate，只包含版本化 JSON 类型，
不包含采集、校验、持久化或 HTTP 逻辑。Agent 在平台边界构造这些类型，Server 在信任边界
校验同一类型，避免两端各维护一份 CPU 位宽、速率精度或 capability 枚举而悄然漂移。

**命令**

```bash
unionc-agent run    # 常驻采集并上报（默认）
unionc-agent once   # 采样、补传全部历史积压并上报当前报文；可重试失败时留在 spool
unionc-agent probe  # 只打印本机能力报告，不联网——排查采集能力的首选
unionc-agent pair --server https://unionc.example.com # 创建本机密钥并等待浏览器激活
unionc-agent doctor # 默认只读检查配置、采集、凭据、配对和 spool
unionc-agent doctor --delivery # 显式执行真实投递检查
unionc-agent status # 只读本地身份、凭据和积压状态
```

`probe` 中的 `host.id` 是每次诊断临时生成且不持久化的 UUID，不是配对后的稳定实例 ID。
身份准备发生在采样之前：pending 状态只有 `run` 会持续轮询；`once` 与
`doctor --delivery` 会立即给出 activation URL 并失败。完全未配对的 `run` 只退避等待并
提示执行 `pair`，不会开始无身份采样。

配置文件路径通过 `--config PATH` 或环境变量 `UNIONC_AGENT_CONFIG` 指定；
两者都缺省时使用内置默认值（endpoint 指向本机回环）。

**状态目录**（Linux 默认 `/var/lib/unionc-agent`，目录 0700、文件 0600）

| 文件 | 内容 | 何时写入 |
|---|---|---|
| `host-id` | Server 分配的稳定实例 UUID | 浏览器配对成功后原子替换 |
| `agent-token` | Agent 本地生成的每实例通信 secret | 配对请求建立前生成，Server 只保存其 SHA-256 |
| `pairing-state.json` | 可恢复的配对 request、轮询 secret 与阶段 | `pair` 建立请求前写入，成功后去除临时 secret |
| `auth-state.json` | `authorized` / `reauth_required` 与原因 | 配对成功，或常驻 `run` 的凭据被拒绝时 |
| `spool/` | 断线续传队列（`*.json` 待发、`*.invalid` 隔离） | `run` 每次采样先写入；一次性投递遇可重试失败时写入 |

所有文件写入都走"临时文件 + fsync + rename + 目录 fsync"原子替换，失败路径不破坏既有内容。

**OTLP 队列**：配置了 `otlp_endpoint` 时，常驻 `run` 在 UnionC ACK 并删除 spool 后，
把报告投进容量 128 的有界队列，由独立任务异步导出；队列满时直接丢弃并告警。
`once` / `doctor --delivery` 排空旧 spool 时不导出旧报告，当前报告则在 UnionC ACK 后同步
尝试 OTLP，可能等待到请求超时。OTLP 始终是尽力而为的次要输出。

**Agent 自身健康**随报文上报：`agent.spool_pending_batches`（spool 积压批次）与
`agent.collector_errors`（本轮 `Transient`/`InvalidData` 类采集失败的能力项计数）。

**平台差异对照**

| 能力 | Linux | Windows | macOS |
|---|---|---|---|
| CPU / 内存 / 磁盘 / 网络 | ✅ sysinfo | ✅ sysinfo | ✅ sysinfo |
| 温度 | ✅ hwmon 直读 | ✅ sysinfo components | ✅ sysinfo components |
| NVIDIA GPU | ✅ NVML | ✅ NVML | — |
| AMD / Intel GPU | ✅ sysfs | ✅ WDDM 计数器 | — |
| Apple GPU | — | — | ❌ 公开 API 无稳定全局占用率 |
| TLS 后端 | rustls | native-tls | native-tls |
| 客户端证书格式 | PEM | PKCS#12 | PKCS#12 |
| 默认状态目录 | `/var/lib/unionc-agent` | `%PROGRAMDATA%\UnionC Agent` | `/Library/Application Support/UnionC Agent` |
| 受管常驻方式 | systemd | 原生 SCM Service `UnionCAgent` | LaunchDaemon |
| 桌面配置入口 | CLI | 通知区托盘 + 随机回环浏览器页（可退出） | CLI |

采集不到的能力**不会被伪装成 0**，而是在 `capabilities` 数组里以 `available: false` +
错误类别（`Unsupported` / `NotPresent` / `DriverMissing` / `PermissionDenied` /
`Transient` / `InvalidData`）如实上报，前端据此显示"不支持"而非"0%"。

### 3.3 管理台前端 `web`

React 19 + TypeScript + Vite + TanStack Query，按业务功能聚合源码和相邻测试。

| 文件 | 职责 |
|---|---|
| `main.tsx` | 挂载入口；集中设置 React Query 默认策略（关窗口聚焦重取、重试 1 次、staleTime 5 秒） |
| `app/App.tsx` | 会话门禁、侧边导航、主题切换、顶层查询和视图懒加载 |
| `app/hooks.ts` / `realtimeApi.ts` | SSE 订阅与重连、事件/轮询状态合并、本机指标滑窗 |
| `shared/api/client.ts` / `paths.ts` | 统一请求、超时、CSRF、401 广播、错误归一化和安全路径片段 |
| `shared/components/` / `shared/lib/` | 通用 UI、ErrorBoundary 与格式化函数 |
| `features/<feature>/api.ts` | 对应业务的端点调用；共享请求底座不感知具体功能 |
| `features/<feature>/types.ts` | 与该业务 Server 响应对应的类型 |
| `features/<feature>/queryKeys.ts` | 该业务的查询键，参数必须完整进入缓存键 |
| `features/overview/` | 总览：健康、本机资源、服务状态 |
| `features/monitoring/` | 主机监控：列表、实时指标、硬件详情、能力、历史曲线、配对管理 |
| `features/sunshine/` | Sunshine 主机、应用、客户端、配置管理 |
| `features/logs/` / `settings/` | Sunshine 日志与管理员改密 |
| `features/agent-activation/` | 公开激活页、严格路径解析与有限摘要 API |

管理台的查询缓存也是会话边界的一部分：注销或全局 401 会立即清空并替换整个
`QueryClient`，使旧会话尚未完成的 mutation 回调只能写回已脱离 UI 的旧 client；注销请求
完成前同时禁止新登录，避免旧 logout 与新 login 的 Cookie 写入乱序。

**数据刷新策略**

| 数据 | 周期 | 备注 |
|---|---|---|
| 服务状态 | SSE 推送（后台每 5 秒探测） | 断线时回落到 `/api/services` 的 10 秒轮询 |
| 主机列表 / 主机详情 | 10 秒 | 只有选中主机才拉详情 |
| 主机历史曲线 | 30 秒 | 曲线本身跨度大，不需要更快 |
| 健康 | 15 秒 | |
| Sunshine 日志 | 30 秒 | 每台主机调用自身 `/api/logs` |
| 本机资源 | 20 秒 | 服务端后台每 2 秒采样，这里只是取快照 |
| Sunshine 主机列表 | 检测中 1.5 秒；稳定后 30 秒 | 仅读取服务端健康快照；TLS/认证探测由唯一后台任务执行，增删改先立即更新页面 |

**视图懒加载**：首屏只需要 Overview，其余四个视图用 `React.lazy` 按需加载，
由 `Suspense` 兜住切换时的空窗。

SQLite 无需浏览器配置。若启动时数据库无法创建或不符合当前 schema，Server 直接启动失败；运行中本地
磁盘或文件异常会使 `/api/ready` 返回 503，各业务请求返回明确的持久层错误。

### 3.4 可选的 OTLP 导出

Agent 配置 `otlp_endpoint` 后，会在 UnionC 接受报告之后，把时序数值以 gzip 压缩的
OTLP/HTTP protobuf 旁路到该端点。Collector 挂掉不会让已成功的 UnionC 主上报失败；反过来
并不成立，UnionC 尚未确认的报告不会先送往 OTLP。常驻与一次性命令的队列差异见 [3.2](#32-agent)。

本仓库**不提供**观测栈的部署物。Agent 只需要一个能收 OTLP/HTTP 的端点，用什么部署、
部署在哪由使用方决定。指标清单、资源属性与接入方式见
[docs/monitoring.md](docs/monitoring.md)。

导出由 `otlp` cargo feature 门控，当前默认构建只启用 `nvidia`，不含 `otlp`。
需要用 `--features otlp` 显式构建；未编译该 feature 却为需要投递的命令配置
`otlp_endpoint` 或 `otlp_token` 时，启动校验会明确失败，不会静默忽略。

源码默认值与具体发行制品要分开理解：当前 Linux、Windows release job 使用默认 feature，
因此含 NVIDIA、不含 OTLP；macOS release job 显式使用
`--no-default-features --features otlp`，因此含 OTLP、不含 NVIDIA。

---

## 4. 功能详解

### 4.1 主机实例与一次性授权配对

默认接入流程不再让浏览器或命令行接触长期通信密钥：

1. 管理员在管理台创建一个待激活实例；Server 预留最终 `instance_id`，生成默认15分钟、
   单次使用的授权密钥（协议字段仍名为 `activation_code`），数据库只保存其 SHA-256；
2. Agent 软件由独立渠道预先安装；Windows 安装人员从通知区托盘选择“配对/重新配对”，
   其他平台或诊断场景运行 `unionc-agent pair --server https://unionc.example.com`；
3. Agent 在本机生成 256-bit agent secret 与独立 polling secret，先持久化可恢复状态，
   只把二者哈希和有限设备摘要提交给 Server；
4. Windows 用户在目标设备的本机配置页一次填写 Server 地址和授权密钥，
   提权 Agent 直接调用激活 API；CLI 和其他平台仍可打印专属
   `/agent/activate/{request_id}` 页面，由用户核对设备摘要后在公开激活页输入密钥；
5. Server 在同一事务中绑定邀请、配对请求、实例和 credential；
6. Agent 轮询到 `active` 后保存 Server 分配的 `instance_id`，继续使用现有
   `/api/agent/v1/report` 数据面。

凭据职责：

| 凭据/标识 | 持有方 | 作用 | 存储 |
|---|---|---|---|
| `instance_id` | Server、Agent | 稳定的逻辑主机身份与历史归属 | 明文 UUID，不是秘密 |
| 一次性授权密钥 | 管理员、安装人员 | 把一个待激活实例授权给一个配对请求 | Server 只存 SHA-256；短时、单次 |
| polling secret | Agent | 查询配对结果 | 明文仅在 Agent 私有 pairing state，Server 只存 SHA-256 |
| agent secret | Agent | 正常上报的 Bearer credential | 明文仅在 Agent 0600 文件，Server 只存 SHA-256 |

浏览器或本机页最终只得到 `instance_id` 和 `active` 状态，不得到 agent secret。创建配对
请求、激活提交和状态轮询都可安全重试；相同授权密钥不能绑定到第二个配对请求。

当前版本只支持 v2 一次性授权配对，不提供 register、enrollment code、静态 enrollment
token、直接 report token 或旧状态回填。`/api/agent/v1/report` 是当前数据面固定路径，
不是旧身份协议的兼容入口。完整协议、响应丢失语义和威胁模型见
[docs/agent-pairing.md](docs/agent-pairing.md)。

### 4.2 重新配对、撤销与删除边界

| 场景 | 操作 | 结果 |
|---|---|---|
| 首次接入 | 创建待激活实例并完成浏览器配对 | 新实例进入 active，Agent secret 从未经过浏览器 |
| 凭据修复/换机 | 对现有 `instance_id` 创建新邀请 | 新 credential 激活、旧 credential 撤销，历史与 instance ID 不变 |
| 主机退役 | `POST /api/monitoring/hosts/{id}/revoke` | 持久标记 revoked，拒绝全部 credential，保留身份和历史 |

撤销与物理删除严格分开。保留 revoked tombstone 是安全要求，可维持凭据拒绝状态、历史和
审计关联。恢复 revoked 实例必须由管理员显式创建绑定该 `instance_id` 的新邀请。

### 4.3 采集与投递

```
每个采集周期:
  读 spool 长度 → 采样 → 先持久入队 → 唤醒独立投递 worker

投递 worker（FIFO，每轮最多 32 批）:
  ├─ 成功              → 校验 ACK、删除队首、重置退避；可选 OTLP 异步旁路
  ├─ 400/409/413/422   → 内容或 ID 冲突，删除队首（重发必然再失败）
  ├─ 401               → 保留队首；持久 reauth_required 并停止投递
  ├─ 403               → 保留队首；持久 reauth_required 并停止投递
  ├─ 421               → 保留队首并退避（反代契约错误，不触发重新注册）
  └─ 其他暂时错误       → 保留队首并指数退避（上限 300 秒）
```

`SendError` 区分内容永久错误、未知/被替换凭据、主机生命周期撤销、credential/host_id
绑定失配和暂时错误。实例遇到 401/403 都需要管理员为同一 `instance_id` 建立新邀请并
重新配对；Agent 不调用其他身份端点自动恢复。

上图专指当前常驻 `run`。一次性的 `once` / `doctor --delivery` 会先排空已有队列，再直传
当前采样；当前采样遇可重试错误才入队。Web 撤销不主动推送；Agent 在下一次报告被拒时才写
`reauth_required`，当前进程停止投递但继续采样到有界 spool。`once` /
`doctor --delivery` 只入队并返回错误，不改授权状态。常驻进程一旦重启，会因没有
authorized reporter 在采样循环前退出并由服务管理器重试，直到重新配对。

spool 采用"临时文件 + fsync + rename + 目录 fsync"原子落盘，文件名以毫秒时间戳零填充前缀，
排序即投递顺序。反序列化失败的报文改名为 `.invalid` 隔离，且**计入容量预算**并
优先淘汰。同一状态目录的所有短时变更由进程内 mutex 与跨进程文件锁共同串行化；崩溃
遗留且已无写入锁的原子临时文件会在打开队列和容量核算时回收。

常驻 `run` 对读、写、补传三类 spool 操作分别计数。单次写失败会丢弃当前未能落盘的采样
并继续运行，同类操作只有**连续** 100 次失败才退出交由服务管理器处理。一次性的
`once` / `doctor --delivery` 遇到 spool I/O 错误会立即失败。

### 4.4 主机状态判定

```
age = now - last_seen_at
interval = latest_interval_seconds（缺省 10，钳制到 1..3600）

age ≤ max(interval × 3,  30s)   → online
age ≤ max(interval × 12, 300s)  → stale
否则                             → offline
```

`lifecycle_status=revoked` 时不再按时间计算，API 固定返回 `status=revoked`。

### 4.5 Sunshine 管理

服务端作为反向代理转发到各 Sunshine 主机的 Web API，凭据保存在服务端（AES-256-GCM
加密后入库），不下发给浏览器。支持应用增删改、配对客户端管理、配置读写、PIN 配对、
封面图片、重启、显示设备重置、本地日志尾读。

**上游主机不被信任**——即使它是管理员亲手配置的。主机可能已被攻陷，非生产环境还允许
关闭 TLS 校验（此时中间人也能构造响应）。因此代理路径上有三重约束：

| 约束 | 取值 |
|---|---|
| 上游响应体上限（流式累计，超限即断） | JSON 4 MiB / 封面 8 MiB |
| 请求体上限（转发前校验，且必须是 JSON 对象） | 应用 256 KiB / 配置 1 MiB / 改密 64 KiB |
| 封面 Content-Type 白名单 | `image/` 下的 jpeg、png、webp、gif、avif，其余降级为 `application/octet-stream` |

其余入参校验：PIN 为 4-8 位数字，客户端 uuid ≤128 字符，客户端名 ≤80 字符，
封面 key ≤512 字符、URL ≤2048 字符且必须是绝对的 http/https，应用下标 ≤10000。
所有这些都禁止控制字符。

**Sunshine 日志**按主机调用其自身 `/api/logs`，由服务端使用已加密保存的凭据代理；
页面并行查询已配置主机，不再把 UnionC 本地单一文件重复标成多台远程主机日志。
本项目面向 20 台以内的部署，30 秒刷新一次无需额外引入集中日志基础设施。

上游契约只按当前 Sunshine API 实现：应用列表必须是 `{ "apps": [...] }` 且应用字段使用
kebab-case；客户端列表必须是 `{ "status": true, "named_certs": [...] }`；单客户端解绑调用
`/api/clients/unpair`；`/api/logs` 的 `text/plain` 由 UnionC 包装成 `{ "content": "..." }`。
Server/Web 不再接受顶层数组、snake_case 应用字段、`named`/`unnamed`/`certs` 或
`lines`/`logs`/`log` 等旧响应形态。
契约核对依据为 Sunshine 官方的
[`docs/api.md`](https://github.com/LizardByte/Sunshine/blob/master/docs/api.md) 与
[`src/confighttp.cpp`](https://github.com/LizardByte/Sunshine/blob/master/src/confighttp.cpp)。

### 4.6 审计日志

所有**状态变更**操作都写入 `audit_logs`，记录动作、目标、操作者与 request id。
操作者与 request id 通过 tokio 的 task-local 上下文从鉴权中间件传递到持久化层，
handler 不需要逐个透传；无上下文时（后台任务）记为 `system`。

| 前缀 | 动作 |
|---|---|
| `monitoring.host.` | `revoke` |
| `sunshine.host.` | `create`、`update`、`delete` |
| `sunshine.app.` | `save`、`close`、`delete` |
| `sunshine.client.` | `pair`、`unpair`、`unpair_all`、`update` |
| `sunshine.` | `config.save`、`password.update`、`cover.upload`、`system.restart`、`display.reset` |

只读查询**不**记审计——那会让日志被轮询淹没，反而掩盖真正的变更。

---

## 5. 技术设计

### 5.1 技术选型

| 层 | 选型 | 理由 |
|---|---|---|
| 服务端 | Rust + axum 0.8 | 单二进制、无 GC 停顿、编译期排除整类错误 |
| 数据库 | 内嵌 SQLite（bundled） | 单节点自包含；JSON 文本存报文体、数值列存摘要 |
| DB 驱动 | sqlx-core / sqlx-sqlite | 异步连接池，不引入 ORM；固定 SQLite 功能集 |
| 采样 | sysinfo 0.39 + 直读 `/proc`、`/sys` | 跨平台基础 + 平台特化补足 |
| 加密 | aes-gcm | AEAD，认证标签保证用错密钥必然解密失败而非产出垃圾 |
| 口令 | bcrypt | 成熟、带自适应成本 |
| 前端 | React 19 + TanStack Query | 声明式数据同步，轮询/失效逻辑集中 |

### 5.2 贯穿全项目的三条原则

**一、差值类指标必须有一个"唯一采样者"**

CPU 使用率、网络与磁盘吞吐都是两次采样之间的差值，而且**读取即消费**——一旦读走
增量，基线就前移了。若把采样放在请求处理里，两个并发观察者会互相吃掉对方的窗口，
后到的直接读到 0。因此服务状态探测与本机资源采样都是"唯一后台任务采样 + 多方读快照"。

**二、热路径不做 O(N) 工作**

- 主机列表/历史只读摘要数值列，不反序列化 30-50KB 的报文体
- 上报限流只做一次哈希查找，桶回收交给周期任务
- 会话鉴权走读锁快路径，过期清理交给周期任务
- Sunshine 探测集中在进程级后台任务中，与浏览器标签数无关

**三、无法采集的指标如实标记，不填 0**

`capabilities` 数组承载每项能力的可用性与失败原因。0 和"读不到"在监控产品里是
完全不同的两件事。

### 5.3 当前数据库基线

- 唯一的 `server/schema/sqlite.sql` 由 `include_str!` 编译进二进制，部署无需另行分发 SQL 文件；
- 空数据目录在一个 `BEGIN IMMEDIATE` 事务中创建完整当前 schema，失败整体回滚；
- 已有数据库必须与当前版本、基线 SHA-256、表/索引/触发器定义精确一致；
- 不接受内置版本的有序前缀，不运行追加 migration，不补列、不回填旧 credential；
- 后续 schema 变化直接替换当前基线，并要求部署方全新建库和重新配对。

`schema_metadata` 只记录这一份当前基线的版本、应用版本与指纹，用于拒绝错误或非当前数据库，不是
升级账本。可变 Sunshine 主机仍逐行保存在 `external_hosts`。

### 5.4 密钥环与轮换

密文格式 `enc:v2:<key_id>:<base64(nonce||ciphertext)>`。加密**恒用当前密钥**，
解密按密文携带的 key_id 在密钥环中查找。批量重加密需要一次短暂停服：

```bash
# 1. 在 /etc/unionc/unionc.env 中把旧密钥移入历史并启用新密钥，重启后验证新旧密文
UNIONC_SECRET_KEY=<新密钥 Base64>
UNIONC_SECRET_KEY_ID=2025q3
UNIONC_SECRET_KEY_PREVIOUS="2025q1:<旧密钥 Base64>"
sudo systemctl restart unionc

# 2. 确认新旧密文都能读取后停服；用与服务相同的密钥环境执行（跑完即退出，不启 HTTP）
sudo systemctl stop unionc
sudo systemd-run --quiet --wait --pipe --collect \
  --uid=unionc --gid=unionc -p WorkingDirectory=/var/lib/unionc \
  -p Environment=UNIONC_DATA_DIR=/var/lib/unionc \
  -p EnvironmentFile=/etc/unionc/unionc.env \
  /usr/bin/unionc rekey
sudo systemctl start unionc

# 3. 从 unionc.env 移除 UNIONC_SECRET_KEY_PREVIOUS 并重启，旧密钥彻底退役
sudo systemctl restart unionc
```

漏掉第 1 步直接换密钥会**拒绝启动**并指出缺失的 key_id，而不是静默读不出数据。

加密面只有 `external_hosts.secret` 里的 Sunshine 密码，离线 `rekey` 只定向更新这些密文，
不会重写无关配置或主机行。密钥环还会拒绝几类明显
的误配：历史密钥与当前密钥同 id、历史密钥 id 重复、密钥材料不是 32 字节。key_id 相同
但材料不同的情形由 AES-GCM 的认证标签兜住——解密必然失败，不会产出垃圾明文。

### 5.5 SQLite 写入与数据保留

运行库固定为 `<UNIONC_DATA_DIR>/unionc.db`，启用 `foreign_keys=ON`、WAL、
`synchronous=FULL` 和 30 秒 busy timeout，文件权限强制为 0600。WAL 允许读取与一个写事务
并行，但 SQLite 对同一个数据库文件仍只有一个写入者；项目因此用进程级写门控避免自身请求
反复撞上 `SQLITE_BUSY`，并用 `BEGIN IMMEDIATE` 在读改写事务开始时就取得写入权，避免
deferred transaction 升级死锁。

遥测历史每批删除 10 000 行、批间让出 50 毫秒、单次最多 1 000 批；审计日志每批删除
1 000 行并在批间主动让出调度。两者都让每批独立提交并累计精确删除数，目的是缩短独占
写入时段，让 Agent 上报和配置写入可以在批次之间进入。删除后的页进入 SQLite freelist，
不承诺活动数据库文件立即缩小；`backup --output` 通过 `VACUUM INTO` 生成紧凑且一致的快照。
不得在运行中裸复制 `unionc.db`，也不得手工删除相邻的 `-wal`/`-shm` 文件。

这一设计面向单 Server、小规模自托管部署。当前产品目标约 20 台主机；若需要多 Server
共享写入、网络文件系统、PITR/流复制，或持续几十次写入每秒与数千万历史行，应使用独立的
大规模存储方案，而不是强行共享 SQLite 文件。

---

## 6. HTTP 接口契约

所有端点都在 `/api` 下（`GET /` 除外，它只返回一行提示文本且同样需要会话）。
全局请求体兜底上限 1 MiB；登录/改密 4 KiB，当前 v1 上报 512 KiB，
监控控制面与 v2 配对 16 KiB，Sunshine 路由 1 MiB。各路由的更小上限优先于全局兜底。

### 6.1 公开端点（无需认证）

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/health` | 存活探针：状态、版本、运行时长 |
| GET | `/api/ready` | 就绪探针：内嵌数据库与数据目录都可用才返回 200 |
| POST | `/api/auth/login` | JSON 登录 |
| GET | `/api/agent/v2/pairing-requests/{id}` | 激活页核对有限设备摘要，不返回任何 credential |
| POST | `/api/agent/v2/activate` | 用一次性激活码把待激活实例绑定到 Agent pairing request |

### 6.2 Agent 端点（按端点鉴权）

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| POST | `/api/agent/v1/report` | 配对生成的每主机 Bearer secret | 上报只读快照，请求体上限 512 KiB |
| POST | `/api/agent/v2/pairing-requests` | 无；匿名入口限流 | 创建浏览器配对请求，Agent 只提交本地 secret 的哈希；请求体上限 16 KiB |
| POST | `/api/agent/v2/pairing-requests/{id}/status` | `Pairing <polling-secret>` | 查询 waiting/active/denied/expired；active 时返回 instance ID；请求体上限 16 KiB |

Agent 端点都**不**走会话中间件与 CSRF；它们按各自协议鉴权。上报的请求体以原始
`Bytes` 提取、在认证与限流**之后**才反序列化——用 `Json<AgentReport>` 提取器会让
未认证请求也能驱动一次完整的 512 KiB JSON 解析。

响应语义：上报成功 **202**。重放同一 `report_id` 仍返回 202，但 `accepted` 为
`false`——幂等，不产生第二行。

### 6.3 控制台端点（会话 Cookie + CSRF）

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/auth/me` | 当前登录用户 |
| POST | `/api/auth/logout` | 注销会话 |
| POST | `/api/auth/change-password` | 改密（与登录共用配额；新密码 12 字符 ~ 72 字节） |
| GET | `/api/audit-logs?limit=&before_id=` | 管理员分页读取/导出审计记录（按 id 游标向旧记录翻页） |
| GET | `/api/system/resources` | 本机资源快照（读后台快照，不触发采样） |
| GET | `/api/services` | Sunshine 服务状态快照（同上） |
| GET | `/api/events` | SSE 状态推送（用短效票据鉴权，非会话 Cookie） |
| POST | `/api/events/ticket` | 签发 60 秒一次性 SSE 票据 |
| GET | `/api/monitoring/hosts` | 主机列表，`?limit&offset`（默认 200，上限 1000），响应含 `total` |
| GET | `/api/monitoring/hosts/{id}` | 主机详情（含完整报文） |
| GET | `/api/monitoring/hosts/{id}/history` | 历史曲线，`?from&to&limit`（默认 300，上限 1000） |
| GET/POST | `/api/monitoring/agent-instances` | 列出邀请 / 创建待激活实例；创建响应才含一次性激活码 |
| DELETE | `/api/monitoring/agent-instances/{invite_id}` | 取消尚未消费的激活邀请 |
| POST | `/api/monitoring/hosts/{id}/revoke` | 持久撤销实例及全部 credential，保留历史与 tombstone |

主机列表**必须分页**且必须告知总数：这是一条随部署规模线性增长的响应（每台主机都带
capabilities 数组），而控制台每 10 秒轮询一次。`COUNT(*) OVER()` 在同一次扫描里带出
总数，因此仍是一次往返——截断而不告知比不分页更糟。

监控控制面所有带请求体的写接口都使用 16 KiB 上限；登录与改密单独收紧到 4 KiB。

### 6.3.1 Sunshine 端点

| 方法 | 路径 | 说明 |
|---|---|---|
| GET/POST | `/api/services/sunshine/hosts` | 列出 / 新建主机 |
| PATCH/DELETE | `/api/services/sunshine/hosts/{id}` | 部分更新 / 删除主机 |
| GET | `/api/services/sunshine/hosts/{id}/status` | 最近一次后台可达性快照 |
| GET | `/api/services/sunshine/hosts/{id}/api-logs` | Sunshine 自身的运行日志 |
| GET/POST | `/api/services/sunshine/hosts/{id}/apps` | 应用列表 / 新增修改 |
| POST | `/api/services/sunshine/hosts/{id}/apps/close` | 结束当前会话 |
| DELETE | `/api/services/sunshine/hosts/{id}/apps/{index}` | 删除应用 |
| GET | `/api/services/sunshine/hosts/{id}/clients` | 已配对客户端 |
| POST | `/api/services/sunshine/hosts/{id}/clients/unpair` | 取消单个配对 |
| POST | `/api/services/sunshine/hosts/{id}/clients/unpair-all` | 取消全部配对 |
| POST | `/api/services/sunshine/hosts/{id}/clients/update` | 启用 / 禁用客户端 |
| POST | `/api/services/sunshine/hosts/{id}/pin` | PIN 码配对 |
| GET/POST | `/api/services/sunshine/hosts/{id}/config` | 读取 / 保存 Sunshine 配置 |
| GET | `/api/services/sunshine/hosts/{id}/config/locale` | 本地化配置 |
| GET | `/api/services/sunshine/hosts/{id}/covers/{index}` | 封面图片（MIME 走白名单） |
| POST | `/api/services/sunshine/hosts/{id}/covers/upload` | 按 URL 上传封面 |
| POST | `/api/services/sunshine/hosts/{id}/restart` | 重启 Sunshine |
| POST | `/api/services/sunshine/hosts/{id}/reset-display` | 重置显示设备配置 |

### 6.4 报文契约

上报报文中的实测 `interval_seconds` 必须落在 **0.1 ~ 3600** 秒，权威入口是
`server/src/monitoring/model/mod.rs` 的 `AgentReport::validate()`。Agent 配置使用整数秒（最小 1），
并在 `agent/src/config.rs` 中保证 jitter 后的最坏实测周期不超过 3600；SQLite schema 只用
`0 < interval_seconds <= 3600` 做粗粒度存储防线，不能代替 HTTP 入口的精确下限。修改任一层
都必须联合评估另外两层和 Agent 的 jitter 边界测试，而不是机械地假设三者数值完全相同。

Agent 侧在**启动时**就校验配置的间隔是否越界，而不是等运行时反复收到 400——后者会被
判为永久内容错误并直接丢弃，只在日志里留下一串 400 和周期性数据缺口。

**结构与数值校验**

| 项 | 约束 |
|---|---|
| `schema_version` | 必须为 1 |
| `report_id` / `host.id` | 必须是 UUID |
| `host.agent_version` | 必须与当前 Server package version 完全一致；旧 Agent 明确返回 400 |
| `collected_at` | 不得超前服务器时间 5 分钟 |
| 设备数量 | 网卡 / 磁盘 ≤1024，传感器 ≤4096，GPU ≤128，能力 ≤256 |
| 百分比类 | 有限且落在 0~100（CPU 总体与逐核、GPU 利用率） |
| 速率类 | 有限且非负 |
| 温度类 | 有限且落在 -273.15 ~ 1000 |
| 内存 / 磁盘 / 显存 | 已用与可用不得超过各自总量 |
| `cpu.logical_count` | 必须为正 |

**文本字段必须穷尽有界**。数量约束管不住内容长度：512 KiB 的 body 之内，一台被攻陷的
Agent 可以把配额全部塞进任意一个不限长的字符串，而这些文本会落库并原样回传控制台。
校验分两类：

- `validate_text`（身份字段，**必须非空**）：`host.name` ≤255、`host.os` ≤64、
  `host.arch` ≤64、`host.agent_version` ≤128、`network.name` ≤255、`disk.name` ≤1024、
  `disk.mount_point` ≤4096、`capability.name`/`source`/`error_kind` ≤128、
  `capability.message` ≤1024
- `validate_optional_text`（描述性字段，**允许为空**）：`host.os_version`/`kernel_version`
  ≤128、`disk.file_system` ≤128、`temperature.id`/`label` ≤255、`temperature.source` ≤64、
  `gpu.id`/`gpu.name` ≤255、`gpu.vendor`/`gpu.source` ≤64

两者都禁止控制字符，区别只在是否允许空串。这个区分不是形式主义：采集侧**确实会**产出
空串——伪文件系统的 `file_system`、无标签传感器的 `label`（`temperature.id` 还会回退到
这个空 label）、取不到版本号时的 `os_version`。对它们套用"必须非空"会让一份完全正常的
报文被整体拒绝，即用可用性换一个并不存在的安全收益。真正要守的是**上界**。

> 校验代码里字段是**逐个列全**的，而不是写一句"其余字段同理"。概括性的描述无法被
> 核查，新增字段时也不会有任何东西提醒你补上——列全虽然啰嗦，但漏掉一个是看得见的。

### 6.5 错误响应

统一 JSON 结构，`code` 是稳定的机器可读标识，客户端不应解析 `message`：

```json
{ "code": "too_many_requests", "message": "登录尝试过于频繁，请一分钟后再试" }
```

| `code` | 状态码 | 含义 |
|---|---|---|
| `bad_request` / `invalid_host` / `local_config_*` | 400 | 入参或本地配置不合法 |
| `unauthorized` | 401 | 未认证；Agent token 未知、失效或被重配替换 |
| `forbidden` / `agent_revoked` | 403 | CSRF/授权或身份绑定校验失败；`agent_revoked` 特指主机生命周期已被管理员持久撤销 |
| `misdirected_request` | **421** | 请求没走对链路——反代契约头或独立代理证明缺失/不匹配 |
| `not_found` | 404 | 资源不存在 |
| `conflict` | 409 | 状态冲突（如邀请/配对状态冲突、report_id 属于别的主机） |
| `too_many_requests` | 429 | 触发限流 |
| `service_unavailable` / `database_unavailable` | 503 | 服务尚未就绪，或本地数据库因磁盘/权限/损坏暂不可用 |
| `process_error` / `upstream_error` | 502 | 上游 Sunshine 主机出错 |
| `storage_error` / `database_error` / `internal_error` | 500 | 内部错误 |

内部错误（IO / SQL / 未分类）对外只返回通用描述，完整信息记入日志，不泄露路径或 SQL。
**421 与 403 的区分是刻意的**，理由见 [8.4](#84-传输与响应加固)。

---

## 7. 数据模型

| 表 | 主键 | 用途 |
|---|---|---|
| `schema_metadata` | `schema_version` | 当前唯一 schema 基线版本、应用版本与 SHA-256 指纹 |
| `audit_logs` | `id` | 审计：动作、目标、明细、操作者、request_id |
| `external_hosts` | `(kind, host_id)` | Sunshine 主机（地址独立成列以走 CHECK，密码独立加密） |
| `monitored_hosts` | `host_id` | 被监控主机、当前状态和 active/revoked 生命周期 tombstone |
| `agent_credentials` | `credential_id` | 每实例 pairing credential 的签发与撤销状态 |
| `agent_instance_invites` | `invite_id` | 管理员创建的待激活实例、激活码哈希、预留 instance ID 与状态 |
| `agent_pairing_requests` | `request_id` | Agent 发起的短时配对请求、设备摘要和两个本地 secret 的哈希 |
| `agent_metric_reports` | `report_id` | 全部历史报告：校验过的 JSON 文本 + 9 个摘要数值列 |

**索引**：`audit_logs(created_at)`、`monitored_hosts(last_seen_at DESC)`、
`agent_metric_reports(host_id, collected_at DESC, report_id)`（历史查询）、
`agent_metric_reports(received_at)`（保留期清理），以及配对请求/邀请的过期与 active
credential 局部索引。

**数据库侧的校验**并不只依赖应用层：全部表使用 SQLite `STRICT` 模式；UUID 以 36 字符
文本存储并在 Rust 写入入口做规范解析；时间统一存 Unix 微秒；`config`/`capabilities`/`payload` 通过 `json_valid` 与 `json_type`
约束对象或数组；credential、授权码与 polling secret 哈希用长度及 `GLOB` 约束为 64 位小写十六进制；
`interval_seconds` 用 `(0, 3600]` 粗粒度 CHECK 拦截损坏值，应用入口再执行 `[0.1, 3600]`
精确契约。`agent_credentials.token_hash` 唯一，因此一次索引查找即可完成上报鉴权。主机
地址的完整 IP/域名规则在 Rust 写入入口统一校验。

**Sunshine 主机按行持久化**：创建、更新和删除只修改目标 `external_hosts` 行，并把对应
审计事件放在同一个事务中提交；PATCH 未携带密码时，已有密文字节保持不变。数据库提交后
才发布新的内存快照，失败时数据库、API 与运行状态保持原状。Sunshine 路由的请求体上限为
1 MiB，与全局兜底相同，但由子路由显式声明其接口契约。

关键设计：

- **报文体只存一份，且只为最新一份保留**。`payload` 在代码里只有一个读取点——详情
  接口经 `latest_report_id` 主键 JOIN。因此写入时就判断这份报文会不会成为最新：
  会则带 payload 落盘并把上一份置空，补传的历史报文直接以 NULL 插入。
  历史 A/B 实测 5000 行：17.3 MiB → 1.4 MiB，**节省 12.4 倍**。
  该数字只说明“历史不重复保存完整 JSON”的量级收益；SQLite 的实际容量必须用当前版本、
  真实报文和目标文件系统重新压测，不能直接套用该数值。
- 代价是失去"回溯任意历史时刻的完整硬件快照"，但该能力当前没有接口暴露。若将来需要，
  正确做法是按更长间隔留样，而不是每个采集周期都留一份。
- **摘要列只存在于报告表**，主机侧通过主键 JOIN 取用，同理避免漂移。摘要口径由 Rust
  侧 `metric_summary()` 在写入时单点计算，SQL 只负责存放。多设备取**最忙的单个**
  （`reduce(f64::max)`）而非求和，避免 veth/bridge、bind mount 造成重复计数。
- 外键 `ON DELETE SET NULL` + 清理逻辑显式排除被引用行，**两道保险**确保长期离线
  主机的详情页不会因保留期清理而变空白。
- `agent_metric_reports.host_id` 仍有 `ON DELETE CASCADE` 作为数据库完整性约束，但正常
  撤销不再物理删除主机行，以保留凭据拒绝状态、历史和审计关联。
- **上报对主机行只做一次写入**。三组列各有条件，用 `CASE` 在同一条 UPDATE 里表达：
  identity/capabilities 只在确实变化时替换；`last_seen_at` 只在报文代表当前状态时刷新；
  `latest_*` 指针只在新报文推进。SQLite 是单写者，减少同一事务内的写语句和被修改页面
  能直接缩短写锁持有时间；相关集成测试钉住每份上报的写入契约。
- **旧报文和重复 report ID 不回写主机状态**。断线恢复时 spool 会按时间升序补传一批历史报文，重放也
  可能把同一份再送一次。写入前先比较 `collected_at`：只有不早于当前 latest 的报文才
  更新主机行，否则一份小时前的报文能把刚更新的能力清单覆盖回去，而 `last_seen_at=NOW()`
  会让任何一次重放都把离线主机刷成 online。

---

## 8. 安全模型

### 8.1 认证与会话

- 管理员口令用 bcrypt（DEFAULT_COST）
- 会话令牌为随机 UUID，仅存进程内存，有效期 7 天；重启或改密后失效
- 会话 Cookie 为 HttpOnly + SameSite=Strict，生产环境加 Secure 与 `__Host-` 前缀
- 未知用户名也会走一次 bcrypt（对比 dummy hash），避免时序差异泄露用户名是否存在
- 并发的 bcrypt 运算由一个容量 4 的信号量约束，防止密码校验打满 CPU
- SSE 用一次性短效票据（60 秒、随机 UUID、验过即删）而非会话 Cookie——
  `EventSource` 不支持自定义请求头
- Web 注销或收到 401 时替换整套会话 QueryClient，并在 logout 完成前阻止新登录，避免旧
  mutation 私有快照或晚到的注销响应污染下一会话

**口令规则**：至少 12 个**字符**，至多 72 **字节**。上限不是形式主义——bcrypt 只取前
72 字节且**静默截断**，不报任何错。后果是一个隐蔽的认证等价类：前 72 字节相同的两个
不同口令互相可以登录，而用户以为自己设了一个 100 字符的强口令。口径是字节而非字符，
因为一个汉字占 3 字节，24 个汉字就到上限了。bootstrap 口令复用同一套校验。

### 8.2 CSRF

双提交模式：登录时下发**每会话随机**的 CSRF 令牌到一个**非 HttpOnly** 的 Cookie，
前端读取后回填到 `X-CSRF-Token` 头，服务端与会话存储的令牌做恒定时间比较。
只有非幂等方法（GET/HEAD/OPTIONS 之外）需要该头。

非 HttpOnly 是双提交模式的必要条件，不降低安全性——跨站页面读不到本站 Cookie。

### 8.3 限流

| 对象 | 配额 | 分桶 |
|---|---|---|
| 登录 / 改密 | 5 次/分钟/**(IP + 用户名)**，10 次/分钟/IP，600 次/分钟全局 | 三层独立判定 |
| Agent 注册 | 10 次/分钟/IP，600 次/分钟全局 | IP + 全局 |
| Agent 上报 | 令牌桶，容量 64，补充 16/秒 | 每主机 |

**分桶键里必须带 IP**。只按用户名计数确实挡住了暴力破解，但同时制造了一个任何人都能
触发的**账号锁定开关**：管理员用户名默认就是 `admin`，攻击者持续发失败请求就能让真正
的管理员在整个窗口内无法登录（按 IP 的桶救不了——三层是独立判定的，用户名桶先满请求
就已被拒）。带上 IP 之后，洪水只会锁住攻击者自己的组合。全局桶同理只作 bcrypt 资源
耗尽的兜底，阈值必须显著高于前两层，否则它本身就成了那个开关。

改密与登录**共用**同一套配额，否则改密就成了绕过登录限流的旁路——而它每次调用要跑
两次 bcrypt，是全站单位请求 CPU 开销最高的一个。

上报用令牌桶而非固定窗口，是为了容纳断线恢复时"一次补传 32 批"的**合法**突发；
补充速率 16/秒高于契约允许的最快合法上报速率（最小间隔 0.1 秒即 10 次/秒），
因此正常配置永远不会被误伤。

**匿名配对/激活限流前置于昂贵解析和数据库写入**。反过来的话，错误请求可免费消耗解析与
查询资源，一个本可观测、可阻断的攻击面会变成资源耗尽入口。

客户端 IP 从 `X-Forwarded-For` 取**最右**一项——那是离本服务最近的可信代理写入的。
取最左项会让攻击者每次换一个伪造 IP 就绕过限流。该实现假定前面**恰好有一层**可信
反代；若在反代之前再叠加 CDN，必须改为"从右往左跳过 N 跳"，否则取到的是内网地址。

### 8.4 传输与响应加固

- 生产环境强制绑定回环，只能经反向代理访问（非回环绑定直接拒绝启动）
- 生产环境要求登录、改密与 Agent API 同时携带 `X-Forwarded-Proto: https`、
  `X-Forwarded-For`，以及由可信反代覆盖写入、与服务端 `UNIONC_PROXY_SECRET`
  恒定时间匹配的 `X-UnionC-Proxy-Secret`；缺任一项或证明不匹配均返回 **421**
- 全局安全响应头：`nosniff`、CSP（`default-src 'none'; frame-ancestors 'none'; base-uri 'none'`）、
  `X-Frame-Options: DENY`、`Referrer-Policy: no-referrer`、
  `Cross-Origin-Resource-Policy: same-origin`，生产额外下发 HSTS（一年、含子域、preload）
- 安全头挂在**最外层**中间件，因此鉴权失败与 panic 兜底的响应同样带上；已被 handler
  显式设置过的头不覆盖
- Sunshine 封面响应的 Content-Type 收敛到图片白名单，其余降级为
  `application/octet-stream`，并强制 `Content-Disposition: inline`
- 上游响应体有大小上限（JSON 4 MiB / 封面 8 MiB），流式累计判断，超限即断开

**为什么 421 而不是 403**。Agent 报告的 403 表示主机生命周期持久撤销，或当前有效
credential 与 body 中的 `host_id` 绑定失配；两者都要求人工纠正身份状态并进入
`reauth_required`。它绝不表示反代配置失败。若反向代理漏透传请求头也返回 403，一次部署
配置错误就会把整批 Agent 伪装成“身份异常”。421 的语义是链路走错且可重试：反代修好后
同一份报文原样重发即可。

**为什么 XFF 也是硬要求**。若把它做成软降级（取不到 IP 就放行），三层配额里的两层会
同时失效，只剩全局兜底。危险之处在于**这不产生任何信号**：一份只配了 XFP 的反代能通过
启动检查、能正常登录、日志里也没有异常，而防爆破能力已经放宽了两个数量级。代理证明则
解决另一个问题：回环地址上的任意本机进程都能伪造 XFP/XFF；只有掌握独立共享值的反代
才能建立可信边界。共享值必须由反代**覆盖**客户端同名头，不能追加，也不能复用数据库主密钥。

- **安全头为什么 API 也需要**：UnionC 同时是一个代理，封面端点会把上游字节原样转发。
  只要上游能影响响应的类型或内容，浏览器就可能把它当文档渲染——而且是在 UnionC 自己
  的源上执行，能读到刻意非 HttpOnly 的 CSRF cookie。

### 8.5 静态数据保护

- 数据目录 0700，配置文件、密钥文件、`unionc.db` 与备份快照均按 0600 保护
- Sunshine 密码用 AES-256-GCM 加密后入库；部署配置只来自环境，不在库内保存第二份副本
- 所有激活码、polling secret 与 Agent credential 只存 SHA-256
- 生产环境主密钥必须由 `UNIONC_SECRET_KEY` 提供，不允许落盘自动生成

数据库仍包含主机资产、遥测历史、审计记录和凭据哈希，应按敏感数据管理。离机备份必须
沿用同等级访问控制；备份数据库与 `UNIONC_SECRET_KEY`（含轮换期历史密钥）应分开保管，
同时纳入恢复演练，否则只有快照而没有密钥时无法解密 Sunshine 密码。

### 8.6 Agent 权限

systemd unit 已做完整硬化：`CapabilityBoundingSet=` 为空、`NoNewPrivileges`、
`ProtectSystem=strict`、`PrivateDevices`、`MemoryDenyWriteExecute`、
`SystemCallArchitectures=native`，仅 `/var/lib/unionc-agent` 可写。

> **GPU 例外**：`PrivateDevices=yes` 会屏蔽 `/dev/nvidia*` 与 `/dev/dri`。需要 GPU
> 指标时安装随包分发的 drop-in（见 [10.4](#104-启用-gpu-采集)）。

---

## 9. 部署

### 9.1 拓扑

```
Internet → 反向代理（TLS 终止）→ UnionC（127.0.0.1:8081）
                                      ↓
                         /var/lib/unionc/unionc.db
```

SQLite 与 Server 位于同一台 Linux 主机的本地磁盘。该拓扑不支持多 Server 共享数据库、
活动库放 NFS/SMB，或依靠数据库流复制实现高可用；需要这些能力时应重新评估存储架构。

反向代理必须透传 `X-Forwarded-Proto` 与 `X-Forwarded-For`，且**自身要追加真实对端
地址**到 XFF 末尾；还必须覆盖写入 `X-UnionC-Proxy-Secret`。反代进程与 UnionC
进程分别通过安全的服务环境获得同一个 `UNIONC_PROXY_SECRET`（64 位小写十六进制）。

| 示例配置 | 用途 |
|---|---|
| `docs/examples/caddy/Caddyfile.console.example` | 管理台：TLS 终止 + 静态前端托管 + API 反代（必需） |
| `docs/examples/caddy/Caddyfile.agent-api.example` | 独立 Agent 域名 + mTLS（可选） |
| `docs/examples/caddy/Caddyfile.telemetry.example` | OTLP 遥测入口 + mTLS（可选，见 `docs/monitoring.md`） |

前端是纯静态产物，**由反向代理提供**，UnionC 服务端只提供 API。

### 9.2 服务端安装

```bash
# 必须在工作区根执行——合并为 Cargo workspace 后产物在根 target/
cargo build --release -p unionc
NFPM_ARCH=amd64 server/packaging/linux/build-packages.sh
sudo dpkg -i unionc_0.3.2_amd64.deb
```

两个 Linux 打包脚本都要求 `nfpm` 位于 `PATH`，或由 `NFPM_BIN` 指向可执行文件；正式发布
工作流固定使用 nFPM v2.47.0。

> ⚠ 不要在 nfpm 的 `contents[].src` 里写 `${VAR}`。nfpm 只对 `name`/`arch`/`version`
> 这类标量字段做环境变量展开，`src` 原样保留，实测（v2.44.0）直接失败：
> `glob failed: ./target/${RUST_TARGET}/release/unionc: no matching files`。
> 交叉编译请先把产物 `install` 到 `target/release/` 再打包。

包会安装：

| 内容 | 位置 |
|---|---|
| 二进制 | `/usr/bin/unionc` |
| systemd unit | `/usr/lib/systemd/system/unionc.service` |
| 环境配置 | `/etc/unionc/unionc.env`（0640 root:unionc，`config|noreplace`） |
| 数据目录 | `/var/lib/unionc`（0700 unionc:unionc） |
| 内嵌数据库 | `/var/lib/unionc/unionc.db`（显式首次 bootstrap 创建，0600 unionc:unionc） |

环境文件中的 `UNIONC_PACKAGE_VERSION=0.3.2` 是不可修改的包归属标记；裸二进制部署不要求
设置它。包安装还以 `/var/lib/unionc-package` 中绑定当前版本与实际 UID/GID 的 root-only
marker 校验账户和数据目录。缺少当前标记的既有环境文件、同名账户或数据目录一律拒绝接管；
普通卸载会保留数据和 marker，只允许同一 0.3.2 重装。

unit 显式设置 `UNIONC_DATA_DIR=/var/lib/unionc` 与 `WorkingDirectory`，并附一组
systemd 硬化：`CapabilityBoundingSet=` 为空、`NoNewPrivileges`、`ProtectSystem=strict`、
`PrivateDevices`、`MemoryDenyWriteExecute`、`SystemCallFilter=@system-service`、
`StateDirectoryMode=0700`、`UMask=0077`，仅 `/var/lib/unionc` 可写。

首次启动：

```bash
# 1. 分别生成数据库主密钥与反向代理证明（不可复用）
openssl rand -base64 32
openssl rand -hex 32

# 2. 编辑 /etc/unionc/unionc.env，填入 UNIONC_SECRET_KEY 与 UNIONC_PROXY_SECRET，
#    把同一个代理证明安全地配置到 Caddy 环境，并临时打开
#    UNIONC_ALLOW_BOOTSTRAP=1 与 UNIONC_BOOTSTRAP_PASSWORD；无需配置数据库

# 3. 启动
sudo systemctl enable --now unionc
sudo journalctl -u unionc -f       # 确认日志第一行的数据目录路径正确

# 4. 管理员配置与数据库创建后，删除上述两个 bootstrap 变量并重启
sudo systemctl restart unionc
```

正式 tag 的 `server-linux` 发布 job 会生成静态 musl x86_64 原始 Server 二进制、DEB 与
RPM，并以无动态解释器门禁避免继承构建机 glibc 下限；tag
版本必须与 Cargo workspace 完全一致。DEB 使用 `passwd + systemd` 依赖，RPM 使用
`shadow-utils + systemd`，不跨发行版复用 `adduser` 名称。门禁会在 Ubuntu 真实验证 DEB
安装、systemd/SQLite 启动、在线备份、同 schema 清单恢复与完整性检查，
并在 Fedora 容器真实安装 RPM，验证脚本顺序、专用用户启动和状态保留卸载。
通过后 Server 与 Agent 制品共同进入 SHA256SUMS、GPG 签名、provenance attestation 和
GitHub Release；两者的构建与生命周期 job 相互独立。

### 9.3 服务端环境变量

| 变量 | 必填 | 说明 |
|---|---|---|
| `UNIONC_PACKAGE_VERSION` | DEB/RPM 固定 | 包内必须精确为 `0.3.2`，仅供生命周期归属校验；不要修改，裸二进制部署不设置 |
| `UNIONC_ENV` | 生产必填 | 设为 `production` 启用全部生产约束 |
| `UNIONC_DATA_DIR` | 强烈建议 | 数据目录绝对路径；unit 已设为 `/var/lib/unionc` |
| `UNIONC_SECRET_KEY` | 生产必填 | 32 字节主密钥的 Base64 |
| `UNIONC_PROXY_SECRET` | 生产必填 | 64 位小写十六进制独立随机值；可信反代覆盖写入同值请求头，不能复用主密钥 |
| `UNIONC_ALLOW_BOOTSTRAP` | 首次部署 | 设为 `1` 才允许创建管理员配置与当前 schema 的新数据库 |
| `UNIONC_BOOTSTRAP_PASSWORD` | 首次部署 | 初始管理员口令，至少 12 字符 |
| `UNIONC_SERVER_BIND` / `UNIONC_SERVER_PORT` | | 默认 `127.0.0.1:8081`；生产强制回环 |
| `UNIONC_SECRET_KEY_ID` / `UNIONC_SECRET_KEY_PREVIOUS` | 轮换时 | 见 [5.4](#54-密钥环与轮换) |
| `UNIONC_RETENTION_DAYS` | | 审计保留天数，默认 90，合法范围 7–3650；无效值拒绝启动 |
| `UNIONC_TELEMETRY_RETENTION_DAYS` | | 遥测保留天数，默认 30，合法范围 1–3650；无效值拒绝启动 |

### 9.4 Agent 安装

Agent 软件分发与 UnionC 管理台解耦。管理台不托管安装包、不识别客户端平台，也不生成
shell、PowerShell 或 pkg 安装命令。二进制必须先通过组织认可的独立渠道安装，例如
apt/rpm 仓库、签名 Windows MSI/winget、签名并公证的 macOS pkg、MDM、GPO 或配置管理。

仓库中的打包文件用于独立 Agent 发布工程，不构成 Server 在线功能。以 Linux 本地构建为例：

```bash
# 必须在工作区根执行
cargo build --release -p unionc-agent
NFPM_ARCH=amd64 agent/packaging/linux/build-packages.sh
sudo dpkg -i unionc-agent_0.3.2_amd64.deb
```

Windows 当前只发布 x64 MSI。可双击安装，或在管理员命令提示符中执行；`PURGE=1` 只用于
已经在 Web 撤销实例后的永久本地清理：

```cmd
msiexec.exe /i UnionC-Agent-0.3.2-x64.msi /qn /norestart
msiexec.exe /x UnionC-Agent-0.3.2-x64.msi /qn /norestart
msiexec.exe /x UnionC-Agent-0.3.2-x64.msi PURGE=1 /qn /norestart
```

安装完成后的授权协议相同，Windows 另提供无需手写命令的托盘入口：

1. 管理员在 Web 创建待激活实例并把一次性授权密钥交给安装人员；
2. Windows 双击交互安装成功后，从通知区托盘右键选择“配对/重新配对”，在随机回环地址的
   本机页面一次填写 Server HTTPS 地址、一次性授权密钥和可选主机名称，并接受机器级
   操作所需的 UAC；其他平台
   或 Windows 诊断场景以能写 Agent 私有状态目录的权限运行：

   ```bash
   unionc-agent pair --server https://unionc.example.com
   ```

3. Windows 提权 Agent 在一次操作中建立请求、提交授权密钥和轮询结果，不再打开远程
   激活页二次输入；CLI 使用专属公开激活 URL；
4. 配对成功后按操作前状态启动/重启系统服务。Agent secret 由程序本地生成，浏览器不会显示它。

本地配置页会自动每 30 秒访问填写地址的 `/api/health`，也可点击“立即检测”；托盘菜单的
“检测连接状态”同时显示 Windows 服务状态与 Server 可达性。该检查不读取 ProgramData 凭据，
因此只代表管理端网络/TLS/HTTP 可达；管理台中的主机在线状态仍以最近一次认证遥测为准。

Windows `/qn`、GPO、Intune/MDM 等静默/SYSTEM 安装不会在 session 0 启动交互进程；用户
下次登录时由 HKLM Run 启动，也可从开始菜单打开。用户菜单“退出”会先请求 UAC 停止
`UnionCAgent`，确认停止后才关闭托盘；拒绝 UAC 或停止失败时托盘保持运行。MSI 同版本重装/
卸载发送的 `WM_CLOSE` 只关闭托盘，服务由 Windows Installer/SCM 的标准事务动作处理。
CLI 故障排查时可先停止服务、运行上述 `pair`（并显式传入
`--config "%ProgramData%\UnionC Agent\config.json"`），完成后再启动服务。

Linux DEB/RPM 包含硬化的 systemd unit、可选 GPU drop-in、保留状态式 remove 和显式
purge；Windows x64 WiX MSI 把只读程序与可变状态分离，以原生 SCM Service 常驻，使用
专属 service SID 隔离 LocalService 的凭据权限，并安装普通用户托盘、HKLM Run 自启动和
开始菜单入口；macOS pkg 使用专用账户、LaunchDaemon、
日志轮转和本机卸载器。Windows 用户安装、同版本重装和卸载不依赖 PowerShell，也不识别或迁移
旧计划任务安装。
跨版本更新不提供迁移：先在 Web 撤销，再按平台规定顺序永久清理旧 Agent，然后由外部分发
渠道安装当前制品并重新配对。Agent 不包含自更新器。

普通卸载默认保留 host-id、agent-token、配置、配对状态和 spool，方便安全重装；永久退役
必须先在 Web 撤销实例，再执行平台 purge。tag 发布强制 Windows Authenticode、macOS
Developer ID + notarization/staple，以及签名的制品清单；缺少签名 secret 时发布失败，不会
降级上传未签名正式制品。完整命令、路径、恢复语义和 secret 清单见
[Agent 安装、同版本重装、卸载与退役](docs/runbooks/agent-lifecycle.md)。

### 9.5 Agent 配置

配置文件（`--config PATH` 或 `UNIONC_AGENT_CONFIG`，包默认
`/etc/unionc-agent/config.json`）或同名环境变量，**环境变量优先**：

| 配置项 | 环境变量 | 默认 | 说明 |
|---|---|---|---|
| `application_version` | — | `0.3.2` | 持久配置必填且必须精确等于当前 Agent 包版本 |
| `endpoint` | `UNIONC_AGENT_ENDPOINT` | `http://127.0.0.1:8081/api/agent/v1/report` | 上报地址 |
| `pairing_endpoint` | `UNIONC_AGENT_PAIRING_ENDPOINT` | 由标准 report endpoint 推导 | v2 配对请求地址；配对成功时持久化 JSON 字段会被清空，环境变量仍可在下次加载时覆盖 |
| `host_name` | `UNIONC_AGENT_HOST_NAME` | 操作系统主机名 | 管理台显示名称，可由 `pair --name` 设置 |
| `interval_seconds` | `UNIONC_AGENT_INTERVAL_SECONDS` | 10 | 采集周期，1-3600 |
| `slow_interval_seconds` | `UNIONC_AGENT_SLOW_INTERVAL_SECONDS` | 30 | 温度等慢速指标周期，不得小于 `interval_seconds` |
| `request_timeout_seconds` | — | 10 | 单次 HTTP 请求超时 |
| `jitter_percent` | — | 10 | 抖动，上限 50，避免机队同步上报 |
| `spool_max_bytes` | — | 64 MiB | 断线队列上限，最小 1 MiB |
| `state_dir` | `UNIONC_AGENT_STATE_DIR` | 平台默认 | 存放 host-id、凭据、spool |
| `otlp_endpoint` / `otlp_token` | `UNIONC_AGENT_OTLP_ENDPOINT` / `_TOKEN` | — | 可选 OTLP 导出 |
| `tls_ca_pem` | `UNIONC_AGENT_TLS_CA_PEM` | — | 自签 CA |
| `tls_identity_pem` | `UNIONC_AGENT_TLS_IDENTITY_PEM` | — | mTLS 客户端证书（Linux，证书+私钥合并的 PEM） |
| `tls_identity_pkcs12` | `UNIONC_AGENT_TLS_IDENTITY_PKCS12` | — | mTLS 客户端证书（Windows/macOS） |
| `tls_identity_password` | `UNIONC_AGENT_TLS_IDENTITY_PASSWORD` | — | PKCS#12 口令 |
| `allow_insecure_http` | `UNIONC_AGENT_ALLOW_INSECURE_HTTP` | false | 非回环地址走明文需显式开启 |

配置文件存在时必须包含表中的完整当前结构；缺字段、未知字段和不同
`application_version` 都会在环境变量覆盖之前被拒绝。配置文件不存在时才使用编译期的
0.3.2 默认值，成功配对会原子写出完整当前结构。

pairing/report 分域时，配对成功会把持久化 JSON 中的 `pairing_endpoint` 清为 `null`。
若服务长期设置 `UNIONC_AGENT_PAIRING_ENDPOINT`，下次加载会自动重新覆盖；否则重新配对前
必须通过配置或环境变量恢复 bootstrap endpoint，才不会从 report endpoint 推导。Server
返回相对激活路径，因此 pairing origin 还必须提供或反代 `/agent/activate/...` SPA。TLS CA 和客户端身份当前由
UnionC、pairing 与 OTLP 共用，OTLP 若需另一套客户端证书必须增加网关或修改代码。

**启动即校验**，而不是等到运行时反复失败：间隔为 0 或超过 3600、
`slow_interval_seconds < interval_seconds`、jitter > 50、超时为 0、
spool 上限小于 1 MiB、非回环的明文 HTTP、endpoint 内嵌凭据、同时配置两种证书格式——
任意一条都会拒绝对应操作。`status` 刻意跳过常规校验并诊断缺失/损坏配置；`probe` 与默认
`doctor` 不因投递 endpoint/TLS/OTLP 配置直接中止，后者会把未来 `run` 的问题列为诊断项。
`doctor --delivery` 恢复完整投递校验。`run` 在没有 credential 但存在 pending pairing
state 时只继续轮询；完全未配对时退避等待 `pair`，均不会启动无身份采样。一次性投递命令
在这两种状态下立即失败。

---

## 10. 运维手册

### 10.1 日常巡检

```bash
systemctl status unionc                                   # 服务状态
curl -s localhost:8081/api/health  | jq                   # 存活
curl -s localhost:8081/api/ready   | jq                   # 就绪（数据库 + 数据目录）
journalctl -u unionc --since '1 hour ago' -p warning      # 近期告警
```

`jq` 只用于格式化 JSON，未安装时删去管道部分即可，不影响探针请求。

### 10.2 新增被监控主机

1. 通过组织的软件渠道安装 Agent；管理台不分发安装包；
2. 管理台打开“主机 → 添加 Agent”，填写实例名称，生成默认15分钟有效的一次性授权密钥；
3. Windows 从托盘选择“配对/重新配对”；其他平台或诊断场景运行
   `unionc-agent pair --server https://unionc.example.com`；
4. Windows 在本机配置页同时填写 Server 地址和一次性授权密钥，接受 UAC 后即可；
   CLI/其他平台打开输出的 `/agent/activate/{request_id}`，核对设备摘要并输入授权密钥；
5. 配对成功后界面只显示状态和 instance ID，Agent 自己保存通信 secret；
6. 启动系统服务；`run` 先把首次报告持久入队，可重试失败会保留队首且不回滚已完成的配对，
   永久内容错误则丢弃该报告。

### 10.3 排查主机不上报

```bash
# 在被监控主机上
unionc-agent probe                       # 本机能力报告，不联网——先确认采集本身正常
journalctl -u unionc-agent -n 100        # 看投递错误
ls -la /var/lib/unionc-agent/spool/      # spool 堆积说明投递失败
```

| 日志现象 | 含义 | 处置 |
|---|---|---|
| `报文被永久拒绝` | 400/409/413/422，内容或 report ID 不符合契约 | 检查 Agent 版本与配置是否与服务端契约一致 |
| `服务端拒绝了当前凭据` | 401（未知、失效或被重配替换） | 在管理台为同一实例创建邀请并重新配对；没有 token 恢复或自动注册入口 |
| `主机已撤销或 credential/host_id 绑定失配` / `reauth_required` | 403 | 不会自动注册或换身份；管理员核对实例后，为同一 instance ID 创建新邀请并重新配对 |
| 提示`这是部署配置问题，不是凭据失效` | 421 | 反代契约头或 `X-UnionC-Proxy-Secret` 缺失/不匹配；改反代配置，**不要**动令牌 |
| `配对请求过期/被拒绝` | 未在有效期内完成浏览器激活 | 重新运行 `pair` 并使用新的实例激活码 |
| `spool 连续 N 次操作失败` | 磁盘持续故障 | 检查磁盘空间与 `/var/lib/unionc-agent` 权限 |
| 大量 `.invalid` 文件 | 报文反序列化失败 | 通常是版本不匹配；这些文件会占配额并被优先淘汰 |

### 10.4 启用 GPU 采集

默认 unit 的 `PrivateDevices=yes` 会屏蔽 GPU 设备节点：

```bash
sudo mkdir -p /etc/systemd/system/unionc-agent.service.d
sudo cp /usr/share/unionc-agent/unionc-agent-gpu.conf \
        /etc/systemd/system/unionc-agent.service.d/gpu.conf
sudo systemctl daemon-reload && sudo systemctl restart unionc-agent
unionc-agent probe | jq '.capabilities[] | select(.name | startswith("gpu"))'
```

### 10.5 主机退役

管理台 → 主机监控 → 选中主机 → **实例管理** → 撤销 Agent。
这会持久标记 revoked 并吊销该实例全部 credential，身份和历史继续保留作为 tombstone，
操作记入审计日志。若以后恢复，必须对同一 instance ID 重新完成管理员授权的浏览器配对。

### 10.6 数据保留

| 数据 | 默认保留 | 变量 |
|---|---|---|
| 审计日志 | 90 天 | `UNIONC_RETENTION_DAYS` |
| 遥测历史 | 30 天 | `UNIONC_TELEMETRY_RETENTION_DAYS` |

清理任务每 24 小时执行一次。遥测每批 10 000 行、批间让出 50 毫秒；审计每批 1 000 行、
批间主动让出调度；理由见 [5.5](#55-sqlite-写入与数据保留)。**每台主机的最新一份报告
永远保留**，即使超出保留期——否则长期离线主机的详情页会变空白。这一例外由两道保险
守住：清理条件显式排除仍被 `latest_report_id` 引用的行，外键的 `ON DELETE SET NULL` 兜底。

容量主要由主机数、采样间隔与保留期的乘积决定。历史 5000 行 A/B 数字不能作为当前
SQLite 文件体积承诺；上线前应使用真实报文压测，并同时观察主库、WAL 和备份快照大小。
当前默认面向约 20 台主机。更大规模应调大采样周期或缩短保留期，并验证集中断线补传时
单写者队列的延迟，而不只看稳定状态下的平均写入率。

### 10.7 备份

运行中不能裸 `cp /var/lib/unionc/unionc.db`。WAL 中可能仍有已提交页面，单独复制主文件会
得到缺数据或损坏的备份。包安装后的维护命令通过临时 systemd unit 加载与正式服务相同的
环境文件，避免把生产主密钥展开到命令行；备份目录需预先允许 `unionc` 用户写入：

```bash
sudo systemd-run --quiet --wait --pipe --collect \
  --uid=unionc --gid=unionc -p WorkingDirectory=/var/lib/unionc \
  -p Environment=UNIONC_DATA_DIR=/var/lib/unionc \
  -p EnvironmentFile=/etc/unionc/unionc.env \
  /usr/bin/unionc backup --output /srv/backup/unionc-$(date -u +%Y%m%dT%H%M%SZ).db
```

完整备份集合包括：

1. 上述 SQLite 快照及同名 `.manifest.json` 清单（当前应用版本、唯一 schema、密钥 ID、SHA-256）；
2. `/var/lib/unionc/unionc-config.json`（管理员哈希）；
3. **主密钥**（`UNIONC_SECRET_KEY`，在 `/etc/unionc/unionc.env` 中，含轮换期历史密钥）。

快照和清单必须成对复制与保留。`--force` 只表示允许覆盖现有活动库，不会绕过清单校验。
恢复始终要求清单存在，并执行 SHA-256、SQLite 完整性、外键、schema 与密文可解性检查。

恢复必须停服，先保留当前数据库，再显式授权覆盖并做完整性检查：

```bash
sudo systemctl stop unionc
sudo systemd-run --quiet --wait --pipe --collect \
  --uid=unionc --gid=unionc -p WorkingDirectory=/var/lib/unionc \
  -p Environment=UNIONC_DATA_DIR=/var/lib/unionc \
  -p EnvironmentFile=/etc/unionc/unionc.env \
  /usr/bin/unionc restore --input /srv/backup/unionc-2026-08-16.db --force
sudo systemd-run --quiet --wait --pipe --collect \
  --uid=unionc --gid=unionc -p WorkingDirectory=/var/lib/unionc \
  -p Environment=UNIONC_DATA_DIR=/var/lib/unionc \
  -p EnvironmentFile=/etc/unionc/unionc.env \
  /usr/bin/unionc integrity-check
sudo systemctl start unionc
```

替换前，`restore` 会处理当前活动 SQLite 库：若它通过校验，会留下可再次用于 `restore` 的
`unionc.pre-restore-*.db` 与 manifest。若当前库已损坏但没有 WAL/SHM，恢复会保留无 manifest
的 `unverified` 原始取证副本后继续；若损坏库仍带 sidecar，命令会先保留完整 main/WAL/SHM
文件族再拒绝替换，避免丢失可能尚未 checkpoint 的页面。恢复点不会自动清理，必须在恢复
演练和异机备份确认后按保留策略删除；无 manifest 的取证副本不能作为受支持的 restore 输入。

备份输出不覆盖同名文件，路径应包含唯一时间戳。`restore` 只接受由当前版本、当前唯一
schema 生成的快照；它会精确校验基线指纹和 schema，不接受版本前缀，也不在 staging 副本
中补跑任何升级。版本或 schema 不一致时必须部署空库，`--force` 不能改变这一边界。

⚠ 丢失主密钥意味着数据库里的 Sunshine 密码永久不可读。卸载包不会删除
`/var/lib/unionc`，但这不能替代异机备份和定期恢复演练。

#### 当前 schema 与旧部署边界

项目只支持开发环境或显式生产 bootstrap 新建当前版本数据库，以及当前 schema 的同版本
恢复。普通生产启动与 `backup`、`integrity-check`、`rekey` 只接受已存在的精确当前库；
`restore` 可把完整验证的备份发布到缺失目标。旧 Server 数据库、旧配置与旧 Agent 身份不能
就地升级或导入。需要留存旧数据时，应先在旧环境导出为独立、长期可读的中立格式，随后
部署空库、安装当前 Agent 并重新配对；UnionC 不提供把该导出重新导入当前库的桥接命令。

### 10.8 故障速查

| 症状 | 原因 | 处置 |
|---|---|---|
| 启动报 `未找到管理员配置` | `UNIONC_DATA_DIR` 指错了 | 核对路径；确认是首次部署再设 `UNIONC_ALLOW_BOOTSTRAP=1` |
| 启动报数据库不存在或 schema metadata 缺失 | 数据库丢失、空文件或数据目录指错 | 不要继续空库；核对数据目录并从一致性备份恢复。仅真正首次部署才临时开启 bootstrap |
| 启动报 `key id ... not in the keyring` | 轮换时漏了历史密钥 | 把旧密钥加入 `UNIONC_SECRET_KEY_PREVIOUS` |
| 启动报 schema/baseline 指纹不匹配 | 数据库不是当前版本创建的精确 schema | 不会自动迁移；导出需要保留的数据后从空数据目录重新部署 |
| 登录 429 | 触发限流 | 等 1 分钟；确认反代正确追加 XFF |
| 登录 / 上报 421 | 反代未透传 `X-Forwarded-Proto` / `X-Forwarded-For`，或代理证明缺失/不匹配 | 配置同一个 `UNIONC_PROXY_SECRET` 并让反代覆盖写入 `X-UnionC-Proxy-Secret`；响应 message 会说明失败项 |
| 启动报 `production unionc must bind to a loopback address` | 生产环境配了非回环绑定 | 改回 `127.0.0.1`，对外由反代暴露 |
| 主机一直 offline | Agent 未上报 | 见 [10.3](#103-排查主机不上报) |
| 资源读数为 0 | 采样任务异常退出 | 查日志 `系统资源采样任务异常退出` |
| `/api/ready` 报数据库不可用 | 数据目录权限、磁盘满、I/O 错误或数据库损坏 | 停止写入，查日志并运行 `integrity-check`；必要时从一致性快照恢复 |

---

## 11. 开发与测试

### 11.1 工作区

`server`、`agent` 与 `protocol` 是同一个 Cargo workspace 的三个成员，共享一份 `Cargo.lock`、
统一的依赖版本与发布 profile。一条命令即可覆盖全部：

```bash
cargo test --workspace                              # 三个 crate 一起测
cargo clippy --workspace --all-targets -- -D warnings
```

> 注意：`server` 仅支持 Linux（`lib.rs` 有 `compile_error!` 固定）。在 Windows/macOS
> 上只能构建 agent，必须显式指定 `-p unionc-agent`，否则 workspace 会把 server 也拉进来。

### 11.2 本地运行

```bash
# 服务端（Linux / WSL；统一使用仓库根 .runtime/server）
./tools/dev-server.sh

# 前端
cd web && npm ci && npm run dev

# 查看某台机器能采到什么（不联网）
cargo run -p unionc-agent --bin unionc-agent -- probe
```

### 11.3 测试

```bash
cargo test --workspace
```

Server 集成测试为每个场景创建隔离的临时 SQLite 文件并真实执行当前 schema 初始化和 SQL，
不需要外部数据库。测试路径不得指向生产数据目录；不包含旧数据库或旧 Server 包升级桥。
OTLP live 接收合同测试由 `UNIONC_AGENT_TEST_REQUIRE_OTLP=1` 守护，因为它依赖真实外部
Collector。它验证 Agent → Collector 能否接收，不覆盖 Collector exporter、时序库落库或查询。

**测试组织**：单元测试就近放在被测模块里，集成测试按**被守护的行为**而非按模块划分：

| 文件 | 守护的契约 |
|---|---|
| `agent_report_validation.rs` | 报文契约的每一条边界，含文本字段穷尽性 |
| `agent_rate_limits.rs` | 匿名配对/激活与上报的分桶限流 |
| `login_rate_limit.rs` | 三层登录配额，以及"打不满别人的桶" |
| `csrf_double_submit.rs` | 每会话随机令牌，固定值必须失败 |
| `http_access.rs` | 公开/受保护路径划分与反代契约 |
| `security_headers.rs` | 全局安全头在错误响应上同样生效 |
| `agent_pairing.rs` | 当前浏览器配对、重新配对、撤销与凭据吊销 |
| `agent_protocol_contract.rs` | Server 与共享 protocol crate 对当前 wire schema 的一致性 |
| `report_ordering.rs` | 补传的旧报文不回写主机状态；重放幂等 |
| `latest_report_retention.rs` | 保留期清理不会删掉被引用的最新报告 |
| `host_row_write_amplification.rs` | 每报文对主机行只产生一次写入 |
| `monitoring_read_path_cost.rs` | 列表/历史读路径不触碰完整报文 JSON |
| `history_query.rs` | 历史查询参数、分页与 404 语义 |
| `database_schema.rs` | 当前唯一 schema、空库初始化与非当前 schema 拒绝 |
| `sqlite_maintenance.rs` | 在线备份、清单恢复、完整性/schema 校验与维护锁 |
| `system_resources.rs` | 读数跟随负载；并发观察者不互相吃掉增量 |
| `service_status_probe.rs` | 探测集中在后台任务，与订阅者数量无关 |
| `otlp_encoding.rs` | 用**官方** proto 解码手写编码器的输出 |
| `otlp_live.rs` | 真实 Collector 确实接受该报文 |

最后两个的分工值得一提：手写编码器的单元测试编解码用的是**同一份**定义，抄错的字段
编号在自洽的两侧同样自洽。`otlp_encoding.rs` 把 `opentelemetry-proto` 作为
dev-dependency 引入（运行时依赖一个字节没变），拿到一份独立的权威解码器；
`otlp_live.rs` 则回答"对端是否接受"——二者缺一不可。

### 11.4 CI

| Job | 平台 | 内容 |
|---|---|---|
| `format` | ubuntu | 全工作区 Rust 格式检查 |
| `server` | ubuntu | clippy(-D warnings) + 基于临时 SQLite 的完整持久层测试 |
| `protocol` | ubuntu | 共享协议 clippy(-D warnings) + 严格序列化/反序列化单元测试 |
| `agent` | ubuntu / windows / macos | clippy + Agent 测试 + 三种 feature 组合；另含 Linux 隔离脚本 lifecycle、Windows PE/WiX 静态校验与 MSI 构建、macOS 脚本/plist/账户 mock 门禁；不执行真实系统包安装生命周期 |
| `otlp` | ubuntu | 官方 proto 解码断言 + Agent → 真实 `otel/opentelemetry-collector-contrib` 的接收合同；不验证下游落库查询 |
| `frontend` | ubuntu | npm high/critical 依赖审计 + lint + typecheck + 单元测试 + build（Node 26） |

表中是 workflow 配置的成功路径，不代表某次运行必然到达全部步骤；同一 Job 的 fmt/clippy
等前置门禁失败时，后续测试和打包步骤会被跳过，必须查看该次日志确认实际覆盖。

agent job 的三次 feature `check` 不是冗余：默认 feature 是 `nvidia`，
其 `#[cfg(not(...))]` 分支在常规构建中从不编译，只有显式关掉 feature 才会暴露编译错误
或整个文件缺失。

当前 workflow 会单独运行 `unionc-protocol`，OTLP job 也同时执行使用官方 proto 解码的
`otlp_encoding` 与真实 Collector 接收合同 `otlp_live`。本地提交前清单仍应显式执行
这些快速检查，以便在推送前定位问题。

### 11.5 代码约定

- 注释解释**为什么**，尤其是"为什么不是那个更显然的写法"
- 性能相关的取舍在注释里附**实测数字**
- 修复缺陷时补一个**在缺陷存在时必然失败**的回归测试
- 只维护当前唯一 schema；变更基线时同时更新严格拒绝旧 schema 的合同测试

---

## 12. 设计决策记录

以下每条都记录一个"看似更简单的写法为何不可取"。改动相关代码前请先读对应条目。

**采样为什么是"后台任务 + 快照"而不是"请求即采样"**
吞吐是读取即消费的差值。若在请求处理中采样，两个浏览器标签同时轮询时，后一个的窗口
只剩几十毫秒，读数塌成 0——实测同一时刻观察者 A 读到 `rx=620`，100ms 后的观察者 B
读到 `rx=0`。唯一采样者 + 多方读快照使读路径与观察者数量无关。

**为什么列表接口不读报文体**
100 台主机 × 30-50KB × 每 10 秒刷新 ≈ 每 10 秒解析 3-5MB JSON，代价随规模线性恶化。
只读摘要数值列的实测对比：列表 9.8ms（读报文体为 119ms），历史 1000 点 26ms（1069ms）。

**为什么没有 covering index**
把 9 个摘要列 INCLUDE 进历史索引以求 Index Only Scan 是零收益：实测读取 1.008ms vs
1.011ms（噪声范围内），但写入慢 37%，索引 12MB 而表才 16MB。报告按时间顺序写入，
同一主机的相邻行物理上本就聚集，回表几乎不产生 I/O。

**为什么 401/403 单独成类而不算普通“可重试”**
算作 Transient 的后果是 Agent 永远重试，数据堆进 spool 直到撑满。401 表示 secret
未知/失效，或 active 主机上的旧 credential 已被重配替换；403 表示主机生命周期已撤销，
或当前有效 credential 与 body `host_id` 失配。二者都进入重新授权且不会触发自动注册，
只有管理员为同一实例完成新配对才能恢复。

**为什么限流要分桶**
只有全局桶时，攻击者用任意用户名打满配额就能让合法管理员在整个窗口内无法登录——
防护本身成了武器。分桶后洪水只影响攻击者自己的桶。

**为什么 CSRF 用每会话随机令牌而非固定值**
固定值（如 `x-csrf-token: 1`）的安全性完全建立在"浏览器不允许跨源发送自定义头"这个
外部前提上——一旦引入 CORS 且配置为 `Allow-Headers: *`，防线瞬间失效且不会有任何测试
失败。随机令牌即便能跨源发头也猜不出值。

**为什么数据目录必须是绝对路径**
相对路径意味着"文件在哪"取决于进程 CWD。从别的目录启动会读不到管理员配置和数据库，
而"状态不存在"与"首次部署"在代码里无法区分；如果自动创建，就会让原账号和全部历史
看上去凭空消失。生产环境因此额外要求显式的 `UNIONC_ALLOW_BOOTSTRAP=1`；未开启时，
缺失或空数据库都会 fail closed。

**为什么采集不到的指标不填 0**
0 和"读不到"在监控产品里是完全不同的两件事。用 `capabilities` 数组承载可用性与失败
原因，前端显示"不支持"而非"0%"。

**为什么反代契约不满足返回 421 而不是 403**
403 属于需要人工纠正的身份语义（主机生命周期撤销或有效 credential/`host_id` 失配），
会让 Agent 停止自动注册并等待浏览器重新授权。二者混用会把一次反向代理漏配契约头或
代理证明误报成整批实例身份异常。421 归入可重试类，反代修好后原样重发即可。

**为什么 XFF 缺失要硬失败而不是降级**
降级**不产生任何信号**：一份只配了 XFP 的反代能通过启动检查、能正常登录、日志里也没有
异常，而按 IP 与按账号的两层登录配额已经静默失效，只剩全局兜底。不可观测的降级比明确
的失败更危险。

**为什么摘要取"最忙的单个设备"而不是求和**
求和会把 veth、bridge、bind mount 重复计入——一台跑容器的主机上，同一份流量可能在物理
网卡、bridge 与若干 veth 上各计一次，读数虚高数倍。取最大值不完美（多网卡真实并发时会
偏低），但它至少不会给出一个凭空放大的数字。详情页仍逐设备展示。

**为什么上报的 body 先取 `Bytes` 再手动反序列化**
axum 的提取器在 handler 体**之前**运行。用 `Json<AgentReport>` 时，一个完全未认证的
请求也能驱动一次完整的 512 KiB JSON 反序列化——等于把解析成本白送给任何匿名调用方。
认证与限流通过后再解析，未认证请求的成本只有一次哈希查库。

**为什么常驻模式补传成功后先出队再导出 OTLP**
反过来的话，一旦 `acknowledge` 失败（文件已被删、权限变更），报文会留在 spool 里、
下一轮重新读取并再次导出，在 Collector 侧产生重复数据点。先出队则最坏只是漏导一次——
OTLP 本就是尽力而为的次要输出，漏一个点远好过重复计数。

**为什么隔离文件要计入 spool 配额**
反序列化失败的报文改名 `.invalid` 留作排查。若容量核算只统计 `.json`，隔离文件就既不
占配额、也永远不被清理——磁盘异常反复触发时它们会悄悄吃掉整个分区，而这恰恰是 Agent
最需要稳健的场景。因此两类文件共用同一份预算，并优先淘汰隔离文件。

**为什么单次 spool I/O 失败不终止进程**
对常驻守护进程而言，遇错即退只会表现为反复崩溃重启，无法改善底层磁盘故障。降级续跑、
丢弃无法持久化的当前采样，并只在**连续** 100 次失败（约合 15 分钟持续故障）时退出交给
服务管理器。

**为什么只有一份当前数据库基线**
项目只承诺最新版本，维护历史 migration 会让路由已经严格而持久层仍需接受旧 shape，扩大
测试和审计面。`server/schema/sqlite.sql` 因此直接描述当前完整 schema；空库原子创建，非当前
schema 明确拒绝。代价是版本变化时现有部署必须导出留存数据、全新建库并重新配对。
