# 本地开发运行手册

本手册提供可重复的本地启动入口。目标是让从不同终端或 IDE 启动 Server 时都使用同一个
明确的数据目录，避免在 `unionc/data`、`server/unionc/data` 等位置产生多套配置、数据库
和主密钥。

## 数据目录约定

仓库内开发统一使用：

```text
<仓库根>/.runtime/server
```

`.runtime/` 已被 Git 忽略。它可能包含管理员密码哈希、Agent 凭据、Sunshine 密文、SQLite
数据库和开发主密钥，不应提交、共享或作为构建制品上传。

这个约定不会改变程序本身的兼容默认值：没有设置 `UNIONC_DATA_DIR` 时，Server 仍使用
`<当前工作目录>/unionc/data`。因此本地开发命令应始终显式设置环境变量。

## Linux 或 WSL

首选入口是仓库自带脚本；它会自行解析仓库根目录，所以也可以从其他工作目录调用：

```bash
./tools/dev-server.sh
```

脚本创建 `.runtime/server`，导出其绝对路径，切换到仓库根并通过根 `Cargo.toml` 执行
`cargo run -p unionc`。传入的参数会原样交给 Server，例如：

```bash
./tools/dev-server.sh integrity-check
```

需要让同一个终端中的后续维护命令复用数据目录，或无法使用脚本时，可手动设置：

```bash
export UNIONC_DATA_DIR="$PWD/.runtime/server"
cargo run -p unionc
```

结束开发会话后可执行 `unset UNIONC_DATA_DIR`，避免该值意外影响其他仓库。

## Linux / WSL 中的 PowerShell

Server 只支持 Linux。以下命令仅适用于运行在 Linux 或 WSL 内的 `pwsh`；不要在原生
Windows PowerShell 中执行 `cargo run -p unionc`，原生 Windows 只能直接开发 Agent 和 Web，
完整 Server 开发请进入 WSL。PowerShell 中从仓库根目录设置路径的方式是：

```powershell
New-Item -ItemType Directory -Force .runtime/server | Out-Null
$env:UNIONC_DATA_DIR = (Resolve-Path .runtime/server).Path
cargo run -p unionc
```

当前终端不再需要该设置时：

```powershell
Remove-Item Env:UNIONC_DATA_DIR
```

不要把 `UNIONC_DATA_DIR` 永久设置成某个仓库路径；多个 checkout 或测试任务需要彼此隔离。

## 同时启动 Web

Server 默认监听 `127.0.0.1:8081`。保持 Server 终端运行，另开终端：

```bash
cd web
npm ci
npm run dev
```

Vite 默认把 `/api` 代理到 `http://127.0.0.1:8081`。如使用其他 Server 端口，按
[Web 说明](../web.md)设置 `UNIONC_DEV_API_TARGET`。

## 隔离临时实验

不希望实验接触日常开发状态时，显式使用系统临时目录：

```bash
export UNIONC_EXPERIMENT_DATA="$(mktemp -d /tmp/unionc-experiment.XXXXXX)"
UNIONC_DATA_DIR="$UNIONC_EXPERIMENT_DATA" cargo run -p unionc
```

确认进程已经退出且数据不再需要后，再清理这个精确的临时目录。不要用宽泛通配符删除
`unionc/data`、`.runtime`、仓库根目录或生产数据目录。

## 已有开发数据

目录约定不会自动搬运以前生成的数据。若仓库中已有 `unionc/data` 或
`server/unionc/data`：

1. 先根据启动日志确认当前进程实际使用的是哪一个绝对路径。
2. 停止所有可能访问该数据库的 Server 和维护命令。
3. 需要保留数据时，优先使用 Server 的 `backup` 与 `restore` 命令；同时保留匹配的配置和
   密钥，详细边界见 [Server 说明](../server.md)。
4. 验证 `.runtime/server` 能完整启动并读取预期数据后，再自行决定是否清理旧目录。

不要只移动 `unionc.db` 而遗漏 `-wal`、`-shm`、配置或密钥，也不要在本次目录整理中直接
删除任何旧运行数据。
