# 本地开发运行手册

本地开发要分别验证 Core、标准模块包和组装后的发行边界。直接运行某个 worker 适合模块单元测试，
不证明 Union Gateway、运行期配置、动态 Web 或发行完整性。

## 前置依赖

- Rust toolchain、Git、Node.js/npm；
- 启用 Sunshine、Host、Sentinel 或 Photo 时，为每个模块准备专用 PostgreSQL database/role；
- 启用 Sentinel 时，按其配置 Schema 准备受限 MediaMTX 伴随进程；
- 启用 Dufs 时，准备仅属于 Dufs 的 SQLite 状态目录和 rooted filesystem；
- Core 使用自己的 SQLite 数据目录，不与任何模块共享数据库。

## Core-only 开发

从仓库根启动未组装模块包的 Core：

```bash
install -d -m 0700 .runtime/server
export UNIONC_DATA_DIR="$PWD/.runtime/server"
export UNIONC_ENV=development
cargo run -p unionc
```

这会运行同一个 v0.5 Core，但当前发行模块目录为空，适合认证、平台 API 和基础设施单元调试。
不要用 Cargo feature 把业务代码链接进 Core；服务端不存在 `module-*` 业务 feature。

## 发行级联调

完整联调应让 Union Builder 2.1 用 schema v2 配置构建一个 debug 发行。配置决定哪些模块包被
纳入，并为每个来源锁定 revision；它不决定模块运行时是否启用。

```bash
cd ../union-builder
cargo run -- check --config /path/to/local-v2.toml
cargo run -- plan --config /path/to/local-v2.toml
cargo run -- build --config /path/to/local-v2.toml --profile debug
```

不要假定仓库中的官方 profile 已经适合当前未提交工作树；发布前必须替换其中明确标记的 revision
占位并通过 `check`。从生成目录的 `bin/unionc` 启动，Core 才能从同一发行根的 `modules/<id>`
读取标准包和从 `share/union/web` 提供 Shell。也可以为隔离开发显式设置绝对路径
`UNIONC_BUNDLED_MODULES_DIR` 和 `UNIONC_PLUGIN_STATE_DIR`，但这只是本地覆盖，不是生产安装布局。

模块首次发现为 disabled。登录 Web 的“设置 → 模块管理”，或使用受登录/RBAC/CSRF 保护的
`/api/platform/modules/<id>/configuration` 与 `enable` 接口，先按包内 JSON Schema 保存完整
配置，再启用模块。数据库 URL、密钥、存储目录和领域参数属于模块配置；Core 环境变量不能选择
模块，也不能修改 worker executable、bind、路由或包内容。

## Web Shell 开发

```bash
cd web
npm ci
npm run dev
```

Vite 默认把 `/api` 代理到 `http://127.0.0.1:8081`，可用 `UNIONC_DEV_API_TARGET` 覆盖。它适合
Shell、设置页和 API 反馈；模块 ESM 必须仍从 Union 的同源 `/modules/<id>/assets/*` 提供，因而
完整动态模块加载应在组装发行或等价的同源开发代理下验证。正式制品必须由 Builder 组装，不能把
手工 `dist` 复制到生产。

## 数据与验收边界

迁移实验只能使用旧 `unionc.db` 的只读副本和隔离数据库。Sunshine、Host、Sentinel、Photo
各使用专用 PostgreSQL database/role；Dufs 与 Core 各用自己的 SQLite/目录。禁止在线双写、跨
模块查询或把旧业务表重新接入 Core 请求路径。

生产 provisioning、逐库 ACL 验证、备份恢复和凭据轮换不使用开发环境变量；遵循
[PostgreSQL 隔离运行手册](postgresql-isolation.md)，并把各模块 `database_url` 保存到 Union
配置中心。

单元测试、debug build 和本地启动都不等于生产验收。正式候选还需记录目标 Linux、反代、
PostgreSQL、文件系统、模块组合、故障恢复、备份恢复和升级/回滚结果。
