# 贡献指南

## 环境要求

| 组件 | 要求 |
|---|---|
| Rust | stable（当前锁定依赖要求 1.95+；edition 2024 本身要求 1.85+） |
| SQLite | Core 与 Dufs 各自内嵌使用，无需单独安装 |
| PostgreSQL | Sunshine、Host、Sentinel、Photo 集成测试各使用专用 database/role |
| Node.js | 24 LTS 或更高（前端） |

服务端**仅支持 Linux**（`lib.rs` 有 `compile_error!` 固定该约束）；Agent 支持
Linux / Windows / macOS。在非 Linux 上开发 Agent 时需显式指定 `-p unionc-agent`，
否则 workspace 默认成员会把服务端一并拉进来。

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
migration 测试属于各自 worker，必须使用相互隔离的专用 PostgreSQL database/role；Dufs 使用
自己的临时 SQLite 和文件根。不能用 Core SQLite 单测替代模块数据库集成测试：

```bash
cargo test -p unionc
```

任何测试都不得指向生产数据。旧 Union SQLite 只允许作为离线 importer 的只读副本，不得把
旧格式转换、回填或双写路径接回 Core 请求路径。模块 schema 变化由模块自己的 migration 和
合同测试负责，不能由 Core 建立跨模块表或共享 migration ledger。

OTLP live 测试需要真实 Collector，并由 `UNIONC_AGENT_TEST_REQUIRE_OTLP=1` 守护。它验证
Agent → Collector 的接收合同，不验证 Collector exporter、时序库落库或查询。

### 提交前必须通过

以下是核心本地门禁；它们刻意包含当前 CI 没有单独执行的 protocol 单测和 OTLP 官方 proto
解码测试。CI 还配置了三个 Agent 平台各自的打包/生命周期门禁和真实 Collector；前序步骤
失败时，后续平台门禁不会执行：

```bash
cargo fmt --all -- --check
cargo clippy -p unionc --all-targets -- -D warnings
cargo clippy -p unionc-agent --all-targets -- -D warnings
cargo test --workspace
# Agent 的 feature 分支平时不编译，必须单独覆盖
cargo check -p unionc-agent --no-default-features --all-targets
cargo check -p unionc-agent --no-default-features --features otlp --all-targets
cargo check -p unionc-agent --no-default-features --features nvidia --all-targets
cargo test -p unionc-agent --features otlp --test otlp_encoding
cd web
npm ci
npm audit --audit-level=high
npm run lint
npm run typecheck
npm test
npm run build
```

真实 Collector 的 `otlp_live` 需要先准备接收端；跨平台安装器门禁也必须依赖对应 CI runner，
不能用一台 Linux 开发机上的本地通过代替。

## 代码约定

### 注释解释"为什么"，不复述"是什么"

这是本项目最重要的约定。好的注释说明**这里防的是哪个具体故障**：

```rust
// ✅ 在昂贵校验前占用名额，避免并发请求同时穿过限流检查。
// ❌ 把当前时间推入 attempts 向量。
```

修复缺陷时，请在注释里写清这里防的是什么故障、换成更直观的写法会表现为什么现象。
代码库里大量此类注释（如 `agent/src/spool.rs` 的隔离文件预算、
`server/src/http/request_body_deadline.rs` 的源速率预约）
是刻意维护的资产。

### 上报间隔的三层边界必须联合评估

三层约束相关但刻意不完全相同，修改任一层都要检查另外两层及 jitter 测试：

- `protocol/src/report.rs` 与 `host-monitoring-worker/src/model.rs`：HTTP 报文中的**实测间隔**
  权威契约为 `[0.1, 3600]`；
- `agent/src/config.rs`：配置是整数秒（最小 1），并保证 jitter 后最坏实测周期不超过 3600，
  `MIN/MAX_REPORT_INTERVAL_SECONDS` 保护生成报文；
- `host-monitoring-worker/migrations/`：模块 PostgreSQL 存储约束用于拦截损坏数据，不能代替
  Rust 入口校验。

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
