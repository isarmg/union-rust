# UnionC Agent

`unionc-agent` 是一个只读、零入站端口的跨平台主机遥测程序。它采集 CPU、内存、
网络、磁盘、温度和可用的 GPU 指标，将完整 capability/快照上报给 UnionC，并可选旁路
发送标准 OTLP/HTTP Protobuf 指标。

## 安全边界

- 不监听任何端口，不实现远程命令、脚本、配置下发或自更新。
- NVIDIA 只调用 NVML query，Linux AMD/Intel 只读取 sysfs/hwmon。
- 缺少驱动、权限或平台 API 时上报 capability 和 N/A，不提升整个进程权限。
- 本地只写稳定 `host-id`、配对状态、每主机 secret 和有大小上限的断线 spool。

## 构建和验证

```bash
cargo test -p unionc-agent
cargo build --release -p unionc-agent
```

默认只启用 `nvidia`；`otlp` 是显式可选能力。不需要 NVIDIA 时可以进一步缩小依赖：

```bash
cargo build --release -p unionc-agent \
  --no-default-features --features otlp
```

## 首次配对与运行

Agent 软件先通过操作系统包管理器、组织软件中心或其他可信渠道独立安装。UnionC 管理台
不托管安装包，也不生成 shell/PowerShell 安装命令。管理员在“监控主机 → 创建 Agent”
生成一次性激活码后，在目标主机执行：

```bash
sudo unionc-agent pair \
  --config /etc/unionc-agent/config.json \
  --server https://unionc.example.com \
  --name sunshine-room-01
```

`pair` 会在本机生成两份 256-bit secret，把**哈希**发给 Server，然后输出专属激活页面。
用户在浏览器核对主机名、平台和架构，输入管理台生成的 `uci_...` 激活码。浏览器不会
得到长期 Agent secret；Server 也从未接收该 secret 的明文。激活成功后 Agent 保存
Server 分配的稳定 `instance_id`，并继续使用现有 `/api/agent/v1/report` 数据面。

配对请求与 secret 在首次 POST 前就写入私有状态文件，因此命令中断或响应丢失后，重新
执行 `pair` 会恢复同一个请求。常驻的 `run` 进程也能继续轮询尚未批准的请求。完整协议见
[agent-pairing.md](agent-pairing.md)。

Linux 日常命令应以服务账户运行，避免 root shell 的权限和设备视图掩盖 systemd 沙箱问题：

```bash
sudo -u unionc-agent unionc-agent status --output human --config /etc/unionc-agent/config.json
sudo -u unionc-agent unionc-agent doctor --output human --config /etc/unionc-agent/config.json
sudo -u unionc-agent unionc-agent probe --output json --config /etc/unionc-agent/config.json
journalctl -u unionc-agent.service -n 100 --no-pager
```

macOS 使用相同命令，但服务账户是 `_unioncagent`、配置位于
`/Library/Application Support/UnionC Agent/config.json`，服务日志位于
`/var/log/unionc-agent.log`。Windows 日常交互由普通用户托盘页面完成。

`status` 严格只读，不创建锁文件、不恢复配对事务，并把 missing、invalid、unreadable 分开
报告。`doctor` 默认执行分项只读检查，不发送报告、不确认删除 spool、也不更换凭据；只有
显式 `doctor --delivery` 才执行真实端到端投递。`--output human|json` 控制显示格式，JSON
带 `schema_version`、稳定状态字段和下一步建议。
`probe` 只在标准输出打印本机快照和 capability，不连接服务端；其中 `host.id` 是每次诊断
临时生成且不持久化的 UUID，不是配对后的稳定实例 ID。
`once` 会先补传状态目录中的历史积压，再发送当前报文，适合由外部调度器周期执行；网络
仍不可用时当前报文留在 spool。配对凭据被拒后不会回退到其他身份建立流程。
主机生命周期被管理员明确撤销时返回 403 + `agent_revoked`；当前有效 credential 与报告
`host_id` 绑定不匹配时返回 403 + `forbidden`；未知/失效、以及主机仍 active 但已被重配
替换的 credential 返回 401。当前常驻 `run` 只在 401 或 `agent_revoked` 时持久记录
`reauth_required`，停止投递但继续采样到有界 spool。`forbidden` 表示该份报文本身属于另一
身份，Agent 会丢弃它并继续 FIFO，因而跨服务器重新配对不会被旧 spool 队首锁死。

配置文件路径可用 `--config PATH` 或 `UNIONC_AGENT_CONFIG` 指定。以下环境变量**优先于**
配置文件：`UNIONC_AGENT_ENDPOINT`、`UNIONC_AGENT_PAIRING_ENDPOINT`、
`UNIONC_AGENT_OTLP_ENDPOINT`、`UNIONC_AGENT_OTLP_TOKEN`、
`UNIONC_AGENT_STATE_DIR`、
`UNIONC_AGENT_INTERVAL_SECONDS`、`UNIONC_AGENT_SLOW_INTERVAL_SECONDS`、
`UNIONC_AGENT_TLS_CA_PEM`、`UNIONC_AGENT_TLS_IDENTITY_PEM`、
`UNIONC_AGENT_TLS_IDENTITY_PKCS12`、`UNIONC_AGENT_TLS_IDENTITY_PASSWORD`、
`UNIONC_AGENT_ALLOW_INSECURE_HTTP`。
配置文件一旦存在，就必须是当前 0.3.2 的完整结构，并包含
`"application_version": "0.3.2"`；缺字段、未知字段或其他应用版本都会在读取时被拒绝，
环境变量不会替旧结构补字段。只有配置文件不存在时才使用编译进 0.3.2 的完整默认配置，
配对成功后再原子写入当前结构。
未指定状态目录时使用 Linux `/var/lib/unionc-agent`、Windows
`%ProgramData%\UnionC Agent` 或 macOS `/Library/Application Support/UnionC Agent`。

配置在**启动时**即校验：整数秒采集间隔必须至少为 1，且 jitter 后最坏实测周期仍须落在
服务端报文契约 0.1~3600 秒内；此外，`slow_interval_seconds` 小于 `interval_seconds`、
jitter 超过 50、
spool 上限小于 1 MiB、非回环的明文 HTTP、endpoint 内嵌凭据、同时配置两种客户端证书
格式、为当前平台配置不受支持的证书格式，或只有 `tls_identity_password` 而没有
`tls_identity_pkcs12`，任意一条都会拒绝启动——而不是静默忽略证书，或等到运行时每次
上报都收到 400、报文在 spool 里被按永久内容错误确认丢弃。

如数据入口采用 mTLS，Linux 可把客户端证书和私钥合并成一个 PEM，并设置
`tls_identity_pem`；Windows/macOS 原生证书栈使用 `tls_identity_pkcs12` 和对应密码。
私有 CA 使用 `tls_ca_pem`。首次配对入口不能被站点级 mTLS 一并锁住，因为尚未配对的
Agent 还没有客户端证书；需要把 bootstrap/pairing 与受 mTLS 保护的 report 入口拆开。
配对成功会把持久化 JSON 的 `pairing_endpoint` 清空，以后未被覆盖时从 report endpoint
推导。若服务长期设置 `UNIONC_AGENT_PAIRING_ENDPOINT`，下次加载会自动恢复该覆盖；否则
分域部署须在重新配对前通过配置或环境变量恢复 bootstrap endpoint。Server 返回相对激活
路径，所以 pairing origin 也必须提供或反代 `/agent/activate/...` SPA，不能只暴露配对 API。
配置、Agent secret 和证书文件必须只允许服务账户读取。

当前版本只支持浏览器配对，不读取长期 enrollment token、旧注册 proof 或配置中的直接
report token。不要复制或重用另一台机器的整个状态目录。
除回环地址外，Agent 默认拒绝明文 HTTP；生产入口应使用 HTTPS，确有隔离内网需求时
才显式设置 `allow_insecure_http`。

## 状态目录

| 文件 | 内容 |
|---|---|
| `host-id` | Server 分配的稳定实例 UUID |
| `agent-token` | Agent 本地生成的长期通信 secret |
| `pairing-state.json` | 可恢复的创建/等待/完成状态；等待期内含 polling secret |
| `auth-state.json` | `authorized` 或 `reauth_required` 诊断状态 |
| `spool/` | 断线续传队列（`*.json` 待发、`*.invalid` 隔离） |

目录 0700、文件 0600，写入一律走"临时文件 + fsync + rename + 目录 fsync"原子替换。
spool 变更另有跨进程文件锁，打开与容量核算会回收崩溃遗留的无主原子临时文件。systemd
unit 用 `StateDirectory` + `StateDirectoryMode=0700` + `UMask=0077` 保证权限——不显式声明时
systemd 用 0755 并**每次启动都重设**，而 Agent 的 chmod 要等到第一次写凭据，中间存在窗口。

## 投递与失败处理

失败分三类，判据是"**要让同一份报文最终被接受，需要改变什么**"：

| 状态码 | 分类 | 处置 |
|---|---|---|
| 400 / 409 / 413 / 422 | 内容或 report ID 冲突（改不了） | `run` 删除已排队报文；一次性命令不入队。重发只会再次失败 |
| 401 | 未知/失效 credential，或主机仍 active 但该 credential 已被重配替换/撤销 | `run` 保留已排队的队首并进入 `reauth_required`；一次性命令把当前报告入队并失败，但不改授权状态 |
| 403 + `agent_revoked` | 主机生命周期已撤销 | `run` 保留已排队的队首并进入 `reauth_required`；一次性命令把当前报告入队并失败，但不改授权状态 |
| 403 + `forbidden` | 当前有效 credential 与报告 `host_id` 不匹配 | 该报文永久无效，`run` 删除队首后继续投递；一次性命令不入队 |
| 其他或无法解析的 403 | 代理/WAF 或不兼容服务端拒绝 | 保留队首并退避，不擅自撤销凭据或丢弃报告 |
| **421** | 链路问题——反代契约头或独立代理证明缺失/不匹配 | `run` 保留队首并退避；一次性命令把当前报告入队。**不是**凭据失效，绝不触发重新注册 |
| 其他 4xx / 5xx / 网络故障 | 仅需等待 | `run` 保留队首并指数退避（上限 300 秒）；一次性命令把当前报告入队 |

421 必须早于 401/403 匹配。二者混用的代价是：一次反向代理漏配请求头的**部署失误**，
在客户端表现为每台 Agent 的凭据失效——故障现象与根因毫不相关。

Web 撤销不会主动推送到本机，Agent 要在下一次报告响应才得知。一次性的 `once` /
`doctor --delivery` 遇到 401 或 `agent_revoked` 时只保留报告并返回错误，不写
`reauth_required`。正在运行的 `run` 写入该状态后会继续采样；重启后则无法取得 authorized
reporter，在进入采样循环前退出并由服务管理器重试，直到同实例重新配对。

spool 文件名以零填充的毫秒时间戳为前缀，字典序即投递顺序；每轮最多补传 32 批，
避免长时间断线恢复后独占网络与采样线程。反序列化失败的报文隔离为 `.invalid`，
且**计入容量预算**并优先淘汰（否则磁盘异常反复触发时它们会绕开 `spool_max_bytes`
把分区吃满）。常驻 `run` 对读、写和补传分别跟踪健康度：单次写失败会丢弃当前未能落盘的
采样并降级续跑，同类操作只有**连续** 100 次失败才退出交给服务管理器。一次性的
`once` / `doctor --delivery` 没有这层健康度封装，spool I/O 错误会立即失败。

官方 Agent 不应靠 Server 的永久 400/413 才发现自己越过协议边界。采集完成、spool
入队、旧 spool 读取和实际发送都会复用同一套报告收敛规则：CPU、设备和 capability
集合按共享上限裁剪，平台文本按 UTF-8 字节安全截断并去除控制字符，最后以**实际发送的
compact JSON**核对不超过 512 KiB。设备枚举先规范排序，并把 Server 摘要所需的速率/温度
峰值放在保留前缀；发生收敛时增加 `agent.report.truncated` capability。这样同一 package
版本的旧构建已经落盘的超大报告也能继续补传，而不是在队首收到 413 后被删除。未知
`schema_version` 不会被伪装成当前版本，而是从 FIFO 隔离出来；本项目仍不承诺跨版本状态迁移。

配对和错误响应均使用流式 64 KiB 上限，避免错误配置或被攻陷的端点用超大响应
耗尽 Agent 内存。Server 返回 2xx 也不代表报告一定成功：Agent 必须解析结构化 ACK，并
核对其中的 `host_id` 与 `report_id`，否则保留原报文重试。

## 平台能力

| 平台 | 基线 | GPU/温度 |
|---|---|---|
| Linux | CPU、内存、网络、磁盘 | hwmon；NVML；AMD/Intel DRM sysfs |
| Windows | CPU、内存、网络、磁盘、可用 ACPI thermal zone | NVIDIA NVML；WDDM GPU Engine 聚合利用率；厂商专有扩展显示 capability gap |
| macOS | CPU、内存、网络、磁盘、sysinfo 可读传感器 | 公共稳定 API 不提供整机 Apple/AMD/Intel GPU 利用率，明确显示 N/A |

Windows/macOS 的私有传感器接口以及需要管理员权限的查询不会作为正式能力启用。
主机卡的网络和磁盘概要取当前最忙单接口/单设备的速率，避免 veth/bridge、bind mount
重复计算；详情页仍展示每个接口和挂载项。

## 打包

- Linux：**在工作区根**执行 `cargo build --release -p unionc-agent`，再
  `NFPM_ARCH=amd64 agent/packaging/linux/build-packages.sh`。唯一打包入口从 Cargo 读取
  当前版本，并先核对 binary 与 `config.example.json` 的版本；一次生成 DEB 和 RPM。
  脚本要求 `nfpm` 在 `PATH`，或由 `NFPM_BIN` 指向可执行文件；发布工作流固定使用
  nFPM v2.47.0。包内含专用用户、加固后的 systemd unit 与 GPU drop-in。

  > ⚠ 不要在 nfpm 的 `contents[].src` 里写 `${VAR}`——nfpm 只对 `name`/`arch`/`version`
  > 做环境变量展开，`src` 原样保留（实测 v2.44.0 直接 `glob failed`）。交叉编译请先把
  > 产物 `install` 到 `target/release/` 再打包。
- Windows：首选 x64 WiX MSI，用户可双击安装或通过 `msiexec.exe /i`、winget、GPO、
  Intune/MDM 部署，不依赖 PowerShell。程序安装到 Program Files，可变状态留在 ProgramData；
  原生 `UnionCAgent` Windows Service 以 LocalService 运行，专属 service SID 访问凭据；独立
  的普通用户托盘伴侣通过 HKLM Run 在每个登录会话启动，并提供本地浏览器配置、配对/
  重新配对、Server 连接检测与服务状态/启停。本地页一次提交 Server 地址和一次性授权密钥；
  连接检测在页面可见时每 30 秒访问一次公开的 `/api/health`，不会读取或展示 Agent secret。
  涉及机器状态的操作才请求 UAC。用户选择退出时，在 UAC 下停止服务成功后才退出托盘；
  该操作不改变 `Automatic` 启动类型，因此下次启动 Windows 时服务仍会自动运行。安装器的
  系统关闭消息不会触发该用户操作。
  静默安装不会在 SYSTEM/session 0 启动托盘，用户下次登录或从开始菜单运行即可。
  当前版本只支持 WiX MSI/SCM 服务安装，不包含旧 PowerShell 计划任务安装入口。
- macOS：设置 `BINARY`、`VERSION` 后执行 `packaging/macos/build-pkg.sh`，生成带专用隐藏
  账户、LaunchDaemon、日志轮转和卸载器的 pkg。开发构建可以不签名；tag 发布强制完成
  Developer ID Application/Installer 签名、Hardened Runtime、notarytool 和 staple。

推送 `v*` 标签时，`.github/workflows/release.yml` 生成 Linux amd64 deb/rpm/tar、Windows
x64 MSI 和 macOS universal pkg。Windows 发布先签名 Agent、托盘伴侣和原生维护三个 EXE，
再构建并签名 MSI；
正式 tag 缺签名 secret 会失败。成功发布包含平台签名、
`SHA256SUMS`、GPG 分离签名和 provenance attestation。管理台仍不托管或选择这些制品，
APT/YUM/winget/MDM 等渠道元数据继续由分发系统负责。

三平台全新安装、同版本重装、普通卸载、purge、退役顺序和发布 secret 见
[Agent 安装、同版本重装、卸载与退役](runbooks/agent-lifecycle.md)。

Linux 基础 unit 使用 `PrivateDevices=yes`，适合不采集 GPU 的主机。需要 GPU 时，确认
本机存在 `render`、`video` 组后，再安装 `packaging/linux/unionc-agent-gpu.conf` 作为
systemd drop-in；不要授予 `CAP_SYS_ADMIN`、`CAP_SYS_RAWIO` 或 root。

### Linux 安装、同版本重装、卸载与退役

DEB/RPM 包负责完整的本地生命周期，Agent 本身不包含自更新器：

| 操作 | DEB | RPM | 本地配置/凭据/spool | Server 实例 |
|---|---|---|---|---|
| 安装 | `apt install ./unionc-agent_*.deb` | `dnf install ./unionc-agent-*.rpm` | 创建并设为 0700/0600 | 不创建，随后浏览器配对 |
| 同版本重装 | `apt install --reinstall ./当前包.deb` | `dnf reinstall ./当前包.rpm` | 保留 | 不变 |
| 普通卸载 | `apt remove unionc-agent` | `dnf remove unionc-agent` | **保留**，便于原身份重装 | 不变 |
| 永久退役 | 管理台撤销后执行 `apt purge unionc-agent` | 管理台撤销后先执行 `unionc-agent-purge --yes`，再 `dnf remove unionc-agent` | **删除** | 仅由管理台撤销 |

跨版本不承诺状态或安装布局迁移。部署新版本应先在 Web 撤销旧实例，再按平台规定顺序完成
卸载与永久本地清理，然后安装当前制品并重新配对；只有同一当前版本的 reinstall 会复用
现有状态。

安装脚本在 systemd 正常运行时会启用、重启并验证服务；任何关键步骤失败都会让包安装失败，
不会打印“服务已运行”的假成功。在容器镜像/chroot 等 systemd 未作为 PID 1 运行的环境中，
包只安装文件并明确提示稍后手工 `systemctl enable --now unionc-agent`。

RPM 普通卸载为防止 `%config(noreplace)` 被移走，会在 root-only 记账目录中通过临时文件和
原子 rename 保存配置，再以相同方式恢复。整个事务会重新验证配置、两个 ownership marker、
备份、服务账户的数值身份以及恢复目标；完整临时配置校验通过后才原子提交。提交前失败会
保留旧配置和备份，且不会沿符号链接读取、覆盖或删除外部文件。原子 rename 是提交点：
提交后恢复配置已经生效，后续服务启动失败不会再回滚配置。

普通卸载刻意保留 `/etc/unionc-agent`、`/var/lib/unionc-agent`、GPU systemd drop-in 和
专用账户。purge 才会清理这些路径以及**由包首次创建**的 `unionc-agent` 用户/组；安装前
已经存在的合法同名账户不会被包接管或删除。若包创建的账户后来被人为改成非预期值，
或专用组后来被其他用户作为 primary/supplementary group 使用，purge 会拒绝删除相应身份
并以失败结束。无法由当前 marker 证明归属的账户一律保留，避免把碰巧同名的身份误删。
Linux 还要求 marker 的父目录保持 `root:root 0700`、两个 marker 保持 `root:root 0600`；
任一类型、属主或权限漂移都会让整组账户记账操作失败关闭，不会沿符号链接删除备份或账户。
`unionc-agent-purge` 要求 root 和显式 `--yes`，删除目标均为脚本内固定绝对路径。

本地 purge **不会调用 Server**，因为卸载时网络和管理凭据都不可靠，也不能让一台主机
自行删除审计记录。永久退役顺序必须是：先在 Web 撤销实例，再 purge 本机。配置引用到
上述目录之外的组织 CA、客户端证书或密钥由部署方管理，不在 purge 范围内。

Linux tar.gz 只是便携二进制，不会创建账户、安装 unit 或提供重装/卸载事务；具体边界见
[`packaging/linux/PORTABLE-README.md`](../agent/packaging/linux/PORTABLE-README.md)。生产环境
需要受管生命周期时应使用 DEB/RPM。

## 设计参考

采样生命周期和跨平台基线参考 [sysinfo](https://docs.rs/sysinfo/latest/sysinfo/)；Linux
设备/挂载过滤与 hwmon 语义参考 [Prometheus node_exporter](https://github.com/prometheus/node_exporter)；
NVIDIA 查询使用 [nvml-wrapper](https://docs.rs/nvml-wrapper/latest/nvml_wrapper/device/struct.Device.html)，
不调用其 setter；指标命名、资源属性和网关链路参考
[OpenTelemetry hostmetrics](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/receiver/hostmetricsreceiver/README.md)
及 [VictoriaMetrics OTLP 集成](https://docs.victoriametrics.com/victoriametrics/data-ingestion/opentelemetry-collector/)。
