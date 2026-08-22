# UnionC 项目全面审查与整改报告

审查日期：2026-07-25
最近整改日期：2026-08-20

> **历史归档说明：** 本报告记录 latest-only 清理前的审查现场，文中关于旧 v1 注册、旧数据库
> 恢复前缀、旧安装迁移以及“保留 README 密码”的陈述均不再代表当前代码。这是历史整改
> 快照，不是当前规范。当前边界以 `docs/reference/capabilities.md`、
> `DOCUMENTATION.md` 和源码为准；README 中的明文口令已
> 删除，旧版本兼容层已移除。
>
> 本文后半部分保留首次审查时的原始问题描述，便于追溯。下表是针对“20 台以内
> Sunshine 与监控 Agent”部署规模完成整改后的最终状态。
>
> 首次审查当时未处理 Windows 托盘与 release/供应链流程；后续状态请以当前文档为准。

## 整改状态

| 原问题 | 状态 | 最终方案 |
|---|---|---|
| Rust 格式检查阻断 CI | 已解决 | 全仓库执行 `cargo fmt --all`，CI 格式检查通过 |
| 静态前端缺少安全响应头 | 已解决 | 在 Caddy 静态 `handle` 内增加文档专用 CSP、HSTS、nosniff、frame 和权限策略 |
| Agent 上报认证前数据库压力 | 已解决 | token 查库前增加按来源 IP 与全局窗口限流，并把无效 token 路径减少为一次索引查询 |
| 业务成功后审计失败返回错误 | 已解决 | 数据库内删除/轮换与审计同事务；外部 Sunshine 操作改为尽力审计 |
| 外部数据库配置与脱敏值写回风险 | 已解决 | Server 改为数据目录内嵌 SQLite，生产环境和管理台不再接受数据库 URL |
| spool 失败计数被读取成功清零 | 已解决 | 读、写、补传使用独立健康计数，补传 I/O 失败也纳入阈值 |
| 多主机日志重复读取本地文件 | 已解决 | 删除伪多主机本地日志端点，按主机代理 Sunshine `/api/logs`，30 秒刷新 |
| `once` 不补传、不自愈 | 已解决 | `once` 先采样并清空历史积压；凭据被拒时明确失败并要求按当前浏览器配对流程重新授权，不再自动注册 |
| Sunshine 配置类型被字符串化 | 已解决 | 改为完整 JSON 对象编辑并在保存前解析校验 |
| Sunshine 更新持锁探测网络 | 已解决 | 发布数据库与内存快照后显式释放设置锁，再审计和网络探测 |
| Agent 响应体无上限 | 已解决 | 配对、上报 ACK 与错误响应按当前契约限制读取大小并校验媒体类型 |
| HTTP 409 被无限重试 | 已解决 | 409 归为永久拒绝并从 spool 确认出队 |
| 监控主机超过单页不可访问 | 已解决 | 前端按 20 台分页；目标规模通常只有一页 |
| LVM/RAID 磁盘吞吐重复统计 | 已解决 | 物理吞吐口径排除 `dm-*` 和 `md*` 逻辑层 |
| 静态发布回滚不完整 | 已解决 | 新版本 chmod 失败时移除坏树并恢复旧树，增加故障回归测试 |
| 前端缺少测试与 lint | 已解决 | 引入 ESLint、Vitest，CI 执行 lint、typecheck、test、build |
| Sunshine 字段缺少长度限制 | 已解决 | 服务端限制名称、账号、密码长度及控制字符，前端同步限制 |
| 部分 mutation 无错误反馈 | 已解决 | 补充结束会话和客户端启停错误提示，并支持显式清空 Sunshine 密码 |
| OTLP CI 使用 `latest` | 已解决 | 固定为 `otel/opentelemetry-collector-contrib:0.157.0` |
| Windows Agent 被 Rust 1.98 Clippy 阻断 | 已解决 | 使用 `as_chunks::<4>()`，并以 serde rename 保持五个 `_version` 字段的 wire key 和版本校验 |
| 前端锁文件含 2 个 high advisory | 已解决 | 仅补丁升级 `brace-expansion` 5.0.9 与 `nanoid` 3.3.18，并新增 high/critical CI 审计门禁 |
| Actions 使用已弃用的 Node 20 runtime | 已解决 | 全部 `checkout` 与 `setup-node` 引用升级到使用 Node 24 的官方 v7 |

### 第二轮深审收口

- 运行模式、保留期和生产代理证明改为严格解析，未知值不再静默降级到开发模式；认证入口
  使用 4 KiB 独立 body 上限，并在 bcrypt/限流键之前约束账号与全部密码输入；
- 改密改为单飞、原子落盘成功后才发布内存 hash，同时撤销其他会话与已建立 SSE；
- 客户端 `x-request-id` 一律由服务端 UUID 覆盖，响应与事务内审计使用同一关联 ID；
- Sunshine 主机改为逐行事务 CRUD 与字段级 PATCH，配置/健康快照按固定锁序原子发布；
  5 秒 TCP 与 30 秒 Web/API 健康探测拆为两个 worker，慢探测不再阻塞状态与 SSE；
- Agent/Server wire DTO 合并到 `unionc-protocol`；CPU 位宽、浮点速率、未来 capability
  错误类型及 CPU 拓扑校验在同一契约与跨 crate 测试中锁定；
- Agent 采样节拍与投递彻底解耦，spool 文件变更串行化；OTLP 改为非默认 feature；
- 前端修复跨主机草稿串用、完整快照覆盖、SSE 旧连接回调、旧 GET 覆盖已完成
  create/update/delete、应用字段别名冲突及响应式布局/错误可见性问题；
- SQLite 恢复只接受当前应用版本、当前 schema 与当前 manifest 结构，并在私有 staging
  中完成校验后原子发布；测试数据库改为 RAII
  临时目录，不再向 `/tmp` 泄漏 `.db/-wal/-shm`；真实网络与硬耗时断言改为确定性测试；
- 删除冗余 runtime settings 表、`PathSettings` 包装、重复配置应用、未使用密码代理、重复 DTO、
  死代码及未使用 dependency/feature。

### Agent 易用性追加整改

- 管理台创建稳定的待激活 Agent 实例与短时、单次激活码；enrollment 注册接口已经删除，
  当前版本不提供兼容入口；
- Agent 增加 `pair`、`doctor`、`status`：通信 secret 和 polling secret 在目标主机生成，
  浏览器只输入一次性码，长期 secret 不经过浏览器、URL 或剪贴板；
- 激活、响应丢失重试、当前配对重新授权、撤销 tombstone 与同实例重新配对均有数据库事务和集成测试；
- 软件分发与 Web 解耦，三平台普通卸载默认保留身份，按平台规定顺序成功完成并验证的显式
  purge 才清理本地凭据；完整退役固定为“先在 Web 撤销，再做平台本地永久清理”；
- 标签发布流水线从同一 nFPM 定义生成 Linux deb/rpm/tar，生成 Windows x64 WiX MSI 和
  macOS universal pkg，并强制平台签名、公证、签名校验清单、provenance 及三平台生命周期
  冒烟测试。Windows MSI 注册原生 SCM service，不依赖 PowerShell 执行安装生命周期。

### 面向当前部署规模的取舍

- 使用数据目录内的 SQLite 保存监控历史、主机配置和审计记录；WAL、`synchronous=FULL`、
  `BEGIN IMMEDIATE` 与进程内写门控共同约束单写者语义；
- 部署边界固定为单 Server、本机磁盘和约 20 台主机，不支持 NFS/SMB 活动库或多节点共享写入；
- 内置 SQLite 一致性备份、强制清单恢复和完整性检查；0.3.2 的 schema 是唯一当前存储基线，不支持
  旧 Server 包或旧数据库就地升级，也不提供旧数据转换/导入桥；
- UnionC/Agent 运行日志继续由 stdout、journald 或本地轮转文件管理；
- Sunshine 日志实时从各主机 API 读取，不复制进业务数据库；
- 20 台以内使用进程内限流和 20 台分页，不引入 Redis、Loki 或 OpenSearch；
- 外部 Sunshine 操作无法参与本地事务，因此选择成功响应优先、审计失败独立告警。

## 整改验证（2026-08-20 本地快照）

- `cargo fmt --all -- --check`：通过；
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`：通过；
- `cargo test --workspace --all-features`：249 项 Rust 测试通过；Server 的配对、限流、审计、
  当前 schema/恢复、并发顺序与 Sunshine 快照测试均使用真实临时 SQLite 文件；
- Agent 无默认 feature、仅 OTLP、仅 NVIDIA 三组 `--all-targets` 本机编译矩阵通过；
- 前端 `npm run lint`、`npm run typecheck`、`npm test -- --run`：通过，11 个测试文件、
  42 项测试通过；Vite 在隔离的临时输出目录完成生产构建，共转换 116 个模块；
- `git diff --check`：通过；本轮未执行依赖漏洞/发布供应链审计；
- 本地未提供真实 OTLP Collector，两项 live 用例按其文档化条件跳过；官方 protobuf 定义
  解码 Agent 编码的 3 项契约测试已真实执行并通过。

### 2026-08-21 GitHub CI 日志复核

用户提供的 `logs_87945624312/` checkout 为当前 HEAD
`24f459166fbc5dff516e3f8862d1b377082fdffd`，因此可作为本次远端门禁证据。该次 6 个 Job
并非全绿：Frontend、Agent macOS、Agent Ubuntu、Server 和 OTLP 完成，Agent Windows
失败。

- Windows 使用 Rust 1.98.0，在 `cargo clippy -p unionc-agent --all-targets -- -D warnings`
  停止：maintenance helper 的 `.chunks_exact(4)` 触发新
  `chunks_exact_to_as_chunks` lint；tray 的五个 `version` 字段在 Windows target 被判定未读。
  Windows Agent Job 后续的 Agent 测试、三组 feature check、release binary、PE、WiX
  和 MSI 门禁都**没有执行**，不能把 workflow 中存在这些步骤等同于本次已验证。
- 日志提供前的本地文档整改验证使用 Rust 1.96.0，GitHub `stable` 已是 1.98.0；两者的 Clippy 结果不同并不
  矛盾，也说明复现远端 lint 前必须比较工具链版本和目标平台。
- Frontend 的 lint、typecheck、42 项测试和 build 均通过，但 `npm ci` 摘要报告 2 个 high
  severity vulnerabilities。该次提交的 workflow 没有独立 `npm audit` 失败门禁，安装命令返回 0
  不能解释为依赖审计通过；日志摘要本身不足以确定具体 advisory 和安全影响。
- 所有 Job 都报告 `actions/checkout@v4` 的 Node 20 action runtime 已弃用并被强制使用
  Node 24；Frontend 的 `actions/setup-node@v4` 也有相同警告。这与项目业务代码警告不同，
  但属于 workflow 维护项。
- 当前 CI 没有单独运行 `unionc-protocol` 单测；OTLP Job 的两个 `otlp_live` 用例验证真实
  Collector 接受，但没有执行 `otlp_encoding` 的 3 个官方 proto 解码断言。本地提交门禁
  仍需显式补这两项。

#### 2026-08-21 日志问题修复

- maintenance helper 改用 Rust 1.98 建议的 `as_chunks::<4>()`；tray 的五个内部字段改名为
  `_version`，同时用 `#[serde(rename = "version")]` 保持 NDJSON 字段名与反序列化时的包版本校验。
- 本地使用 Rust 1.98.0 对 `x86_64-pc-windows-msvc` 执行 Agent 全目标 Clippy，以及无默认
  feature、仅 OTLP、仅 NVIDIA 三组 `--all-targets` check，全部通过；Windows GNU 的同类
  Clippy 也通过。Agent 83 项测试通过。
- 锁文件把 dev-only 传递依赖 `brace-expansion` 从 5.0.8 升至 5.0.9、`nanoid` 从 3.3.16
  升至 3.3.18；`npm ci` 与显式 `npm audit --audit-level=high` 均报告 0 vulnerabilities，
  前端 lint、typecheck、42 项测试和 116 模块生产构建通过。
- CI 与 release workflow 的 8 处 `actions/checkout@v4` 及 CI 的
  `actions/setup-node@v4` 已升级到 v7；Frontend Job 新增 high/critical npm 审计门禁。

这些本地与交叉编译结果已解除原日志中的前置阻断，但不能冒充新的远端全绿证据。Windows
原生测试、release PE、WiX 与 MSI 步骤仍须在 GitHub `windows-latest` 重新运行后确认。

### 2026-08-22 GitHub CI 新日志复核与修复

用户随后提供的 `logs_88207120021/` 中，6 个 Job 实际 checkout 的源码均为当前 HEAD
`9dab39ff56229f8fa940c7e2ac91fed9b5b0a90b`。Server、Frontend、OTLP 和 Agent macOS
成功；Agent Ubuntu 与 Agent Windows 因各一项测试失败而失败。日志开头出现的 Runner
Image Provisioner 提交号不是项目源码提交号。

- Ubuntu 的 `collectors::tests::missing_temperature_is_not_reported_as_zero` 把
  `Components::new()` 当成“没有温度传感器”。但 Linux 实现不会使用该参数，而会读取运行器
  真实的 `/sys/class/hwmon`；有可读传感器时断言结果为空必然失败。测试现改为给
  `core_capabilities` 传入显式空切片，并精确验证 `system.temperature` 被报告为
  `Unsupported` 能力缺口。`linux_hwmon` 原有测试继续验证缺失读数返回 `None`，不会伪造
  `0 °C`。运行时采集逻辑没有改动。
- Windows 的
  `windows_tray::tests::connection_probe_rejects_missing_or_mismatched_server_version` 断言消息
  包含连续文本“不可用”，而缺少必填 `version` 时确定返回的消息是“Server 未返回可用的
  UnionC 健康状态（格式或版本信息无效）”。测试现精确核对该错误消息，并保留对
  `offline` 状态和空版本的断言；运行时探测逻辑没有改动。
- 新日志已经证明上一轮修复生效：Windows Rust 1.98.0 全目标 Clippy 通过；Frontend 使用
  `actions/checkout@v7` 和 `actions/setup-node@v7`，`npm ci` 与独立 audit 均报告 0
  vulnerabilities，lint、typecheck、42 项测试和 116 模块构建全部通过。Server 的 154 项
  测试、真实 OTLP Collector 的 2 项强制集成测试，以及 macOS Agent 的 81 项测试和三组
  feature check 也全部通过。
- 本次修复后，本地 Agent 全套 83 项测试和无默认 feature、仅 OTLP、仅 NVIDIA 三组
  `--all-targets` check 通过；Rust 1.98.0 的 Windows MSVC 全目标 Clippy、Rust 格式检查和
  `git diff --check` 均通过。沙箱内首次复跑时，3 个需要绑定
  `127.0.0.1` 临时端口的配对测试收到 `EPERM`；允许环回监听后同一测试集全部通过，故该
  现象不是项目回归。

这批日志仍不是全绿证据。Ubuntu 测试失败后的三组 feature check 与 Linux packaging
lifecycle，以及 Windows 测试失败后的三组 feature check、release 三个二进制、PE、WiX
和 MSI 步骤均被跳过；合入本次两处测试修复后仍须重新运行 GitHub CI 才能补齐远端证据。

### 2026-08-22 GitHub CI 第三批日志复核与修复

`logs_88210527892/` 的 6 个 Job 均 checkout 当前 HEAD
`57c6fcd813f576e90a21b5d7db92a625afd31e7c`。OTLP、Agent Ubuntu、Server、Agent macOS 和
Frontend 成功；Agent Windows 只在 `validate WiX MSI authoring` 失败。整批日志没有其他
项目级 error 或 warning。

- 上一批两项测试修复已获远端验证：Ubuntu Agent 83 项测试、Linux lifecycle、Windows
  Agent 89 项测试及三组 feature check 全部通过。Windows 原生 release 三个二进制构建成功，
  PE 检查也分别确认 service 为控制台 subsystem、maintenance 与 tray 为 GUI subsystem。
- WiX 作者测试从首次加入时就要求托盘源码含有
  `placeholder=\"http://127.0.0.1:3001\"`，但同一提交中的产品实现及项目文档始终使用
  `placeholder=\"https://unionc.example.com\"`。远端 HTTP 只允许回环、生产示例使用 HTTPS
  才符合当前安全契约，因此修正陈旧测试，不把产品提示降回开发地址。
- 修正首项并继续完整执行脚本后，又发现两条此前被前置异常遮蔽的陈旧断言：脚本仍寻找旧的
  `send('/pair')` 调用，并要求 release workflow 硬编码两个版本号。现改为验证授权密钥输入在
  当前 `startOperation('/pair', ...)` 前被清空；版本测试则验证 Windows MSI 从
  `unionc-agent` Cargo metadata 取得单一版本源、tag 不一致时 fail closed，并把解析结果传给
  unsigned build、signed build 和 lifecycle 三个步骤。
- 修改后的完整 `Test-WixAuthoring.ps1` 已在与 CI 相同的 Windows PowerShell 5.1 中独立
  运行两次，均输出 lifecycle、tray、service、rollback 与 purge 检查通过并以 0 退出；
  `git diff --check` 也通过。三处改动都只更新静态门禁，没有修改产品运行时行为。

Windows Job 因作者测试失败而没有执行最后的 `build WiX 4 MSI`，所以这批日志仍不能证明
MSI 实际构建成功。合入测试修复后须重跑 Windows Job，以补齐 WiX 构建证据。

## 首次审查结论（整改前）

> 以下内容是 2026-07-25 首次审查时的历史快照。其中外部数据库、数据库测试跳过、
> 前端无测试等描述只用于解释当时问题，不代表当前 0.3.2 状态。

本次审查覆盖 Rust 服务端、跨平台 Agent、React 前端、CI、部署配置、依赖安全和测试体系。

共确认：

- 7 个高优先级问题；
- 9 个中低优先级问题；
- Rust 与 npm 锁定依赖未发现已知安全漏洞；
- 常规静态检查、Rust 测试和前端生产构建整体通过；
- `cargo fmt --all -- --check` 当前失败，会阻断 CI；
- 当前环境没有运行当时所需的外部数据库和真实 OTLP Collector，因此相应集成测试未实际执行。

## 高优先级问题

### 1. CI 当前必然被格式检查阻断

`cargo fmt --all -- --check` 执行失败，而 CI 在 `.github/workflows/ci.yml:40` 明确执行该命令。

涉及位置包括：

- `agent/src/collectors/mod.rs:455`
- `server/src/sunshine/http/hosts.rs:10`
- `server/src/sunshine/logs.rs:88`
- `server/src/system/http.rs:362`
- `server/src/system/http.rs:379`
- `server/tests/sunshine_host_persistence.rs:176`

虽然这些只是格式差异，但会导致所有 PR 的 CI 失败。

建议：执行 `cargo fmt --all`，并在提交前钩子或本地检查脚本中加入格式校验。

### 2. 生产静态前端没有真正获得安全响应头

`docs/examples/caddy/Caddyfile.console.example:20` 由 Caddy 直接提供静态前端文件，但该配置在第 45 行认为 UnionC 会替静态页面下发 CSP、HSTS、`nosniff` 等安全头。

实际上，`server/src/http/security_headers.rs:56` 的安全中间件只覆盖 UnionC API 响应，无法影响 Caddy 直接返回的 HTML、JavaScript 和 CSS。

影响包括：

- HTML 文档没有 CSP；
- 静态页面缺少点击劫持保护；
- 首次页面响应没有 HSTS；
- API 响应上的 CSP 对前端文档不起作用。

建议：在 Caddy 静态站点配置中显式加入一套与 React 构建产物兼容的 CSP、HSTS、`X-Content-Type-Options`、`Referrer-Policy` 和 frame 限制。不能直接复用后端的 `default-src 'none'`，否则会阻止前端资源加载。

### 3. Agent 上报接口可被匿名请求放大为数据库压力

`server/src/monitoring/http.rs:124` 的上报处理流程是：

1. 检查反向代理契约；
2. 提取任意 Bearer token；
3. ping 数据库；
4. 查询 token 对应的主机；
5. 认证成功后才执行按主机限流。

因此，攻击者可以持续发送随机 Bearer token，让每次匿名请求产生数据库 ping 和 token 查询。现有按主机令牌桶只保护有效 token 的写入路径，无法保护认证查询本身。

建议：使用已经解析出的客户端 IP，在任何数据库操作之前增加按 IP 和全局预认证限流；必要时在反向代理层同时设置请求速率限制。

### 4. 操作已生效后，审计失败仍向客户端返回失败

监控主机删除和令牌轮换先提交业务变更，再执行可能失败的审计 INSERT：

- `server/src/monitoring/http.rs:252`
- `server/src/monitoring/http.rs:282`

其中令牌轮换风险最高：旧令牌已经失效，但若审计写入失败，客户端会收到错误并拿不到唯一一次返回的明文新令牌。

Sunshine 代理操作也普遍存在相同问题，例如：

- `server/src/sunshine/http/proxy.rs:18`
- `server/src/sunshine/http/proxy.rs:288`

上游操作已经完成后，审计失败会让客户端误认为操作失败，随后重试可能产生重复或非幂等副作用。

建议：

- 数据库内的业务变更与审计写入放入同一事务；
- 无法回滚的外部 Sunshine 操作采用尽力审计；
- 审计失败记录告警和指标，但不要把已经成功的外部操作伪装成失败。

### 5. 数据库 URL 的查询参数密码可能被保存为 `********`

后端会遮盖数据库 URL 中的以下查询参数：

- `password`
- `sslpassword`
- `sslkeylogfile`

实现位于 `server/src/system/http.rs:131`。

前端 `web/src/views/SettingsView.tsx:138` 只把 userinfo 中的密码拆分到独立密码字段，却原样保留完整 query string。保存校验在第 198 行也只检查独立密码字段是否等于 `********`。

对于以下形式的历史连接串：

```text
driver://user@db/database?password=secret
```

后端返回的掩码可能被前端再次作为真实 query 参数保存：

```text
driver://user@db/database?password=********
```

这会导致重启后数据库连接失败。

建议：

- 后端改为返回结构化连接字段和 `has_password` 状态；或
- 前端识别所有敏感查询参数中的掩码，要求用户重新输入；
- 不要把后端返回的脱敏 URL 当作可无损编辑、保存的配置源。

### 6. Spool 连续失败保护可能永远不会触发

Agent 主循环在 `agent/src/main.rs:154` 中，只要 `pending_count()` 成功，就会把 `SpoolHealth` 的连续失败计数清零。

随后，`agent/src/main.rs:304` 的 spool 写入如果失败，只会把计数从 0 增加到 1。

在“目录仍可读取，但磁盘已满、文件系统只读或写权限丢失”的场景中，每轮流程都会变成：

```text
读取队列成功 → 失败计数归零 → 写入失败 → 失败计数变为 1
```

因此，文档承诺的连续 100 次失败退出永远无法达到，采样会持续丢失。

此外，`flush_spool` 自身的 I/O 失败没有纳入 `SpoolHealth` 计数。

建议：

- 按具体操作分别跟踪读取、写入、确认出队的健康状态；或
- 只有完整 spool 周期成功时才清零统一计数；
- 增加“目录可读但持续不可写”的主循环级回归测试。

### 7. 多 Sunshine 主机日志被重复读取并错误标注

`server/src/sunshine/http/hosts.rs:186` 的日志接口虽然接收主机 ID，但在确认主机存在后始终读取同一个本地路径。

该固定路径来自：

```text
server/src/infra/paths.rs:78
```

前端 `web/src/views/LogsView.tsx:14` 却为每台 Sunshine 主机分别请求一次，并在第 25 行开始把结果标成对应主机的日志。

当存在 N 台主机时，会产生：

- 每 15 秒重复读取同一个文件 N 次；
- 同一批日志在页面中出现 N 份；
- 本地日志被错误标注为不同远程主机产生的日志。

建议：明确日志端点的产品语义。若它是 UnionC 本机日志，应改为单一无主机 ID 的端点和一次查询；若目标是远程主机日志，应调用 Sunshine 日志 API，而不是读取 UnionC 本地文件。

## 中低优先级问题

### 8. `unionc-agent once` 不补传既有 spool，也不会自愈失效凭据

`agent/src/main.rs:45` 的 `once` 模式只采集并发送当前报文，不调用 `flush_spool`。

结果是：

- 之前失败保留的报文不会在后续 `once` 运行中补传；
- 当前发送成功也不会清理历史积压；
- 收到 401/403 时只把报文写入 spool 后退出；
- 下一次运行仍会从 `agent-token` 加载被拒绝的旧 token，见 `agent/src/transport.rs:39`。

建议：让 `once` 在发送当前采样前或后补传有限数量的积压，并复用 `run` 模式的重新注册逻辑；或者明确禁止将 `once` 用于定时任务，并修正文档中“失败时报文留在 spool”可能造成的恢复预期。

### 9. Sunshine 配置编辑器会破坏非字符串类型

`web/src/views/SunshineView.tsx:529` 将所有配置值通过 `String(...)` 展示，并在输入变化时保存为字符串。

如果 Sunshine 配置包含布尔值、数字或对象，编辑后可能变成：

- `true` → `"true"`
- `5` → `"5"`
- 对象 → `"[object Object]"`

建议：使用 JSON-aware 编辑器、配置 schema 或按原类型解析输入；无法安全编辑的复杂值应只读显示。

### 10. Sunshine 更新操作在网络探测期间仍持有全局设置锁

`server/src/sunshine/http/hosts.rs:97` 获取 `_settings_guard`，但没有在网络探测前显式释放。

虽然第 129 行注释写着“写锁释放后再执行网络检测”，Rust guard 实际会一直存活到函数作用域结束。

共享 HTTP 客户端超时为 15 秒，见 `server/src/infra/http_client.rs:15`。离线主机可能导致所有 Sunshine 增删改操作被全局阻塞约 15 秒。

建议：发布内存快照后显式执行 `drop(_settings_guard)`，然后再做可达性和连接探测。

### 11. Agent 对服务端响应体没有大小限制

以下路径会完整读取响应体：

- 注册：`agent/src/transport.rs:90` 使用 `bytes()`
- UnionC 上报：`agent/src/transport.rs:143` 使用 `text()`
- OTLP 上报：`agent/src/transport.rs:181` 使用 `text()`

请求超时只能限制时间，不能限制响应体大小。恶意、被劫持或错误配置的端点可以返回超大响应，消耗 Agent 内存。

建议：使用流式读取并设置明确上限，例如错误响应 64 KiB、注册响应更小；同时校验 `Content-Length`，但不能仅依赖它。

### 12. 服务端和 Agent 对 HTTP 409 的错误分类不一致

`server/src/monitoring/http.rs:147` 会在 report ID 已属于另一台主机时返回 409，并明确注明同一个 ID 重试永远不会成功。

Agent 在 `agent/src/transport.rs:332` 中只把 400、413 和 422 归类为永久错误，409 会被视为瞬时错误。

结果是冲突报文可能永久留在 spool 队首，阻塞后续正常报文。

建议：根据状态码和稳定错误码把该 409 归类为永久拒绝，并确认出队；不要把所有 409 一概处理，避免误伤未来可恢复的冲突类型。

### 13. 监控页面无法访问第 201 台之后的主机

`web/src/api.ts:103` 的 `monitoringHosts()` 不接受分页参数，只请求服务端默认页。

服务端默认最多返回 200 台。`web/src/views/MonitoringView.tsx:445` 虽然会提示结果被截断，但没有上一页、下一页或继续加载功能。

建议：实现分页或无限滚动，并让查询 key 包含 `limit`、`offset`；至少提供“加载更多”能力。

### 14. Linux 磁盘吞吐可能重复统计 LVM/RAID

`server/src/system/resources.rs:259` 同时把以下设备计入总吞吐：

- 物理盘；
- `md*` RAID 设备；
- `dm-*` device mapper 设备。

使用 LVM、dm-crypt 或软件 RAID 时，同一批 I/O 可能同时出现在逻辑设备和底层物理盘的 `/proc/diskstats` 中，造成重复计算。

建议：依据 `/sys/block/*/slaves` 拓扑，只选逻辑层或叶子物理层中的一个口径，并增加 LVM/RAID fixture 测试。

### 15. 前端发布脚本的回滚并不完整

`web/scripts/publish-static.mjs:23` 的流程是：

1. 将当前 `dist` 改名为 `dist.previous`；
2. 将 `dist.next` 改名为 `dist`；
3. 递归执行 chmod；
4. 删除旧版本。

如果步骤 2 成功、步骤 3 失败，catch 分支因为 `dist` 已经存在而不会恢复 `dist.previous`。

这与 `docs/web.md:27` 声称的“失败会回滚到上一版本”不一致。

建议：失败时先把不完整的新 `dist` 移走或隔离，再恢复 `dist.previous`；增加模拟 chmod 失败的测试。

### 16. 前端缺少行为测试和 lint

`web/package.json:7` 仅提供：

- 开发服务器；
- TypeScript 类型检查；
- 生产构建；
- 预览服务器。

仓库中没有前端测试文件，也没有 ESLint 等静态规则。

因此，以下问题无法被现有 CI 捕获：

- 数据库脱敏 URL 的往返保存；
- Sunshine 配置值类型保持；
- 日志聚合语义；
- 监控分页；
- mutation 错误展示；
- 静态发布回滚。

建议优先为纯函数和关键数据流增加 Vitest 测试，再为关键管理操作增加 React Testing Library 测试。

## 其他可改进项

### Sunshine 主机字段缺少长度限制

`server/src/sunshine/http/common.rs:18` 只验证名称和用户名非空，没有为名称、用户名和密码设置合理长度上限。

全局 body 上限虽然能防止无限输入，但仍允许较大字符串进入数据库、加密字段、列表响应和审计内容。

建议在 API 模型层设置清晰的字段长度和控制字符限制。

### 部分前端 mutation 错误不会展示

例如：

- 结束 Sunshine 会话的 `closeMutation` 没有对应的 `MutationError`；
- 客户端启用/禁用使用的 `updateM` 没有对应错误提示。

相关位置：

- `web/src/views/SunshineView.tsx:321`
- `web/src/views/SunshineView.tsx:415`

用户点击后如果请求失败，界面可能没有可见反馈。

### CI 使用可变的 OTLP Collector 镜像标签

`.github/workflows/ci.yml:86` 使用：

```yaml
otel/opentelemetry-collector-contrib:latest
```

这会降低 CI 的可复现性，上游最新镜像变化可能在没有代码改动时导致测试失败。

建议固定明确版本，重要发布流程可进一步固定镜像 digest。

## 验证结果

### 已通过

- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo check -p unionc-agent --no-default-features --all-targets`
- `cargo check -p unionc-agent --no-default-features --features otlp --all-targets`
- `cargo check -p unionc-agent --no-default-features --features nvidia --all-targets`
- `cargo test --workspace` 中能够在当前环境执行的测试
- 前端 TypeScript 类型检查
- 前端生产构建到临时目录
- `git diff --check`
- RustSec 依赖审计：扫描 `Cargo.lock` 中 343 个 crate 依赖，未发现漏洞
- npm 生产依赖审计：未发现漏洞

### 未通过

- `cargo fmt --all -- --check`

### 当前环境未真实执行的测试

- 2 个 OTLP Collector 集成测试：未设置 `UNIONC_AGENT_TEST_OTLP_ENDPOINT`

CI 已通过 `UNIONC_TEST_REQUIRE_DATABASE=1` 和 `UNIONC_AGENT_TEST_REQUIRE_OTLP=1` 强制相应环境不能静默跳过，因此这是本次本地审查的验证边界，不代表 CI 配置完全缺失。

### 测试覆盖概况

- Rust 源码和测试目录约有 124 个 `#[test]`/`#[tokio::test]` 标记；
- 数据库测试辅助会在本地缺少连接串时输出警告并返回；
- 前端当前没有测试文件。

## 建议处理顺序

### 第一阶段：阻断与安全问题

1. 执行 Rust 格式化，恢复 CI；
2. 修复 Caddy 静态前端安全头；
3. 为 Agent 上报增加预认证限流；
4. 修复令牌轮换、主机删除和 Sunshine 副作用后的审计错误语义。

### 第二阶段：数据与可靠性问题

1. 修复数据库脱敏 URL 的保存流程；
2. 修复 spool 健康计数和 `once` 补传；
3. 修复 Sunshine 日志的端点语义；
4. 保持 Sunshine 配置值类型；
5. 修正 HTTP 409 错误分类。

### 第三阶段：扩展性与工程质量

1. 实现监控主机分页；
2. 修正磁盘吞吐统计口径；
3. 完善静态发布回滚；
4. 增加前端单元测试、组件测试和 lint；
5. 补充字段长度限制和 mutation 错误提示；
6. 固定 CI 中外部服务镜像版本。
