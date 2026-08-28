# Union

Union 是这一组项目唯一面向用户的服务端产品、公共入口和 Release。五个服务端业务模块由
Cargo feature 在构建期选择，运行时由 Union supervisor 启动为只监听回环地址的私有 worker；
它们不是可单独部署或发布的公共服务。

| feature | 私有 worker | 固定地址 | gateway identity prefix | 数据所有权 |
|---|---|---|---|---|
| `module-sunshine` | `sunshine` | `127.0.0.1:18104` | `/modules/sunshine` | PostgreSQL schema `sunshine` |
| `module-host-monitoring` | `host-monitoring` | `127.0.0.1:18105` | `/modules/host-monitoring` | PostgreSQL schema `host_monitoring` |
| `module-sentinel-monitor` | `sentinel-monitor` | `127.0.0.1:18101` | `/modules/sentinel-monitor` | 专用 PostgreSQL database/role |
| `module-photo-backup` | `photo-backup` | `127.0.0.1:18102` | `/modules/photo-backup` | 专用 PostgreSQL database/role + 明文媒体目录 |
| `module-dufs` | `dufs` | `127.0.0.1:18103` | `/modules/dufs` | 模块私有 SQLite + 文件根 |

模块进程只接受 Union 每次启动生成的 `gateway-v1` 身份头。地址、端口、前缀、binary 和健康
端点都编译在 Union 中；`SARMG_*_URL` 动态上游已被拒绝。模块崩溃会被 supervisor 退避重启，
健康契约不兼容时不会被宣布就绪。

Sunshine 与 Host 为保持 Web/Agent 线协议兼容，对外仍使用固定的
`/api/services/sunshine/*`、`/api/monitoring/*` 和 `/api/agent/*`；表中的 prefix 是
worker 必须验证的内部身份，不是另一组公开入口。

## 仓库组成

| 目录 | 职责 |
|---|---|
| `server` | Union 网关、认证、catalog、supervisor 与核心系统能力（Linux） |
| `sunshine-worker` | 从核心拆出的 Sunshine 私有 worker |
| `host-monitoring-worker` | 从核心拆出的主机监控私有 worker |
| `agent` | 安装在远端主机的采集 companion，不由 supervisor 启动 |
| `protocol` | Host worker 与 Agent 共用的版本化 DTO |
| `web` | 随 Union 构建的管理前端 |

Union 核心仍使用自己的 `unionc.db` 保存核心审计等平台状态；运行时不再承载 Sunshine 或
Host 业务域。0.4.0 schema 中可物理保留旧域表作为只读迁移/回滚证据，关闭回滚窗口后再由
后续 migration 删除。这里的核心 SQLite 与 Dufs 的模块 SQLite 是两份彼此独立的数据库。

Photo 手机客户端和跨平台 Agent 是远端 companion：它们随 Union 兼容矩阵版本化，但不是
服务端编译模块，也不由 supervisor 启动。模块仓库不发布可独立运行的程序。

## 构建与安装

正式组合只使用
[`union-builder` v1.0.0](https://github.com/isarmg/union-builder/releases/tag/v1.0.0)。
官方 profile 是 `minimal`、`storage`、`monitoring` 和 `full`；Builder 固定源码 revision、
执行锁定依赖构建、生成完整校验清单，并提供 `verify`、`stage`、`install` 和 `rollback`。

```bash
union-builder check --config profiles/full.toml
union-builder plan --config profiles/full.toml
union-builder build --config profiles/full.toml --profile release
union-builder verify --release dist/full
sudo union-builder install --release dist/full --root /opt/union
sudo union-builder rollback --root /opt/union
```

这组命令描述受支持的发行路径，不代表当前工作树已经完成生产环境验收。开发者仍可分别运行
crate 测试；不能把开发启动方式当作模块独立部署承诺。

当前 `v0.4.0` 是正式的架构/构建里程碑 Release，**不是 production-ready 资格声明**。真实
PostgreSQL/SQLite/文件系统迁移、网关/媒体运行检查、生产服务升级以及服务/数据回滚仍须在部署
前逐项验收。

## 关键边界

- Union 是唯一公网监听者；worker 必须保持回环绑定，前置反代只指向 Union。
- Sunshine/Host 各自拥有 PostgreSQL schema；Sentinel/Photo 各自拥有专用 PostgreSQL
  database/role。禁止跨 owner 外键、查询和共享迁移。Dufs 的 SQLite 是有意保留的例外。
- Photo 只要求传输链路使用 HTTPS；服务器保存的原始文件、缩略图和派生物保持未加密，
  磁盘加密和备份加密属于部署者责任。
- 旧版 `unionc.db` 中的 Sunshine/Host 域表只允许被离线导入器读取，并作为导入核验、回滚
  证据源；新架构不会在线双写，也不会把旧库重新接回运行路径。
- 服务端不向 Agent 下发命令、脚本、文件或自更新；多租户、插件热装载和任意自定义 worker
  也不在范围内。

完整需求、部署和迁移边界从[文档中心](docs/README.md)开始阅读。许可证为
[Apache License 2.0](LICENSE-APACHE)。
