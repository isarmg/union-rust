# Windows 原生安装生命周期

Windows 的首选制品是 x64、per-machine 的 WiX MSI：

```text
UnionC-Agent-<version>-x64.msi
```

终端用户可以双击安装，并在 Windows“设置 → 应用 → 已安装的应用”中卸载。安装、同版本
重装、服务注册和卸载均由 Windows Installer 与原生维护程序完成，**不要求系统安装或启用
PowerShell**。0.5.0 不提供跨版本升级或状态迁移；检测到同一 UpgradeCode 的其他版本时会
fail closed，必须用与已安装产品同版本的可信 MSI 执行同一次 `/x ... PURGE=1` 永久清理，
再安装、创建新实例并配对。

当前只发布 `x86_64-pc-windows-msvc` 制品；尚未提供 ARM64 MSI，也不把 Windows 的 x64
仿真能力声明为原生 ARM64 支持。

## 运行模型与固定路径

- 服务程序：`%ProgramFiles%\UnionC Agent\unionc-agent.exe`
- 用户托盘伴侣：`%ProgramFiles%\UnionC Agent\unionc-agent-tray.exe`
- 配置、身份、令牌和 spool：`%ProgramData%\UnionC Agent`
- SCM service name：`UnionCAgent`
- 显示名称：`UnionC Agent`
- 账户：`NT AUTHORITY\LocalService`
- 启动类型：Automatic
- ImagePath 参数：
  `--windows-service run --config "%ProgramData%\UnionC Agent\config.json"`
- 登录自启动：
  `HKLM\Software\Microsoft\Windows\CurrentVersion\Run\UnionCAgentTray`
- 开始菜单：`UnionC Agent`

全新安装由原生维护程序在受保护状态根中写入一份不含凭据的最小默认
`config.json`；它只用于让尚未配对的服务健康常驻，随后由 `pair` 原子更新。同一 0.5.0
包的重装和普通卸载都不覆盖已有配置。

Agent 是原生 Windows Service，处理 SCM 的 Stop 和 Shutdown 控制。安装器把 service SID
设为 `Unrestricted`，状态树只允许 SYSTEM、Administrators、专属的
`NT SERVICE\UnionCAgent` 以及受限的 OWNER RIGHTS；共享 LocalService SID 不获得状态
目录访问权。安装器同时启用“非崩溃失败也执行恢复动作”，因此服务主动以非零状态停止时
同样由 SCM 在 60 秒后重启，并在一天后重置失败计数。

托盘伴侣不是服务，也不采集或上报遥测。它以每个已登录用户的普通权限运行，在任务栏通知
区域提供以下入口：

- 打开只监听随机 `127.0.0.1` 端口的本地配置页；
- 配对；
- 自动或手动检测已填写 Server 的 `/api/health` 可达性；
- 查看、启动或停止 `UnionCAgent` 服务；
- 停止本次开机中的 `UnionCAgent` 服务并彻底退出托盘。

连接检测采用 2 秒连接超时、4 秒总超时，拒绝重定向并限制响应为 16 KiB。它只说明当前
桌面会话能否到达 UnionC 管理端；管理台中的主机 online/offline 仍以最近一次通过凭据验证的
遥测上报为准。

配对和服务启停属于机器级操作，只有在用户明确选择后才由 Windows UAC 启动
固定的原生提权模式；普通托盘进程不能读取 `%ProgramData%` 中的主机凭据。浏览器配置页
使用每次启动随机生成的 capability URL，只绑定 loopback，不应收藏、复制给其他人或配置到
反向代理。用户菜单“退出”会通过固定 UAC 模式停止 `UnionCAgent` 服务，只有确认服务停止后
才关闭当前托盘；如果用户拒绝 UAC 或停止失败，托盘保持打开。该路径只调用 SCM 的 Stop，
不会禁用服务或修改 `Automatic` 启动类型，所以下次启动 Windows 时 Agent 服务仍会自动运行。
本次开机内从开始菜单重新打开托盘不会暗中重启服务，用户可在页面中明确选择启动。

## 安装、配对和同版本重装

交互安装可直接双击 MSI。无人值守安装使用管理员终端：

```cmd
msiexec.exe /i UnionC-Agent-0.5.0-x64.msi /qn /norestart /l*v "%TEMP%\unionc-agent-install.log"
```

交互双击全新安装成功、且没有映像等待重启替换时，MSI 会在当前用户会话中启动托盘；
静默部署不会从 SYSTEM 会话启动任何托盘进程，用户下次登录时才由 Windows 启动。若当前
会话未出现图标，可从开始菜单运行“UnionC Agent”。随后右键托盘图标选择“配对”，
在本地页面一次填写服务器 HTTPS 地址和管理台生成的一次性授权密钥，
确认后接受一次 UAC 提权即可。Agent 自行建立请求、提交授权密钥并轮询结果，不会再打开
远程激活页要求二次输入。授权密钥只传给本次提权进程，不写入 `%LOCALAPPDATA%` 托盘偏好。

配对失败会恢复服务进入配对前的运行状态；配对成功时，只有原先正在运行的服务才会被重新
启动。CLI 仍作为故障排查入口，可在管理员命令提示符中执行：

```cmd
sc.exe stop UnionCAgent
"%ProgramFiles%\UnionC Agent\unionc-agent.exe" pair --config "%ProgramData%\UnionC Agent\config.json" --server https://unionc.example.com
sc.exe start UnionCAgent
```

WiX 只用 `UpgradeVersion OnlyDetect=yes` 检测同一产品族的其他版本，不编排
`RemoveExistingProducts`，也没有 major-upgrade rollback 或故障注入路径。0.5.0 的 repair/
reinstall 仍使用相同 ProductCode 和当前事务快照。repair 和卸载会在 Windows Installer
事务开始后、`StopServices`、`RemoveShortcuts` 和 `RemoveFiles` 之前，向托盘发送 `WM_CLOSE`
并等待其优雅退出；不强杀跨会话进程，文件仍被占用时走 Windows Installer 标准重启机制。
只有 fresh interactive install 会在事务提交后通过 WiX unelevated ShellExecute helper 启动
托盘；repair、卸载、静默/SYSTEM 部署不启动。

安装前原生维护程序只检查当前 0.5.0 SCM 服务、固定程序路径和当前精确 ACL。任何同名但
ImagePath、参数、账户或类型不同的服务都会使安装 fail closed；计划任务和旧脚本部署不会被
识别、停止、接管、删除或迁移。

## 状态接管保护

普通用户可能在首次安装前预置 `%ProgramData%\UnionC Agent`。为避免安装器把恶意
`config.json` 收紧 ACL 后交给 LocalService 使用，原生维护程序只接受以下状态之一：

- 本轮开始时目录不存在；
- 存在当前 0.5.0 创建的、版本绑定的 managed-state marker，且其内容、owner、精确 DACL、
  文件类型、link count 和状态根目录全部通过联合检查。这只用于普通卸载后的同版本重装。

状态根若是普通文件、junction、符号链接或 dangling reparse point，或者状态树含特殊文件、
reparse point 或多硬链接文件，都会在任何 ACL/服务变更前被拒绝。marker 不是一个可脱离
ACL 单独信任的“魔术文件”：维护程序同时验证其固定内容、普通单链接文件类型，以及
marker/状态根的完整精确 ACL。安装状态下专属 service SID 按设计可修改状态树；普通卸载
会移除该 ACE 并保留 marker，purge 则随整个状态树清理。程序树另行拒绝 reparse point、
特殊文件和多硬
链接文件；MSI 应用后会强制并复核 SYSTEM owner、Administrators 完全控制和 service SID
只读/执行权限，并为 BUILTIN\Users 增加同样的严格只读/执行权限，使普通用户能够启动托盘但
不能修改已安装程序。该权限不扩展到 ProgramData 状态树。旧 marker、无版本 marker 和旧的
service-only 三 ACE 程序 ACL 均被拒绝，不会原地改写为当前格式。

## 普通卸载与永久清理

从“已安装的应用”卸载，或执行下面的命令，默认删除服务和程序，但保留配置、主机身份、
凭据、配对进度及未发送 spool：

```cmd
msiexec.exe /x UnionC-Agent-0.5.0-x64.msi /qn /norestart /l*v "%TEMP%\unionc-agent-uninstall.log"
```

普通卸载会递归移除可由管理员重建的 service SID ACE，只留下 SYSTEM、Administrators 和
OWNER RIGHTS 安全边界。同一 0.5.0 包重装时通过版本绑定 marker 复用身份；其他版本不能接管。

永久退役必须先在 UnionC Web 管理台永久删除实例，再显式传入唯一允许的 `PURGE=1`：

```cmd
msiexec.exe /x UnionC-Agent-0.5.0-x64.msi PURGE=1 /qn /norestart /l*v "%TEMP%\unionc-agent-purge.log"
```

Purge 先完整扫描状态树并拒绝所有 reparse point，然后在同一卷原子移动到受保护的固定
隔离名称 `%ProgramData%\UnionC Agent.purge-quarantine-0.5.0`。在事务进入 commit 之前，任一
失败都会回滚该 rename 并恢复完整状态；commit 阶段的递归删除则是不可逆操作。为避免把已
卸载的产品回滚到部分删除的状态树，commit 删除错误不会再触发 MSI 产品回滚，因而
`msiexec` 成功只证明产品已卸载，**不能单独证明本地凭据已经清空**。

普通卸载和 purge 都会移除 `unionc-agent-tray.exe`、开始菜单入口及机器级登录自启动项；
普通卸载仍保留受保护的机器身份和凭据，只有显式 purge 才清理这些状态。

每个 Windows 用户的 `%LOCALAPPDATA%\UnionC Agent\tray.json` 只保存精确的当前应用版本、
非敏感的 Server URL，不含设备名称或 Agent secret；缺少版本或版本不匹配的文件会被
拒绝。机器级 MSI 不枚举或修改其他用户配置文件，因此普通卸载
和 `PURGE=1` 都不承诺删除这些桌面偏好；需要时由对应用户自行删除。机器级 purge 仍会删除
ProgramData 中的身份、通信凭据、配对状态和 spool。

自动化在 purge 后必须同时确认原状态目录和上述隔离目录均不存在。若受占用、磁盘或安全
软件影响而留下受保护的隔离目录，应保留 MSI 日志，先重启释放占用，再由管理员只针对这个
固定路径核实并清理；在隔离目录消失前不得把退役标记为完成。若已做过普通卸载而后决定永久
清理，可先重新安装可信的 0.5.0 MSI（版本 marker 与精确 ACL 联合验证后接管），再执行带
`PURGE=1` 的卸载。

## 构建与签名

WiX 工程位于 `agent/packaging/windows/wix/`，固定使用 WiX Toolset 4.0.6。以下
构建和验证命令均从**仓库根目录**运行。先生成三个 x64 MSVC PE，再构建 MSI：

```cmd
cargo build --release -p unionc-agent --target x86_64-pc-windows-msvc --bin unionc-agent --bin unionc-agent-maintenance --bin unionc-agent-tray
agent\packaging\windows\wix\build-msi.cmd 0.5.0 ^
  target\x86_64-pc-windows-msvc\release\unionc-agent.exe ^
  target\x86_64-pc-windows-msvc\release\unionc-agent-maintenance.exe ^
  target\x86_64-pc-windows-msvc\release\unionc-agent-tray.exe
```

正式发布顺序必须是：先分别 Authenticode 签名并验证三个 EXE，再构建 MSI，最后签名并验证
MSI；四者必须具有同一有效签名者。`unionc-agent.exe` 必须是 Console PE，维护程序与托盘必须
是 Windows GUI PE，避免闪出命令窗口。WiX 构建不得关闭 ICE validation；tag 发布缺少证书
时必须失败，不能降级成未签名包。构建入口和 WiX 工程都会执行 Agent 的 `--version`，并要求
其输出精确等于 `unionc-agent 0.5.0`；不能用其他版本号包装当前二进制。

## 验证

静态/构建验证：

```powershell
.\agent\packaging\windows\tests\Test-PeSubsystems.ps1
.\agent\packaging\windows\tests\Test-WixAuthoring.ps1
```

PE 校验默认读取仓库的 `target\x86_64-pc-windows-msvc\release`，也可用
`-ReleaseRoot PATH` 指定已构建目录。生成 MSI 后，可在**管理员权限、没有既有 UnionC Agent
安装或状态的一次性 Windows VM** 中运行与 Release 相同的生命周期测试：

```powershell
.\agent\packaging\windows\tests\Test-MsiLifecycle.ps1 `
  -ProductVersion 0.5.0 -ArtifactDirectory .\dist
```

脚本会真实安装、卸载并 purge 软件；它会先拒绝不干净的机器，不能在生产或需要保留
UnionC Agent 状态的主机上执行。

发布前还必须在一次性、干净的 x64 Windows VM 中执行：恶意预置状态拒绝、同名 service
碰撞拒绝、其他 UpgradeCode 版本 fail closed、fresh install、同一 0.5.0 repair/reinstall、浏览器
配对、托盘单实例/本地 capability URL、登录自启动、运行中托盘的优雅卸载、fresh-install
rollback、普通卸载/当前 marker 重装、显式 purge，以及 program/state root junction 防护。
测试需同时核对 SCM 配置、service SID 类型、托盘不是
服务、Program Files 的 Users 只读执行权限、ProgramData 不授权 Users、Run/开始菜单项、
Apps & Features 条目、所有退出码，并在 purge 后确认状态根与固定 quarantine 都不存在。
