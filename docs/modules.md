# 编译期模块与私有进程

Union 使用“构建期选能力、运行时隔离故障”的模型：Cargo feature 决定发行包中有哪些 worker，
Union supervisor 在运行时把每个已选模块作为私有子进程启动。没有动态插件目录、管理台安装
按钮或运行时下载。

## 固定编译图

| Cargo feature | worker binary | 回环端口 | gateway identity prefix | liveness / readiness |
|---|---|---:|---|---|
| `module-sentinel-monitor` | `sentinel-monitor` | 18101 | `/modules/sentinel-monitor` | `/health/live` / `/health/ready` |
| `module-photo-backup` | `photo-backup` | 18102 | `/modules/photo-backup` | `/health/live` / `/health/ready` |
| `module-dufs` | `dufs` | 18103 | `/modules/dufs` | `/__dufs__/health` / `/__dufs__/ready` |
| `module-sunshine` | `sunshine` | 18104 | `/modules/sunshine` | `/health/live` / `/health/ready` |
| `module-host-monitoring` | `host-monitoring` | 18105 | `/modules/host-monitoring` | `/health/live` / `/health/ready` |

正式构建必须使用 `--no-default-features` 再显式传入 profile 中的 feature 集。没有选择的模块
不会进入 catalog、路由、supervisor 或发行目录；默认 feature 只方便仓库开发。

Sunshine/Host 的浏览器与 Agent 兼容路由仍固定在
`/api/services/sunshine/*`、`/api/monitoring/*`、`/api/agent/*`。identity prefix 用于
证明 worker 实例和资源基址，不意味着绕过 Union 再公开一组 worker URL。

## gateway-v1

Union 为每个 worker 启动生成独立的 64 位十六进制 token（约 244-bit 随机熵），并通过
清空后的环境传递：

- `UNION_MODULE_PROTOCOL=gateway-v1`
- `UNION_MODULE_AUDIENCE=<固定模块 id>`
- `UNION_MODULE_TOKEN=<本次进程生命周期 token>`
- `UNION_MODULE_PREFIX=<固定公共前缀>`

Union 覆盖这些请求头，worker 在所有网关入口验证四项，并在健康响应回显 protocol、audience
和 prefix。token 不落盘、不跨重启复用，也不能替代产品层登录、Agent 凭据或模块域认证。
Union 会移除来自公网的伪造内部头、hop-by-hop 头和不应跨边界的 Cookie。

不再支持 `SARMG_SENTINEL_URL`、`SARMG_PHOTO_BACKUP_URL`、`SARMG_DUFS_URL` 或任何
管理员指定的模块 URL。可执行文件固定从同一不可变发行目录的
`libexec/union/modules/<id>` 解析，不能经 `PATH` 替换。

## 进程和数据边界

supervisor 负责启动、健康握手、PID/状态、崩溃退避与关机时先 SIGTERM 后强制终止。worker
不注册 systemd unit、不单独面向公网、不拥有独立 Release。

| 模块 | 数据边界 |
|---|---|
| Sunshine | PostgreSQL schema `sunshine`；凭据用模块专属 key 加密 |
| Host monitoring | PostgreSQL schema `host_monitoring`；Agent 配对、最新报告和历史均归它所有 |
| Sentinel | 专用 PostgreSQL database/role；MediaMTX 是其受约束的运行伴随依赖 |
| Photo | 专用 PostgreSQL database/role；媒体内容保存在模块数据目录且服务端明文 |
| Dufs | 模块数据目录中的 SQLite 与文件根；这是刻意保留的例外 |

Sunshine/Host 可以位于同一个 PostgreSQL database，但必须使用各自 role/schema/migration。
Sentinel/Photo 分别使用专用 database/role。四者都具有独立 migration history 和备份恢复
单元；禁止跨模块表、外键和事务。

## 官方 profiles

| profile | 模块 |
|---|---|
| `minimal` | 无业务 worker，仅 Union 核心 |
| `storage` | Photo + Dufs |
| `monitoring` | Sentinel + Host monitoring |
| `full` | 五个模块 |

profile 由 `union-builder` v1.0.0 固定完整 Git revision。模块配置只能补充密钥、数据库连接和
模块业务参数，不能改变 binary、bind、端口、prefix 或网关 audience。

## 远端 companion

Agent 和 Photo 手机客户端在被管理设备上运行，因此不属于上述服务端模块图。它们随 Union
Release 的兼容矩阵发布/记录；supervisor 从不启动、更新或卸载它们。Agent 仍通过专属设备
凭据访问固定 Host worker 路由，不能使用 gateway token。

## 明确非目标

- 运行时安装、卸载、下载或热加载模块；
- 将 worker 当作独立产品暴露、部署或发布；
- 共享数据库 schema、共享 session 表或共享管理员 Cookie；
- 任意上游 URL、任意 shell 构建命令或第三方二进制注入；
- 将 Dufs/Photo 强行合并。二者只共享 blob-transfer 契约和通用错误语义，文件浏览与照片
  资产管理仍是不同领域。
