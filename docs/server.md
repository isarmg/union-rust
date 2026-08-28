# Union 服务端

Union Core 仅支持 Linux `amd64`（`x86_64`）和 Linux `arm64`（`aarch64`），是系统唯一公网入口
和 Web 管理平台。前置 TLS 反代只连接 Core 的 loopback listener；Sunshine、Host Monitoring、
Sentinel Monitor、Photo Backup 和 Dufs 均作为 Runtime 监管的本地私有进程运行，不能直接暴露或
单独发布。其他操作系统/CPU 架构在 Core 编译边界被拒绝；远端 Agent 的平台范围由 Host 仓库单独
维护，不改变服务器发行边界。

## 不可变发行布局

Union Builder 2.0 生成并验证以下发行：

```text
<install-root>/
├── current -> releases/<release-id>
├── previous -> releases/<release-id>       # 存在上一发行时
└── releases/<release-id>/
    ├── bin/unionc
    ├── modules/
    │   └── <id>/
    │       ├── manifest.json
    │       ├── permissions.json
    │       ├── version.json
    │       ├── config/schema.json
    │       ├── backend/<executable>
    │       ├── frontend/...
    │       └── migrations/...
    ├── share/union/web/
    ├── share/licenses/
    ├── union-release.json
    └── SHA256SUMS
```

Core 从自身发行根发现 `modules/<id>`，不会通过 Cargo feature、静态 spec、`PATH` 或管理员填写的
URL 寻找业务实现。Builder 的 `verify` 检查摘要、身份、version/source revision、Manifest 引用、
可执行位、路径边界和文件集合。不要在已激活 slot 中单独替换 worker、Manifest 或前端；代码和
静态资源升级的最小单位是完整 Union 发行。

## Core 环境与模块配置

Core 的主要部署环境变量：

- `UNIONC_ENV=production`
- `UNIONC_DATA_DIR`：Core SQLite 与 Plugin Runtime 配置/启停状态的绝对根路径；模块业务存储
  仍由各自配置和数据 owner 决定
- 可选 `UNIONC_PLUGIN_STATE_DIR`：把 Plugin Runtime 配置/启停状态放到另一个私有绝对路径；它与
  `UNIONC_DATA_DIR` 都是 Core 保留目录，不能作为模块 storage tree 或其父/子目录
- `UNIONC_SECRET_KEY`：Core 32-byte Base64 主密钥
- `UNIONC_PROXY_SECRET`：可信 TLS 反代证明，64 位小写 hex
- 首次创建核心管理员时临时使用 `UNIONC_ALLOW_BOOTSTRAP=1` 和
  `UNIONC_BOOTSTRAP_PASSWORD`
- 可选 `UNIONC_SERVER_BIND`、`UNIONC_SERVER_PORT`、`UNIONC_RETENTION_DAYS`

这些变量不能选择发行模块，也不能改变模块 binary、bind、路由或公共 prefix。模块数据库 URL、
密钥、存储目录和领域参数由包内 `config/schema.json` 定义，通过 Web“模块管理”或平台配置 API
保存到 Core 私有状态目录。Runtime 按 Manifest `config_pointer` 只向目标进程传入允许的环境项，
并另外注入保留的 `UNION_PLUGIN_*`/`UNION_MODULE_*` 上下文。

模块配置是完整 JSON 值。GET 会把 Manifest 声明的 secret 字段显示为 `***`；PUT 不会把该标记
与旧秘密合并，更新时必须提供所有隐藏字段的完整新值。平台状态目录和文件系统权限是当前静态
保护边界，文档不把脱敏等同于配置文件静态加密。

若新发行的配置 Schema 拒绝旧值，Core 保留旧文件但不会注入 worker，并通过配置 API 返回有界、
不含配置值的 `validation_error`；模块保持未配置，管理员可按新 Schema PUT 完整替代值。这样升级
不会因为旧配置不兼容而失去修复入口。

Core 对所有 `x-union-resource: storage_tree` 声明执行同目录和父子目录冲突检查，并把实际解析后的
`UNIONC_DATA_DIR` 与 Plugin Runtime 状态根作为保留树。旧模块目录若覆盖保留根、位于其中或包含
保留根，升级后会保留原配置但 fail-closed 为未配置；先迁移业务数据，再提交互不重叠的新目录。

## 运行期模块管理

首次发现的发行包为 disabled，必须配置完成后才能 enable。Core 提供：

- `GET /api/platform/modules`
- `POST /api/platform/modules/rescan`
- `GET|PUT /api/platform/modules/<id>/configuration`
- `POST /api/platform/modules/<id>/enable`
- `POST /api/platform/modules/<id>/disable`

这些接口要求 Core 登录和对应 RBAC，mutation 还要求 CSRF。重扫仅重新发现当前发行的本地只读
包；运行期没有模块安装、升级、卸载、上传、下载或公网仓库选择 API。新增、移除或升级模块必须
由 Builder 产生并激活新发行。

Runtime 从 Manifest 注册权限、配置 Schema、服务/事件元数据、后端路由和 Web 资源，校验版本、
依赖及配置后启动进程。五个标准模块均使用 `process` execution；v0.5 Manifest 为它们保留互不
冲突的固定 loopback 端口，以避免选取临时端口与子进程 bind 之间的竞争窗口。端口仍不是公共接口；公网调用永远使用
`/api/modules/<id>/*`。Manifest 未声明、模块 disabled、启动失败或不健康的路由不可用。

## 数据所有权、迁移与备份

Core 使用独立 SQLite，只保存认证、审计和其他平台状态，不承载模块业务表。业务数据边界为：

| Owner | 数据边界 |
|---|---|
| Sunshine | 专用 PostgreSQL database/role；可在库内使用 `sunshine` schema |
| Host Monitoring | 专用 PostgreSQL database/role；可在库内使用 `host_monitoring` schema |
| Sentinel Monitor | 专用 PostgreSQL database/role |
| Photo Backup | 专用 PostgreSQL database/role + 独立明文媒体目录 |
| Dufs | 独立 SQLite + 配置 + rooted filesystem |

数据库可以运行在同一 PostgreSQL cluster，但不能共享 database、role、业务表、migration、外键、
事务或备份恢复单元。声明 PostgreSQL migration 的进程模块在绑定 readiness 前执行自身 SQLx
migration；Core 不以另一个 ledger 重复执行同一 SQL。Dufs 的 embedded/SQLite migration 仍由
Dufs 自己负责。

这些边界保证所有权和运维单元分离，但 v0.5 直接启动的 worker 默认仍与 Core 使用同一 OS UID，
所以它们是同一受信任发行域，不构成抵抗恶意模块的内核级数据隔离。低信任模块必须先通过受信任
service/container adapter 获得独立 UID、凭据和文件 ACL，不能直接加入标准 process profile。

从旧版迁移时，先停止旧写入并保存旧 `unionc.db` 的只读快照，再使用各模块的离线 importer 和
verify 流程。导入后不在线双写；旧域表只能作为核验与回滚证据，不能重新挂到 Core 请求路径。
源码存在 importer 或测试不代表真实数据迁移已通过。

Core 的 `backup`、`restore`、`integrity-check` 只覆盖 Core SQLite，不覆盖四个 PostgreSQL
database、Dufs SQLite/文件根或 Photo 内容。生产备份必须分别记录每个 owner 的一致性点，恢复时
逐一验证。Builder 文件回滚也不撤销 migration 或切换后产生的业务数据。

生产数据库角色、ACL、逐库只读验收、备份恢复单元和凭据轮换使用
[PostgreSQL 模块数据库隔离运行手册](runbooks/postgresql-isolation.md)。仓库提供可审计的
`provision-module.sql` 与 `verify-module-isolation.sql` 模板，但不会自动连接或修改部署方
PostgreSQL；必须由授权管理员在目标 cluster 分别对四库执行并保存验收证据。

## Photo 传输与存储边界

Photo 上传、下载和 API 只能通过 Union HTTPS 入口。服务器端原始文件、缩略图和派生物保持可由
服务读取的明文字节，以支持校验、去重、媒体处理和 Range 下载。静态磁盘加密、备份加密、密钥
托管和介质销毁属于部署层责任，不属于应用层端到端加密。

## 构建、安装与回滚

正式组合入口是 `union-builder` 2.0 的 schema v2 profile：

```bash
union-builder check --config profiles/full.toml --server-target linux-amd64
union-builder plan --config profiles/full.toml --server-target linux-amd64
union-builder build --config profiles/full.toml --cargo-profile release --server-target linux-amd64
union-builder verify --release dist/full --server-target linux-amd64
sudo union-builder stage --release dist/full --root /opt/union
sudo union-builder install --release dist/full --root /opt/union
sudo union-builder rollback --root /opt/union
```

ARM64 构建和验证必须把全部 `--server-target` 一致替换为 `linux-arm64`，并使用独立输出目录。

profile 是锁定 revision 的发行包含集合，不映射 Cargo feature，也不保存 enabled 状态。仓库中带
`TODO(release)` 的 revision 在正式发布前必须替换并通过 `check`。`stage` 放入已验证的不可变
slot，`install` 切换完整发行并保留 previous；`rollback` 只切回文件 slot。本文不声称这些命令或
任何官方 profile 已在当前目标主机完成生产验收。

正式 GitHub Release 同时组装 `union-<version>-full-linux-amd64.tar.gz` 与
`union-<version>-full-linux-arm64.tar.gz`，并用一个外层 `SHA256SUMS` 覆盖两者。每份完整包都必须
包含精确五个 worker、保留 Core/worker 可执行位且不含 Agent；内部 `SHA256SUMS` 覆盖发行文件。
`union-release.json` 的 `distribution.platform` 必须为 `linux`，`architecture` 必须与包名及运行
Core 的 `amd64|arm64` 架构一致。`stage` 可跨机预置，`install`/`rollback` 在切换指针前拒绝
与当前 Linux 主机不匹配的包。正式包由 Ubuntu 24.04 原生 GNU runner 链接，其 glibc/系统 ABI
是当前兼容基线；目标枚举不自动承诺任意旧 Linux 发行版。发布门禁通过仍不等同于生产可用验收。

## 健康与故障

Runtime 只有在进程存活、Manifest 健康端点可达且 `gateway-v1` identity 匹配时才把模块视为
available。它记录 PID、健康消息和重启次数，按 Manifest 策略退避重启；停用或 Core 关闭时先
请求优雅终止，再在超时后结束进程。模块仍须自行保证数据库 migration、外部依赖和数据目录满足
readiness。

## 非目标

多 Server active-active、在线模块市场、模块独立 systemd/公网入口、跨模块数据库访问、从管理台
修改 worker 地址、服务端远程执行 Agent 命令，以及承诺任意旧数据库自动升级，都不在 v0.5
范围内。
