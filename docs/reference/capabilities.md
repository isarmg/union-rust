# UnionC 功能与边界说明

本文回答四个问题：UnionC 当前有哪些功能、哪些功能构成产品核心、哪些功能只为核心能力
提供可靠性与安全保障、哪些功能可以按需关闭，以及项目明确不准备承担哪些职责。

本文按当前代码而不是产品设想编写。接口细节与部署命令仍以
[DOCUMENTATION.md](../../DOCUMENTATION.md) 为准；本文更关注能力分层、依赖关系、失败语义和
启用条件。

审阅范围覆盖仓库内全部一方源码、当前数据库基线、测试、构建脚本和三平台打包定义，包括
`server/`、`agent/`、`protocol/`、`web/`、`.github/workflows/` 与 `docs/`。
`target/`、`node_modules/`、`web/dist/`、`dist/`、运行时 SQLite 文件和 Git 元数据
属于生成物、依赖或运行数据，不作为功能实现重复阅读。若本文与其他说明冲突，以当前源码、
唯一数据库基线和包清单为准。

## 1. 功能等级

| 等级 | 名称 | 判断标准 | 缺失后的影响 |
|---|---|---|---|
| **C：核心** | 产品核心能力 | 直接构成“只读、多主机、跨平台监控” | 产品不再是 UnionC |
| **G：保障** | 安全、可靠性与运维保障 | 本身不产生主要业务价值，但保证核心能力可信、可恢复、可维护 | 核心功能可能能跑，但不适合长期或生产使用 |
| **T：可取舍** | 可简化或替换的产品深度/实现 | 当前已经实现，但可在接受明确代价后裁剪或换实现 | 产品仍成立，但体验、诊断深度或目标平台会收缩 |
| **O：可选** | 独立增强能力 | 可以关闭或完全不部署，核心监控仍成立 | 只失去对应增强能力 |
| **D：交付边界** | 外部部署与集成职责 | 项目定义协议或提供参考，但不由管理台在线完成 | 需要使用方或独立发布渠道完成 |
| **N：明确不做** | 安全边界或非目标 | 刻意不实现，不属于待办项 | 避免扩大受攻击面和产品职责 |

`T` 与 `O` 的区别是：`O` 已有明确的不启用方式或独立依赖，`T` 往往需要调整产品
承诺、删改代码或换一种实现。例如 OTLP 是编译时可选功能；而把历史曲线裁成“只看当前值”
属于产品取舍，不是现成开关。

“核心”还取决于交付口径。若产品承诺是“只读多主机监控”，Agent 链路是唯一业务核心，
Sunshine 可完全不配置；若对外承诺是当前仓库的完整产品“监控 + Sunshine 管理”，则 O1
应提升为该产品组合的业务核心。本文用前一种最小闭环判级，并在第 17 节给出不同组合。

## 2. 一页总览

| 编号 | 能力 | 等级 | 默认状态 | 主要组件 |
|---|---|---:|---|---|
| C1 | 管理员创建待激活 Agent 实例 | C | 启用 | Server、Web |
| C2 | 浏览器一次性码激活 | C | 启用 | Server、Web |
| C3 | Agent 本地生成并保管通信密钥 | C | 启用 | Agent、Server |
| C4 | CPU、内存、磁盘、网络采集 | C | 启用 | Agent |
| C5 | 温度与 GPU 能力探测 | C/O | 平台相关 | Agent |
| C6 | Agent 主动 HTTPS 上报 | C | 启用 | Agent、Server |
| C7 | 当前状态、详情与历史查询 | C | 启用 | Server、Web、SQLite |
| C8 | 主机撤销、重新配对与生命周期管理 | C | 启用 | Server、Web、Agent |
| G1 | 本地 spool、断线补传与退避 | G | 启用 | Agent |
| G2 | 报文校验、ACK 校验、幂等与乱序保护 | G | 启用 | Agent、Server |
| G3 | 管理员认证、会话、CSRF 与改密 | G | 启用 | Server、Web |
| G4 | HTTPS 反代契约、安全响应头与可选 mTLS | G | 生产必需/部分可选 | Server、Agent、反向代理 |
| G5 | 限流、请求体上限和上游响应上限 | G | 启用 | Server |
| G6 | 当前 SQLite schema、保留期、完整性与同版本备份恢复 | G | 启用 | Server、SQLite |
| G7 | 审计、请求 ID、结构化日志、健康与就绪探针 | G | 启用 | Server |
| G8 | Agent 状态、doctor 与 probe 诊断 | G | 启用 | Agent |
| O1 | Sunshine 多主机管理 | O | 配置后启用 | Server、Web |
| O2 | OTLP/HTTP 指标导出 | O | Cargo 默认未编译；当前 macOS 发布构建显式启用 | Agent、外部 Collector |
| O3 | NVIDIA、AMD、Intel、Windows WDDM GPU 扩展采集 | O | 平台/feature 相关 | Agent |
| O4 | SSE 服务状态推送 | O/G | 启用，失败可回落轮询 | Server、Web |
| O5 | Agent 客户端证书认证 | O/G | 默认关闭 | Agent、反向代理 |
| O6 | Windows 通知区托盘与本机浏览器配置 | O | MSI 默认安装，用户可退出 | Agent tray、Windows SCM |
| T1 | Server 本机总览、短期浏览器曲线与视觉增强 | T | 启用 | Server、Web |
| T2 | 逐设备明细、温度/GPU 深度与历史展示深度 | T | 平台相关 | Agent、Server、Web |
| D1 | Linux、Windows、macOS 软件分发 | D | 外部完成 | 独立发布渠道、包管理器、MDM |
| D2 | TLS 证书签发、域名和反向代理 | D | 部署方完成 | Caddy/nginx/组织 PKI |
| D3 | OTLP 接收端、异机备份和灾备保管 | D | 部署方完成 | 外部基础设施 |
| N1 | 服务端向 Agent 下发命令 | N | 永不启用 | — |
| N2 | 远程执行、文件传输、进程控制 | N | 永不启用 | — |
| N3 | Agent 在线自更新 | N | 永不启用 | — |
| N4 | 管理台托管或动态生成安装包 | N | 永不启用 | — |

## 3. 核心能力

### C1–C3：实例创建与一次性授权激活

管理员在管理台创建一个**待激活实例**。服务端预留稳定 `instance_id`，生成短时、单次的
一次性授权密钥（协议字段 `activation_code`），数据库只保存其哈希。待激活实例不是在线主机；
过期或取消后不会进入正常
主机列表。

已独立安装的 Agent 通过 `pair`（Windows 也可从托盘选择“配对/重新配对”）后：

1. 在本机生成高熵通信 secret 和 polling secret；
2. 只把二者的 SHA-256 哈希以及有限的设备展示信息提交给 Server；
3. 当前 Windows x64 MSI 的本机配置页在发起前收集 Server 地址和授权密钥，提权 Agent 直接
   提交密钥；CLI/其他平台则输出专属浏览器激活 URL；
4. Windows 无需二次输入；公开激活页允许用户核对设备摘要后输入同一枚一次性授权密钥；
5. Server 在一个事务中绑定待激活实例、配对请求与通信密钥哈希；
6. Agent 轮询到 `active` 后保存 Server 分配的 `instance_id`，开始上报。

长期通信 secret 不经过浏览器、URL、剪贴板或 Server 响应。本机页或公开激活页只证明
“持有一次性授权密钥并确认了这台设备”，不能读取主机数据，也不能代表 Agent 上报。

配对涉及四种标识，不能混用：

| 标识 | 是否保密 | 生命周期 | 用途 |
|---|---|---|---|
| `instance_id` | 否 | 主机历史的整个生命周期 | Server 分配的稳定主机身份 |
| `activation_code`（授权密钥） | 是 | 短时、单次 | 把管理员创建的实例授权给一个配对请求 |
| `pairing request_id` | 否 | 短时 | 浏览器页面与 Agent 配对请求的关联键 |
| `polling_secret` | 是 | 配对完成或过期为止 | Agent 查询配对结果 |
| `agent secret` | 是 | 撤销或重新配对为止 | 正常上报的 Bearer 凭据；明文只在 Agent 本机 |

### C4–C5：跨平台只读采集

所有平台都采集：

- 主机身份：主机名、OS、OS 版本、内核版本、架构、Agent 版本；
- CPU：总占用、逻辑/物理核心数量、每核占用；
- 内存：总量、使用量、可用量、swap；
- 网络：接口累计字节、速率、包数量与错误计数；
- 磁盘：设备、挂载点、文件系统、容量、可用空间、读写累计量与速率；
- 运行时间与 Agent 自身健康。

温度与 GPU 属于“协议核心、采集实现按平台可选”：协议始终能表达这些数据，但平台不支持、
没有设备、缺驱动或权限不足时，Agent 通过 capability 明确说明原因，不用数值 `0` 冒充
“采集成功且当前占用为零”。

### C6：单向数据面

Agent 只发起出站 HTTPS 请求。正常上报采用每实例凭据，Server 验证凭据归属与报文中的
`host.id` 一致后才落库。

数据面刻意保持普通 HTTP POST，而不是 WebSocket：

- 无 Agent 入站端口；
- 易穿越 NAT、企业代理与防火墙；
- 断网重试和请求幂等语义清晰；
- 不产生服务端命令通道。

### C7：当前状态与历史

Server 保存两类数据：

- `monitored_hosts`：主机身份、最近出现时间、能力与最新报告指针；
- `agent_metric_reports`：每次报告的指标摘要，完整 JSON 只保留每台主机最新一份。

管理台提供：

- 主机分页列表；
- online、stale、offline 状态；
- 当前 CPU、内存、网络、磁盘、温度和 GPU；
- 完整硬件与 capability 详情；
- 最多 1000 个历史采样点的趋势图；
- 待激活、已激活、已撤销等生命周期状态。

历史热路径只读取摘要数值列，不为画曲线重复读取和反序列化完整 JSON。

### C8：生命周期

生命周期操作分开表达：

- **取消待激活实例**：使尚未使用的激活码失效；
- **撤销 Agent**：保留实例和历史，但拒绝全部有效凭据；
- **重新配对**：为现有实例建立新凭据，不建立第二份历史身份；
- **历史清理**：由保留期任务负责；当前管理台不提供按主机硬删除。若未来增加，必须与
  凭据撤销分离并保留最小 tombstone，不能用删除主机行代替退役。

被撤销的 Agent 没有自动复活或直接 token 轮换入口。Server 返回明确的撤销语义，Agent
进入需要人工重新配对的状态。

## 4. 保障能力

### G1：断线保护

常驻 `run` 在发送前先把每份报告持久写入本地 spool；一次性投递只在可重试失败时入队：

- 临时文件写入、`fsync`、原子 rename；
- 固定容量预算，避免无限占满系统盘；
- FIFO 补传；
- 损坏文件隔离，不阻塞后续队列；
- 网络和 5xx 使用带 jitter 的指数退避；
- 可确定为内容错误的报文不会反复塞满队列。

OTLP 不进入这条可靠投递承诺；它只在 UnionC ACK 后尽力旁路，Collector 失败不会反向改变
主上报结果。

### G2：协议正确性

Server 对 schema、UUID、时间、数量、字符串长度、控制字符和数值范围做完整校验。Agent
只有在解析 Server JSON ACK 且核对 `host_id`、`report_id` 后，才能删除对应 spool 项。
报告中的 `host.agent_version` 必须与当前 Server package version 完全一致，旧 Agent 即使
沿用相同的 `schema_version=1` 也会被明确拒绝。

`report_id` 提供至少一次传输下的存储幂等。相同主机、相同 ID 的后续请求采用首写为准，
不产生第二条历史，也不刷新 `last_seen_at` 或改变身份/能力；同一 ID 若已属于另一主机则
返回冲突。

乱序补传进入历史，但不会让当前状态倒退到旧采样。

### G3：控制台认证

- 单管理员本地账号；
- bcrypt 密码哈希；
- HttpOnly 会话 Cookie；
- 每会话随机 CSRF token，双提交校验；
- 修改密码与撤销该账号全部会话由后端在同一状态转换中完成；Web 不再依赖第二次尽力而为
  的注销请求，全部浏览器都必须重新登录；
- 登录限流同时考虑来源 IP 与用户名；
- 未知用户名执行 dummy hash，缩小时序差异。

当前不是多租户系统，也没有普通用户、组织、owner 或 RBAC。

### G4–G5：网络与输入防护

生产环境要求：

- Server 只绑定回环地址；
- 由可信反向代理终止 HTTPS；
- 正确传递 `X-Forwarded-Proto` 和 `X-Forwarded-For`；
- Server 与可信反代分别以安全环境配置共享同一个 `UNIONC_PROXY_SECRET`，其值必须是
  **64 位小写十六进制**；反代必须覆盖（不能追加或透传客户端提供的）
  `X-UnionC-Proxy-Secret`，并写入该共享值；
- 对控制台、Agent 和上游响应设置独立体积上限；
- 在昂贵 JSON 解析和数据库操作之前尽可能完成限流与凭据检查；
- 对所有响应下发 CSP、`nosniff`、Referrer Policy 等安全头。

生产模式下，登录、改密与 Agent API 缺少上述代理证明，或证明与
`UNIONC_PROXY_SECRET` 不匹配，都会返回 421。该值不能使用占位符、不能复用数据库主密钥，
也不能由外部客户端自行提供。

Agent 支持系统信任库、额外私有 CA 和可选客户端证书。若启用 mTLS，首次配对入口必须与
要求客户端证书的数据入口分开，因为尚未配对的 Agent 不可能预先持有客户端证书。

### G6：数据库与保留期

- SQLite 是 Server 内嵌的唯一持久层，不提供内存替代实现；
- 活动库固定在本机数据目录，不支持网络文件系统、多 Server 共享写入或水平扩展；
- WAL 允许读取与一个写事务并行，但写入仍按数据库级串行；当前默认面向单 Server、约 20 台
  主机，断线集中补传和更大规模必须先做容量压测；
- 空库由二进制内嵌的唯一当前基线初始化，已有库必须与当前 schema 精确一致；
- 不运行旧 schema 回填、前缀升级或格式转换；
- 遥测与审计按独立保留期清理；
- 遥测每批 10 000 行、审计每批 1 000 行独立提交，避免无上限删除长时间独占写锁；
- 每台主机最新报告有保留例外；
- `backup`、`restore` 与 `integrity-check` 提供一致性快照、带 SHA-256 清单的恢复和完整性检查；
- 数据库、管理员配置和主密钥仍需作为一个灾备集合异机保管。

### G7–G8：可观测与诊断

Server 提供：

- liveness 与 readiness；
- 请求 ID 生成和响应传播；
- 结构化请求日志；
- 状态变更审计；
- 后台服务探测和本机资源快照；
- SSE 单次票据与轮询回退。

Agent 提供：

- `probe`：只做本地采集，不联网；输出中的 `host.id` 每次临时生成且不持久化；
- `status`：查看实例、凭据、配对和 spool 状态；
- `doctor`：默认只读检查配置、采集、凭据、配对和 spool；只有显式增加
  `--delivery` 才验证网络投递；
- `once`：补传积压并投递当前采样；
- `run`：后台常驻。

## 5. 可选能力

### O1：Sunshine 管理

Sunshine 是独立增强模块，不是只读 Agent 监控的依赖。功能包括：

- 多 Sunshine 主机增删改查，并按持久化位置稳定排序；
- 连通性和认证状态探测；
- 应用列表、保存、关闭与删除；
- 已配对客户端列表、启停、单个或全部解除配对；
- 配置读取、保存与 locale 查询；
- PIN 配对、保存或清空 UnionC 用来连接 Sunshine 的管理凭据、重启和显示重置；
- 封面读取与上传（当前仅后端 API，Web 尚无入口）；
- 按主机代理读取 Sunshine API 日志。

Sunshine 密码由 Server 加密存储，浏览器不保存。上游主机被视为不可信，响应有体积上限、
图片 MIME 白名单和 TLS 校验策略；HTTP 3xx 不自动跟随，避免把固定 API 请求转发或重放
到未经配置的新目标。

当前 Web 对 Sunshine 的实际操作面如下：

| 分区 | 用户能做什么 | 数据与安全语义 |
|---|---|---|
| 主机侧栏 | 新增、选择、改名、修改 IPv4/IPv6/域名、端口、账号、密码和 TLS 校验；打开原生 Web；清密码、删除 | 新增会立即写入一个默认实体；密码不回显，空密码有显式清除语义；生产环境禁止关闭 TLS 校验 |
| 应用 | 查看、新建/编辑名称、命令、工作目录、退出超时；结束当前会话；删除 | 严格使用当前 Sunshine `{apps}` 与 kebab-case 字段；编辑常用字段时保留当前 API 返回的高级字段 |
| 客户端 | 查看、启用/禁用，单个或全部解除配对 | 严格使用当前 `{status,named_certs}` 响应；解配与批量操作有确认 |
| PIN 配对 | 提交 4～8 位数字 PIN 和可选设备名 | Server 直接代理到选中 Sunshine，不经 Agent |
| 配置 | 键值只读预览，或编辑完整 JSON 对象并保存 | 保留字符串、数字、布尔和嵌套类型；非对象或无效 JSON 禁止保存 |
| 系统 | 重启 Sunshine、重置显示设备持久化 | 都是高权限上游操作，Web 确认后调用；不是控制监控 Agent |
| 日志 | 查看选中主机的上游 API 日志 | 30 秒刷新，只渲染最新 2,000 行；不写入 UnionC 业务库 |
| 封面/locale | 后端支持读取/上传封面和 locale | 当前 Web 无入口 |

主机 create、PATCH、DELETE 使用乐观缓存、失败回滚和 mutation 屏障，避免较早发出的 GET
晚返回后造成“已创建主机消失、已删除主机复活、已修改字段回滚”。这部分一致性机制是管理
操作可靠性保障；视觉上的即时动画可以简化，跨请求顺序保护不应直接删除。

### O2：OTLP 导出

启用 `otlp` feature 并配置 `otlp_endpoint` 后，Agent 会把数值指标编码为 gzip 压缩的
OTLP/HTTP protobuf，投递到外部接收端。

- 只在 UnionC ACK 后尝试，不影响已经成功的主上报；
- 常驻 `run` 使用容量 128 的独立有界队列，队列满或接收端失败只记录告警；
- `once` / `doctor --delivery` 不导出旧 spool，当前报告在主上报成功后同步尝试 OTLP；
- 不含完整资产/capability 快照，但包含识别时间序列所需的主机与设备属性；
- 仓库不提供 Collector、时序数据库或可视化栈。

### O3：扩展 GPU 采集

- NVIDIA：NVML，受 Cargo feature 控制；
- Linux AMD/Intel：DRM/sysfs；
- Windows：WDDM 性能计数器；
- macOS：缺少稳定公开的系统级 GPU 占用接口时报告 Unsupported。

### O4：SSE

SSE 只用于管理台及时接收服务状态变化。连接使用 60 秒有效、单次消费且绑定会话的 ticket。
SSE 不可用时 Web 回落到普通轮询，因此它不是核心数据面。

### O5：Agent 客户端证书与私有 CA

Agent 的主上报和配对客户端可使用系统信任库，也可额外装载私有 CA。Linux 使用 PEM
客户端身份，Windows/macOS 使用 PKCS#12。客户端证书属于部署增强，不改变每实例 Bearer
凭据与报文身份校验；公网标准 TLS 可以不启用，企业私有 PKI 或高安全网络则可把它提升为
部署必需项。

当前 UnionC、pairing 与 OTLP 共用同一个 HTTP client 的 CA/客户端身份设置；OTLP 若要求
另一套客户端证书，需要通过认证网关隔离或修改代码。

若反向代理要求 mTLS，首次配对入口必须与已经持证的上报入口分开：未完成配对的 Agent
不可能预先拥有只在配对后签发的证书。证书签发、吊销与组织 PKI 不由 UnionC 管理台承担。

### O6：Windows 通知区托盘

Windows x64 MSI 默认安装一个与 `UnionCAgent` 服务分离的 GUI 托盘伴侣。它按登录会话以
普通用户权限运行，提供本机状态、配对/重新配对、Server `/api/health` 可达性检测以及服务
启停；只有配对和服务
控制等机器级操作才通过固定、非通用的子命令请求 UAC。用户菜单的“退出”也是机器级操作：
确认停止 LocalService 身份下的采集服务后才关闭托盘；如果 UAC 被拒绝或停服务失败，
托盘保持运行并显示错误。多用户/RDP 会话仍共享同一个机器级 Agent 身份。

本机配置页只绑定随机 `127.0.0.1` 端口，使用短时随机 capability 建立浏览器会话，并对
请求来源、方法、体积和并发设限；用户一次填写 Server 地址与短时、单次的授权密钥，
由提权 Agent 完成配对，不再跳转远程页面二次输入。授权密钥不写入托盘偏好，浏览器也不能
读取 `%ProgramData%` 内的 Agent secret。托盘
不是核心上报进程：它可以退出、被用户禁用自启动，或在无桌面会话的静默部署中完全不运行，
CLI 仍保留为诊断和自动化入口。

本地页打开后会自动检测连接并每 30 秒刷新，也可由用户立即重试；托盘菜单会把该结果与
Windows 服务状态并列显示。轻量探测只验证管理端可达，不替代 Server 依据认证遥测时间计算的
online/stale/offline 状态。

## 6. 交付边界

### D1：Agent 软件分发

管理台不托管、不拼装、也不动态选择安装包。Agent 应由独立且可信的软件渠道安装：

- Linux 包仓库、deb/rpm 或组织配置管理；
- Windows 签名 MSI、winget、GPO 或 MDM；
- macOS 签名并公证的 pkg 或 MDM；
- 无人值守部署可预装程序，但不能把已配对的状态目录克隆到多个主机。

管理台只负责实例创建、激活状态、撤销和重新配对。软件签名、当前版本安装/重装和卸载属于 Agent
发布工程，而不是 Server 在线功能。仓库内发布工程现在提供 DEB/RPM、Windows x64 WiX
MSI 和 macOS pkg：普通卸载保留身份，按平台顺序成功完成并验证的显式 purge 才清理本地
状态；完整退役仍要求先在 Web 撤销实例。Windows MSI 使用原生 SCM Service、独立托盘伴侣和 maintenance helper，不依赖
PowerShell；HKLM Run 为每个登录会话启动普通用户托盘，开始菜单提供手工入口。用户菜单
“退出”会先停止服务，而 MSI 在重装/卸载时发送的系统关闭消息只关闭托盘，
服务仍由 Windows Installer 与 SCM 处理。旧 PowerShell 计划任务安装不会被识别或迁移。具体能力和命令见
`docs/runbooks/agent-lifecycle.md`。

### D2–D3：外部基础设施

使用方负责：

- 域名、证书、反向代理和可选组织 PKI；
- SQLite 快照、管理员配置和主密钥的异机保管及恢复演练；
- 可选 OTLP 接收端；
- 操作系统服务管理与 Agent 软件更新渠道。

## 7. 明确不做的能力

以下能力会改变“只读 Agent”的信任边界，因此不是路线图中的缺口：

- 服务端下发 shell、PowerShell 或任意任务；
- 远程控制服务、进程、文件或系统配置；
- 从 Server 向 Agent 推送二进制或在线自更新；
- 通过 Agent 建立反向隧道；
- 把 Agent 变成通用远程管理工具；
- 管理台托管各平台安装包；
- 把 OTLP Collector、反向代理或分布式数据库集群内嵌进 UnionC。

Sunshine 管理是 Server 直接访问管理员显式配置的 Sunshine API，并不经监控 Agent 执行，
因此没有突破上述边界。

## 8. 平台矩阵

| 项目 | Linux Agent | Windows Agent | macOS Agent | Linux Server |
|---|---|---|---|---|
| CPU/内存/磁盘/网络 | 支持 | 支持 | 支持 | 本机快照 |
| 温度 | hwmon | sysinfo 能力范围内 | sysinfo 能力范围内 | — |
| NVIDIA GPU | NVML | NVML | 不支持 | — |
| AMD/Intel GPU | sysfs | WDDM 聚合 | 不支持 | — |
| 系统 CA | rustls | native-tls | native-tls | 由反代负责入站 TLS |
| 私有 CA | PEM | PEM CA | PEM CA | 由反代配置 |
| 客户端证书 | PEM | PKCS#12 | PKCS#12 | 由反代验证 |
| 推荐常驻方式 | systemd | LocalService 原生 SCM Service | LaunchDaemon | systemd |
| 桌面配置入口 | CLI | 通知区托盘 + 随机回环浏览器页（可退出） | CLI | — |
| 仓库内受管生命周期 | DEB/RPM：fresh、同版本 reinstall、remove、purge | x64 WiX MSI：fresh、同版本 reinstall、remove、`PURGE=1` | pkg：fresh、同版本 reinstall、remove、purge | DEB/RPM |
| 推荐组织分发方式 | apt/rpm/配置管理 | MSI/winget/GPO/MDM | pkg/MDM | apt/rpm |

平台缺少某个传感器或驱动属于 capability 差异，不应让整个 Agent 启动失败。

## 9. 关键运行流程

### 9.1 Server 首次启动

1. 解析数据目录并建立私有目录；
2. 初始化或载入主密钥环；
3. 载入管理员配置；
4. 空目录创建 SQLite 当前 schema，已有库则要求精确匹配；
5. 从数据库载入持久化的 Sunshine 主机配置；
6. 建立共享状态并启动资源采样、服务探测、内存清理和保留期任务；
7. 在回环地址监听，由反向代理提供外部 HTTPS。

### 9.2 Agent 首次接入

1. 外部渠道安装 Agent；
2. 管理员在 Web 创建待激活实例并把一次性授权密钥交给安装人员；
3. Windows 安装人员从托盘选择“配对/重新配对”；其他平台或诊断场景运行 `pair`；
4. Windows 在目标设备的本机配置页输入 Server 地址和授权密钥后直接配对；CLI/其他平台
   则打开公开激活页、核对设备摘要并输入密钥；
5. Agent 轮询到成功状态，以 `Activating` 日志幂等提交多个本地文件：各文件原子替换，
   `Active` 最后写入；
6. 系统服务进入 `run`：首次报告先持久入队；可重试失败保留队首且不回滚配对，永久内容
   错误则丢弃该报告。

### 9.3 正常上报

1. 读取 spool 健康与积压数量；
2. 采集快指标，按慢周期刷新温度等指标；
3. 先把当前采样持久入队，再唤醒独立 worker 按 FIFO 补传（每轮最多 32 批）；
4. Server 鉴权、限流、解析、校验并事务写入；
5. Agent 校验 ACK 后删除已确认的队首报文；
6. 可选 OTLP 在 UnionC ACK 后通过常驻模式的独立队列异步导出。

### 9.4 撤销与恢复

- 网络/5xx：Agent 退避并补传；
- 报文永久错误：记录并丢弃该报文，避免污染 spool；
- 当前常驻 `run` 收到 401（未知/失效/被替换 credential）或 403 + `agent_revoked`（主机
  生命周期撤销）：写 `reauth_required` 并停止投递，但当前进程继续采样到有界 spool；
  403 + `forbidden` 表示 credential/`host_id` 失配，只丢弃该份旧身份报文并继续 FIFO；
  无法识别的 403 保留队首并退避；
- 重新配对：管理员为同一实例生成新激活过程，历史不变；
- 历史清理：按保留期后台执行；当前不提供会移除 tombstone 的按主机硬删除。

## 10. 失败与降级语义

| 故障 | 主要行为 | 是否影响核心监控 |
|---|---|---|
| Agent 到 Server 网络中断 | 本地 spool + 退避，恢复后补传 | 暂时不可见，不丢失配额内数据 |
| SQLite 文件不可读写、磁盘满或损坏 | readiness 失败，依赖持久层的接口不可用 | 是 |
| OTLP 接收端不可用 | 告警、可选队列丢弃 | 否 |
| Sunshine 主机不可达 | 该主机状态异常，其他主机继续 | 否 |
| SSE 断线 | Web 回落轮询 | 否 |
| 单项传感器不可用 | capability 标记 unavailable | 否 |
| 激活码过期 | 待激活实例失败，可重新创建 | 尚未接入，不影响既有主机 |
| Agent 身份收到 401 或 `agent_revoked` | 当前 `run` 停止投递并等待同实例重新配对；Web 撤销要到下次报告才被本机感知 | 只影响该实例 |
| spool 持续不可写 | 当前采样无法落盘时丢弃并告警；常驻模式同类错误连续 100 次后退出交给服务管理器 | 有数据丢失风险 |
| 报告 ACK 无法验证 | 报告保留并重试，不假定成功 | 暂时积压 |

## 11. 配置责任

| 配置类别 | 归属 | 示例 |
|---|---|---|
| Server 启动配置 | 环境变量/本地私有配置 | bind、数据目录、主密钥、生产模式、保留期 |
| Server 运行配置 | 环境变量 + SQLite 业务表 | bind/目录/保留期由环境提供；Sunshine 主机存 `external_hosts` |
| Agent 运行配置 | 本机配置文件/环境变量 | Server URL、周期、state dir、TLS、OTLP |
| Agent 身份与 secret | Agent 私有状态目录 | instance ID、agent secret、pairing state |
| 浏览器偏好 | 浏览器本地状态 | 主题、当前视图 |
| 安装和更新策略 | 外部软件分发系统 | apt、MSI、pkg、MDM、配置管理 |

## 12. 测试层级

| 层级 | 覆盖内容 |
|---|---|
| Rust 单元测试 | 报文校验、采样转换、spool、配置、错误分类、密钥与 URL 处理 |
| Server + SQLite 集成测试 | 临时数据库上的当前 schema、认证、CSRF、配对、限流、生命周期、乱序、保留、查询成本 |
| Agent 合同测试 | 配对状态、ACK 验证、断线补传、撤销语义 |
| Web 单元测试 | 会话错误页与跨会话缓存隔离、严格 UUID 激活路由、SSE、日志截断、监控转换、Sunshine 当前契约与 mutation 竞态 |
| 三平台 CI | Agent 在 Linux、Windows、macOS 编译与测试 |
| OTLP live 测试 | 验证 Agent → 真实 OpenTelemetry Collector 的接收合同；不覆盖 exporter/时序库查询 |
| 发布验证 | 独立发布工程应覆盖 fresh install、同版本 reinstall、卸载、签名与恢复；不属于 Web 功能 |

数据库测试始终创建隔离的临时 SQLite 并真实执行，不依赖外部数据库服务。测试只覆盖
当前版本从空目录建库和同 schema 备份恢复；不覆盖旧 Server 包或旧数据库就地升级，也不
提供旧数据转换/导入桥。

## 13. 当前产品边界与扩展条件

当前权限模型适合一个管理员管理自己的或组织受管的设备。若未来要成为公共多租户服务，
必须在开放注册前增加：

- 用户、组织、角色和 `agent_enroll` 权限；
- instance owner 与租户级数据隔离；
- 持久会话、配额与审计归属；
- 分布式限流和多实例协调；
- 隐私告知、数据保留政策和删除请求流程。

这些不是本次浏览器配对必须引入的复杂度。当前实现仍坚持单管理员、自托管、显式创建实例
的产品边界。

## 14. 核心设计不变量

修改代码时，以下不变量优先于局部便利性：

1. Agent 永远不接受 Server 命令；
2. 正常运行不需要 Agent 入站端口；
3. 浏览器永远不接触长期 Agent 通信 secret；
4. 撤销是持久状态，不能被自动注册绕过；
5. 一次性码只能绑定一个实例和一个配对请求；
6. Agent 只有验证结构化 ACK 后才确认报告投递成功；
7. 缺失指标用 capability/`null` 表达，不用 `0` 冒充；
8. 可选集成失败不能阻塞核心上报；
9. Server 只打开与当前唯一 schema 精确一致的数据库；
10. 软件安装与更新不经管理台或 Agent 数据面完成。

## 15. 功能入口与交付状态

同一项“代码里存在的能力”可能只面向浏览器、Agent 协议、运维 CLI 或外部部署系统。
判断产品是否真的提供某项功能时，必须同时看入口和目标用户，不能把“后端有路由”写成
“管理台已经支持”。

### 15.1 参与者

| 参与者 | 能做什么 | 不能做什么 |
|---|---|---|
| 单一管理员 | 登录管理台，查看监控，管理 Agent 生命周期与 Sunshine，修改自身密码 | 没有组织、角色、细粒度权限或只读账号 |
| Agent 安装人员 | 在目标机器安装 Agent，使用一次性授权密钥完成配对，运行本地诊断 | 不能读取管理台数据，也不能借 Agent 执行远程命令 |
| UnionC Agent | 本地只读采集、主动上报、断线缓存、可选 OTLP 导出 | 不监听 Server 命令，不自更新 |
| Sunshine 主机 | 接受 Server 直接发起的管理 API 请求 | 不经监控 Agent 中转 |
| 部署/运维人员 | 配反代与证书、安装包、备份恢复、密钥轮换和更新渠道 | 这些动作不由 Web 在线代办 |

### 15.2 Web 已提供的页面

| 页面/入口 | 当前用户功能 | 数据刷新与状态 | 重要边界 |
|---|---|---|---|
| 登录页 | 账号密码登录、会话错误重试 | 启动先验证现有会话；401 才进入登录页 | 当前只有单管理员账号 |
| 总览 | Server 本机 CPU、内存、网络、磁盘吞吐；磁盘容量；Sunshine 服务健康 | 资源 20 秒轮询；浏览器内保留最多 180 个短期点；服务优先 SSE | 不是全部 Agent 的聚合总览，短期曲线不持久化 |
| 主机 | 创建/取消待激活实例；一次性密钥；20 台分页；当前值、硬件、capability、历史；撤销和重新配对 | 列表/详情 10 秒，历史 30 秒，邀请状态 10 秒 | 当前页在线数不是全库在线数；全部操作保持 Agent 只读 |
| 公开激活页 | 核对有限设备摘要并提交一次性授权密钥 | 只读取指定 pairing request | 不要求管理员会话，也不返回长期通信 secret |
| Sunshine | 主机增删改查；应用、客户端、PIN、配置、重启与显示重置 | 探测中约 1.5 秒刷新，稳定后 30 秒；mutation 有缓存屏障和失败回滚 | 侧栏“+”会立即创建一个默认主机实体，不是先打开草稿表单 |
| 日志 | 选择一台 Sunshine 主机，查看其 API 日志 | 30 秒刷新，最多渲染最新 2,000 行 | 不是 UnionC Server 运行日志，也不是审计日志 |
| 设置 | 查看当前账号并修改密码 | 修改成功后既有会话失效并重新登录 | 没有运行参数、保留期、告警、用户或角色配置 |
| 全局工具 | 手工刷新、SSE 状态、深浅主题、退出 | 主题保存在浏览器 localStorage | 主导航是内存状态，刷新回总览，不能深链 |

Web 的统一请求层提供 15 秒超时、Cookie 会话、非只读请求自动附加 CSRF 头、401 会话失效
广播、错误 JSON 归一化和路径段编码。请求与失效事件都绑定发起时的会话代际，旧会话迟到
的 401 不能注销新登录；注销或当前代际的 401 会立即替换整个 QueryClient，隔离旧会话仍
在途的 mutation 回调。logout 完成前禁止新登录，避免 Cookie 写入竞态。除总览外的四个
主视图懒加载；顶层错误边界可处理渲染阶段崩溃。SSE 只更新服务状态，系统资源与 Agent
遥测仍使用普通查询。

### 15.3 Server 有、Web 尚未提供入口

| 能力 | 当前入口 | 为什么不应写成“Web 已支持” | 建议 |
|---|---|---|---|
| 审计记录分页导出 | `GET /api/audit-logs` | Web 没有 API 封装、类型或页面；“日志”页只看 Sunshine | 管理型部署建议补只读审计页或接入外部审计系统 |
| Sunshine locale | `GET .../config/locale` | Web 配置编辑器未调用 | 只有做本地化配置 UI 时才接入 |
| Sunshine 封面读取/上传 | `GET .../covers/{index}`、`POST .../covers/upload` | Web 没有封面组件或 API 封装 | 独立体验增强，可保持 API-only |
| Sunshine 单主机 status | 单主机状态 API | Web 使用列表中的探测快照 | 仅在需要单主机独立刷新时接入 |
| 健康与就绪 | `/api/health`、`/api/ready` | 面向反代、服务管理器和监控系统 | 应保持无 UI 的运维探针 |

Agent 的 v2 创建配对请求/轮询状态与 `/api/agent/v1/report` 属于机器协议；备份、恢复、完整性检查、
重加密和管理员密码重置属于离线 CLI。它们没有 Web 页面是有意的职责分离。

### 15.4 后端接口能力总览

| 路由族 | 调用者与权限 | 已实现能力 |
|---|---|---|
| `/api/auth/*` | 浏览器；登录后使用 Cookie，会话内修改需 CSRF | JSON 登录、当前用户、注销、改密；后端改密原子撤销全部会话及其 SSE |
| `/api/health`、`/api/ready` | 公共运维探针 | 区分进程存活与数据库/数据目录可服务 |
| `/api/system/resources`、`/api/services` | 管理员 | Server 本机资源快照与 Sunshine 健康快照；读取不触发现场采样/网络探测 |
| `/api/events/*` | 管理员会话 + 60 秒单次 ticket | 首帧快照和后续服务状态 SSE；慢消费者跳到最新状态，不做事件重放 |
| `/api/audit-logs` | 管理员 | `before_id` 游标分页的审计导出；当前没有 Web 页面 |
| `/api/monitoring/agent-instances*` | 管理员；修改需 CSRF | 创建、查询、取消一次性实例邀请，以及对同一实例重新配对 |
| `/api/monitoring/hosts*` | 管理员；生命周期修改需 CSRF | 主机分页、详情、历史和显式撤销 |
| `/api/agent/v1/report` | Agent Bearer + 限流 | 当前唯一权威指标上报数据面 |
| `/api/agent/v2/*` | 未配对 Agent、公开激活页或 pairing secret | 创建/查询配对、提交一次性码、轮询最终状态 |
| `/api/services/sunshine/hosts*` | 管理员；修改需 CSRF | 主机 CRUD/状态以及 apps、clients、config、locale、logs、PIN、restart、reset-display、covers 代理 |

控制台会话、登录限流、Agent 限流和 SSE ticket 都是单进程内存状态；服务重启会清空它们，
不会删除 SQLite 中的主机、历史、邀请、凭据 tombstone、Sunshine 配置或审计记录。

### 15.5 CLI 与服务入口

| 组件 | 命令/入口 | 功能 |
|---|---|---|
| Server | 默认启动 | 新建或严格校验当前 SQLite schema、启动 HTTP 和后台采样/探测/保留任务 |
| Server | `backup --output` | 创建一致性 SQLite 快照和带 SHA-256/schema/key-id 的清单 |
| Server | `restore --input [--force]` | 停服状态下校验、创建恢复点并原子替换活动库 |
| Server | `integrity-check` | 校验 SQLite、外键、当前 schema 指纹与密文可解性 |
| Server | `rekey` | 用当前密钥重新加密已有 Sunshine 凭据 |
| Server | `reset-admin-password` | 离线重置本地管理员密码，不修改业务数据 |
| Agent | `run` / `once` | 常驻采集；或补传后执行一次当前采样 |
| Agent | `pair` | 唯一受支持的一次性授权浏览器配对 |
| Agent | `probe` / `status` / `doctor` | 本机能力、身份/积压状态和无副作用诊断；`doctor --delivery` 才测试投递 |
| Windows | 服务 + 托盘 + maintenance helper | LocalService 采集、普通用户入口、事务式安装/同版本重装/卸载/purge |

## 16. 核心、可取舍与可选能力的决策清单

### 16.1 最小产品核心

若仍把产品定义为“只读、多主机、跨平台状态监控”，下列能力必须保留：

1. 至少 CPU、内存、磁盘、网络四类基础采集，以及 capability/`null` 的缺失语义；
2. 稳定实例 ID、每实例通信凭据和一个经过管理员授权的接入流程；
3. Agent 主动出站上报、Server 身份校验、结构化 ACK 和报告幂等；
4. 当前状态落库、主机列表/详情/历史摘要查询与 online/stale/offline/revoked 状态；
5. 撤销、同实例重新配对和不允许凭据自动复活的生命周期；
6. 管理员认证以及能完成上述查看和生命周期操作的 Web 或等价客户端。

spool、输入校验、CSRF、限流、schema 校验/恢复等被列为 `G`，不是因为它们可以在生产删除，
而是因为它们不直接创造“看见主机状态”的业务价值。生产版应把 `C + G` 一起视为不可裁剪
基线；只保留 `C` 适合短期原型，不适合长期运行。

### 16.2 可以取舍，但必须接受代价

| 当前能力 | 可怎样简化/替换 | 明确代价 | 取舍前提 |
|---|---|---|---|
| 历史曲线与 30 天默认历史 | 只保留当前值，或缩短保留期/降低采样频率 | 产品从“状态与趋势监控”退化为“当前快照”；排障和容量判断能力下降 | 同步修改产品承诺、API、UI 和容量模型 |
| 逐网卡、逐磁盘、逐 GPU/温度详情 | 只展示摘要 | 不能定位具体设备，capability 诊断深度下降 | 仍须保留“不支持不等于 0”的语义 |
| Server 本机总览与浏览器短期曲线 | 删除总览，直接进入主机页 | 失去控制台主机资源与 Sunshine 服务概览 | 外部系统已监控 Server 本身 |
| SSE | 全部使用 10 秒轮询 | 状态变化显示更慢、请求数略增 | 轮询继续可用；当前代码已自动回退 |
| Windows 托盘 | 仅保留 CLI/企业部署 | Windows 首次配对、服务控制与连通性排查更难用 | 使用者有 CLI、GPO/MDM 或运维工具 |
| Agent `probe`、`once`、human 输出 | 只保留常驻服务和 JSON 状态 | 现场排查、安装验收和脚本使用成本增加 | 有等价外部诊断手段 |
| `status`、默认只读 `doctor` | 用日志和服务管理器替代 | 无副作用诊断能力显著变差，支持成本上升 | 不建议裁剪 |
| Web 深浅主题、跑马灯、卡片动画 | 使用更简单的静态样式 | 只影响体验，不影响业务语义 | 保留可读性、焦点态和错误反馈 |
| 完整三平台发行 | 只交付实际需要的平台 | 不再能宣称其余平台受支持 | 同步收缩 CI、文档和支持范围 |
| macOS 自带日志轮转 | 接入组织统一日志方案 | 若既不替换也不轮转，文件会无限增长 | 新方案先覆盖再删除 |
| 审计导出 | 交给外部审计/日志系统 | 本地无法追溯管理变更 | 外部系统必须保留 actor、action、target、request ID |

### 16.3 真正可选、可不启用的能力

| 能力 | 当前启用方式 | 不启用后的影响 |
|---|---|---|
| Sunshine 管理 | 配置至少一台 Sunshine 主机；当前没有编译期开关，零主机即可不使用 | Agent 监控完全不受影响；Web 仍会显示 Sunshine/日志导航 |
| OTLP 导出 | 源码默认构建需显式启用 `otlp` feature 并配置 endpoint；当前 macOS release job 会显式启用 | 只失去外部时序副本；UnionC 主上报不变 |
| NVIDIA NVML | 当前默认 Agent build 启用 `nvidia`；可用 `--no-default-features` 构建掉 | NVIDIA 数据显示 unavailable，基础指标不变 |
| AMD/Intel/WDDM 扩展 GPU | 由平台、设备、驱动和权限决定 | capability 说明原因，整份报告仍有效 |
| Linux GPU 设备访问 | 管理员安装 systemd drop-in 放宽 `PrivateDevices` | 默认沙箱更强，但可能读不到 GPU 设备节点 |
| 私有 CA / mTLS | Agent 配置 CA 和 PEM/PKCS#12 身份，反代验证 | 标准公网 TLS 仍可运行 |
| Windows 托盘会话 | MSI 默认安装；用户可退出、自启动可禁用，静默环境可不运行 | 后台 Windows Service 仍可独立采集上报 |
| Portable tar | 手工部署二进制和配置 | 不获得账户、ACL、服务、升级与卸载事务保障 |

当前 `agent/Cargo.toml` 的默认 feature 只有 `nvidia`，不含 `otlp`。因此默认构建的 Linux、
Windows release 制品包含 NVIDIA 支持而不含 OTLP；当前 macOS release job 则显式使用
`--no-default-features --features otlp`，包含 OTLP 而不含 NVIDIA。未编译 `otlp` 却为需要
投递的命令配置 OTLP endpoint/token 时，Agent 会在启动校验阶段明确报错；不会把一项看似
已配置的导出静默当作成功。若发布策略要求三平台一致，应先统一 workflow 再改本文矩阵。

### 16.4 最新版本唯一边界

当前构建不含旧协议兼容层：没有 v1 register、enrollment code、静态 enrollment token、
浏览器明文 token 轮换、DELETE 撤销别名、Basic 登录、Sunshine PUT 全量替换、旧数据库前缀
升级或 Windows PowerShell 计划任务迁移。Agent 本地状态、Web 路由与 Sunshine 响应也只接受
当前 canonical schema。

这意味着现有旧部署不能原地升级：必须导出需要长期留存的数据，部署空的当前数据库，安装
当前 Agent，清理旧本机身份后重新配对。新增任何旧字段 alias、默认回填或静默降级都必须先
改变本项目“只支持最新版本”的明确产品边界。

### 16.5 不应作为取舍删除的保障

以下机制可以更换实现，但不能无替代地删掉：

- 报告、身份、文本、数量、数值和时间边界校验；
- 匹配 `host_id/report_id` 的 ACK、重复报告幂等和乱序不回退当前状态；
- 有界 spool、原子文件、退避与明确的数据丢失上限；
- 管理员会话、随机 CSRF、改密后会话失效、登录与 Agent 分桶限流；
- 生产 HTTPS、回环绑定、可信反代证明、安全响应头和上游响应体上限；
- Sunshine 密码加密、密钥轮换与浏览器不回显；
- 当前 schema 指纹、单实例锁、备份清单、同版本恢复验证和最新报告保留例外；
- 撤销 tombstone 和“本地 purge 不等于 Server 撤销”的退役顺序；
- 对应受支持平台的文件权限、服务账户、安装回滚、同版本重装和 purge 安全检查。

## 17. 推荐产品组合与裁剪顺序

| 组合 | 应包含 | 可以不包含 | 适用场景 |
|---|---|---|---|
| 开发验证 | 基础采集、单 Server、SQLite、配对、上报、主机页 | 生产反代、正式包、Sunshine、OTLP、托盘 | 本机短期验证；不能作为生产安全基线 |
| 核心监控产品 | 全部 `C + G`、Linux Server、至少一个目标平台 Agent、Web 主机生命周期 | Sunshine、OTLP、SSE、GPU 深度、Windows 托盘 | 自托管的可靠只读监控 |
| 完整当前产品 | 核心监控 + Sunshine + 三平台包 + Windows 托盘 + GPU 扩展 | OTLP、mTLS/私有 CA | 当前仓库主要用户体验 |
| 企业加固 | 完整产品 + 组织 PKI/mTLS + 异机备份 + 外部日志/审计 + MDM/包仓库 | 公共 OTLP 仍可按需 | 受管终端与生产部署 |

建议按以下顺序裁剪：

1. 删除完全不用的独立模块：OTLP、Sunshine、未支持平台制品、托盘或 GPU feature；
2. 再简化体验层：SSE、总览、主题、逐设备详情和诊断命令；
3. 最后才讨论历史深度、审计与备份等产品/运维能力，并为每项提供替代方案；
4. 永远不要用“精简”名义删除凭据隔离、校验、ACK、撤销、CSRF、限流、原子落盘或恢复验证。

若只想部署基础监控，最干净的现状组合是：Server + Web + 默认或
`--no-default-features` Agent，不配置 Sunshine 主机，不构建 OTLP，不运行 Windows 托盘。
需要注意：Sunshine 页面目前不是动态 feature，零主机时导航仍存在；若要从产品界面完全
去掉，必须同时调整 Web 导航、Server 路由/后台探测和相应测试。

## 18. 当前限制、非功能与容量边界

### 18.1 架构和规模

| 限制 | 当前事实 | 扩展意味着什么 |
|---|---|---|
| Server 平台 | 代码在非 Linux 目标直接编译失败 | 若支持 Windows/macOS Server，需要替换系统资源、信号、权限与打包假设 |
| 数据库 | 数据目录内单个 SQLite，WAL、单写者、进程内写门控 | 不支持 NFS/SMB 活动库、共享写入或多 Server 水平扩展 |
| 目标规模 | 当前设计和页面主要面向约 20 台主机 | 更大规模需压测写队列、集中补传、保留期、分页与数据库体积 |
| 版本基线 | 当前发布只有一个 SQLite schema | 只支持空目录建库与当前 schema 快照恢复；没有旧格式升级或导入桥 |
| 控制台身份 | 单管理员，会话与 SSE ticket 在进程内 | 重启使会话失效；多用户/RBAC/SSO 需新增完整身份与授权模型 |
| 前端部署 | 静态文件由反代提供，API 与 Web 要同版本 | Rust 中间件不能替静态文件补 CSP/HSTS；反代配置属于安全模型 |

### 18.2 数据和监控语义

- 历史行保存数值摘要；每台主机只为最新报告保留完整 JSON payload。因此可以画趋势，但
  不能回放任意历史时刻的完整网卡、磁盘、传感器和 capability 快照。
- 列表/历史热路径不解析完整 payload；详情按 `latest_report_id` 读取最新完整报告。
- 摘要中的网络、磁盘、GPU 等多设备值采用“最忙的单个设备”，不求和，避免 bridge、
  veth、bind mount 等重复计数；多块真实设备同时繁忙时可能低估总量。
- revoked 主机保留身份、历史和 tombstone。当前没有按主机物理硬删除 UI；保留期任务只
  删除历史报告和审计记录，并为每台主机保留最新一份报告。
- 温度/GPU 缺失通过 capability 和 `null` 表达。macOS 没有稳定的全局 GPU 利用率；
  Windows AMD/Intel 目前主要是 WDDM 聚合，未实现 ADLX/IGCL 厂商增强。
- 没有告警规则、阈值通知、邮件/短信/Webhook、主机分组/标签、维护窗口或 SLA 计算。

### 18.3 Agent 可靠性边界

- 默认 10 秒采样、30 秒慢采样、10% 抖动、64 MiB spool；长期断网超过配额时会优先淘汰
  隔离文件，再淘汰最老待发报告，因此不是无限期无损队列。
- 常驻 `run` 单次 spool 写失败会丢失该次采样；同类 I/O 连续失败 100 次后进程退出，依赖
  服务管理器重启。一次性命令遇 I/O 错误立即失败。服务显示 Running 只代表本地基础设施
  已就绪，不代表已配对或 Server 可达。
- 每轮最多补传 32 份；采样与网络 worker 解耦，网络慢不会改变采样节奏。错过的采样 tick
  会跳过，不会在恢复后突发补采。
- 只有匹配 host/report ID 的 JSON ACK 才能确认出队；`accepted=false` 代表重复报告已经
  幂等处理，也视为确认成功。
- 常驻 `run` 的 OTLP 使用容量 128 的内存队列，没有磁盘 spool；进程退出、队列满或
  Collector 故障都可能丢点，但不会改变 UnionC 主上报的成功状态。一次性命令不补发旧
  spool 到 OTLP，当前报告在主 ACK 后同步尝试。
- 本地 purge 不访问 Server。完整退役先在 Web 持久撤销，再按 DEB/RPM/Windows/macOS 各自
  的受支持顺序完成并验证本地永久清理。

### 18.4 Web 和管理能力边界

- 主导航没有 URL Router；除公开激活路径外，页面不能深链，刷新回到总览。
- 总览只显示 Server 本机资源和 Sunshine 服务，不聚合全部 Agent；短期 sparkline 只存在
  浏览器内存。
- 主机页每页 20 台，页面标题的在线计数只覆盖当前页。没有搜索、筛选、排序或跨页聚合。
- “日志”只显示单台 Sunshine API 日志，30 秒刷新并截到 2,000 行；没有 UnionC 运行日志、
  审计日志 UI、搜索、级别筛选、下载或实时追尾。
- Settings 只改管理员密码；数据库、保留期、密钥、反代、TLS 和 Agent 参数都不在浏览器
  修改，避免运行配置来源不明。
- Sunshine 应用 UI 只编辑常用字段，但保存时保留未知高级字段；完整 JSON 配置编辑器是
  高权限原始入口，权威校验仍在 Server/上游。
- Sunshine 侧栏“+”立即持久化默认主机，误触会产生实体；这是一项现有交互取舍。
- 响应式与键盘基础已覆盖，但 Tab 未实现方向键/roving tabindex，页面切换无焦点移动，
  部分触控目标小于 44×44，当前没有浏览器 E2E、视觉回归或可访问性自动门禁。

### 18.5 交付边界

- 仓库可构建 Server Linux x86_64 musl 二进制、DEB/RPM；Agent Linux x86_64
  tar/DEB/RPM、Windows x64 MSI、macOS universal pkg。没有 Linux/Windows ARM64 包、
  非 Linux Server 包或容器镜像。
- 包全新安装/同版本重装/普通卸载/purge 的本地生命周期由仓库实现；Server DEB/RPM 以
  当前版本环境标记和 UID/GID ownership marker 拒绝接管旧安装布局。软件仓库、MDM、GPO、winget、
  域名、证书、反代和更新策略仍由部署方提供。
- 普通 Agent 卸载有意保留身份、凭据和 spool，便于重装继续同一实例；显式 purge 才清理。
- 正式 tag 流水线要求 Windows Authenticode、macOS Developer ID/公证、SHA-256、GPG 签名
  和 provenance。开发制品可无签名，不应当作正式交付物。
- Release workflow 不生成或上传 Web 静态制品；部署方必须自行执行前端构建并由可信反代
  托管。仓库也不提供容器镜像、APT/YUM/winget 仓库或 MDM/GPO 分发服务。
- 全新 Server DEB/RPM 安装不会自动 enable/start；管理员必须先配置主密钥、生产代理证明、
  首次 bootstrap 和外部静态站点/反代。普通卸载保留数据库，但没有 Agent 式自动 purge。
- Release workflow 会拒绝不属于 `main` 历史的正式 tag，并在同一次运行中复用完整 CI；
  四个平台的打包任务显式依赖来源校验和 CI。GitHub 端仍需用 tag ruleset 限制 `v*` 标签
  的创建、更新和删除。当前没有覆盖率门槛、SBOM、依赖漏洞定时扫描或定时 CI。
- 当前 workspace 未声明 `rust-version`；依赖的实际最低 Rust 版本可能高于旧系统工具链，
  应以 CI 使用的 stable 工具链或后续明确的 MSRV 为准。

### 18.6 关键默认值与硬边界

| 项目 | 当前值/上限 | 含义 |
|---|---|---|
| 管理员会话 | 默认 7 天，进程内 | 重启即失效；不是持久身份会话或 SSO |
| 实例邀请/配对 | 默认 15 分钟；Web 可选 15/30/60 分钟或 24 小时 | 激活码短时、单次，明文只在创建响应出现 |
| Agent 采样 | 快指标 10 秒、慢指标 30 秒、10% jitter、请求超时 10 秒 | 可配置，但必须满足 Server 协议范围 |
| 主机在线判定 | `max(3 × interval, 30s)` 内 online；`max(12 × interval, 300s)` 内 stale | 是基于最后一次有效新报告推导，不是 Server 主动心跳 |
| Agent spool | 默认 64 MiB；每轮最多补传 32 份 | 超预算最终淘汰最老报告，不承诺无限期无损 |
| 上报请求 | 最大 512 KiB；schema v1；采样时间最多超前 5 分钟 | Agent 对实际 compact JSON 字节收敛，Server 仍在不可信边界拒绝超限/非法输入；旧 spool 读取时也应用当前契约 |
| 设备数量 | CPU 4096 核、网卡 1024、磁盘 1024、温度 4096、GPU 128、capability 256 | 防止异常主机放大解析与数据库成本 |
| 后端主机查询 | 默认 200、最大 1000；Web 固定每页 20 | Web 的页内计数不代表全库聚合 |
| 后端历史查询 | 默认最近 300、最大 1000，响应按时间升序 | 只返回数值摘要，不回放完整历史 payload |
| 审计查询 | 默认 100、最大 500，`before_id` 游标 | 后端已实现，Web 暂无入口 |
| 数据保留 | 遥测默认 30 天，审计默认 90 天；每天清理 | 每台主机最新报告有保留例外 |
| Server 后台采样 | 本机资源 2 秒；Sunshine TCP 5 秒、API 30 秒 | HTTP 只读快照，不按访客数重复探测 |
| SSE | ticket 60 秒且单次消费 | 无持久重放，断线后 Web 回退轮询 |
| Sunshine 上游边界 | JSON 4 MiB、封面 8 MiB；封面 MIME 白名单；HTTP 3xx 不跟随 | 上游主机按不可信输入处理，重定向目标不会被自动请求 |

## 19. 代码证据、测试与维护索引

本文不是只根据页面或 README 反推功能。下面给出行为的主要事实来源，便于代码变化后复核。

### 19.1 主要代码入口

| 领域 | 权威实现 | 阅读重点 |
|---|---|---|
| 共享 Agent 协议 | [`protocol/src/lib.rs`](../../protocol/src/lib.rs)、[`report.rs`](../../protocol/src/report.rs)、[`pairing.rs`](../../protocol/src/pairing.rs) | schema v1、主机指标/capability、配对和 ACK DTO |
| Server 入口与路由 | [`server/src/main.rs`](../../server/src/main.rs)、[`server/src/startup.rs`](../../server/src/startup.rs)、[`server/src/http/mod.rs`](../../server/src/http/mod.rs) | CLI、启动顺序、后台任务、中间件与路由装配 |
| 认证与访问控制 | [`server/src/auth/http.rs`](../../server/src/auth/http.rs)、[`server/src/http/access_control.rs`](../../server/src/http/access_control.rs) | 登录、Cookie、CSRF、改密、限流、反代证明、公共路径 |
| Agent 控制面与数据面 | [`server/src/monitoring/http/mod.rs`](../../server/src/monitoring/http/mod.rs)、[`model/mod.rs`](../../server/src/monitoring/model/mod.rs)、[`store/mod.rs`](../../server/src/monitoring/store/mod.rs) | v1/v2 接入、校验、幂等、乱序、查询、撤销、重配对和保留 |
| Sunshine | [`server/src/sunshine/http/mod.rs`](../../server/src/sunshine/http/mod.rs)、[`server/src/sunshine/client.rs`](../../server/src/sunshine/client.rs)、[`server/src/sunshine/status.rs`](../../server/src/sunshine/status.rs) | CRUD、上游代理、体积限制、快慢探测与状态快照 |
| 系统与事件 | [`server/src/system/http.rs`](../../server/src/system/http.rs)、[`server/src/system/resources.rs`](../../server/src/system/resources.rs) | health、ready、本机资源、审计导出、SSE ticket/stream |
| SQLite 与密钥 | [`server/src/infra/database/mod.rs`](../../server/src/infra/database/mod.rs)、[`server/src/infra/database/maintenance/mod.rs`](../../server/src/infra/database/maintenance/mod.rs)、[`server/src/infra/secrets.rs`](../../server/src/infra/secrets.rs)、[`server/schema/sqlite.sql`](../../server/schema/sqlite.sql) | 当前 schema 初始化/校验、事务、保留、同版本备份恢复、完整性与 Sunshine 密文 |
| Agent 运行与配置 | [`agent/src/main.rs`](../../agent/src/main.rs)、[`agent_app/mod.rs`](../../agent/src/agent_app/mod.rs)、[`agent/src/config.rs`](../../agent/src/config.rs) | 命令选择、采样/投递 worker、jitter、停止语义和配置约束 |
| Agent 身份与可靠投递 | [`agent/src/pairing/mod.rs`](../../agent/src/pairing/mod.rs)、[`agent/src/transport.rs`](../../agent/src/transport.rs)、[`agent/src/spool.rs`](../../agent/src/spool.rs) | 可恢复配对、secret、ACK、错误分类、有界磁盘队列 |
| Agent 采集 | [`agent/src/collectors`](../../agent/src/collectors) | 三平台基础指标、Linux hwmon/DRM、Windows WDDM、可选 NVML |
| Windows 本机入口 | [`agent/src/windows/tray`](../../agent/src/windows/tray)、[`agent/src/windows/maintenance`](../../agent/src/windows/maintenance) | 托盘、随机回环页、UAC 固定命令、SCM 与 MSI 生命周期辅助 |
| Web 框架与请求层 | [`web/src/app/App.tsx`](../../web/src/app/App.tsx)、[`shared/api/client.ts`](../../web/src/shared/api/client.ts)、[`app/hooks.ts`](../../web/src/app/hooks.ts) | 认证状态、内存导航、懒加载、CSRF、超时、SSE 与轮询 |
| Web 业务页面 | [`web/src/features`](../../web/src/features) | 总览、主机/邀请/历史、公开激活、Sunshine、日志和设置 |
| CI 与发布 | [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)、[`.github/workflows/release.yml`](../../.github/workflows/release.yml) | 三平台构建测试、包生命周期、签名、公证、清单和 provenance |

### 19.2 本轮可执行验证

2026-08-21 在当前工作区执行了下列无外部状态依赖的检查：

| 检查 | 结果 | 说明 |
|---|---|---|
| `cargo test --workspace --all-features` | 全部通过 | 覆盖三个 crate 的单元/SQLite 集成/协议/OTLP 编码测试；无真实 Collector 环境时 live 用例按代码约定提前返回 |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | 通过 | 三个 crate 的全部本机 target/feature 无警告 |
| `cargo fmt --all -- --check` | 通过 | Rust 源码格式无偏差 |
| `npm test` | 12 个文件、52 项通过 | Web API 当前状态码/媒体类型、SSE、激活、跨会话缓存隔离、配对密钥生命周期、日志截断、Sunshine mutation 竞态等 |
| `npm run lint` | 通过 | ESLint 无错误 |
| `npm run typecheck` | 通过 | TypeScript build graph 类型检查通过 |
| `npm run build` | 通过 | Vite 先构建到 `dist.next`，再由发布脚本原子换入本地 `web/dist` |
| `git diff --check` | 通过 | 文档与现有工作树无空白错误 |

本轮生产构建已通过原子发布脚本更新本地 `web/dist`；没有在本机重做 DEB/RPM/MSI/pkg
安装/同版本重装、平台签名、公证或真实 OTLP Collector 验证；这些路径由 CI/release workflow 和平台
生命周期脚本覆盖。测试通过证明的是当前代码契约，不等于对目标机器、反代、证书、磁盘容量、
Sunshine 版本和长期负载的生产验收。

### 19.3 文档维护规则

后续增加或删除功能时，至少同步更新：第 2 节总览等级、第 15 节实际入口、第 16 节取舍、
第 18 节限制/默认值，以及对应测试。尤其要分别写清“源码含有”“默认 build 含有”“某平台
发行制品含有”“后端有 API”“Web 已有入口”五种状态，不能相互替代。
