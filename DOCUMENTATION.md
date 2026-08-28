# Union 项目手册

Union v0.5 采用 Modular Monolith Core + Runtime Plugin + 独立业务进程，并保留向独立 Service
演进的边界。Core 是唯一公网入口和控制面，当前负责认证、RBAC、配置、控制面审计、模块注册、
Gateway、健康检查、进程监管和生命周期，不直接实现业务领域。完整架构入口见仓库
[README](README.md) 和 `docs/` 文档中心。

## 两个选择阶段

| 阶段 | 负责者 | 决定内容 |
|---|---|---|
| 发行构建 | Union Builder | 当前不可变发行包含哪些标准模块包 |
| 系统运行 | Union Plugin Runtime | 已包含模块的发现、校验、migration、注册、启停、监管和健康 |

这不是旧式编译期业务 feature：模块 Backend/Frontend 不链接进 Core/Web Shell。也不是开放的在线
插件市场：未包含模块或新代码必须由 Builder 纳入新的 Union 发行。

## 固定命名空间

- 平台 API：`/api/platform/*`
- 模块 API：`/api/modules/<id>/*`
- 模块页面：`/modules/<id>/*`
- 模块资源：`/modules/<id>/assets/*`

工作进程验证 `gateway-v1` protocol、audience、per-process token 和 API prefix。浏览器不能直连
worker；Core 覆盖内部身份头、执行平台 RBAC，并仅在 Manifest 显式标记的设备/领域端点保留模块
自己的凭证验证。

Dufs 不属于领域凭证例外：其全部公开路由均为 `auth=platform`，统一使用 Union 会话、CSRF 和
`dufs.files.read/write/delete` RBAC。Core 注入经过验证的 `X-Union-Principal`，Dufs worker 仅在
`gateway-v1` 四项身份同时有效时信任它，不提供独立登录或自己的会话 Cookie。

每条 Manifest route 可声明 `request_body.max_bytes` 字节上限与
`request_body.total_timeout_seconds` 绝对读取期限；缺省为 1 MiB/30 秒。Core 对声明长度和实际流式
字节执行上限，并从接纳请求起计时到 body EOF，持续滴入小分块不会重置期限。worker 只能进一步
收紧，不能扩展 Core 的入口预算。

v0.5 的进程模块契约到此为止：Manifest/兼容与依赖、配置到环境的受控注入、Gateway 路由、健康
检查、启停和故障重启已经落地；Core 内部 Rust SDK、Event Bus、任务、通知、服务发现和 SDK 审计
接口没有暴露为独立进程可调用的远程线协议。当前五个标准 Manifest 因此不声明业务事件，也不能
把这些内部抽象描述为 worker 已可使用的公共能力。

## 模块包

每个模块提供 `manifest.json`、`permissions.json`、`config/schema.json`、`version.json`、Backend、
Frontend 和自己的 migration。Manifest 声明 Core/Platform/Plugin API 兼容范围、依赖、执行模式、
route/upstream mapping、权限、健康、生命周期、服务和事件。

Sunshine、Host、Sentinel、Photo 各使用专用 PostgreSQL database/role（可在各自库内继续使用
命名 schema）；Dufs 保留独立 SQLite + rooted filesystem，Core 使用独立控制面 SQLite。四个
PostgreSQL database 可以共用 cluster，但不能共享 database、role、业务表、migration、外键或事务。
五个标准 worker 默认由 Core 以同一 OS UID 启动，属于同一受信任发行域；独立进程提供故障、数据
所有权和生命周期边界，不等同于恶意插件沙箱。低信任模块必须改用独立 UID、Container 或 Service。

模块 Schema 用 `x-union-resource: postgresql_database` 声明 PostgreSQL database/role 所有权，
用 `x-union-resource: storage_tree` 声明状态或内容目录。Core 拒绝同 endpoint 复用 database/role
以及相同或父子重叠的绝对目录；旧冲突值不会注入 worker。该检查只降低配置串线风险，不识别 DNS
别名、符号链接或挂载关系，更不能在同 UID 信任域中形成 OS 沙箱。

## 前端

Shell 不内置业务 route。它根据运行时 catalog，只动态加载当前发行已包含、已启用且当前用户拥有
页面权限的模块 ESM entry；模块通过 `activate(hostSdk)` 使用 Shell React 并只能实现 Manifest 已
声明组件。前端权限过滤是可用性边界，后端仍必须逐请求执行授权。

Core 和 Shell 不采集或展示整机 CPU、内存、网络、磁盘与挂载点；这些资源监控能力归 Host
Monitoring 模块。Shell 自带总览仅展示模块生命周期和服务状态。

## 生产声明

源码测试、Manifest 校验和本地分发构建不等同于生产完成。真实 PostgreSQL、SQLite/文件系统、
大文件流式代理、Range、SSE/媒体、进程故障、发行升级与数据恢复仍须按发布矩阵验收。
