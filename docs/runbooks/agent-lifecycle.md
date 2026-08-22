# Agent 安装、同版本重装、卸载与退役

本文说明 UnionC Agent 在 Linux、Windows 和 macOS 上的完整本地生命周期。Agent 软件
仍由包仓库、组织软件中心、MDM 或其他可信渠道分发；UnionC Web 只创建实例、授权配对、
撤销和重新配对，不托管安装包，也不向主机下发安装或更新命令。

## 1. 两种删除语义

“卸载程序”和“永久退役主机”不是同一个操作：

| 操作 | 删除程序/服务 | 保留配置、host-id、agent-token、配对状态和 spool | 删除 Server 实例 |
|---|---:|---:|---:|
| 普通卸载 | 是 | 是 | 否 |
| 当前版本重装 | 替换 | 是 | 否 |
| 本地永久清理 | 因平台而异：可能与卸载合并，也可能必须先执行 | 否 | 否 |
| Web 撤销 | 否 | 是 | 将实例标为 revoked 并吊销全部 credential |
| 完整退役 | 是 | 否 | 必须先执行 Web 撤销 |

普通卸载默认保留身份，避免误卸载后重装产生第二个主机实例。`purge` 是显式、不可恢复的
本地操作，但它没有管理台管理员权限，因此不会假装完成 Server 撤销。

永久退役的跨平台固定部分是：

1. 在 UnionC Web 的监控主机页面撤销实例；
2. 按本页对应平台的顺序执行卸载与 purge（DEB 合并执行；RPM 必须先 purge 再 remove；
   Windows 在同一次 MSI `/x` 中传 `PURGE=1`；macOS 用 `uninstall.sh --purge`）；
3. 验证命令成功，且服务、程序和状态目录均已消失；Windows 还必须确认该版本的固定
   quarantine 已消失。

如果主机已经丢失，至少应完成第 1 步；Server 的 revoked tombstone 会持续拒绝旧凭据。

## 2. 所有平台共同流程

### 2.1 安装与首次配对

1. 通过可信软件渠道安装与平台匹配的 Agent；
2. 管理员在 Web 创建 Agent 实例和短时一次性激活码；
3. 在目标主机以能写 Agent 私有状态目录的身份执行：

   ```text
   unionc-agent pair --server https://unionc.example.com
   ```

4. 打开命令输出的专属浏览器 URL，核对主机摘要并输入一次性码；
5. 使用 `unionc-agent status` 或系统服务管理器确认授权和运行状态。

安装包不含激活码或长期 token。长期 report secret 在 Agent 本地生成，浏览器不会读取它。

### 2.2 版本边界与重装

安装器只保证当前版本的全新安装、同版本重装、普通卸载和 purge。跨版本升级、旧状态转换、
旧服务/ACL/marker 接管不在支持范围内。部署新版本时先在 Web 撤销旧实例，按平台规定顺序
永久清理旧 Agent，再安装当前制品并重新配对。Windows MSI 检测到同一产品的其他版本会 fail closed，
不会运行 MajorUpgrade。

同一当前版本的重装可以保留状态目录。不要把已配对的状态目录放进系统镜像或克隆到其他主机。

## 3. Linux

### 3.1 受管安装

正式受管安装使用同一份 `agent/packaging/nfpm.yaml` 生成 DEB 和 RPM。包包含专用
`unionc-agent` 非登录账户、硬化 systemd unit、配置、GPU drop-in 示例和本地 purge 工具。

```bash
# Debian / Ubuntu
sudo dpkg -i unionc-agent_VERSION_amd64.deb

# Fedora / RHEL 系
sudo dnf install ./unionc-agent-VERSION.x86_64.rpm
```

配对命令：

```bash
sudo unionc-agent pair --config /etc/unionc-agent/config.json \
  --server https://unionc.example.com
```

主要路径：

| 内容 | 路径 |
|---|---|
| 程序 | `/usr/bin/unionc-agent` |
| 配置 | `/etc/unionc-agent/config.json` |
| 身份、凭据、配对状态、spool | `/var/lib/unionc-agent` |
| systemd unit | `/usr/lib/systemd/system/unionc-agent.service` |
| 可选 GPU drop-in | `/etc/systemd/system/unionc-agent.service.d/gpu.conf` |
| purge 工具 | `/usr/sbin/unionc-agent-purge` |

配对后的只读验证与日志：

```bash
sudo -u unionc-agent unionc-agent status --output human --config /etc/unionc-agent/config.json
sudo -u unionc-agent unionc-agent doctor --output human --config /etc/unionc-agent/config.json
systemctl status unionc-agent.service
journalctl -u unionc-agent.service -n 100 --no-pager
```

### 3.2 卸载和 purge

DEB 普通卸载保留本机身份：

```bash
sudo apt remove unionc-agent
```

DEB 永久本地清理：

```bash
# 先在 Web 撤销实例
sudo apt purge unionc-agent
```

RPM 没有独立的 package-manager purge 事务，因此先调用包内工具：

```bash
# 先在 Web 撤销实例
sudo unionc-agent-purge --yes
sudo dnf remove unionc-agent
```

purge 只删除固定的 UnionC 路径。专用用户和组只有在当前 root-only marker 证明它们确由
本包创建、创建时 UID/GID 与当前属性仍精确匹配、且组没有其他 primary/supplementary 成员
时才会删除；查询失败一律保留。碰巧同名的预存账户不会被接管。

Linux `.tar.gz` 是 portable binary bundle，不是安装包，不提供账户、权限、升级或卸载语义。

## 4. Windows

Windows 发布包是 x64、per-machine 的 WiX 4 MSI。终端用户可以直接双击安装，或交给
winget、GPO、Intune/MDM 等渠道；安装、同版本重装、服务注册和卸载不调用也不要求 PowerShell。
当前版本只识别由 MSI 建立的 `UnionCAgent` SCM 服务和受管状态目录，不接管或迁移旧
PowerShell 计划任务安装。

当前发布边界是 `x86_64-pc-windows-msvc`；尚未提供 ARM64 MSI。

### 4.1 安装、权限和同版本重装

交互安装可双击 MSI。无人值守安装在管理员命令提示符中执行：

```cmd
msiexec.exe /i UnionC-Agent-0.3.2-x64.msi /qn /norestart /l*v "%TEMP%\unionc-agent-install.log"
```

主要路径：

| 内容 | 路径 |
|---|---|
| 遥测服务程序 | `%ProgramFiles%\UnionC Agent\unionc-agent.exe` |
| 用户托盘伴侣 | `%ProgramFiles%\UnionC Agent\unionc-agent-tray.exe` |
| 配置、身份、凭据和 spool | `%ProgramData%\UnionC Agent` |
| Windows Service | `UnionCAgent`（显示名 `UnionC Agent`） |
| 登录自启动 | HKLM `Run\UnionCAgentTray`（每个登录用户各自运行） |
| 手工启动入口 | 开始菜单“UnionC Agent” |

Agent 通过 SCM 原生运行并处理 Stop/Shutdown，账户为 `NT AUTHORITY\LocalService`，启动类型
为 Automatic。安装器启用 unrestricted service SID；状态 ACL 只授权 SYSTEM、
Administrators、专属 `NT SERVICE\UnionCAgent` SID 和受限 OWNER RIGHTS，不授权共享的
LocalService SID。全新安装还会在该受保护目录生成不含凭据的最小默认配置；同版本重装不
覆盖它。服务 SID、安全 ACL 或“非崩溃失败执行 SCM 恢复动作”的标志无法建立时，安装失败
并回滚。

托盘伴侣是独立的 Windows GUI 程序，不在 session 0 运行，也不承载遥测。它以普通用户权限
提供本地状态、Server 连接检测、配对/重新配对和服务启停入口；它不提供直接打开
Web 管理台的入口。机器级变更才触发 UAC。其配置页只
监听随机 `127.0.0.1` 端口并使用进程内随机 capability URL，不把主机凭据暴露给浏览器，
也不允许远程访问。`%ProgramFiles%` 仅给 BUILTIN\Users 读取/执行权限，受保护的
`%ProgramData%` 状态 ACL 不增加普通用户权限。

交互安装或同版本重装成功、且没有映像等待重启替换时，会以安装者的非提权会话启动托盘；`/qn`、
GPO、Intune/MDM 等静默/SYSTEM 部署不会启动 session 0 托盘，下一次用户登录时由 HKLM Run
启动。用户也可从开始菜单立即打开。右键托盘选择“配对/重新配对”，在本机配置页一次输入
服务器 HTTPS 地址、管理台生成的一次性授权密钥和可选名称，接受 UAC 后由 Agent 完成配对，
无需再到远程页面输入密钥。配对失败会恢复服务原运行状态；成功后仅当
服务原本运行时才重启。

CLI 仍可用于管理员故障排查：

```cmd
sc.exe stop UnionCAgent
"%ProgramFiles%\UnionC Agent\unionc-agent.exe" pair --config "%ProgramData%\UnionC Agent\config.json" --server https://unionc.example.com
sc.exe start UnionCAgent
```

MSI 只允许全新安装、当前产品的 repair/reinstall 和卸载。稳定 UpgradeCode 只用于检测；
发现任何其他版本会 fail closed，要求先卸载，不会自动移除或迁移。重装与卸载会在事务开始后、文件移除前，通过
用户态和提权两次 Windows 消息请求可达的托盘优雅退出，但不强制终止；受会话隔离影响的其他
用户托盘或其他文件占用仍存在时，由 Windows Installer 安排重启。Run 值和开始菜单入口
参与同一安装/重装/卸载事务。fresh install 或交互式当前版本重装仅在事务已经提交且
没有 `ReplacedInUseFiles` 时，通过 WiX 的 unelevated ShellExecute helper 启动新托盘；repair、
卸载、静默/SYSTEM 部署或仍有映像等待替换时不启动，等待重启/下次登录或从开始菜单打开。

托盘“退出”会请求 UAC，确认 `UnionCAgent` 服务已停止后才结束当前登录用户的托盘伴侣。
拒绝 UAC 或停服务失败时托盘不退出，避免界面消失但采集仍运行的歧义。下次登录仍会遵循 Windows
“启动应用”偏好自动运行；服务的 `Automatic` 启动类型不会改变，因此下次启动 Windows 时
Agent 服务会恢复自动运行。本次开机内若从开始菜单重新打开托盘，服务仍保持停止，直到用户
明确选择启动或系统重启。多用户/RDP 会话各自拥有一个托盘实例，但共享同一个机器级 Agent
服务与实例身份。

重装和卸载是不同的系统路径：MSI 发送的 `WM_CLOSE` 只要求托盘伴侣优雅退出，不调用用户
菜单的“停止本次服务并退出托盘”流程；服务仍由 Windows Installer 的 `StopServices` 和 SCM
事务化处理。

程序树和状态树会拒绝 reparse point、特殊文件与多硬链接文件；预存状态必须通过当前
managed marker 固定内容、普通单链接类型及状态根/marker 精确 owner 与 DACL 的联合验证。
MSI 应用后另行强制并复核程序树的 SYSTEM owner 与只读 service SID/BUILTIN\Users ACL；
ProgramData 状态树仍不授权 Users。任何旧 ACL 模板、旧任务或未知 marker 都会 fail closed，
不会被当前安装器转换或接管。marker 不是脱离这些检查即可独立证明归属的信任令牌。

### 4.2 卸载和 purge

在“设置 → 应用 → 已安装的应用”中卸载，或执行以下命令，默认删除服务和程序但保留本地
状态：

```cmd
msiexec.exe /x UnionC-Agent-0.3.2-x64.msi /qn /norestart /l*v "%TEMP%\unionc-agent-uninstall.log"
```

永久本地清理必须显式传入唯一允许的属性 `PURGE=1`：

```cmd
# 先在 Web 撤销实例
msiexec.exe /x UnionC-Agent-0.3.2-x64.msi PURGE=1 /qn /norestart /l*v "%TEMP%\unionc-agent-purge.log"
```

普通卸载会移除 service SID ACE，只留下 SYSTEM、Administrators 和 OWNER RIGHTS 安全边界；
同时移除托盘程序、开始菜单入口和 HKLM Run 登录自启动项。重新安装时由 managed marker
与精确 ACL 联合验证后接管原身份。Purge 先在同一卷把状态根
原子移动到 `%ProgramData%\UnionC Agent.purge-quarantine-0.3.2`：进入 commit 前的失败会回滚
rename，commit 阶段的递归删除则不可逆。为避免把产品回滚到部分删除的树，commit 删除失败
不会触发 MSI 产品回滚，受保护的 quarantine 可能保留；因此 `msiexec` 成功只表示产品卸载，
不能单独作为凭据已删除的证明。自动化必须检查状态根和固定 quarantine 均不存在；若仍有
quarantine，保留日志、重启释放占用，再由管理员核实并只清理该固定路径，完成前不得标记
退役成功。如果普通卸载后才决定 purge，先用可信的同版本 MSI 重装，再执行 `PURGE=1`。

每个用户的 `%LOCALAPPDATA%\UnionC Agent\tray.json` 只含精确的当前应用版本、Server URL
和可选显示名称，不含 Agent secret；缺少版本或版本不匹配的文件不会被当前托盘读取。
机器级卸载器不会枚举或修改其他用户配置文件，因此 remove 和 `PURGE=1` 都
不承诺删除这些桌面偏好；需要时由对应用户自行删除。机器级 purge 仍删除 ProgramData 中的
身份、通信凭据、配对状态和 spool。

## 5. macOS

macOS pkg 安装专用隐藏账户、LaunchDaemon、日志轮转任务、命令符号链接和本机卸载器。

```bash
sudo installer -pkg unionc-agent-VERSION.pkg -target /
sudo -u _unioncagent /usr/local/bin/unionc-agent pair \
  --config '/Library/Application Support/UnionC Agent/config.json' \
  --server https://unionc.example.com
```

主要路径：

| 内容 | 路径 |
|---|---|
| 程序 | `/usr/local/libexec/unionc-agent` |
| 命令链接 | `/usr/local/bin/unionc-agent` |
| 配置、身份、凭据和 spool | `/Library/Application Support/UnionC Agent` |
| LaunchDaemon | `/Library/LaunchDaemons/com.unionc.agent.plist` |
| 卸载器 | `/usr/local/share/unionc-agent/uninstall.sh` |
| 日志 | `/var/log/unionc-agent.log` |

配对后的只读验证与日志：

```bash
sudo -u _unioncagent /usr/local/bin/unionc-agent status --output human \
  --config '/Library/Application Support/UnionC Agent/config.json'
sudo -u _unioncagent /usr/local/bin/unionc-agent doctor --output human \
  --config '/Library/Application Support/UnionC Agent/config.json'
sudo launchctl print system/com.unionc.agent
sudo tail -n 100 /var/log/unionc-agent.log
```

重新安装 pkg 时，preinstall 会先停止日志轮转和当前 Agent；postinstall 保存配置与状态并重新启动。
日志达到 10 MiB 后由 root LaunchDaemon 调用 `newsyslog`，保留七份压缩归档。

默认卸载：

```bash
sudo /usr/local/share/unionc-agent/uninstall.sh
```

永久本地清理：

```bash
# 先在 Web 撤销实例
sudo /usr/local/share/unionc-agent/uninstall.sh --purge

# 无人值守环境
sudo /usr/local/share/unionc-agent/uninstall.sh --purge --yes
```

交互 purge 要求输入 `PURGE`。root-only ownership marker 只允许删除本 pkg 实际创建且未被
改造的 `_unioncagent` 用户/组；冲突时保留 marker、receipt 和卸载器并返回非零，修复后
可以重试。

## 6. 发布签名和供应链验证

`.github/workflows/release.yml` 区分两类运行：

- `workflow_dispatch`：生成可供测试的开发制品，Windows MSI 与 macOS pkg 可以未签名且会
  明确作为开发产物；
- `v*` tag：强制生产签名。缺少任何必要 secret 会失败，不发布降级的未签名制品。

由于 MSI ProductVersion 的约束，包含 Windows 制品的 tag 必须是严格的
`vMAJOR.MINOR.PATCH`，且字段范围为 `255.255.65535` 以内；预发布标识应写在 Release 说明
而不是 MSI 版本字段中。tag 版本还必须与 `unionc-agent` 的 Cargo package version 精确
一致，避免制品元数据与 Agent 自报版本不一致。

正式发布所需 GitHub Actions secrets：

| 平台/用途 | Secret |
|---|---|
| Windows Authenticode PFX | `WINDOWS_SIGNING_PFX_BASE64` |
| Windows PFX 密码 | `WINDOWS_SIGNING_PFX_PASSWORD` |
| macOS 证书 P12 | `MACOS_SIGNING_P12_BASE64` |
| macOS P12 密码 | `MACOS_SIGNING_P12_PASSWORD` |
| Developer ID Application identity | `MACOS_APPLICATION_IDENTITY` |
| Developer ID Installer identity | `MACOS_INSTALLER_IDENTITY` |
| App Store Connect API 私钥 | `APPLE_NOTARY_KEY_P8_BASE64` |
| API key ID / issuer ID | `APPLE_NOTARY_KEY_ID` / `APPLE_NOTARY_ISSUER_ID` |
| Release manifest GPG 私钥 | `LINUX_SIGNING_KEY_BASE64` |
| GPG 密码 | `LINUX_SIGNING_KEY_PASSPHRASE` |
| 固定 GPG 指纹 | `LINUX_SIGNING_KEY_FINGERPRINT` |

tag 发布会执行：

- Windows Agent、托盘伴侣和原生维护程序先分别 Authenticode 签名并验证，再由 WiX 4.0.6
  构建 MSI，最后签名并验证 MSI；四个制品必须使用同一签名者；
- macOS universal Mach-O 使用 Hardened Runtime 和 Developer ID Application 签名；
- pkg 使用 Developer ID Installer 签名，再通过 `notarytool`、staple 和 Gatekeeper 验证；
- 对所有制品生成 `SHA256SUMS`、分离 GPG 签名和 GitHub provenance attestation。

GPG 公钥随 Release 提供只是传输便利；使用方仍应通过组织网站、配置管理或其他独立渠道
固定并核对指纹。APT/YUM 仓库运营方还必须签名仓库元数据。

下载后的最低验证流程：

```bash
# 公钥和指纹必须先通过独立渠道确认
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum --check SHA256SUMS

# 可选：验证 GitHub 构建 provenance
gh attestation verify unionc-agent_ARTIFACT --repo OWNER/REPOSITORY
```

Windows 安装前使用组织固定的证书发布者或指纹验证 MSI；安装后还可验证落地的 Agent：

```cmd
signtool.exe verify /pa /all /v UnionC-Agent-0.3.2-x64.msi
signtool.exe verify /pa /all /v "%ProgramFiles%\UnionC Agent\unionc-agent.exe"
signtool.exe verify /pa /all /v "%ProgramFiles%\UnionC Agent\unionc-agent-tray.exe"
```

也可以在文件属性的“数字签名”页检查发布者和时间戳。只看到“签名有效”还不够，仍须与组织
独立渠道固定的发布证书身份核对。

macOS 安装前验证 Installer 签名、Gatekeeper 和 stapled ticket：

```bash
pkgutil --check-signature unionc-agent_VERSION.pkg
spctl --assess --type install --verbose=4 unionc-agent_VERSION.pkg
xcrun stapler validate unionc-agent_VERSION.pkg
```

## 7. 自动验证

发布工作流会执行：

- Linux：脚本隔离测试、真实 DEB install → remove → reinstall → purge，以及 Fedora 容器内
  的真实 RPM install → remove → reinstall → purge；
- Windows：原生 Service/托盘/维护程序编译与 PE subsystem 校验、WiX 静态与 ICE 构建验证，
  以及真实 MSI 的不可信预置状态拒绝、同名 foreign task/service 拒绝、托盘文件
  与 Run/开始菜单注册、运行中托盘优雅关闭、fresh install → preserve uninstall → same-version reinstall →
  `PURGE=1`，并验证 service SID、配置、owner 和 DACL；
- macOS：shell/plist 与账户/嵌套组 fail-closed mock 校验，以及 pkg install → reinstall →
  preserve uninstall → reinstall → purge；正式 tag 额外执行 codesign、pkg signature、
  notary、stapler 和 Gatekeeper 验证。

Rust 三平台编译和 Agent 功能测试继续由常规 CI 承担。包生命周期测试不替代真实组织环境
中的代理、私有 CA、GPO/MDM、EDR 和系统升级兼容性验收。

工作流调用的校验入口与包定义放在一起，可在对应平台复现：

| 平台 | 入口 |
|---|---|
| Linux 静态生命周期 | `agent/packaging/linux/tests/test-lifecycle.sh` |
| Linux DEB / RPM | `agent/packaging/linux/tests/smoke-deb.sh` / `smoke-rpm.sh` |
| Windows PE / WiX / MSI | `agent/packaging/windows/tests/Test-*.ps1` |
| macOS shell/plist / pkg | `agent/packaging/macos/tests/validate-packaging.sh` / `smoke-pkg.sh` |

DEB、MSI 和 pkg smoke 会修改操作系统安装状态，只能在一次性测试机运行；Linux/macOS
脚本要求显式传入 `--allow-system-changes`，Windows 脚本会拒绝已有安装或状态的机器。

## 8. 故障处理

- 安装失败：先看安装器返回的原始错误和回滚错误；不要删除保留状态来“重试”。
- 服务未启动：Linux 使用 `systemctl status`/`journalctl -u`；macOS 使用
  `launchctl print system/com.unionc.agent` 和 `/var/log/unionc-agent.log`；Windows 从托盘
  打开本地状态页面。随后以服务账户运行 `status --output human`。
- 状态或诊断：`status` 和默认 `doctor` 都是只读操作；需要真正发送报文时必须显式使用
  `doctor --delivery`，该模式可能补传并确认删除已有 spool。
- 配对未完成：重新运行 `pair` 会恢复已持久化的 pending request，过期后才创建新请求。
- 凭据 revoked：管理员显式创建同一实例的重新配对邀请；不存在直接 token 恢复入口。
- purge 不完整：Linux/macOS 修复被改造的专用账户或占用关系后使用保留的清理工具重试；
  Windows 检查固定 quarantine，保留 MSI 日志并在释放占用后由管理员定点清理。
- 主机已经不可访问：立即在 Web 撤销实例，本地清理只能等介质恢复或销毁时完成。
