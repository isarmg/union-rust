# 需求、能力与边界

## 产品需求

Union 必须作为唯一服务端产品交付，并满足：

1. Builder 2.1 的 `minimal`、`storage`、`monitoring`、`full` profile 形成确定、可验证的发行包含集合；
2. Core/Web 不编译业务实现，五个标准模块作为发行内独立私有进程运行；
3. Core 是唯一公网入口，统一实施 TLS 反代信任、登录、RBAC、CSRF、请求清洗、审计和 Gateway；
4. Runtime 只发现 Builder 纳入当前发行的只读模块包，并按 Manifest 注册路由、权限、配置、
   Frontend、健康和生命周期；
5. 管理员可在运行期配置、启用和停用发行内模块，但不能安装、升级、卸载或上传模块代码；
6. 模块只绑定 loopback，并验证每进程 `gateway-v1` protocol/audience/token/prefix；
7. 每个模块拥有独立数据 owner、migration、存储目录和备份恢复责任，不在线双写或跨模块访问。
8. Core/服务器发行只支持 Linux amd64 与 Linux arm64；两个目标分别构建完整 `full` 包，包内不含
   远端 Agent，且发行清单目标必须与运行 Core 的平台和架构一致。

## 模块能力

| 模块 | 核心职责 | 持久层 |
|---|---|---|
| Sunshine | 多主机配置、状态、受控代理、凭据审计 | 专用 PostgreSQL database/role，库内 `sunshine` schema |
| Host Monitoring | Agent 配对、认证、上报、最新状态与历史查询 | 专用 PostgreSQL database/role，库内 `host_monitoring` schema |
| Sentinel Monitor | 摄像头/流配置、状态协调、受限媒体入口 | 专用 PostgreSQL database/role |
| Photo Backup | TLS 传输、分片、哈希/去重、元数据、缩略图、Range/ETag | 专用 PostgreSQL database/role + 独立明文内容目录 |
| Dufs | 通用文件浏览、上传下载和目录权限 | 独立 SQLite + rooted filesystem |

Dufs 与 Photo 可以共享稳定的 blob-transfer 合同、错误 envelope、哈希和 Range 基础语义，但不共享
业务表、进程或存储。Photo 的资产、相册、时间线和媒体派生语义不进入 Dufs；通用目录浏览和任意
文件树不进入 Photo。

## 发行与运行状态

- Builder profile 锁定 Core、模块源码 revision 和发行包含，不映射 Cargo feature，也不记录
  enabled/disabled。
- Runtime catalog 包含当前发行的全部有效模块；设置页显示 disabled/unconfigured，导航只显示
  enabled 且获授权的模块。
- 模块必须先通过 JSON Schema 配置再启用。停用停止进程并关闭 API/Frontend 可达性，不删除配置、
  数据或发行包。
- 新模块或新版本只能进入新的不可变 Union 发行。重新扫描不联网，也不会扩大当前发行代码集合。

## 数据规则

- Core 使用独立 SQLite，仅保存平台状态。
- Sunshine、Host、Sentinel、Photo 各使用专用 PostgreSQL database/role、migration history 和
  备份单元；它们可以共用 cluster，但不能共用 database。
- 禁止跨 owner 外键、JOIN、写事务、“公共业务表”和直接文件访问。
- Dufs SQLite 和文件根只服务 Dufs，不成为平台或模块共享数据库。
- 旧 `unionc.db` 中的 Sunshine/Host 表只供离线导入、核验和 rollback evidence；当前 Core 请求
  路径不得访问。切换后禁止双写。
- Photo 服务端内容保持明文；HTTPS 只保护传输。磁盘/备份加密由部署层负责。

## 安全边界

- 外部只能连接 Union；worker loopback bind/port 是 Runtime 内部细节。
- `platform` Manifest 路由由 Core 会话/RBAC/CSRF 授权；`module` 路由由 Agent、移动端、ACL 或
  媒体等领域凭据授权，但同样必须通过 Union。
- gateway token 是 Core→worker 的进程 capability，不是用户、设备或管理员身份。
- Agent 零入站端口，服务端不执行远程命令、脚本、文件传输或 Agent 自更新。
- Photo 手机客户端与 Agent 是由 Builder Release 集中发布的远端 companion，不进入
  服务器模块包或进程树。
- 模块配置不能改变 worker executable、bind、API base、audience 或包字节。

## 非目标

动态代码市场、运行时安装/热替换、模块独立公网部署/Release、多 Server active-active、多租户隔离、
跨模块事务、自动接管任意旧数据库、应用层端到端照片加密和内置 PostgreSQL/MediaMTX 运维，均不在
v0.5 承诺范围内。平台 RBAC 是当前公共能力，不属于非目标。

## 完成判定

不能用“代码已写”代替验收。发布候选至少应记录：

- 四个官方 profile 的 `check`、`plan`、`build` 和 `verify`，以及锁定的完整 revision；
- 未纳入模块在 `modules/`、release manifest、catalog、路由和资源中均缺席；
- 纳入但 disabled/unconfigured 的模块不会启动或暴露 API/Frontend，配置后可启停；
- 五个 worker 的 gateway 拒绝、健康回显、崩溃退避和优雅关机测试；
- 四个专用 PostgreSQL database、Core/Dufs SQLite、模块 migration、离线 import/verify 和备份恢复；
- Photo HTTPS 传输与服务器端明文读取验证；
- 两个完整发行间 install/rollback、Manifest/摘要篡改拒绝和数据兼容判断；
- 目标 Linux、PostgreSQL、反代、文件系统及 companion 版本矩阵。

本文定义要求，不声称当前机器或任何未发布候选已经完成上述生产验收。
