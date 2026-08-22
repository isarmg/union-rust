# UnionC

> **平台**：服务端仅支持 **Linux**（`lib.rs` 中的 `compile_error!` 固定该约束，CI 只跑 ubuntu）。
> Agent 是跨平台的，CI 覆盖 Linux / Windows / macOS 三个平台。

UnionC 是从 `union` 派生的精简控制台后端。它只保留：

- 本地管理员登录、会话、修改密码和 CSRF 防护；
- 内嵌 SQLite 持久层、加密 Sunshine 凭据和审计日志；
- 系统 CPU、内存、磁盘、网络监控与 SSE 状态推送；
- 跨平台 Agent 的一次性授权配对、指标快照、有限历史和查询 API；
- Sunshine 多主机配置、状态和管理 API 代理。

Proxmox VE、静态博客、文件服务及其路由、模型、进程管理和数据库表均不包含在此项目中。

## 开发运行

在仓库根目录运行：

```bash
./tools/dev-server.sh
```

默认监听 `127.0.0.1:8081`。首次开发启动会在数据目录下生成 `unionc-config.json` 和开发管理员密码。

### 数据目录

管理员配置、内嵌数据库和开发环境 AES 主密钥都存放在**数据目录**里，位置由
`UNIONC_DATA_DIR` 决定：

| 场景 | 数据目录 |
|---|---|
| 设置了 `UNIONC_DATA_DIR` | 该路径（会被规范化为绝对路径） |
| 仓库内开发约定 | `<仓库根>/.runtime/server`（通过环境变量显式设置） |
| 未设置时的程序默认值 | `<当前工作目录>/unionc/data` |

数据库固定为 `<数据目录>/unionc.db`：空目录会按当前唯一 schema 创建；已有文件必须与当前
schema 精确一致，否则启动失败。它只能放在 Server 本机磁盘，
不支持把活动数据库放在 NFS、SMB 或其他网络文件系统，也不提供多 Server 共享写入。

`tools/dev-server.sh` 会解析仓库根目录、创建 `.runtime/server`，导出绝对
`UNIONC_DATA_DIR` 后从根 manifest 启动 Server；即使从其他目录调用也不会改变数据位置。
程序不会自动迁移以前生成的 `unionc/data` 或 `server/unionc/data`；需要保留的数据应先停止
相关进程并按备份/恢复流程处理，不要直接删除数据库、WAL、配置或密钥。跨平台开发命令见
[本地开发运行手册](runbooks/development.md)。

解析结果会在启动日志第一行打印出来。**部署务必显式设置 `UNIONC_DATA_DIR`**——
随包提供的 systemd unit 已设为 `/var/lib/unionc`。依赖工作目录的相对路径意味着从别的
目录启动就读不到配置，而"配置文件不存在"与"首次部署"在代码里无法区分，会被静默地
当成后者、新建一个管理员账号。因此生产环境下"配置文件不存在"默认是**启动失败**，
只有显式设置 `UNIONC_ALLOW_BOOTSTRAP=1` 才允许创建。

### 管理员密码重置

管理员密码只保存 bcrypt 哈希，遗失后无法恢复明文。停止正在运行的 Server，并使用同一个
数据目录执行离线重置：

```bash
sudo systemctl stop unionc
sudo -u unionc env UNIONC_DATA_DIR=/var/lib/unionc /usr/bin/unionc reset-admin-password
sudo systemctl start unionc
```

命令生成一个随机 32 位密码并只输出一次，不连接数据库，也不修改业务数据、Agent 凭据或
主密钥。原配置文件的 UID/GID 与 0600 权限会被保留；进程内旧会话会随重启全部失效。

### 环境变量

生产环境应设置：

- `UNIONC_ENV=production`
- `UNIONC_DATA_DIR`（数据目录的绝对路径）
- `UNIONC_SECRET_KEY`（32 字节密钥的 Base64；生产不允许落盘自动生成）
- `UNIONC_PROXY_SECRET`（独立的 64 位小写十六进制随机值；可信反代覆盖写入同值请求头）
- 首次部署：`UNIONC_ALLOW_BOOTSTRAP=1` + `UNIONC_BOOTSTRAP_PASSWORD`（至少 12 个字符），
  管理员创建完成后应移除这两项
- 可选 `UNIONC_SERVER_BIND`、`UNIONC_SERVER_PORT`（适合容器和测试覆盖）
- 可选 `UNIONC_SECRET_KEY_ID`、`UNIONC_SECRET_KEY_PREVIOUS`、`UNIONC_RETENTION_DAYS`、
  `UNIONC_TELEMETRY_RETENTION_DAYS`（审计/遥测分别默认 90/30 天）

## 保留期与文件回收

历史报文的清理由后台任务按 `UNIONC_TELEMETRY_RETENTION_DAYS` 执行，**分批**删除
(每批 10 000 行，批间让出 50 毫秒）。SQLite 同一时刻只有一个写事务，分批可缩短写锁持有
时间，避免日常清理长时间阻塞 Agent 补传。删除会把页放回数据库空闲列表，但不会保证文件
立刻缩小；一致性 `backup` 生成的快照会重新整理页面。不要对正在运行的数据库执行外部
`VACUUM`，也不要直接复制或删除 `-wal`、`-shm` 文件。

审计日志按 `UNIONC_RETENTION_DAYS` 清理，每批 1 000 行并独立提交，批间主动让出调度；
返回值累计所有批次的精确删除数。它与遥测使用同一写门控，不能退化成一次无上限 DELETE。

## 部署

`server/packaging/` 下提供了 systemd unit 与 nFPM 包定义。打包前须让 `nfpm` 位于 `PATH`，
或用 `NFPM_BIN` 指向可执行文件；发布工作流固定使用 nFPM v2.47.0：

```bash
cargo build --release -p unionc
NFPM_ARCH=amd64 server/packaging/linux/build-packages.sh
```

包会安装二进制到 `/usr/bin/unionc`、unit 到 `/usr/lib/systemd/system/unionc.service`、
环境配置模板到 `/etc/unionc/unionc.env`，并创建 `unionc` 系统用户与 `/var/lib/unionc`（0700）。
unit 已包含一组 systemd 硬化选项，并显式设置 `UNIONC_DATA_DIR` 与 `WorkingDirectory`。
`Type=notify` 只会在环境、密钥、SQLite、路由和监听端口全部初始化成功后报告就绪；已启用
服务的同版本重装会等待这一就绪信号，并在启动失败或随后不再 active 时让包配置失败。

包内环境文件的 `UNIONC_PACKAGE_VERSION=0.3.2` 是安装归属标记，不是可调运行参数，不能
删除或修改。安装脚本还会把 `/var/lib/unionc-package` 中的版本化 marker 与实际 UID/GID、
账户 home/shell、数据目录所有权和 0700 权限逐一比对；既有文件、账户或目录缺少当前标记
时会 fail closed，不执行旧安装接管。marker 目录必须保持 `root:root/0700`，其中状态文件
必须是至多 512 字节、`root:root/0600` 且没有其他硬链接的真实文件；安装钩子不会先修复不可信
元数据再读取其中内容。root 生命周期脚本会覆盖调用者传入的 `PATH`，版本校验也固定调用包内
`/usr/bin/unionc`，不能由同名外部
程序替换。`/etc/unionc` 必须是不可由非 root 写入的真实目录；`unionc.env` 必须是 root 拥有、
0640 且没有其他硬链接的真实文件，其组只能是 root 或当前 marker 记录的 `unionc` GID。普通
卸载保留数据与 marker，仅支持同一 0.3.2 重装。新建专用账户前，安装脚本先原子发布并同步
`pending-group` 或 `pending-user` 意图（包含预选 UID/GID），再要求账户工具创建同一个数值身份；
确认 NSS 中名称、UID/GID、home、shell 以及唯一、锁定的 shadow/gshadow 记录均严格匹配后，
才同步发布 `managed-group` 或 `managed-user` 提交 marker；即使 `/etc` 与 `/var` 分属不同文件
系统，账户数据库也必须先通过独立持久化屏障，之后才允许 marker 提交，最后删除 pending。
账户工具即使已产生完整副作用却返回失败、首次 NSS 查询失败，或进程在 marker rename 后中断，
同版本重装也会从磁盘状态前向完成，且不会重复创建身份；若只提交了 group/passwd 而缺少对应
gshadow/shadow，安装会保留 pending 并 fail closed，待账户数据库修复后再完成。成功安装后不应
留下 pending 文件。该证明边界信任 root 与 NSS 管理员，不试图防御同权限主体伪造完全相同的
数值身份和账户记录。

### 正式发布制品与门禁

`.github/workflows/release.yml` 的独立 `server-linux` job 构建静态 musl x86_64 Linux Server
原始二进制、DEB 与 RPM，并拒绝仍含动态解释器的制品，避免把包暗中绑定到构建机的 glibc
版本。发布工作流会先确认严格版本 tag 的提交属于 `main` 历史，并对该提交复用完整 CI；
来源校验和 CI 均成功后才允许任何平台开始打包。tag 必须与 Cargo workspace 的严格
`MAJOR.MINOR.PATCH` 版本完全一致；
Server 制品与 Agent 三平台制品最终一起进入 SHA256SUMS、GPG 分离签名、provenance
attestation 和 GitHub Release，Agent 原有 job 与生命周期门禁保持独立。

包依赖按发行版分别声明：DEB 依赖提供 `useradd/groupadd` 的 `passwd` 与 `systemd`，RPM
依赖 `shadow-utils` 与 `systemd`；不能把 Debian 的 `adduser` 名称复用到 RPM。发布 job 会：

- 检查包内容、架构和上述依赖；
- 在 Ubuntu 上真实安装 DEB，验证专用用户、0700 数据目录、0600 `unionc.db` 和就绪探针；
- 运行在线一致性备份、当前 schema 清单恢复与完整性检查；
- 在 Fedora 容器真实安装 RPM，验证脚本顺序、服务状态与完整性检查；
- 验证普通移除保留 `/var/lib/unionc`，不会把卸载误当成数据 purge。

这些长生命周期检查位于 `server/packaging/linux/tests/`，不再内嵌在 GitHub Actions
YAML 中。RPM 检查只改变一次性 Fedora 容器；DEB 检查会真实安装、启动和移除本机服务，
因此只允许在可丢弃的 Ubuntu 测试机执行，并要求显式传入 `--allow-system-changes`。

原始二进制制品不代替 unit、环境文件和账户初始化，适合自定义部署系统；一般主机安装优先
使用 DEB/RPM。

## SQLite 备份与同版本恢复

数据库启用 WAL 时，运行中只复制 `unionc.db` 不是一致性备份；同时复制 `-wal`/`-shm`
也很容易得到跨时点文件。在线备份应使用 Server 自带的一致性快照命令：

包安装场景下，维护命令必须加载与 `unionc.service` 相同的环境文件，否则拿不到生产主密钥。
下面用临时 systemd unit 加载 `/etc/unionc/unionc.env`，不会把密钥展开到命令行；请先确保
`unionc` 用户对备份目标目录有写权限：

```bash
sudo systemd-run --quiet --wait --pipe --collect \
  --uid=unionc --gid=unionc -p WorkingDirectory=/var/lib/unionc \
  -p Environment=UNIONC_DATA_DIR=/var/lib/unionc \
  -p EnvironmentFile=/etc/unionc/unionc.env \
  /usr/bin/unionc backup --output /srv/backup/unionc-$(date -u +%Y%m%dT%H%M%SZ).db
```

完整灾备集还必须包含 `/var/lib/unionc/unionc-config.json` 与
`/etc/unionc/unionc.env` 中的 `UNIONC_SECRET_KEY`（以及轮换期历史密钥）。数据库快照可恢复
主机、历史和审计，但缺少主密钥时 Sunshine 密码密文永久不可读。
`backup` 会同时生成 `<输出路径>.manifest.json`，其中记录当前应用版本、唯一 schema 版本、密钥 ID 和快照
SHA-256；复制、校验与保留快照时必须把该清单作为一对文件处理。

恢复会替换活动数据库，必须先停止服务；目标已经存在时还必须显式给出 `--force`。
`--force` 只表示允许替换现有目标，并不会绕过安全校验：数据库与清单必须同时存在，且
SHA-256、schema、外键与密文可解性都必须通过。不要恢复来源不明的快照：

```bash
sudo systemctl stop unionc
sudo systemd-run --quiet --wait --pipe --collect \
  --uid=unionc --gid=unionc -p WorkingDirectory=/var/lib/unionc \
  -p Environment=UNIONC_DATA_DIR=/var/lib/unionc \
  -p EnvironmentFile=/etc/unionc/unionc.env \
  /usr/bin/unionc restore --input /srv/backup/unionc-2026-08-16.db --force
sudo systemd-run --quiet --wait --pipe --collect \
  --uid=unionc --gid=unionc -p WorkingDirectory=/var/lib/unionc \
  -p Environment=UNIONC_DATA_DIR=/var/lib/unionc \
  -p EnvironmentFile=/etc/unionc/unionc.env \
  /usr/bin/unionc integrity-check
sudo systemctl start unionc
```

替换前，`restore` 会处理当前活动 SQLite 库：若它通过校验，会输出一个
`unionc.pre-restore-*.db` 及配套 manifest，可再次交给 `restore --input` 回退。若当前库已损坏
但没有 WAL/SHM，恢复会保留无 manifest 的 `unverified` 原始取证副本后继续；若损坏库仍带
sidecar，命令会先保留完整 main/WAL/SHM 文件族再拒绝替换，避免丢弃可能尚未 checkpoint
的页面。上述恢复点不会自动清理；完成恢复演练并另有异机备份后，再按保留策略成对删除
DB/manifest，或删除整组取证文件。无 manifest 的副本不能作为受支持的 `restore` 输入。

`restore` 只接受由**同一当前应用版本及 schema**生成的快照。它会精确校验 manifest、schema 指纹、
外键、SQLite 完整性和密文可解性，然后在数据目录内发布；不会把旧账本当作前缀、不会运行
staging 升级，也不会回填缺失字段。版本、schema 或指纹任一不一致都拒绝恢复，`--force`
也不能绕过。

### 当前存储边界

项目只支持空目录全新建库和当前版本备份的同 schema 恢复。旧 Server 数据库不能就地打开、
升级或导入。需要留存旧数据时，应先在旧系统中导出为独立的中立格式，再部署当前版本并
重新配对 Agent；该导出不属于 UnionC 当前版本的可导入数据源。

## 密钥轮换

加密面只有 `external_hosts.secret` 中的 Sunshine 密码；绑定地址、端口和目录等部署配置
直接来自环境变量，不再以第二份加密 JSON 保存到数据库。密文带 key_id 标记，加密恒用
当前密钥，解密按 key_id 在密钥环中查找。

轮换时只有批量重加密这一步需要短暂停服。Server 进程会持有单实例数据库锁，离线
`rekey` 若检测到服务仍在运行会直接拒绝，而不会与在线写入竞争：

```bash
# 1. 在 /etc/unionc/unionc.env 中把旧密钥移入历史并启用新密钥，然后重启验证。
UNIONC_SECRET_KEY=<新密钥的 Base64>
UNIONC_SECRET_KEY_ID=2025q3
UNIONC_SECRET_KEY_PREVIOUS="2025q1:<旧密钥的 Base64>"
sudo systemctl restart unionc

# 2. 确认新旧密文都能读取后停止 Server；用与服务相同的密钥环境执行离线重加密。
sudo systemctl stop unionc
sudo systemd-run --quiet --wait --pipe --collect \
  --uid=unionc --gid=unionc -p WorkingDirectory=/var/lib/unionc \
  -p Environment=UNIONC_DATA_DIR=/var/lib/unionc \
  -p EnvironmentFile=/etc/unionc/unionc.env \
  /usr/bin/unionc rekey
sudo systemctl start unionc

# 3. 从 unionc.env 移除 UNIONC_SECRET_KEY_PREVIOUS 并重启，旧密钥彻底退役。
sudo systemctl restart unionc
```

`UNIONC_SECRET_KEY_PREVIOUS` 支持逗号分隔多把历史密钥（`id1:key1,id2:key2`），
以应对连续多次轮换。历史密钥的 id 不得与当前 id 重复。

如果跳过第 1 步直接换密钥，服务会**拒绝启动**并指出缺失的 key_id：

```
Error: encrypted secret uses key id '2025q1', which is not in the keyring
(known ids: 2025q3); add the retired key to UNIONC_SECRET_KEY_PREVIOUS to read it
```

## 只读主机监控

### 一次性授权配对（推荐）

配对由管理员邀请、Agent 请求和一次性授权确认三部分组成。当前 Windows Agent 可在
目标设备本机配置页一次填写 Server 地址和授权密钥；CLI/其他平台使用公开浏览器激活页：

- `GET/POST /api/monitoring/agent-instances`：管理员列出或创建待激活实例；创建响应中的
  `activation_code` 只出现一次，数据库只保存其 SHA-256；
- `DELETE /api/monitoring/agent-instances/{request_id}`：取消尚未使用的邀请；
- `POST /api/agent/v2/pairing-requests`：Agent 提交设备摘要、长期 secret 哈希和独立的
  polling secret 哈希；原始 secret 从不离开 Agent；
- `GET /api/agent/v2/pairing-requests/{request_id}`：激活页读取有限设备摘要供用户核对；
- `POST /api/agent/v2/activate`：Windows 提权 Agent 或公开浏览器页提交配对 request ID
  与一次性授权密钥；
- `POST /api/agent/v2/pairing-requests/{request_id}/status`：Agent 用
  `Authorization: Pairing ...` 轮询，成功后取得非秘密的最终 `instance_id`。

激活事务会一次性绑定邀请、配对请求、实例和 credential。为已有 `instance_id` 创建邀请
表示重新配对：实例 ID 和历史不变，旧 credential 被撤销。创建请求和激活请求都支持
响应丢失后的幂等重试；激活码不能被另一个 request 抢走。

协议细节与状态机见 [agent-pairing.md](agent-pairing.md)。

### 数据面与查询

- `POST /api/agent/v1/report`：使用实例级 Bearer secret 上报只读快照，512 KiB 请求上限。
  成功返回 202 和包含 `host_id/report_id` 的 ACK；重放同一 `report_id` 返回
  `accepted: false`，不产生第二行，也不改变主机当前状态；
- `GET /api/monitoring/hosts`、`/{id}`、`/{id}/history`：管理员会话只读查询。
  列表支持 `?limit&offset`（默认 200、上限 1000）并返回 `total`；历史支持
  `?from&to&limit`（默认 300、上限 1000）；
- `POST /api/monitoring/hosts/{id}/revoke`：持久撤销该实例的全部 credential，保留实例
  tombstone 和历史。之后只有管理员为同一实例明确创建新邀请并完成浏览器配对才能恢复。

凭据查找区分“当前无效”和“实例退役”：未知或已被新 secret 取代的凭据返回 401；
`lifecycle_status=revoked` 的实例返回 403。
当前常驻 `run` 对 401、403 都会持久进入 `reauth_required`、停止投递并继续把采样写入
有界 spool，且不会调用其他身份端点自动复活。`once` / `doctor --delivery` 不写授权状态，
只把可重试报告入队并返回失败。421 仍只表示反向代理链路错误。

### 当前协议边界

身份建立只支持 `/api/agent/v2/pairing-requests` 与 `/api/agent/v2/activate`。Server 不提供
旧 register、enrollment code、全局 enrollment token、浏览器明文 token 轮换或旧 credential
回填。`POST /api/agent/v1/report` 是当前唯一数据面路径，名称中的 `v1` 不代表旧身份协议
仍受支持。生产模式强制 UnionC 只绑定回环地址，所有 Agent API 由 HTTPS 反向代理暴露。
代码中没有主机命令、配置下发、进程控制或自更新端点。

### 反向代理契约

生产环境下登录、改密与 Agent 接口都要求同时携带 `X-Forwarded-Proto: https`、
`X-Forwarded-For` 与反代覆盖写入的 `X-UnionC-Proxy-Secret`，缺任一项返回
**421 Misdirected Request**。前两个头提供协议与来源信息，独立共享密钥证明它们确实由
可信反代写入，而不是任意本机进程伪造。XFF 若做成软降级，按 IP 与按账号的两层登录
配额会**静默失效**、只剩全局兜底。

按 IP 的限流取最后一个 XFF 头的**最右**一项（离本服务最近的可信代理写入的那个）。
该项必须是可直接解析的裸 IP；非法、为空或携带端口都会返回 421，不能向左回退到客户端
可控值。该实现假定前面恰好有一层可信反代；若再叠加 CDN，必须相应调整，否则取到的是
内网地址。
管理台域名的反代配置见 `docs/examples/caddy/Caddyfile.console.example`（含静态前端托管与 SSE 缓冲设置）；
需要独立 Agent 域名和 mTLS 时，可从 `docs/examples/caddy/Caddyfile.agent-api.example` 开始。

## 验证

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

持久层测试使用独立的临时 SQLite 文件并真实执行当前 schema 初始化和 SQL，不需要外部
数据库服务。测试不包含旧数据库或旧 Server 包升级桥；后续 schema 变化直接替换当前基线，
并要求现有部署全新建库。
