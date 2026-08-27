# Union 项目手册

本页是架构总览；细节由 [文档中心](docs/README.md)索引。

Union 是唯一公共服务端产品与 Release。五个 Cargo feature 在构建期决定发行图；每个已选
模块在运行时都是由 supervisor 管理的回环 worker。固定映射如下：

| 模块 | feature | port | prefix | store |
|---|---|---:|---|---|
| Sentinel | `module-sentinel-monitor` | 18101 | `/modules/sentinel-monitor` | 专用 PostgreSQL database/role |
| Photo | `module-photo-backup` | 18102 | `/modules/photo-backup` | 专用 PostgreSQL database/role + 明文内容 |
| Dufs | `module-dufs` | 18103 | `/modules/dufs` | SQLite + 文件根 |
| Sunshine | `module-sunshine` | 18104 | `/modules/sunshine` | PostgreSQL `sunshine` |
| Host | `module-host-monitoring` | 18105 | `/modules/host-monitoring` | PostgreSQL `host_monitoring` |

所有公网请求先进入 Union。Union 清洗内部头并注入进程生命周期的 `gateway-v1` identity；
worker 验证 protocol、audience、token、prefix 并在健康响应回显。动态 `SARMG_*_URL`、
独立 worker systemd 和模块独立 Release 均不支持。

Sunshine 和 Host 已从核心源码拆为同仓 worker；Union 核心运行时只读写审计等平台状态。
0.4.0 schema 可继续物理保留旧域表，但它们仅供离线 import/verify/rollback evidence，在线
route 和 repository 不再使用。Sentinel、Photo 分别拥有专用 PostgreSQL database/role；
Dufs SQLite 是明确
例外。数据库可以共用 cluster，但不能共享 schema、role、migration、业务表、外键或事务。

远端 Agent 和 Photo 手机客户端是 companion，不是服务器模块。它们随 Union compatibility
matrix 管理，supervisor 不启动它们。Agent 保持零入站和无远程命令边界。

正式组合、前端构建、校验、安装与 rollback 只使用 `union-builder` v1.0.0 和
`minimal/storage/monitoring/full` 官方 profile。完整 release 是不可分割的安装/回滚单元；
数据库兼容与数据回滚必须另行核验。

Photo 只保证 HTTPS 传输；服务器文件保持明文以支持哈希、去重、媒体处理和 Range 下载。
静态磁盘与备份加密属于部署责任。

本手册定义产品边界，不声称当前候选已经完成生产验收。生产验收要求、非目标和证据清单见
[需求、能力与边界](docs/reference/capabilities.md)。
