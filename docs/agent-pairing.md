# Agent 一次性授权配对协议

本文描述当前 UnionC Agent 的一次性授权配对与实例删除策略。身份建立只使用 v2 配对；
不改变指标数据面：配对完成后仍通过 `/api/agent/v1/report` 主动上报。

## 设计目标

- 管理员先在管理台预留一个实例；
- Agent 软件通过独立渠道安装，管理台不托管安装包；
- Windows 本机页或公开激活页只提交短时一次性授权密钥，不接触长期 Agent secret；
- Agent 本地生成 secret，Server 只保存 SHA-256；
- 配对中断或响应丢失后可以安全重试；
- 退役只允许永久删除；再次接入必须创建新的实例与历史身份；
- 继续保持 Agent 零入站端口、无命令通道、无自更新器；
- 线路状态与本地状态都采用唯一的当前 schema，不接受旧字段或旧状态名。

## 参与方

| 参与方 | 职责 | 持有的秘密 |
|---|---|---|
| 管理员浏览器 | 创建待激活实例、把授权密钥交给安装人员、改名或永久删除实例 | 管理员会话、一次性授权密钥 |
| Windows 本机页 | 一次填写 Server 地址和授权密钥，请求固定 UAC 配对模式 | 本次操作内的授权密钥 |
| 公开激活页 | CLI/其他平台核对设备信息、输入授权密钥 | 一次性授权密钥 |
| Agent | 生成通信 secret、建立配对请求、轮询状态、持久化身份 | agent secret、polling secret |
| Server | 校验并原子绑定邀请与配对请求、保存哈希、认证报告 | 不持有 Agent secret 明文 |

同一人可以同时扮演管理员和安装人员。Windows 的交互流程在目标设备本机页完成；
公开激活页不要求管理员与安装人员处于同一设备或同一浏览器。

## 状态机

### 管理员邀请

```text
pending ──激活──> active
   │
   ├──过期──> expired（响应层计算状态）
   └──取消──> cancelled
```

### Agent 配对请求

```text
waiting ──绑定邀请──> active
   │
   ├──过期──> expired（响应层计算状态）
   └──拒绝──> denied
```

### 主机实例

```text
激活事务创建 active 实例 ──管理员永久删除──> 数据库中不存在
```

删除后的设备再次接入时，管理员创建新的待激活实例；Server 分配新的 `instance_id`。

网络错误不会触发状态转换。Server 只在数据库事务提交后返回成功；客户端丢失成功响应时，
用相同请求重新查询或提交会得到同一结果。

## Windows 本机一次提交

Windows 托盘打开只监听随机 `127.0.0.1` 端口的本机配置页。用户在同一表单中填写：

- UnionC Server HTTPS 地址；
- 管理台创建实例时显示一次的授权密钥；

用户提交并确认 UAC 后，提权 Agent 在同一次操作中建立 pairing request、调用
`POST /api/agent/v2/activate` 提交授权密钥，再用 polling secret 轮询到 `active`。
成功后本机页只显示结果，不会再打开远程激活页要求二次输入。授权密钥不写入
托盘偏好、URL 或日志，长期 agent secret 仍只存在受保护的 Agent 状态目录。

## CLI/其他平台公开激活时序

CLI 和其他平台输出专属 `/agent/activate/{request_id}`，时序如下：

```text
管理员 Web                Server                  Agent                 安装浏览器
    │                        │                       │                       │
    │ POST agent-instances   │                       │                       │
    ├───────────────────────>│                       │                       │
    │ instance_id + code     │                       │                       │
    │<───────────────────────┤                       │                       │
    │                        │                       │                       │
    │              独立、安全地交付一次性 code                              │
    │───────────────────────────────────────────────────────────────────────>│
    │                        │                       │                       │
    │                        │ POST pairing request  │                       │
    │                        │<──────────────────────┤                       │
    │                        │ request_id + URL      │                       │
    │                        ├──────────────────────>│                       │
    │                        │                       │ 打开/打印 URL          │
    │                        │                       ├──────────────────────>│
    │                        │ GET request summary   │                       │
    │                        │<──────────────────────────────────────────────┤
    │                        │ os/arch/version       │                       │
    │                        ├──────────────────────────────────────────────>│
    │                        │ POST activate(code)   │                       │
    │                        │<──────────────────────────────────────────────┤
    │                        │ 原子绑定并激活凭据      │                       │
    │                        ├──────────────────────────────────────────────>│
    │                        │                       │                       │
    │                        │ POST status + polling secret                  │
    │                        │<──────────────────────┤                       │
    │                        │ active + instance_id  │                       │
    │                        ├──────────────────────>│                       │
    │                        │                       │ 按日志提交并开始上报     │
```

## 管理台接口

管理台接口走现有管理员会话和 CSRF 防护。

### 创建待激活实例

```http
POST /api/monitoring/agent-instances
Content-Type: application/json
X-CSRF-Token: ...

{
  "display_name": "机房 A 主机",
  "expires_in_minutes": 15
}
```


创建响应：

```json
{
  "request_id": "管理员邀请 UUID",
  "instance_id": "预留的最终主机 UUID",
  "display_name": "机房 A 主机",
  "status": "pending",
  "expires_at": "2026-08-15T12:15:00Z",
  "created_at": "2026-08-15T12:00:00Z",
  "activation_code": "uci_..."
}
```

`activation_code` 只在创建响应出现一次，响应设置 `Cache-Control: no-store`。
`display_name` 是 Server 为该实例持有的名称，不来自 Agent。激活时用它初始化主机卡片；
后续 Agent 上报不会覆盖。每次创建邀请都会预留新的 `instance_id`。

### 列出与取消邀请

```http
GET    /api/monitoring/agent-instances
DELETE /api/monitoring/agent-instances/{invite_request_id}
```

列表不返回激活码。过期但尚未清理的数据库记录在响应中表示为 `expired`。

### 管理主机

```http
PATCH  /api/monitoring/managed-instances/{instance_id}
DELETE /api/monitoring/managed-instances/{instance_id}
```

`PATCH` 的 JSON 为 `{"remark":"..."}`，只更新 Server 持有的名称；Agent 上报不会覆盖。
`DELETE` 永久删除实例、历史、credential、配对请求和全部关联邀请，不可恢复。当前版本
没有主机撤销端点，也没有对既有 `instance_id` 再签发 credential 的端点。

## Agent 接口

这些端点不使用管理员会话。生产环境仍要求 HTTPS 反向代理契约。

### 创建配对请求

```http
POST /api/agent/v2/pairing-requests
Content-Type: application/json

{
  "host": {
    "id": "本地临时安装 UUID",
    "os": "linux",
    "os_version": "...",
    "kernel_version": "...",
    "arch": "x86_64",
    "agent_version": "0.3.6"
  },
  "token_hash": "64位小写SHA-256十六进制",
  "polling_secret_hash": "另一份64位小写SHA-256十六进制"
}
```

两个哈希必须不同。请求中的 `host.id` 不是最终身份；最终 `instance_id` 由管理员创建邀请时
预留，防止匿名 Agent 决定数据库身份。

响应：

```json
{
  "request_id": "Agent配对请求UUID",
  "activation_url": "/agent/activate/Agent配对请求UUID",
  "expires_in": 900,
  "poll_interval": 5
}
```

`activation_url` 是相对路径。Agent 用 `pair --server` 的可信 origin 拼出完整 URL，不能
根据不可信响应头或页面 Host 推导 Server 地址。

### 查询供浏览器核对的设备摘要

```http
GET /api/agent/v2/pairing-requests/{request_id}
```

返回有限、非秘密字段：

```json
{
  "request_id": "...",
  "os": "linux",
  "arch": "x86_64",
  "agent_version": "0.3.6",
  "status": "waiting",
  "expires_at": "2026-08-15T12:15:00Z"
}
```

该接口不会返回 token hash、polling secret hash 或最终 credential。

### Agent 轮询状态

```http
POST /api/agent/v2/pairing-requests/{request_id}/status
Authorization: Pairing <polling-secret>
```

等待状态：

```json
{"status":"waiting"}
```

成功状态：

```json
{"status":"active","instance_id":"最终主机UUID"}
```

Agent 状态与公开摘要在线路上只返回 `waiting`；客户端不接受 `pending` 等旧拼写。
Agent 必须遵守 `poll_interval`。网络错误使用退避，但不能因此
生成新的 secret 或新的配对请求；否则浏览器批准的请求可能与 Agent 正在等待的请求错位。

## 一次性授权激活接口

公开浏览器激活页不要求管理员登录；Windows 提权 Agent 也可以调用同一端点。一次性
授权密钥本身是一个短时 capability，因此必须高熵、
短时、单次且受限流保护。

```http
POST /api/agent/v2/activate
Content-Type: application/json

{
  "request_id": "Agent配对请求UUID",
  "activation_code": "uci_..."
}
```

成功响应只含公开状态：

```json
{"instance_id":"最终主机UUID","status":"active"}
```

浏览器不会收到 Agent secret、token、refresh token 或可以替代 Agent 上报的任何凭据。

## Agent 本地状态

配对相关文件写入 Agent 的私有状态目录：

| 文件 | 内容 | 权限/处理 |
|---|---|---|
| `host-id` | Server 分配的最终 `instance_id` | 原子替换，Unix 0600 |
| `agent-token` | Agent 本地生成的通信 secret | 原子写入，Unix 0600 |
| `active-binding.json` | 当前 credential generation、instance ID 与 report endpoint | 原子替换，Unix 0600 |
| `pairing-state.json` | request ID、polling secret、Server origin、到期时间 | 配对期间私有保存，成功后删除或标记完成 |
| `auth-state.json` | `authorized` 或 `reauth_required` 诊断状态 | 原子替换，Unix 0600 |
| `spool/` | 未送达的报告 | 私有目录、容量受限 |

配对完成前生成的 agent secret 必须先可靠落盘，再把它的哈希提交给 Server。顺序反过来会
在本机写盘失败时创建一个 Server 已认可但 Agent 已丢失明文的 credential。

配对完成使用 `Activating` 作为 crash-safe 提交日志：token、host ID、`active-binding.json`
和授权状态各自以原子文件替换幂等写入，最后才把 pairing state 写成 `Active`。这是可恢复的
多文件提交，不是一次覆盖全部文件的文件系统原子事务。服务恢复只需要写私有状态目录；
root 管理的系统配置不属于该事务。`active-binding.json` 是当前 credential 投递端点的权威
记录，主配置继续提供 TLS、超时和采样设置。显式 `pair` 命令完成完整代际核对后，才把
report endpoint 同步到持久化 JSON 并清空其中的 `pairing_endpoint`。若
`UNIONC_AGENT_PAIRING_ENDPOINT` 长期存在，下次配置加载会重新应用该覆盖；否则
pairing/report 分域部署须在下一次配对前通过配置或环境变量恢复 bootstrap endpoint。
pairing origin 必须能提供 Server 相对路径指向的 `/agent/activate/...` SPA。

## 报告认证与 ACK

配对成功后使用现有数据端点：

```http
POST /api/agent/v1/report
Authorization: Bearer <agent-secret>
```

Server 只按 secret 的 SHA-256 查找仍存在的 credential。当前成功契约唯一为 HTTP 202、
`Content-Type: application/json` 和完整 JSON ACK；Agent 不能把其他 2xx 或其他媒体类型
当作成功。ACK 还必须核对：

- `host_id` 等于本机保存的 `instance_id`；
- `report_id` 等于本次发送的报告 ID；
- 响应结构完整且在体积上限内。

`accepted=false` 仍是当前契约的一部分：它表示同一 `report_id` 已在先前请求中持久化，
常见于 Server 已提交但首次响应丢失后的幂等重放。此时 ACK 必须复用首次 `received_at`，
Agent 可以安全删除对应 spool 项。

代理误路由返回的 200 HTML、空响应或其他 JSON 都视为投递失败，报告保留在 spool。

## 再次配对

再次执行配对会建立另一台逻辑主机，而不是修改既有实例：

1. 管理员点击“+”创建新邀请，Server 预留新的 `instance_id`；
2. Agent 建立新的 pairing request，浏览器或 Windows 本机页完成绑定；
3. Server 在同一事务中创建新主机和唯一 credential；
4. Agent 以可恢复的 `Activating` 日志替换本地 host ID、token 与 credential endpoint 绑定；
5. 旧 Server 实例及其历史保持原样，直到管理员明确删除。

新配对仍在 creating/pending，或最终被拒绝、过期时，本地 `auth-state` 已是 `authorized`
的 Agent 会继续使用原 host/token 上报；只有新实例成功激活并完成本地提交后才切换身份。
报告写事务会再次核对该 host 与精确 token hash 是否仍存在：若管理员删除先提交，在途报告
返回 401 且不落库；若报告先取得写事务，则报告先完成、删除随后级联清理，顺序明确。

尚未获管理员批准的 pending pairing request 不属于审计历史：过期后会由后续创建事务
每批最多清理 512 条，并且全库最多保留 4096 条仍在有效期内的未决请求。达到上限时创建
接口返回 429，避免匿名请求在 15 分钟有效期内无界扩大 SQLite/WAL 与磁盘占用；已激活
记录不计入这个上限。被拒绝的请求会保留 30 天供 Agent 读取终态，之后与过期 pending
记录共用同一批 512 条的有界清理。未激活邀请的 expired/cancelled 终态同样保留 30 天；
后续创建邀请时每批最多回收 512 条，已激活邀请只随所属实例永久删除。

## 永久删除

删除是唯一的主机退役操作：

- 删除事务先写审计，再清理该实例的配对请求和邀请；
- 删除主机行会级联清理历史报告与 credential；
- 删除后旧 secret 的上报返回 401 + `unauthorized`；
- 数据不可恢复；设备再次接入必须创建新实例并取得新的 `instance_id`。

## 响应丢失与并发语义

| 场景 | 要求 |
|---|---|
| 创建配对请求响应丢失 | Agent 使用已持久化状态恢复，不生成另一份通信 secret |
| 激活事务成功但浏览器响应丢失 | 相同 code + 相同 request 重试返回同一成功结果 |
| code 已绑定 A，又尝试绑定 B | 拒绝，不能转移实例 |
| 两个浏览器同时提交同一码 | SQLite `BEGIN IMMEDIATE` 在单写者事务中串行校验并消费，只有一个 request 能提交 |
| Agent 状态响应丢失 | 使用 polling secret 重试，持续返回同一 instance ID |
| 首次报告投递失败 | 配对仍成功；`run` 已先把报告写入 spool，可重试失败时保留，永久内容错误时丢弃 |
| 新配对期间本地凭据并发 | 原 reporter 继续工作；新实例成功激活并完成本地提交后才切换，失败不会破坏原授权 |
| 切换服务器后遗留旧身份 spool | 新服务端返回 403 + `agent_host_mismatch`；Agent 只丢弃不可能匹配新凭据的旧报文并继续 FIFO |

## 安全要求

- 激活码数据库只存 SHA-256；
- 默认 15 分钟过期、单次使用；
- 配对请求默认 15 分钟过期；
- 所有秘密响应使用 `Cache-Control: no-store`；
- 激活码只放 POST body，不放 URL、查询参数或日志；
- 匿名入口使用独立的小 body 上限；
- 在 JSON 解析与数据库写入前执行来源限流；
- 状态轮询校验 polling secret，Agent 遵守 Server 返回的间隔，Server 另有来源限流；
- 激活页不加载第三方脚本，使用严格 CSP 和 `Referrer-Policy: no-referrer`；
- 激活前展示 OS、架构、Agent 版本和配对 request；
- 操作写入审计日志，但审计 detail 不包含激活码或任何 secret；
- mTLS 若在站点级强制，bootstrap/pairing 必须使用不要求客户端证书的独立入口。

## 版本边界

当前版本不实现 `/api/agent/v1/register`、enrollment code、长期 enrollment token、浏览器
返回明文 report secret 或旧 credential 回填。`/api/agent/v1/report` 中的 `v1` 是当前数据
面固定路径，不表示仍支持旧的身份建立流程。旧 Agent 必须清理本地旧身份、安装当前版本并
创建新实例并配对；Server 只接受当前协议和当前数据库 schema。

## 软件分发边界

配对协议假定 Agent 已通过可信渠道安装。UnionC Web 不提供：

- 安装包下载或镜像；
- 平台/架构自动识别；
- PowerShell、shell 或 pkg 安装命令拼接；
- 在线更新或二进制下发。

签名包、系统服务全新安装、同版本重装和卸载由独立发布渠道、操作系统包管理器或组织 MDM 完成。
Web 只提示用户先安装 Agent，再运行 `unionc-agent pair --server https://...`。

## 测试清单

Server 与 Agent 的合同测试至少覆盖：

- 新实例完整配对；
- 再次配对获得新的 instance ID，且不会合并旧实例历史；
- 激活码错误、过期、取消和重放；
- 配对请求错误、过期和 polling secret 错误；
- 同码并发提交；
- 激活响应和状态响应丢失后的重试；
- 永久删除级联清理历史与 credential，旧 secret 随后收到 401；
- Server/Agent 在配对中途重启；
- Activating 日志能在任意私有状态文件提交中断点幂等恢复，不依赖系统配置可写，且
  `Active` 最后写入；
- 错路由 200 响应不能被当作报告成功；
- 首次报告失败不回滚配对；
- 旧状态名、旧 pairing-state 字段及旧身份端点被严格拒绝。
