# Union

Union 是一个基于 Rust 的自托管统一服务管理平台，采用“中央控制面 + 独立业务工作进程”的
模块化架构。Union Core 是系统唯一公网入口和 Web 管理平台，统一负责身份认证、RBAC、配置、
控制面审计、模块注册、反向代理、进程监管、健康检查和生命周期管理，不直接承载业务逻辑。

v0.5 的标准模块均为独立进程。当前进程模块契约只覆盖 Manifest、配置注入、回环网关、健康检查
和生命周期；Core 内部的 Rust SDK、Event Bus、任务、通知、服务发现与 SDK 审计抽象不是 worker
可远程调用的线协议。若后续开放这些能力，必须先定义带版本和双向认证的进程协议。

Builder 在发行构建阶段决定包含哪些模块包；Core 二进制不通过 Cargo feature 编译业务逻辑。
Union 启动后读取当前不可变发行中的 Manifest，校验兼容和依赖，注册路由/权限/migration/前端
资源，并按管理员配置动态启停模块。运行时不从公网任意下载可执行插件。

## 当前业务模块

| 模块 | 执行模式 | API base | 数据所有权 |
|---|---|---|---|
| Sunshine | 受监管本地进程 | `/api/modules/sunshine` | 专用 PostgreSQL database/role；内部 schema `sunshine` |
| Host Monitoring | 受监管本地进程 | `/api/modules/host-monitoring` | 专用 PostgreSQL database/role；内部 schema `host_monitoring` |
| Sentinel Monitor | 受监管本地进程 | `/api/modules/sentinel-monitor` | 专用 PostgreSQL database/role |
| Photo Backup | 受监管本地进程 | `/api/modules/photo-backup` | 专用 PostgreSQL database/role + 明文媒体目录 |
| Dufs | 受监管本地进程 | `/api/modules/dufs` | 私有 SQLite + rooted filesystem |

工作进程只监听 loopback，并验证 Union 为每次启动生成的 `gateway-v1` 身份。模块崩溃由 Runtime
按 Manifest 策略退避重启；未通过 readiness、依赖或兼容门禁的模块不会注册公开路由。后续重型
模块可以通过受信任 adapter 演进为 Container 或 Service，但仍遵守相同 Manifest、API、权限、
事件和数据边界。

Manifest 的每条后端路由可用 `request_body.max_bytes` 和 `request_body.total_timeout_seconds` 声明
入口预算；未声明时安全默认值为 1 MiB 和 30 秒。Core 会在流式转发期间计算实际字节数，并从请求
进入开始施加直到 body EOF 的绝对读取期限；这不是收到新分块即可续期的 idle timeout。worker 可以
使用更严格的限制，但不能放宽 Core 的上限。

Dufs 的全部公开路由均使用 Union 平台认证：Core 验证统一会话、CSRF 和
`dufs.files.read/write/delete` RBAC 后，覆盖注入 `X-Union-Principal`；worker 只有在
`gateway-v1` 身份同时验证通过时才信任该 principal。Union 发行中的 Dufs 不保留独立登录、Dufs
会话 Cookie 或第二套账号认证。

## Web Shell

`web` 只构建统一 Shell：基础布局、认证状态、导航、权限门、模块加载器和错误边界。导航与页面
来自 `GET /api/platform/modules` 的运行时 catalog；Shell 只加载当前发行已包含、已启用且当前用户
至少拥有一条页面权限的模块。每个模块包提供自己的 ESM entry、styles 和 Manifest
route/menu/component 声明；Shell 通过 `activate(hostSdk)` 注入唯一 React runtime 和限定 API
client。启停模块不需要重建 Web Console，单模块加载/渲染失败不会破坏登录或其他模块。

## 仓库组成

| 目录 | 职责 |
|---|---|
| `server` | Core Platform、Plugin Runtime、Gateway、认证/RBAC、配置、审计和生命周期 |
| `web` | 无业务内置路由的动态 Web Shell |

本仓库只维护 Core 和 Web Shell。业务模块源码分别位于独立仓库：Sunshine 位于
[`sunshine-worker`](https://github.com/isarmg/sunshine-worker)，Host worker 及双方共用的
`unionc-protocol`、跨平台 `unionc-agent` 均位于
[`host-monitoring`](https://github.com/isarmg/host-monitoring)，Sentinel、Photo、Dufs 也各自位于
模块仓库。

这些拆分只改变源码所有权，不改变产品和部署边界：模块不作为独立公网服务发布，仍由 Builder
固定各仓库 revision、组装进同一 Union distribution，并由 Plugin Runtime 作为回环私有进程监管。
正式 `full` 服务器发行包含 Host worker，不包含需要在目标主机独立安装的 Agent；Agent 是 Host
仓库产出的 companion artifact，不是 Union 公网服务，也不属于 Core 的私有 worker 进程树。

## 支持平台与发行

Union Core 和完整服务器发行只支持 Linux `amd64`（Rust `x86_64`）与 Linux `arm64`（Rust
`aarch64`）。CI 在 GitHub 官方固定版本的原生 AMD64、ARM64 runner 上分别执行 Core Clippy 和
测试；其他操作系统或 CPU 架构会在 Core 编译边界直接失败。这个限制不适用于 Host 仓库单独交付
的远端 Agent。

正式标签只发布两个完整 `full` 服务器包：

- `union-<version>-full-linux-amd64.tar.gz`
- `union-<version>-full-linux-arm64.tar.gz`

两份包各自包含 Core、Web Shell 和精确五个私有 worker，不含 Agent，并共同由 Release 根目录的
`SHA256SUMS` 覆盖。包内 `union-release.json` 必须声明 `platform=linux` 及匹配的
`architecture=amd64|arm64`；Core 启动时再次校验清单目标与自身架构，不能跨架构误用发行包。
Builder 的 `install`/`rollback` 还会在修改活动指针前执行同一宿主目标门禁，`stage` 则可为另一台
机器预置已验证包。正式二进制使用 Ubuntu 24.04 原生 GNU 工具链链接，当前运行兼容基线因此是
该 runner 的 glibc/系统 ABI，而不是未经验证的任意旧 Linux 发行版。
这些制品仍是架构/构建里程碑，不代表已经完成生产环境迁移、升级、回滚和运行验收。

## 数据边界

Core 使用独立控制面 SQLite，只保存平台状态，不承载模块业务表。四个 PostgreSQL 模块各拥有专用
database/role、独立 migration 和备份责任；它们可以共用 cluster，但不能共用 database，且禁止
跨 owner 外键、直接查询、运行时 join 和共享事务。Dufs 独占自己的 SQLite 与文件根。旧
`unionc.db` 中 Sunshine/Host 表只可用于离线迁移/核验/回滚证据，不再进入正常请求路径。

模块配置 Schema 必须用 `x-union-resource: postgresql_database` 标注 PostgreSQL URL，用
`x-union-resource: storage_tree` 标注状态或内容目录。Core 会拒绝同一 PostgreSQL endpoint 上重复的
database 或 role，以及相同、父子重叠或非绝对规范形式的 storage tree；冲突的磁盘旧配置保留但
标记为未配置，且不会注入 worker。Core 实际解析的 `UNIONC_DATA_DIR` 和 Plugin Runtime 状态根
（包括外置 `UNIONC_PLUGIN_STATE_DIR`）也是保留 storage tree；模块目录与任一保留根相同、为其
祖先或后代都会被拒绝。这是面向声明值的误配置防护，不解析 DNS 别名、符号链接或挂载关系，也不
替代独立 UID、文件权限、数据库权限、Container 或 Service 隔离。

Photo 和 Dufs 只要求传输链路加密：外部请求必须经过 Union TLS，服务器保存的内容仍是服务可直接
读取的原始明文字节。摘要用于完整性，不构成端到端或静态内容加密。

整机 CPU、内存、网络、磁盘和挂载点属于 Host Monitoring 业务域。Core 不采集这些指标，也不再
提供 `/api/system/resources`；Shell 总览只展示 Core 所监管模块的生命周期和服务状态。是否把 Host
Monitoring 纳入发行由 Builder 决定，是否运行由 Core 在运行期决定。

## 构建与验证

正式发行由 `union-builder` v2 清单固定 Core 和模块 revision，分别构建 Core/Web Shell 与模块包，
校验 Manifest/摘要，再组装 `minimal`、`storage`、`monitoring` 或 `full` 发行集合：

```bash
union-builder check --config profiles/full.toml --server-target linux-amd64
union-builder plan --config profiles/full.toml --server-target linux-amd64
union-builder build --config profiles/full.toml --cargo-profile release --server-target linux-amd64
union-builder verify --release dist/full --server-target linux-amd64
```

构建 ARM64 时将四处目标统一替换为 `linux-arm64`；同一发行目录不能混用两个架构。

本仓库开发验证：

```bash
cargo fmt --all -- --check
cargo test --workspace --locked
cd web
npm ci
npm run lint
npm run typecheck
npm test
npm run build
```

## 关键边界

- Union 是唯一公网监听者；所有模块请求都通过其 TLS、Gateway 和适用的 RBAC/领域授权。
- Manifest 不执行任意 shell，不接受任意公网 upstream；Runtime 只发现 Builder 纳入当前发行的包。
- 模块之间不依赖内部源码或数据库。v0.5 不提供通用的进程间 Platform SDK 或 Event Bus 线协议；
  当前五个 Manifest 不声明业务事件，跨模块交互只能在后续版本化协议落地后增加。
- 模块禁用会停止进程，并使其 API、前端资源和导航不可用，但不会隐式删除配置或业务数据。
- `in_process` 只用于随 Core 信任根注册的低风险工厂；现有五个模块均为独立进程。
- v0.5 的五个进程由 Core 以同一 OS UID 启动，属于同一受信任发行域；进程边界提供崩溃和生命周期
  隔离，不是抵抗恶意模块的文件或凭据沙箱。

许可证为 [Apache License 2.0](LICENSE-APACHE)。
