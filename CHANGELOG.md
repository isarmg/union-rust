# 更新日志

本文件记录 UnionC 的显著变更。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [0.3.6] — 2026-08-24

### 修复

- 修复 Windows MSI 对程序和状态文件应用、校验 ACL 时的兼容性问题：按文件与目录生成继承标志，
  接受 Windows 对受保护 DACL 的规范化结果，并以子项优先顺序更新权限，避免安装或卸载维护事务失败。
- 修复 `PURGE=1` 卸载时状态隔离目录的句柄重命名缓冲与目标路径处理，避免清除状态的准备事务失败。

### 变更

- Windows MSI 支持显式设置 `UNIONC_MAINTENANCE_DIAGNOSTICS=1`；维护失败时会以有界私有文件
  保留首个错误链，便于定位被后续回滚掩盖的根因。

## [0.3.5] — 2026-08-23

### 安全

- 收紧 Agent 配对与传输边界：浏览器配对拒绝远程明文 HTTP，Agent API 禁止自动重定向，
  激活地址、持久端点和明文策略在使用前后持续校验，并正确识别 IPv6 回环地址。
- 为审计详情、Sunshine 上游响应与错误、SSE 票据、Agent 报告队列、采集器输出、Windows
  PDH/托盘 IPC/偏好和维护目录遍历增加明确资源上限；拒绝非普通队列文件及不完整 ACL 恢复计划。
- Linux、macOS、Windows 安装与卸载流程改为 fail closed：校验实际 ELF 架构、服务、任务、
  收据、版本绑定 marker 和精确 ACL；Windows ACL、目录重命名与删除绑定到已验证文件句柄。

### 修复

- 修正配对及实例生命周期竞态：旧实例删除不再误删新配对，邀请取消失败可重试，快照失败有
  退避；持久提交后的偏好保存、服务重启或事件输出失败不再误报整体配对失败。
- 提升 GPU 采集准确性与恢复能力：NVML 初始化可重试并准确区分错误和部分可用，Windows
  PDH 会话可重建，WDDM 按物理引擎聚合进程实例并区分空结果与全无效数据，Linux GPU
  与温度采集错误分类更准确。
- 修复 Server 监控与 Sunshine 数据边界：分页顺序稳定，大整数计数不再在浏览器失真，
  保留期清理持续追平积压，旧 HTTP 状态不再覆盖 SSE；畸形或超限集合被一致拒绝。
- 修复 Web 交互与可访问性：繁忙时保留“新增”触发，管理选项卡支持完整 ARIA 与键盘导航，
  一次性授权密钥到期即清除；孤立折线采样可见，关闭面板恢复焦点。
- 修复静态前端发布失败窗口、运行时长的非单调时钟计算，以及 Agent 队列扫描、读取和磁盘
  预算不一致问题。

### 变更

- 发布链路锁定 Cargo/npm 依赖、外部 Actions、容器镜像、runner 与语言工具链；同一 ref 的
  发布串行执行，并以提交时间生成可复现的 Linux 便携归档。

## [0.3.4] — 2026-08-23

### 变更

- 主机卡片的“Server 备注”统一显示为“名称”，使用与 Sunshine 地址、账号和密码相同的
  无边框内联编辑方式；Sunshine 和主机的名称进入编辑状态后都只显示底部横线，不显示
  矩形焦点框；新邀请的默认名称改为“概览”。
- 主机卡片点击后不再显示选中轮廓，操作区提供“详情”和“删除”；只有“详情”会打开或
  收起面板。
- 主机详情改为与 Sunshine 管理页一致的三卡宽、三卡高相邻面板；概览、网络、磁盘、GPU、
  温度和采集能力使用表格展示，历史页继续使用原有趋势卡片。详情面板不再单独占用一行
  显示标题；主机详情和 Sunshine 管理面板的关闭按钮均与各自分类标签位于同一行。

### 移除

- 删除已激活主机的撤销状态、撤销端点、同实例 credential 再签发与对应 Web/Agent 分支。
  再次接入时始终创建新实例；旧实例由管理员单独永久删除。

## [0.3.3] — 2026-08-23

### 修复

- Agent 投递重试到期后不再让已过期的定时器继续参与 `tokio::select!`，避免等待
  DNS、TCP 或 TLS 的 HTTP 投递 Future 被循环立即取消，导致后台 Agent 持续离线而
  `doctor --delivery` 单次诊断仍能成功。

### 新增

- 主机实例内容块支持内联编辑 Server 备注，并提供明确的永久删除操作；删除在单一事务中
  清除该实例的历史报文、凭据、配对请求和邀请，同时保留独立审计记录。

### 变更

- 主机页移除独立“创建 Agent”表单，改为与 Sunshine 相同的侧栏“+”入口，默认创建
  15 分钟邀请；重新配对、撤销和删除均移动到各自主机内容块内。
- 主机内容块移除 CPU、GPU 和网络三行摘要；完整实时指标仍保留在选中主机的详情区。
- Sunshine 空列表不再重复显示“暂无主机，点击 + 新建”提示。
- 主机名称明确改为仅由 Server 持有的备注：初次邀请创建备注，后续 Agent 上报和重新配对
  都不会覆盖。备注修改与永久删除使用独立的 `/api/monitoring/managed-instances/{id}`
  管理端点，原 `/api/monitoring/hosts/{id}` 继续只接受 GET。

### 移除

- Agent 不再采集、配置或上报设备名称：移除配置文件 `host_name`、环境变量
  `UNIONC_AGENT_HOST_NAME`、`pair --name`、Windows 托盘名称输入，以及 JSON 配对/报告和
  OTLP 资源中的 `host.name`。公开激活摘要也不再暴露设备名称。

## [0.3.2] — 2026-08-22

### 修复

- Server 不再把活动 SQLite 被删除、重命名、替换或新增硬链接后的旧文件描述符误判为健康；
  readiness、依赖数据库的管理面与 Agent 数据路径会 fail closed，连接池拒绝继续签出旧 inode，
  且首次观察到身份异常后要求停服检查并重启。
- Server 数据目录现在逐级拒绝符号链接和不安全的可写祖先，只认领私有空目录或具有严格
  权限的既有 UnionC 数据，并以持久 marker 防止误把系统路径或其他应用目录当作自身数据。
- 数据库权限、inode 身份与重开检查进入 readiness 和连接签出路径，避免权限漂移、ABA
  替换或缺失数据库被健康缓存掩盖。
- Agent 关停信号改为可靠广播，避免接收方在启动竞态中漏掉停止通知。
- Server 的 Sunshine 集成测试改用进程内测试密钥，不再依赖仓库中残留的开发数据目录，
  消除干净 GitHub runner 上才暴露的测试执行顺序依赖。

### 新增

- Windows Agent 托盘增加 Server 连接检测：本地配置页自动每 30 秒检查一次，也可手动
  重试；托盘菜单可同时查看 Windows 服务状态和管理端 `/api/health` 可达性。检测采用
  4 秒总超时、禁用重定向并限制响应大小，同时明确主机在线状态仍以认证遥测为准。

### 变更

- `v*` 标签发布当前明确作为 unsigned GitHub Pre-release：Windows MSI 与 macOS pkg 文件名
  标记 `unsigned`，只生成未签名 `SHA256SUMS`，不执行平台签名、Apple 公证、GPG 签名或
  provenance attestation。

### 移除

- 项目改为只支持当前版本：删除 Agent v1 register、enrollment code、全局 enrollment
  token、直接 token/host ID 配置、自动重新注册和管理端明文 token 轮换。
- 删除 Basic Auth、主机 DELETE 撤销别名、Sunshine 主机 PUT 全量替换，以及旧 Sunshine
  响应字段/形态归一化；当前 API 与 Web 仅接受 canonical schema。
- SQLite 折叠为唯一当前基线，启动与 restore 只接受精确当前 schema；不再识别迁移前缀、
  运行 staging 升级或回填旧 credential。
- 删除 Windows PowerShell 计划任务安装/迁移、旧 ACL/marker 接管，以及 Linux/macOS 旧
  ownership marker 兼容。旧部署必须全新安装并创建新实例配对，不提供原地迁移。

## [0.3.1] — 2026-08-16

### 修复

- Server 接受 Windows 无卷标卷产生的空 `disk.name`，继续以挂载点展示该卷，避免整份
  遥测被 HTTP 400 永久拒绝并让已配对主机持续显示离线。
- Windows Agent 托盘菜单和本地配置页移除“打开 UnionC 管理台”直达入口；本地页面只保留
  配对、重新配对与服务控制。

## [0.3.0] — 2026-08-16

### 新增

- Server 提供 `backup --output`（SQLite 一致性快照）、`restore --input [--force]` 与
  `integrity-check` 运维命令；备份附带校验清单，恢复要求停服并通过单实例文件锁防止
  替换活动数据库。
- 正式发布新增静态 musl Linux x86_64 Server 原始二进制、DEB 与 RPM，并以真实安装门禁覆盖
  SQLite 首启、备份/恢复/完整性检查、SQLite 版本升级和保留数据卸载；制品进入统一的
  校验和、GPG 签名与 provenance attestation。

### 变更

- Server 持久层改为数据目录内的内嵌 SQLite（`unionc.db`），移除外部数据库配置；首次启动
  自动建库和迁移。0.3.0 是新的 SQLite 存储基线，只支持全新部署，不支持旧 Server 包或
  旧数据库的就地升级，也不提供旧数据转换/导入桥。
- 如需保留旧数据，用户应在旧环境停止写入后自行导出并独立留存，再把 0.3.0 部署到空数据
  目录；项目不提供旧格式导入，也不保证自行导出的数据可重新导入 0.3.0。
- SQLite 以 WAL、`synchronous=FULL`、外键校验、30 秒 busy timeout 和进程内写门控运行；
  部署边界明确为单 Server、本机磁盘，不支持共享 NFS/SMB 数据库或多节点写入。
- 遥测与审计保留期清理改为短事务分批提交，避免大批过期审计一次 DELETE 长时间独占
  SQLite 写锁，并保留跨批次的精确删除计数。
- 持久层测试改用隔离的临时 SQLite 文件，不再需要本机数据库服务或“缺库即跳过”。
- `restore` 在替换当前活动 SQLite 库前创建恢复点：健康库保存为带 manifest、可再次恢复的
  `pre-restore` 快照；无法验证的数据库保留为无 manifest 的原始 main/WAL/SHM 取证副本，
  有未确认 sidecar 时拒绝替换，避免把损坏或未 checkpoint 的数据误标为受支持备份。

### 修复

- Sunshine 主机新增、修改和删除会立即更新控制台缓存；主机列表只读取 Server 的健康
  快照，TLS/认证探测移至单一后台任务，离线或无响应的 Sunshine 不再阻塞页面刷新。

## [0.2.1] — 2026-08-16

### 变更

- Windows 本机配置页可一次填写 Server 地址和管理台生成的短时授权密钥，提权
  Agent 在本机完成请求建立、授权和轮询，不再要求用户在远程激活页二次输入密钥。
- 用户从通知区菜单选择“退出”时，经 UAC 成功停止 `UnionCAgent` 服务后再退出
  托盘；安装器升级/卸载发送的 `WM_CLOSE` 只关闭托盘，服务仍由 Windows Installer
  和 SCM 按事务生命周期处理。

## [0.2.0] — 2026-08-16

### 新增

- **Windows 原生托盘伴侣**：安装后在当前用户的通知区域常驻，提供本机浏览器配置、
  配对、管理台入口、服务状态与启动/停止操作。遥测仍由隔离的 Windows Service
  执行；服务启停和配对等需要修改受保护状态的操作都通过 UAC 明确授权。
- **浏览器配置安全边界**：配置页只监听随机回环端口并使用一次性内存能力令牌；长期
  Agent 凭据不会交给浏览器或桌面用户进程。配对完成后才让原有服务重新加载新身份。

### 变更

- Windows MSI 新增签名的 GUI 托盘程序、登录自启动和开始菜单入口，并在升级/卸载前请求
  托盘正常退出；静默企业部署不会在 Windows Installer 服务会话中启动交互界面。

## [0.1.1] — 未发布

### 修复

- **Windows 安装器不再闪现控制台窗口**：原生 MSI 自定义操作以 Windows GUI 子系统运行，
  安装、升级和卸载期间不再弹出或闪退 PowerShell/控制台窗口；安装器仍不依赖 PowerShell。

## [0.1.0] — 未发布

首个正式版。以下是本版本具备的能力。

### 监控

- **只读多主机监控**：跨平台 Agent（Linux / Windows / macOS）采集 CPU、内存、
  网络、磁盘、温度与 GPU 指标并上报；服务端提供主机列表、详情与历史曲线接口。
  采集不到的指标以 capability 形式如实标记，不用 0 冒充。
- **浏览器授权配对**：管理员先创建稳定实例和短时一次性激活码；Agent 本地生成长期
  report secret 与独立 polling secret，浏览器只负责核对设备和授权，从不接触长期凭据。
  配对请求在首次 POST 前持久化，支持命令中断与响应丢失恢复。
- **主机撤销与重新配对**：`POST /api/monitoring/hosts/{id}/revoke` 持久拒绝全部旧
  credential 并保留历史；恢复时为同一实例重新配对。403 进入 `reauth_required`，不会
  借旧全局 enrollment token 自动复活。
- **旧协议迁移**：v1 register、注册码和已有 token 暂时兼容，但新 Web 不再签发旧码、
  展示长期 token 或生成安装命令。
- **断线补传**：Agent 本地 spool 持久化未送达报文，恢复后按时间序补传，
  单轮上限 32 批次；反序列化失败的报文隔离为 `.invalid` 并计入容量预算。
- **可验证投递**：Agent 只有在解析结构化 ACK 并核对 `host_id/report_id` 后才确认成功；
  重放同一 `report_id` 不新增历史，也不刷新主机当前状态。
- **OTLP 导出**（可选，`otlp` feature）：手写的 OTLP Metrics protobuf 子集，
  可把时序数据推送到任意 OTLP/HTTP 端点，由 CI 对真实 Collector 端到端验证。

### 管理

- **Sunshine 主机管理**：应用、客户端、配置、配对、封面与日志代理。
- **本机资源**：服务端自身的 CPU / 内存 / 磁盘 / 网络吞吐，经 SSE 推送状态变化。
- **审计日志**：所有状态变更操作记录 actor 与 request id，按保留期自动清理。
- **密钥环与轮换**：AES-256-GCM 加密数据库敏感字段，支持保留历史密钥的三步轮换流程，
  附 `unionc rekey` 子命令。

### 部署

- **完整 Agent 本地生命周期**：Linux 由同一 nFPM 定义生成 DEB/RPM；Windows 提供 x64
  WiX MSI、原生 SCM Service、普通用户通知区托盘、service SID 隔离、计划任务旧版迁移和
  事务式升级，不再要求用户使用 PowerShell；macOS pkg 提供 preinstall、LaunchDaemon、
  日志轮转与卸载器。
  三平台普通卸载保留身份，显式 purge（Windows 为 `PURGE=1`）清理本地状态；永久退役要求
  先在 Web 撤销实例。
- **受保护的正式发布**：`v*` tag 依次签名 Windows Agent、托盘伴侣、maintenance 三个 EXE
  和最终 MSI，强制 Authenticode；macOS 使用 Developer ID +
  notarization/staple，并生成 SHA256SUMS、GPG 分离签名和 provenance attestation；缺少签名
  secret 时拒绝发布。发布流程对三平台执行安装、升级、保留卸载和 purge smoke test。
- 反向代理示例配置：管理台、独立 Agent 域名（mTLS）、OTLP 遥测入口（mTLS）。
- Agent 软件通过独立发布渠道、包管理器或 MDM 分发；管理台不托管安装包，不生成跨平台
  安装命令，也不承担 Agent 在线更新。

### 安全

- 会话走 HttpOnly Cookie，生产环境使用 `__Host-` 前缀；每会话随机 CSRF 令牌，
  双提交模式 + 恒定时间比较。
- 全局安全响应头：`nosniff`、CSP `default-src 'none'`、`X-Frame-Options`、
  `Referrer-Policy`、`Cross-Origin-Resource-Policy`，生产环境额外下发 HSTS。
- 生产环境强制回环绑定与 HTTPS 反代，强制 Sunshine TLS 证书校验。
- 反向代理契约：登录、改密与 Agent 接口要求 `X-Forwarded-Proto: https` 与
  `X-Forwarded-For`，缺任一项返回 **421 Misdirected Request**（不用 403——
  那是凭据语义，会被 Agent 误判为令牌失效）。
- 登录限流按 (IP, 用户名) 复合键分桶，避免"知道用户名即可锁定管理员"的 DoS；
  Agent 注册按来源 IP 分桶，上报按主机令牌桶。
- Agent 报文的**每一个**文本字段都纳入长度与控制字符校验；密码长度上限 72 字节
  （bcrypt 会静默截断超出部分）。
- Sunshine 上游响应流式读取并设体积上限（JSON 4 MiB / 封面 8 MiB），
  封面 Content-Type 收敛到图片白名单。
- Agent 上报体在**认证之后**才反序列化；注册限流前置到凭据校验**之前**。

### 已知限制

- 服务端仅支持 Linux。
- Windows Agent 安装程序当前只发布 x64 MSI，尚无原生 ARM64 制品。
- 不提供容器化部署物；OTLP 端到端测试与生产观测栈需自备兼容接收端。
