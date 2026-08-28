# 发行模块与私有进程

Union v0.5 把“发行包含”和“运行状态”分成两个阶段：

| 阶段 | 负责组件 | 可以改变什么 |
|---|---|---|
| 发行构建 | Union Builder 2.1 | 哪些标准模块包进入不可变 Union 发行 |
| 系统运行 | Union Core / Plugin Runtime | 发行内模块的配置、启用、停用、监管和健康 |

Core 和 Web Shell 不链接业务模块代码。增加当前发行没有的模块、替换 Backend/Frontend 或升级
模块版本，都必须由 Builder 生成并验证一个新发行；运行期没有安装、升级、卸载、上传或联网下载
模块代码的入口。

## 标准模块包

Builder 从锁定的源码 revision 构建每个模块，并在发行根生成：

```text
modules/<id>/
├── manifest.json
├── permissions.json
├── version.json
├── config/schema.json
├── backend/<executable>
├── frontend/...
└── migrations/...
```

`manifest.json` 是运行契约的事实源，声明模块身份和版本、Core/Platform/Plugin API 兼容范围、
依赖、进程入口、配置到环境的映射、回环 bind、后端路由、权限、前端贡献、migration、健康探针、
重启策略、服务和事件。Builder 校验单包及整个发行的依赖图、兼容范围、文件引用和摘要；Runtime
只发现当前发行中 `distribution=bundled` 的本地只读包。

v0.5 已实现的**进程模块契约**仅包括 Manifest/兼容与依赖校验、配置到环境的受控注入、回环
`gateway-v1` 路由、健康探针、启停、故障重启和关闭。Core 内部虽然有 Rust SDK 及 Event Bus、任务、
通知、服务发现、审计等抽象，但没有把它们暴露为独立进程可调用的远程线协议。五个标准进程模块的
`publishes/subscribes` 因而均为空；只有后续定义带版本、授权和双向认证的进程协议后，worker 才能
使用相应能力。模块自身的业务审计仍写入其私有数据库或日志，Core 只持久化平台控制面操作审计。

首次发现的包默认未启用。管理员必须先按模块的 JSON Schema 保存完整配置，之后才能启用。配置和
enable/disable 状态保存在 Core 数据目录，配置读取时对声明的敏感字段脱敏；这些状态不会修改发行
包。重新扫描只重新读取当前发行的本地包并校验/应用模块图，不会访问网络或引入新代码。

升级后的 Schema 若不再接受旧配置，Core 会保留原文件但把模块标记为未配置，既不向 worker 注入
旧值，也不回显其中的秘密；管理界面会显示兼容错误和新 Schema，管理员可提交一份完整新配置来
恢复。配置 Schema 的破坏性变化仍应提升 `configuration.version` 并在发行说明中给出转换步骤。

配置 Schema 使用封闭的资源注解声明所有权：

- `x-union-resource: postgresql_database` 要求规范的 `postgresql://` URL，并在同一 endpoint 上
  阻止两个模块复用 database 或 role；
- `x-union-resource: storage_tree` 要求非根、绝对且词法规范的路径，并阻止模块内或模块间相同、
  父子重叠的目录树；Core 实际数据根和 Plugin Runtime 状态根属于保留目录，模块目录与其相同、
  为其祖先或后代同样被拒绝。

Core 在保存新值、判断 configured 状态和向 worker 注入前复核全部声明。冲突的磁盘旧配置保留用于
管理员修复，但不会被回显或注入。这是保守的误配置门禁，不解析 DNS alias、symlink、bind mount
或实际进程访问权限；同 UID worker 仍处于共同信任域，强隔离必须依靠独立 UID、数据库/文件权限、
Container 或 Service。

保留目录取自进程实际解析后的 `UNIONC_DATA_DIR` 与 Plugin Runtime 状态根，包括外置
`UNIONC_PLUGIN_STATE_DIR`。升级前若模块 storage tree 与这些目录重叠，旧 JSON 不会被删除，但该
模块将 fail-closed 为未配置；管理员必须先迁移业务数据，再提交独立目录配置并重新启用。

## 当前标准模块

五个模块均以受监管的本地私有进程运行，不能作为独立公网服务发布或部署：

| 模块 | 公共 API base | 数据所有权 |
|---|---|---|
| Sunshine | `/api/modules/sunshine` | 专用 PostgreSQL database/role（内部 schema `sunshine`）；模块专属凭据密钥 |
| Host Monitoring | `/api/modules/host-monitoring` | 专用 PostgreSQL database/role（内部 schema `host_monitoring`）；Agent 配对、遥测和历史归它所有 |
| Sentinel Monitor | `/api/modules/sentinel-monitor` | 专用 PostgreSQL database/role；MediaMTX 是其受约束伴随依赖 |
| Photo Backup | `/api/modules/photo-backup` | 专用 PostgreSQL database/role 与模块媒体目录；服务端内容为明文 |
| Dufs | `/api/modules/dufs` | 模块私有 SQLite、配置和 rooted filesystem |

Sunshine、Host、Sentinel 与 Photo 各自使用专用 PostgreSQL database/role、migration history
和备份边界；同一 PostgreSQL cluster 只提供基础设施复用，不构成数据库共享。禁止模块直接访问
其他模块的数据库、表、文件、migration、事务或内部实现。Dufs 的 SQLite 是明确记录的模块私有
例外，不是平台共享数据库；Core 也只使用自己的控制面 SQLite。

Core 不再采集整机 CPU、内存、网络、磁盘或挂载点，也不提供 `/api/system/resources`。这些指标、
历史和主机视图属于 Host Monitoring；未被 Builder 纳入发行或在运行期未启用 Host Monitoring 时，
Union 不提供整机资源监控。Core 总览只展示模块生命周期和服务状态。

五个标准进程默认与 Core 使用同一 OS UID，属于 Builder 验证的同一受信任发行域。独立进程隔离的
是崩溃、生命周期和数据所有权，不是恶意代码对其他模块文件或凭据的访问；低信任模块必须先采用
独立 UID、Container 或 Service adapter。

## Manifest 驱动的网关

外部客户端只连接 Union。每个 Manifest 在 `/api/modules/<id>` 下声明允许的方法和路径，以及
转发给 worker 的内部 `upstream_path`。Runtime 只为已启用且健康可用的模块解析这些路由，拒绝
未声明路径；模块进程只绑定 loopback，具体内部端口不是公共兼容契约。

每次启动进程时，Runtime 清空继承环境，只注入 Manifest 映射的配置和保留的 `UNION_PLUGIN_*` /
`UNION_MODULE_*` 上下文。`gateway-v1` 的 protocol、audience、每进程随机 token 和 API prefix
用于证明请求来自当前 Union 实例；token 不落盘、不跨进程重启复用，也不能替代管理员会话、
CSRF、Agent/移动端凭据或其他模块领域授权。

每条 backend route 还可声明 `request_body.max_bytes` 与
`request_body.total_timeout_seconds`。未声明时分别使用 1 MiB 和 30 秒；Manifest 接受的硬上界为
1 TiB 和 24 小时。Core 会先拒绝超限的 `Content-Length`，并在 chunked 或不可信长度的流式请求中
继续累计实际字节；总期限从请求进入 Core 起持续到 body EOF，是不能靠周期性小分块续期的绝对
期限。worker 可再收紧自己的限制，但不能延长 Core 已声明的字节或时间预算。

Manifest 路由明确区分两类认证：

- `platform`：Core 在转发前执行管理员会话和声明的 RBAC permission；
- `module`：Core 仍执行网关隔离和请求清洗，领域凭据由模块验证，例如 Agent、Photo 设备或
  Sentinel 媒体 token。

Dufs 的全部 Manifest route 都是 `platform`：Core 统一验证会话、CSRF 和
`dufs.files.read/write/delete` 权限，再覆盖注入 `X-Union-Principal`。worker 只在当前
`gateway-v1` protocol、audience、token 和 prefix 同时有效时接受该 principal；不读取 Union Cookie，
也不提供生产 Dufs 登录、退出或 Dufs 会话 Cookie。

管理员不能配置任意 worker URL、binary、bind、公共 prefix 或 audience。进程可执行文件只能从
当前模块包内解析，不能经 `PATH` 或公网地址替换。

## 生命周期和 Web 贡献

Runtime 负责兼容/依赖校验、配置门禁、启动、readiness、PID/状态、健康轮询、故障退避和关闭。
进程模块自己执行其 SQLx migration，并以 readiness 作为完成门禁。停用模块会停止其进程，并使
模块 API、前端资源和导航不可用；配置与业务数据不会被隐式删除。

Web Shell 通过 `GET /api/platform/modules` 读取运行时 catalog。设置页显示 Builder 已包含的全部
发行模块，包括 disabled 和 unconfigured 项；主导航及 ESM loader 只处理已包含、已启用且当前
用户至少拥有一条页面权限的模块。模块前端从 `/modules/<id>/assets/*` 同源加载并调用标准
`activate(hostSdk)`，启停模块不要求重新构建 Core 或 Web Shell。这里的 Web `hostSdk` 只提供
React 和以 API base 为默认前缀的浏览器客户端，不代表进程 worker 获得了 Rust Platform SDK
远程通道，也不是隔离同源 ESM 的安全沙箱；模块前端属于 Builder 验证的受信任发行代码。

运行期管理接口为：

- `GET /api/platform/modules`
- `POST /api/platform/modules/rescan`
- `GET|PUT /api/platform/modules/<id>/configuration`
- `POST /api/platform/modules/<id>/enable`
- `POST /api/platform/modules/<id>/disable`

这些管理操作受 Core 登录、RBAC、CSRF 和审计边界约束。配置响应中的敏感字段显示为 `***`；
PUT 是完整值写入而不是占位符合并，必须显式提供所有被隐藏字段的新值。

## 官方发行集合

| Builder profile | 发行包含 |
|---|---|
| `minimal` | 无业务模块包，仅 Core 与 Web Shell |
| `storage` | Photo Backup + Dufs |
| `monitoring` | Sentinel Monitor + Host Monitoring |
| `full` | 五个标准模块 |

profile 固定源码 revision 和发行包含集合，不记录模块运行时是否启用。正式 `full` profile 包含
Host worker；同一 `host-monitoring` 仓库产出的跨平台 Agent 与 Photo 手机客户端则是远端
companion artifact，不属于服务器模块包或进程树。它们由各模块仓库维护、由
Union Builder Release 集中构建和发布，在远端独立安装，按兼容矩阵管理，并只通过
Union 网关访问相应模块。Photo 当前产物为 Android arm64 未签名 APK 与 iOS/iPadOS
未签名 device `.app` 归档，它们是后续签名输入，不是已上架商店制品。

## 明确非目标

- 运行时安装、升级、卸载、上传、下载或在线选取模块代码；
- 将五个 worker 当作独立公网产品或独立 Release；
- 共享业务 schema、管理员 Cookie、跨模块外键、JOIN 或事务；
- 用 Manifest 执行任意 shell，或把任意公网 URL 当作模块 Backend；
- 将 Dufs 与 Photo 强行合并。两者可以共享稳定的传输语义，但文件树与照片资产仍是不同领域。
