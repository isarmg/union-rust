# 贡献指南

## 环境要求

| 组件 | 要求 |
|---|---|
| Rust | 1.98.0（与 CI 和正式构建一致） |
| SQLite | Core 内嵌使用，无需单独安装 |
| Node.js | 24 LTS 或更高（前端） |

Core 服务端**仅支持 Linux**（`lib.rs` 有 `compile_error!` 固定该约束）。本仓库只维护 Core 和
Web Shell；Sunshine 与 Host worker 不属于本仓库 workspace，分别在
[`sunshine-worker`](https://github.com/isarmg/sunshine-worker) 和
[`host-monitoring`](https://github.com/isarmg/host-monitoring) 开发与验证。Host/Agent DTO 和跨平台
Agent 也由 `host-monitoring` 仓库维护、构建和测试。

## 构建与测试

本地启动 Server 时统一把运行数据放在仓库的 `.runtime/server`，避免从不同工作目录启动后
生成多个难以辨认的数据目录。完整的 Bash、PowerShell 示例见
[本地开发运行手册](docs/runbooks/development.md)。

```bash
cargo build --workspace
cargo test --workspace
```

### 数据层测试遵守模块所有权

Core 持久层测试会在系统临时目录创建隔离 SQLite。Sunshine、Host、Sentinel、Photo 的数据与
migration 测试属于各自模块仓库，必须在对应仓库中使用相互隔离的专用 PostgreSQL
database/role；Dufs 使用自己的临时 SQLite 和文件根。不能用 Core SQLite 单测替代模块数据库
集成测试：

```bash
cargo test -p unionc
```

任何测试都不得指向生产数据。旧 Union SQLite 只允许作为离线 importer 的只读副本，不得把
旧格式转换、回填或双写路径接回 Core 请求路径。模块 schema 变化由模块自己的 migration 和
合同测试负责，不能由 Core 建立跨模块表或共享 migration ledger。

### 提交前必须通过

以下是本仓库 Core 与 Web Shell 的本地门禁：

```bash
cargo fmt --all -- --check
cargo clippy -p unionc --all-targets -- -D warnings
cargo test --workspace
cd web
npm ci
npm audit --audit-level=high
npm run lint
npm run typecheck
npm test
npm run build
```

Host worker、protocol、Agent feature、真实 Collector 和跨平台安装器门禁必须在
`host-monitoring` 仓库执行；不能用本仓库的 Core 测试或一台 Linux 开发机上的本地通过代替。

## 代码约定

### 注释解释"为什么"，不复述"是什么"

这是本项目最重要的约定。好的注释说明**这里防的是哪个具体故障**：

```rust
// ✅ 在昂贵校验前占用名额，避免并发请求同时穿过限流检查。
// ❌ 把当前时间推入 attempts 向量。
```

修复缺陷时，请在注释里写清这里防的是什么故障、换成更直观的写法会表现为什么现象。
代码库里大量此类注释（如 `server/src/http/request_body_deadline.rs` 的源速率预约）
是刻意维护的资产。

### 上报间隔的三层边界必须联合评估

这三层代码均由 [`host-monitoring`](https://github.com/isarmg/host-monitoring) 仓库维护，相关改动和
测试应在该仓库完成：

- `host-monitoring` 仓库的 `protocol/src/report.rs` 与
  `host-monitoring-worker/src/model.rs`：HTTP 报文中的**实测间隔**权威契约为
  `[0.1, 3600]`；
- `host-monitoring` 仓库的 `host-m-agent/src/config.rs`：配置是整数秒（最小 1），并保证 jitter 后最坏实测周期不超过 3600，
  `MIN/MAX_REPORT_INTERVAL_SECONDS` 保护生成报文；
- `host-monitoring` 仓库的 `host-monitoring-worker/migrations/`：模块 PostgreSQL 存储约束用于
  拦截损坏数据，不能代替 Rust 入口校验。

正式 Union profile 必须以不可变 revision 纳入 Host worker；跨平台 Agent 由同一 Host 仓库产出
companion artifact、独立安装，并通过 Union 兼容矩阵与服务器发行对应。Agent 不进入 Union
服务器 distribution 或 Core supervisor。

### 每个数据 owner 只维护自己的当前 schema

Core SQLite、四个模块 PostgreSQL 和 Dufs SQLite 各有自己的 schema/migration 事实源。变更只能
落在数据 owner 内，不得跨库建外键、JOIN、事务或共享 migration。旧数据转换只能进入明确的
离线 importer；正常请求路径不读取旧 schema。

### 测试

用例名描述**行为**而非方法名，并在必要时注明它守的是什么边界：

```rust
/// 9 个 ASCII 字符 + 一个三字节汉字 = 12 字节，能通过长度检查。
/// 按字节切 `str` 会在此 panic（切点不在字符边界），必须返回 400。
#[test]
fn multibyte_input_is_rejected_instead_of_panicking() { ... }
```

新增测试请确认它**在缺陷存在时必然失败**——否则它守护不了任何东西。

## 提交 PR

1. 从 `main` 切分支
2. 保持提交聚焦，提交信息说明动机而非改动清单
3. 涉及行为变更时同步更新 `DOCUMENTATION.md` 与 [文档中心](docs/README.md) 中列出的相应主题文档
4. 安全相关问题请勿走 PR，见 [SECURITY.md](SECURITY.md)

## 许可

提交即表示你同意你的贡献以 [Apache License 2.0](LICENSE-APACHE) 发布。
