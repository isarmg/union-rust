# 08. SQLite、生命周期与一致性

UnionC 不依赖外部数据库服务。Server 二进制内嵌 SQLite，活动数据库固定在数据目录的 `unionc.db`。简单部署不等于简单数据语义：配对、幂等、乱序与备份都依赖严谨事务。

## 1. 数据目录

未设置时，程序会回退到当前工作目录下的 `unionc/data`。生产部署必须按运维规范显式固定 `UNIONC_DATA_DIR`，不能把依赖工作目录的回退当作安全默认值；包安装的 systemd unit 使用 `/var/lib/unionc`。

```text
<data-dir>/
├── unionc.db                         业务、遥测、审计
├── unionc.db-wal / unionc.db-shm     SQLite 运行期 sidecar，可能存在
├── unionc-config.json                管理员用户名、bcrypt hash、应用版本
├── unionc.secret                     仅开发模式可自动生成的 AES 主密钥
├── .unionc-server.lock               单 Server advisory lock，可能存在
├── .unionc-maintenance.lock          维护命令串行锁，可能存在
└── unionc.pre-restore-*              restore 产生的回退点或取证文件，可能存在
```

生产主密钥来自环境，不应依赖开发密钥文件。

活动数据库只能在 Server 本机磁盘：不支持 NFS/SMB，不支持多个 Server 共享写，也不支持水平扩展副本共同处理写入。

## 2. 初始化与严格 schema

空数据库会在一个写事务中：

1. 建立 `schema_metadata`；
2. 执行二进制内嵌的 `server/schema/sqlite.sql`；
3. 记录当前 schema 版本、应用版本和基线 checksum；
4. 提交后才允许 HTTP 监听。

再次打开已有数据库时，Server 会构造一份内存参考库逐项比较；版本、checksum、表、索引、视图、触发器都必须与当前版本精确一致，不能多也不能少。

仓库虽然保留了 `server/migrations/` 目录骨架（其中没有 SQL migration 文件），当前运行时没有按版本逐步执行的 migration 链，也不会把“看起来像旧表”的数据库升级成当前格式。支持：

- 空目录全新建库；
- 正常重新打开与当前版本精确匹配的活动库；
- 当前同版本工具生成并通过清单校验的恢复。

## 3. SQLite 运行特性

数据库连接会启用：

- foreign keys；
- WAL；
- `synchronous=FULL`；
- 30 秒 busy timeout；
- 当前 schema 的业务表使用 `STRICT` 建表；
- JSON1/当前内嵌 SQLite 能力；
- 私有 0600 数据库文件权限。

WAL 允许读者与一个写者并行，但 SQLite 同一时刻仍只有一个写事务。Server 还使用进程内写门控和 `BEGIN IMMEDIATE`，让写请求在进入事务时就按顺序取得写资格，避免“先读后升级写锁”的竞争死锁。

## 4. 表与关系

```text
schema_metadata

external_hosts                         audit_logs
  Sunshine 配置                         状态变更审计

agent_instance_invites
          │ 绑定
          ▼
agent_pairing_requests
          │ 激活生成/替换
          ▼
monitored_hosts ◄──── agent_credentials
      │ 1                    多个历史 credential
      │
      └────< agent_metric_reports
              多份历史；latest 指针回到其中一份
```

### 4.1 `schema_metadata`

只保存当前 schema 的唯一元数据行，用于拒绝旧版本、篡改或部分 schema。

### 4.2 `audit_logs`

记录 action、target、安全 detail、actor、可信 request ID 和微秒时间。审计不是调试日志：它关注谁改变了什么状态，而不是每个读取请求。

### 4.3 `external_hosts`

当前只允许 `kind='sunshine'`。保存主机地址、普通 JSON 配置、排序位置和 AES-GCM 密文 secret。

### 4.4 `monitored_hosts`

稳定 Agent 实例：身份、capabilities、注册/最后出现时间、latest 报告指针、latest interval、`active/revoked` 生命周期。

撤销不会删除这行，因为历史、审计和重新配对都需要稳定 tombstone。

### 4.5 `agent_metric_reports`

每份报告一行：host、采样/接收时间、间隔、九项摘要、可空完整 payload。`report_id` 是全局主键，外键连接 host。

### 4.6 `agent_credentials`

保存 credential ID、host ID、Agent secret 的 SHA-256、签发/撤销时间，并预留 `last_used_at`。当前报告认证路径尚未更新 `last_used_at`；有效性判断依赖 `revoked_at` 与主机生命周期。重新配对时旧 credential 被撤销，历史记录保留。

### 4.7 `agent_instance_invites`

管理员邀请：预留 instance ID、activation code hash、展示名、状态、过期/激活/撤销时间。部分唯一索引保证一个 instance 同时最多一个 pending 邀请。

### 4.8 `agent_pairing_requests`

Agent 请求：临时 host 摘要、token hash、polling hash、状态、邀请关联、最终 instance 和到期时间。

## 5. 时间如何保存

SQLite 表中的时间统一保存 Unix epoch 微秒整数；Rust 边界使用 UTC `DateTime`；JSON 使用标准 UTC 文本。

为什么不是秒？同一秒内可能有多份补传或并发请求，秒精度不足以稳定排序与保留原始采样时刻。为什么不直接存随意字符串？整数比较、索引和范围查询更直接，也避免多种时区格式。

## 6. 激活事务

一次授权激活必须原子完成：

```text
BEGIN IMMEDIATE
  1. 查 pairing request，确认 pending 且未过期
  2. 按 activation hash 查 invite，确认 pending 且未过期
  3. 防止 code 被别的 request 抢占
  4. 新实例时创建 monitored_hosts；重配时沿用原实例
  5. 撤销该实例旧 credential
  6. 插入新 token hash
  7. invite → active
  8. pairing request → active + instance_id
  9. 写审计
COMMIT
```

任何一步失败，全部回滚。不能出现“邀请已用，但 credential 没写”或“credential 已生效，配对状态仍 waiting”。

相同 request/code 的响应丢失重试应返回同一成功结果；另一个 request 使用相同 code 必须冲突。

## 7. 报告写事务

`store_monitoring_report` 的核心决策：

1. 规范化 host/report UUID；
2. 取得当前 latest ID、时间和“身份是否真正变化”；
3. 用采样时间与 report ID 判断是否成为 latest；
4. 统一计算九项摘要；
5. `INSERT ... ON CONFLICT DO NOTHING RETURNING`；
6. 重复 ID 查询首次接收时间并幂等返回；
7. 仅新报告且成为 latest 时更新 host；
8. 将上一 latest 的 payload 置空；
9. 提交。

单个 host 行在一次有效报告中最多执行一次条件 UPDATE。身份和 capability 未变化时不重写大 JSON，减少 WAL 与单写者锁占用。

## 8. 为什么完整 payload 只留最新

每个历史点真正用于曲线的只有摘要数值。若每 10 秒保存 10–20 KiB 完整 JSON，长期容量会迅速膨胀。

当前策略：

| 数据 | 每个时间点 | 每台主机最新 |
|---|---:|---:|
| 采样/接收时间 | 保存 | 保存 |
| 九项摘要 | 保存 | 保存 |
| 完整 CPU 核、网卡、磁盘、温度、GPU、capability JSON | 不保存 | 保存一份 |

代价是不能回看任意历史时刻的完整设备明细。若未来确有需求，应设计低频完整快照或专门表，而不是无界保留每份大 JSON。

## 9. latest 与乱序

报告是否成为当前状态，依据 `collected_at`，而不是网络到达时间：

- 更晚采样成为 latest；
- 相同采样时间以 report ID 字典序稳定决胜；
- 更早采样只进历史；
- 幂等重放既不进历史也不刷新心跳。

`last_seen_at` 只在新插入且成为 latest 时更新，并用数据库中的旧值取最大值保持单调。因此
补传一小时前的报告不会让已经离线的主机突然显示 online，等待写锁的请求或系统时钟回拨也
不能把已观察到的在线时间改回更早。

## 10. 主机生命周期

```text
邀请：pending ──激活──> active
        ├─到期（响应计算为 expired）
        └─取消──> revoked/cancelled 展示

配对：pending/waiting ──激活──> active
         ├─到期──> expired 展示
         └─拒绝──> denied

实例：active ──管理员撤销──> revoked
        revoked ──新邀请 + 新配对──> active
```

重新配对保留 instance 和历史；Server 撤销旧 credential，并激活 Agent 预先生成的新 secret 所对应的哈希。Server 不生成或回传 Agent secret。硬删除主机与撤销身份语义不同，当前管理台不提供按主机硬删除。

## 11. 数据保留期

默认：审计 90 天，遥测 30 天。启动后先清一次，之后约每 24 小时。遥测截止时间按 Server 接收时间 `received_at` 计算，不按 Agent 采样时间 `collected_at`；因此离线补传的旧采样不会刚入库就因采样时间较旧而被删除。审计则按 `audit_logs.created_at` 计算。

- 遥测每批最多 10,000 行，批间让出约 50 ms；
- 审计每批最多 1,000 行；
- 每批独立短事务；
- 每台主机 latest 报告有保留例外；
- 删除释放 SQLite 页面供复用，但不承诺主文件立即缩小。

不要对运行库执行外部 `VACUUM`，不要手工复制或删除 WAL/SHM。在线一致性整理应使用内置 backup。

## 12. 备份与恢复

### 备份

```bash
UNIONC_DATA_DIR="$UNIONC_TUTORIAL_SERVER_DATA" \
cargo run -p unionc -- backup --output /tmp/unionc-tutorial-backup.db
```

维护命令必须加载与运行 Server 相同的数据目录和密钥环境。生产包场景还要加载 systemd 服务使用的当前/历史密钥，不能在另一个工作目录裸跑命令。

`backup` 不覆盖已存在的输出文件，也不覆盖已存在的同名 manifest。重复练习时请换一个新文件名；生产使用唯一 UTC 时间戳，并始终成对移动或删除 DB/manifest。

它使用 SQLite 一致性快照机制并生成配套：

```text
unionc-tutorial-backup.db
unionc-tutorial-backup.db.manifest.json
```

manifest 包含应用版本、schema、key ID 和快照 SHA-256。两者必须成对复制、校验和保留。

### 完整灾备集

数据库快照还不够。必须同时安全保管：

- `unionc-config.json`；
- 生产环境的当前 `UNIONC_SECRET_KEY`；
- 生产轮换期仍需读取密文的历史密钥；
- 开发环境若使用自动生成密钥，则包括数据目录中的 `unionc.secret`；
- 数据库与 manifest。

没有主密钥，Sunshine password 密文永久不可解；没有管理员配置，账号 hash 无法恢复。

### 恢复

恢复会替换活动库，必须停止 Server。`--force` 只允许替换已存在目标，不跳过 checksum、版本、schema、外键、完整性与密文可解性校验。

替换前的活动库若健康，命令会留下可再次交给 `restore` 的 `unionc.pre-restore-*.db` 与 manifest。若旧库已损坏且没有 WAL/SHM，可能只留下无 manifest、不能直接 `restore` 的 `unverified` 取证副本，然后继续替换；若损坏库仍有 WAL/SHM，命令会保留 main/WAL/SHM 完整文件族并拒绝替换，避免丢掉未 checkpoint 页。这些恢复点/取证文件不会自动清理；确认演练成功且已有异机备份后，再按类型成对或成组清理。

恢复只接受当前同版本快照。旧版本数据需在旧系统导出中立格式、全新部署并重新配对；当前项目不提供导入桥。

## 13. 不要直接改生产数据库

手工 SQL 可能绕过：

- Rust 语义校验；
- 配对/撤销事务；
- 审计；
- latest/payload 不变量；
- 加密格式与 key ID；
- 内存快照刷新。

调试应先使用 API、测试临时库、内置 integrity-check 和备份副本。即使安装了 `sqlite3` CLI，也不要在运行中的生产库试验写语句。

## 14. 相关测试

```bash
cargo test -p unionc --test database_schema
cargo test -p unionc --test report_ordering
cargo test -p unionc --test latest_report_retention
cargo test -p unionc --test history_query
cargo test -p unionc --test sqlite_maintenance
cargo test -p unionc --test host_row_write_amplification
```

这些测试分别守护当前 schema、乱序/幂等、最新 payload、历史热路径、备份恢复和写放大。

## 15. 本章自检

1. 为什么 `schema_metadata` 不在普通业务基线里仍不可缺少？
2. SQLite WAL 是否意味着可以有多个写者？
3. 激活为什么必须是一个事务？
4. latest 依据采样时间还是到达时间？
5. 删除超期历史后文件为何不一定立即缩小？
6. 一份可恢复的灾备集为什么不能只有 `.db`？

下一章：[09. 认证、安全与信任边界](09-认证安全与信任边界.md)。
