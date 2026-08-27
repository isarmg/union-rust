# Union 服务端

Union 服务端仅支持 Linux。它是唯一公网产品：前置 TLS 反代只连接 Union 的回环监听，所有
已编译业务模块均由内置 supervisor 启动为私有 worker。

## 启动布局

受支持的 Builder 安装布局是：

```text
<install-root>/
├── current -> releases/<release-id>
├── previous -> releases/<release-id>       # 有上一版本时
└── releases/<release-id>/
    ├── bin/unionc
    ├── libexec/union/modules/<id>
    ├── share/union/web/
    ├── share/union/modules/<id>/
    ├── union-release.json
    └── SHA256SUMS
```

Union 从自身可执行文件推导同一发行根，拒绝符号链接、不可信权限、非普通文件、多硬链接或
发行根之外的 worker。不要单独替换 worker 或前端；完整 release 是校验与回滚的最小单位。

## 环境变量

核心配置：

- `UNIONC_ENV=production`
- `UNIONC_DATA_DIR`：Union 核心私有数据和各模块数据目录的绝对路径
- `UNIONC_SECRET_KEY`：核心 32-byte Base64 key
- `UNIONC_PROXY_SECRET`：外层可信 TLS 反代证明，64 位小写 hex
- 首次核心初始化临时使用 `UNIONC_ALLOW_BOOTSTRAP=1` 和
  `UNIONC_BOOTSTRAP_PASSWORD`
- 可选 `UNIONC_SERVER_BIND`、`UNIONC_SERVER_PORT`、`UNIONC_RETENTION_DAYS`

已编译模块的配置由 supervisor 从 `UNIONC_*` 读取并映射到清空后的 worker 环境。缺少该
profile 所需必填项时启动失败；未编译模块的配置不会启用它。

| 模块 | 必填 Union 环境变量 |
|---|---|
| Sunshine | `UNIONC_SUNSHINE_DATABASE_URL`, `UNIONC_SUNSHINE_CREDENTIAL_KEY` |
| Host | `UNIONC_HOST_MONITORING_DATABASE_URL` |
| Sentinel | `UNIONC_SENTINEL_DATABASE_URL`, `UNIONC_SENTINEL_APP_JWT_SECRET`, `UNIONC_SENTINEL_CREDENTIALS_KEY` |
| Photo | `UNIONC_PHOTO_BACKUP_DATABASE_URL`, `UNIONC_PHOTO_BACKUP_ADMIN_USERNAME`, `UNIONC_PHOTO_BACKUP_ADMIN_PASSWORD` |
| Dufs | `<data>/modules/dufs/dufs.yaml`（0600、普通文件、受服务 UID 所有） |

完整模板见 [`server/packaging/linux/unionc.env.example`](../server/packaging/linux/unionc.env.example)。
`SARMG_*_URL` 已废止；只要设置就会 fail closed。

## 数据所有权与迁移

Union 核心运行时只读写审计等核心平台状态，不再在线读写 Sunshine/Host 业务数据。
两者分别由同仓
`sunshine-worker`、`host-monitoring-worker` 各自持有 PostgreSQL schema。Sentinel 和
Photo 各自持有专用 PostgreSQL database/role；Dufs 因本地文件索引语义保留模块私有 SQLite。

迁移时先停止旧版 Sunshine/Host 写入并保存旧 `unionc.db` 的只读快照，再运行 worker
自带的离线 importer、verify 和 rollback-evidence 命令。导入完成后不双写；快照中的旧域表
只作为核验和回滚证据，不能重新挂到新 Union 在线路径。0.4.0 的 SQLite schema 仍可物理保留
这些旧域表，以便离线导入和回滚；没有在线 route、repository 或双写路径可以使用它们。只有
正式迁移验收通过、回滚窗口关闭后，后续 migration 才能删除旧表。rollback 只能恢复切换前
状态，不能假装丢弃切换后已经写入 PostgreSQL 的数据。

核心的 `backup`、`restore`、`integrity-check` 只覆盖 core SQLite；它们不会备份四个
模块的 PostgreSQL、Dufs SQLite/文件根或 Photo 内容。生产备份必须把这些 owner 的一致性点
组合记录，恢复时也分别验证。旧 Sunshine 凭据的解密/重加密只发生在离线 importer 内，不再
提供针对在线 core 数据库的通用 rekey 流程。

## Photo 的加密边界

Photo 上传、下载和 API 经过 Union HTTPS 入口，worker 强制可信网关声明的安全传输语义。
服务器端文件保持未加密，以便校验、去重、Range 下载、缩略图和备份。静态磁盘加密、备份
加密、密钥托管与介质销毁是部署层责任，不属于 Photo 应用层端到端加密。

## 构建、安装和回滚

不要从本仓库的旧 GitHub Actions 或 nFPM 脚本拼装正式发行。唯一受支持入口是
`union-builder` v1.0.0 与官方 profile：

```bash
union-builder build --config profiles/full.toml --profile release
union-builder verify --release dist/full
sudo union-builder stage --release dist/full --root /opt/union
sudo union-builder install --release dist/full --root /opt/union
sudo union-builder rollback --root /opt/union
```

`stage` 只放入已验证的不可变 slot；`install` 原子切换 `current` 并保留 `previous`；`rollback`
重新激活完整上一发行。回滚不迁移数据库，因此执行前必须按各模块 runbook 判断 schema/data
兼容性。本文不声称这些命令已在当前目标主机完成生产验收。

## 健康和故障

Union 只有在 worker 进程存活、固定健康端点可达且回显的 gateway identity 匹配时才把模块
视为 ready。崩溃会退避重启，避免紧密重启循环。关机先发送 SIGTERM，超时后终止。公网调用
永远通过 Union；直接访问 18101–18105 不受支持，即使本机调试可以连通。

## 非目标

多 Server active-active、运行时模块市场、模块独立 systemd、跨 schema JOIN/事务、从管理台
修改 worker 地址、服务端远程执行 Agent 命令，以及承诺任意旧数据库自动升级都不在范围内。
