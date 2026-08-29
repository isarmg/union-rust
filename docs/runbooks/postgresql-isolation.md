# PostgreSQL 模块数据库隔离运行手册

本手册适用于 Union 发行内的 Sunshine、Host Monitoring、Sentinel Monitor 和 Photo Backup。
四者可以使用同一 PostgreSQL cluster，但必须拥有不同 database、NOLOGIN owner、LOGIN runtime
role、凭据、migration history 和备份恢复单元。Core 与 Dufs 不使用这些库：Core 使用自己的
SQLite，Dufs 使用模块专属 SQLite 与 rooted filesystem。

## 固定边界

| 模块 ID | Database | NOLOGIN owner | LOGIN runtime | Migration schema | Owner membership |
|---|---|---|---|---|---|
| `sunshine` | `union_sunshine` | `union_sunshine_owner` | `union_sunshine_runtime` | `sunshine`，runtime owner | 否 |
| `host-monitoring` | `union_host_monitoring` | `union_host_monitoring_owner` | `union_host_monitoring_runtime` | `host_monitoring`，runtime owner | 否 |
| `sentinel-monitor` | `union_sentinel_monitor` | `union_sentinel_monitor_owner` | `union_sentinel_monitor_runtime` | `public`，database owner | 是 |
| `photo-backup` | `union_photo_backup` | `union_photo_backup_owner` | `union_photo_backup_runtime` | `public`，database owner | 是 |

每个 database 都撤销 `PUBLIC` 的 `CONNECT`、`CREATE` 和 `TEMPORARY`，只向对应 runtime
授予 `CONNECT`。命名 schema 和 `public` 都撤销 `PUBLIC` 权限；Sunshine/Host runtime 不能使用
`public`，Sentinel/Photo runtime 只能通过其本库 NOLOGIN owner 的成员关系使用本库 `public`。
禁止模块角色加入其他模块 owner、连接其他模块 database，或建立跨库 FDW/dblink。

## Provision

使用受控的 cluster superuser 连接和仓库模板
[provision-module.sql](../examples/postgresql/provision-module.sql)。模板不会创建、删除本机
数据库，除非管理员明确执行；它不会接收或记录密码，新 LOGIN 初始为 `PASSWORD NULL`。

以下命令分别执行，且不得包在一个事务中（PostgreSQL 的 `CREATE DATABASE` 不允许这样做）：

```bash
psql "$CLUSTER_ADMIN_DATABASE_URL" -v module_database=union_sunshine \
  -v module_owner=union_sunshine_owner -v module_runtime=union_sunshine_runtime \
  -v module_schema=sunshine -v schema_owner=union_sunshine_runtime \
  -v runtime_inherits_owner=false -f docs/examples/postgresql/provision-module.sql

psql "$CLUSTER_ADMIN_DATABASE_URL" -v module_database=union_host_monitoring \
  -v module_owner=union_host_monitoring_owner -v module_runtime=union_host_monitoring_runtime \
  -v module_schema=host_monitoring -v schema_owner=union_host_monitoring_runtime \
  -v runtime_inherits_owner=false -f docs/examples/postgresql/provision-module.sql

psql "$CLUSTER_ADMIN_DATABASE_URL" -v module_database=union_sentinel_monitor \
  -v module_owner=union_sentinel_monitor_owner -v module_runtime=union_sentinel_monitor_runtime \
  -v module_schema=public -v schema_owner=union_sentinel_monitor_owner \
  -v runtime_inherits_owner=true -f docs/examples/postgresql/provision-module.sql

psql "$CLUSTER_ADMIN_DATABASE_URL" -v module_database=union_photo_backup \
  -v module_owner=union_photo_backup_owner -v module_runtime=union_photo_backup_runtime \
  -v module_schema=public -v schema_owner=union_photo_backup_owner \
  -v runtime_inherits_owner=true -f docs/examples/postgresql/provision-module.sql
```

`CLUSTER_ADMIN_DATABASE_URL` 必须来自部署 secret 注入或受限 `PGPASSFILE`，不能提交到仓库。
随后在受信任的交互式 `psql` 会话中逐个运行 `\password <runtime-role>`；该命令使用隐藏输入，
不会把新密码写入 SQL、shell history 或进程参数。要求 cluster 使用 SCRAM，四个密码必须随机且
互不相同。owner 始终保持 NOLOGIN。

把各 runtime URL 写入 Union Web“设置 → 模块管理”或
`PUT /api/platform/modules/<id>/configuration` 的完整模块 JSON，例如：

```text
postgresql://union_sunshine_runtime:<URL-encoded-secret>@<db-host>/union_sunshine
```

具体 TLS 参数、CA 路径和其他字段以模块 `config/schema.json` 与部署网络为准。不要把 URL 放入
Core environment、systemd unit 或模块包；配置中心只生成当前 Schema 的私有 JSON，并通过标准
`UNION_PLUGIN_CONFIG` 路径交给目标进程。
配置 GET 返回的 `***` 不能作为 PUT 值，修改时必须从 secret manager 提供完整新值。

## 只读验收

使用对应 runtime 凭据连接每个库，并分别运行
[verify-module-isolation.sql](../examples/postgresql/verify-module-isolation.sql)。凭据应由
`PGPASSFILE` 或 secret provider 提供；以下命令的连接串故意不含密码：

```bash
psql "postgresql://union_sunshine_runtime@<db-host>/union_sunshine" \
  -v expected_database=union_sunshine -v expected_owner=union_sunshine_owner \
  -v expected_runtime=union_sunshine_runtime -v expected_schema=sunshine \
  -v runtime_inherits_owner=false -f docs/examples/postgresql/verify-module-isolation.sql

psql "postgresql://union_host_monitoring_runtime@<db-host>/union_host_monitoring" \
  -v expected_database=union_host_monitoring -v expected_owner=union_host_monitoring_owner \
  -v expected_runtime=union_host_monitoring_runtime -v expected_schema=host_monitoring \
  -v runtime_inherits_owner=false -f docs/examples/postgresql/verify-module-isolation.sql

psql "postgresql://union_sentinel_monitor_runtime@<db-host>/union_sentinel_monitor" \
  -v expected_database=union_sentinel_monitor -v expected_owner=union_sentinel_monitor_owner \
  -v expected_runtime=union_sentinel_monitor_runtime -v expected_schema=public \
  -v runtime_inherits_owner=true -f docs/examples/postgresql/verify-module-isolation.sql

psql "postgresql://union_photo_backup_runtime@<db-host>/union_photo_backup" \
  -v expected_database=union_photo_backup -v expected_owner=union_photo_backup_owner \
  -v expected_runtime=union_photo_backup_runtime -v expected_schema=public \
  -v runtime_inherits_owner=true -f docs/examples/postgresql/verify-module-isolation.sql
```

脚本只查询 catalog/ACL，不创建对象；任一数据库名、owner、role attribute、schema owner、PUBLIC
ACL、owner membership 或跨库 `CONNECT` 检查失败都会以非零状态结束。四次 PASS 才构成隔离
验收；它不代替模块 migration/readiness 和真实备份恢复演练。

## 备份与恢复单元

- Sunshine：`union_sunshine` 的一致性 `pg_dump`，不与其他数据库合并。
- Host：`union_host_monitoring` 的一致性 `pg_dump`。
- Sentinel：`union_sentinel_monitor` dump 与其声明的 companion 配置按同一恢复点记录。
- Photo：`union_photo_backup` dump、原始媒体、缩略图和派生目录必须记录同一一致性点；服务器端
  文件仍是明文，备份加密属于部署层责任。
- Core SQLite、Dufs SQLite/rooted filesystem 各自备份，不能用 PostgreSQL dump 替代。

恢复到新建的专用 database：先用本模板重建角色、ACL 和 schema，再恢复该模块唯一 dump，运行
模块 migration/verify/readiness，最后运行只读隔离脚本。不要把四库恢复到一个 database，也不要
用跨库视图、外键、事务或共享 migration ledger 拼接恢复。Builder slot 回滚不会回滚数据库。

## 凭据轮换

1. 在 Union 中 disable 目标模块，确认其私有进程退出；其他三个模块无需停止。
2. 在受信任 `psql` 会话执行 `\password <target-runtime-role>`，把新秘密写入 secret manager。
3. 对目标模块配置执行完整 PUT，替换 URL 中的全部凭据；不要提交 `***`。
4. enable 模块，检查 migration、readiness 和网关，再以新凭据执行隔离脚本。
5. 删除旧秘密并记录审计证据。失败时只回退该模块的凭据和配置，不改其他数据库。

每次轮换只处理一个 LOGIN role。禁止复用四者密码、给 runtime 增加 superuser/createdb/createrole/
bypassrls，或为了应急重新授予 `PUBLIC CONNECT`。
