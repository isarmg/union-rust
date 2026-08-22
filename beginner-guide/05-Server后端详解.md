# 05. Server 后端详解

UnionC Server 是 Linux 上的单进程异步服务。它把 HTTP、SQLite、内存快照和后台任务组合在一起，但代码仍按业务功能分区。

## 1. 技术栈

`server/Cargo.toml` 的主要依赖：

| 库 | 用途 |
|---|---|
| Axum | HTTP 路由、提取器、中间件、响应 |
| Tokio | 异步 runtime、网络、信号、任务、锁、通道 |
| SQLx SQLite | 异步数据库访问 |
| bundled SQLite | 固定 SQLite 能力，不依赖发行版动态库 |
| Serde / serde_json | JSON |
| reqwest + rustls | 访问 Sunshine 等 HTTPS 上游 |
| bcrypt | 管理员密码哈希 |
| AES-GCM | Sunshine 密码加密 |
| tracing | 结构化日志 |
| sysinfo | Server 本机资源采样 |

`server/src/lib.rs` 在非 Linux 平台触发编译错误，这是明确的平台约束。

## 2. 进程入口与命令

入口是 `server/src/main.rs`。无参数时执行 `Serve`，还支持离线或维护命令：

```text
unionc                         启动 HTTP Server
unionc --version               输出版本
unionc rekey                   离线重加密 Sunshine secret
unionc reset-admin-password    离线重置管理员密码
unionc backup --output PATH    在线一致性 SQLite 快照
unionc restore --input PATH [--force]  停服恢复同版本快照
unionc integrity-check         校验数据库、schema、外键和密文
```

解析命令后才决定是否启动 HTTP。`rekey`、`restore` 等写操作会取得数据库文件锁，运行中的 Server 会让它们失败，而不是与线上写入竞争。

## 3. 启动顺序

核心函数是 `startup::initialize()`：

1. `infra::paths::init()`：把数据目录一次解析为绝对路径；
2. `RuntimeEnvironment::from_environment()`：一次性快照环境变量；
3. `Settings::load()`：建立基础运行设置；
4. 生产模式检查独立 proxy secret；
5. `ensure_layout()`：建立并校验私有目录；
6. 计算 `unionc.db`，取得单 Server 文件锁；
7. 初始化 AES-GCM 密钥环；
8. 读取管理员配置，开发首次启动可生成，生产必须显式 bootstrap；
9. 连接 SQLite，空库建当前基线，已有库必须精确匹配；
10. 保留环境派生的部署设置，并从数据库加载唯一可变应用配置——Sunshine 主机列表；
11. 计算监听地址；
12. 在 blocking task 中建立 dummy bcrypt hash；
13. 启动唯一 Server 本机资源监控器；
14. 构造 `AppState`；
15. 启动 Sunshine 探测、内存回收、数据库保留期任务；
16. 返回地址与状态，`main` 构造路由并监听。

任何关键步骤失败都会拒绝监听，不存在数据库未就绪但 API 已对外开放的“半启动”状态。

## 4. `AppState` 中有什么

`server/src/state.rs` 的 `AppState` 是请求和后台任务共享的总入口：

| 字段 | 内容 |
|---|---|
| `settings` | 不可变运行设置 |
| `database` | SQLite 连接池 |
| `database_health` | 带短 TTL 的健康结果缓存 |
| `started_at` | 启动时间 |
| `hosts` | Sunshine 配置、健康快照、刷新通知和设置写锁 |
| `auth` | 会话、CSRF、登录桶、bcrypt 并发限制、SSE 票据 |
| `agents` | 配对/报告匿名限流与每主机令牌桶 |
| `services` | 最新服务状态与 broadcast 通道 |
| `resources` | Server 本机资源快照监控器 |

Axum 会为每个请求 clone `AppState`。内部使用 `Arc`，因此不是复制整个数据库。

## 5. 路由树

`server/src/http/mod.rs` 组合路由。

### 系统与认证

```text
GET  /api/health
GET  /api/ready
POST /api/auth/login
POST /api/auth/logout
GET  /api/auth/me
POST /api/auth/change-password
GET  /api/services
GET  /api/system/resources
GET  /api/audit-logs
POST /api/events/ticket
GET  /api/events?ticket=...
```

公开路径只有 health、ready、login。当前前端建立 SSE 时使用一次性 ticket；Server 也保留无 ticket 时的管理员 Cookie 认证路径。其余控制台路由需要管理员会话。

### 监控控制台

```text
GET    /api/monitoring/hosts
GET    /api/monitoring/hosts/{host_id}
GET    /api/monitoring/hosts/{host_id}/history
POST   /api/monitoring/hosts/{host_id}/revoke
GET    /api/monitoring/agent-instances
POST   /api/monitoring/agent-instances
DELETE /api/monitoring/agent-instances/{request_id}
```

### Agent 独立路由

```text
POST /api/agent/v1/report
POST /api/agent/v2/pairing-requests
GET  /api/agent/v2/pairing-requests/{request_id}
POST /api/agent/v2/pairing-requests/{request_id}/status
POST /api/agent/v2/activate
```

报告体上限 512 KiB，配对控制面 16 KiB。它们不经过管理员会话中间件。

### Sunshine

基础 CRUD 是 `/api/services/sunshine/hosts`；每台主机下还有 status、apps、clients、config、api-logs、pin、restart、reset-display、covers 等受控代理端点。最大配置写入为 1 MiB。

## 6. 中间件顺序

从外到内理解：

```text
assign_request_id
  → TraceLayer
    → security_headers
      → 全局 1 MiB body fallback
        ├─ console require_auth
        │   ├─ 公开路径放行
        │   ├─ SSE ticket 特殊认证
        │   └─ Cookie → 会话 → DB 健康 → CSRF → 审计上下文
        └─ Agent 路由自己的认证与更小 body limit
```

Axum 的 `.layer()` 阅读顺序容易让初学者困惑：后添加的外层会先看到请求。判断实际行为时，应结合 `http/mod.rs` 顶部的分层图和安全头测试。

请求 ID 永远由 Server 覆盖生成，不能接受客户端自报值，因为它会写入审计并用于关联日志。

## 7. 跟踪报告 handler

`server/src/monitoring/http/mod.rs::report_metrics` 是理解后端的最佳样本。其逻辑顺序大致为：

1. 验证生产代理链路；
2. 取得来源 IP，消耗认证前限流额度；
3. 解析 Bearer secret，计算 SHA-256；
4. 数据库按哈希判断 credential 与 host 生命周期：未知或已替换/撤销的 credential 返回 401，已撤销的 host 返回 403；
5. 消耗已认证主机的令牌桶；
6. 检查 JSON Content-Type；到这里都由 `AuthenticatedReport` 请求头提取器完成；
7. 只有前述检查全部通过，`Bytes` 提取器才在 512 KiB 上限内读取原始 body；
8. handler 使用 `serde_json::from_slice` 反序列化；
9. 调用 `report.validate()`；
10. 比对 credential host 与 body host；
11. 调用 `store_authenticated_monitoring_report`，在写事务中复核同一个 credential 仍然有效；
12. 返回 202 与严格 ACK。

昂贵 JSON 校验和数据库写入前先做来源/凭据防护，可以降低滥用成本。该顺序还会在认证或媒体类型失败时阻止应用层轮询、聚合原始 body；底层网络栈仍可能按自己的缓冲策略预读少量数据。查 credential 本身需要数据库，所以前面还有认证前 IP/全局限流。

## 8. 功能模块详解

### 8.1 `auth`

- 单管理员，不是多用户/RBAC；
- bcrypt 哈希，未知用户也执行 dummy hash 缩小时序差；
- 会话只存在内存，Server 重启后全部失效；
- 开发 cookie 名为 `session/csrf`，生产使用 `__Host-` 前缀；
- 修改密码序列化执行，先验证再哈希、持久化、发布；
- 改密撤销其他会话，前端随后主动注销当前会话。

### 8.2 `monitoring`

- `model/`：DTO、校验、指标摘要，入口是 `model/mod.rs`；
- `http/`：管理员邀请、公开配对、报告、查询、撤销，入口是 `http/mod.rs`；
- `store/`：配对事务、credential、报告幂等、历史和保留期，入口是 `store/mod.rs`。

### 8.3 `system`

- `resources.rs`：唯一的 2 秒后台采样器；
- `http.rs`：health、ready、资源、服务状态、审计、SSE；
- HTTP 只读快照，不临时触发 `/proc`/磁盘扫描。

### 8.4 `sunshine`

- `model.rs`：配置和代理请求类型；
- `client.rs`：上游 URL、TLS、认证、响应体上限；
- `status.rs`：并发 TCP/API 探测与快照发布；
- `http/hosts.rs`：配置 CRUD；
- `http/proxy.rs`：应用、客户端、配置、日志等代理。

写配置后先把健康状态设为 pending，再通知后台探测，避免旧配置的绿色状态暂时冒充新配置结果。
Sunshine 的创建、更新和删除同时维护 SQLite 和运行时内存快照。数据库 helper 在内部提交，
因此两者不能直接绑在可被客户端断开取消的 handler future 上；当前实现用独立任务完成
“数据库提交 → 内存发布 → 通知探测”整段序列。请求断开只是不再等待响应，不会把 Server 留在
“数据库已改、内存未改”的状态。

### 8.5 `infra`

- `paths.rs`：数据目录唯一解析；
- `database/`：连接、当前 schema、写门控、设置、审计、维护；
- `secrets.rs`：带 key ID 的 AES-256-GCM 密钥环；
- `network.rs`：主机地址语法校验、规范化和 IPv6 URL authority 格式化；它不提供 SSRF 目的地址隔离；
- `http_client.rs`：共享外部客户端。

## 9. 后台任务

| 任务 | 周期/触发 | 为什么不放 HTTP handler |
|---|---|---|
| Server 本机资源 | 2 秒 | 多个页面共享一次采样，读取稳定且便宜 |
| Sunshine TCP 快探测 | 约 5 秒或配置变化 | 页面数不会放大探测量 |
| Sunshine API 慢探测 | 约 30 秒或 generation 变化 | 慢请求不阻塞 TCP 状态和 SSE |
| 内存回收 | 10 分钟 | 避免热路径对所有会话/桶做 O(n) 扫描 |
| 遥测/审计保留期 | 启动立即一轮，之后约 24 小时 | 分批短事务，统一维护历史 |

后台任务使用 skip missed ticks 或 completion-relative cadence，避免暂停后突然密集“补 tick”。

## 10. 数据库健康缓存

公开的 ready 与需要数据库的管理面路径共享一次最小查询，结果带约 1 秒 TTL。缓存过期时只允许一个请求探测数据库，其余并发请求等待并复用结果。若每个请求都执行 `SELECT 1`，公开探针本身会争抢连接池、放大数据库负载；若永久缓存，又不能及时发现故障。

health 与 ready 也不能混为一谈：进程可能活着，但 SQLite 无法完成业务。

## 11. 错误模型

`server/src/error.rs` 的 `AppError` 把错误映射为：

```json
{"code":"not_found","message":"..."}
```

预期业务错误可返回安全消息；IO、SQLx、anyhow 等内部错误在日志记录完整信息，对外只给通用描述，避免泄露文件路径、SQL 或密钥上下文。

新增错误时要同时考虑：HTTP 状态、稳定机器码、客户端是否会据此采取不可逆动作。例如代理错误使用 421，避免 Agent 把它误当 403 撤销。

## 12. 推荐阅读顺序

1. `server/src/main.rs`；
2. `server/src/startup.rs` 的 `initialize`；
3. `server/src/state.rs` 的 `AppState`；
4. `server/src/http/mod.rs` 与 `http/access_control.rs`；
5. 选一个功能，按 `model → http → store/client`；
6. 相邻 `server/tests`；
7. 最后再读数据库维护、打包和罕见错误分支。

## 13. 验证命令

```bash
cargo check -p unionc
cargo test -p unionc
cargo test -p unionc --test agent_protocol_contract
cargo test -p unionc --test security_headers
```

Server 集成测试使用各自的临时 SQLite，不需要外部数据库。测试不得指向生产数据目录。

## 14. 本章自检

1. Server 为什么在监听端口前初始化 SQLite？
2. `AppState::clone()` 为什么不复制整个状态？
3. Agent 路由为何不套管理员会话中间件？
4. 为什么本机资源和 Sunshine 状态由后台任务维护？
5. 一份报告写库前经过哪些便宜防护与昂贵检查？
6. 内部数据库错误为什么不直接返回完整文本？

下一章：[06. Agent 详解](06-Agent详解.md)。
