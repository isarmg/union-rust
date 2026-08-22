# 贡献指南

## 环境要求

| 组件 | 要求 |
|---|---|
| Rust | stable（当前锁定依赖要求 1.95+；edition 2024 本身要求 1.85+） |
| SQLite | 随 Server 二进制内嵌，无需单独安装 |
| Node.js | 24 LTS 或更高（前端） |

服务端**仅支持 Linux**（`lib.rs` 有 `compile_error!` 固定该约束）；Agent 支持
Linux / Windows / macOS。在非 Linux 上开发 Agent 时需显式指定 `-p unionc-agent`，
否则 workspace 默认成员会把服务端一并拉进来。

## 构建与测试

```bash
cargo build --workspace
cargo test --workspace
```

### 服务端测试使用隔离的 SQLite 文件

常规持久层测试会在系统临时目录创建各自的 SQLite 数据库，并真实执行当前 schema 初始化、事务、
保留期清理和 Agent 端到端路径，不需要启动外部数据库服务；长期开发环境可按系统临时
目录策略清理遗留的测试数据库：

```bash
cargo test -p unionc
```

不要把测试内部的数据库路径改成生产数据目录。项目只测试当前版本在空目录中创建的
数据库，不测试或承诺任何旧 Server 数据库的就地升级，也不得把旧格式转换、回填或导入
路径重新接入正常构建。schema 发生变化时直接更新当前基线和合同测试；现有部署必须导出
仍需保留的数据后全新部署。

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
代码库里大量此类注释（如 `spool.rs` 的隔离文件预算、`store.rs` 的报文体保留策略）
是刻意维护的资产。

### 上报间隔的三层边界必须联合评估

三层约束相关但刻意不完全相同，修改任一层都要检查另外两层及 jitter 测试：

- `server/src/monitoring/model.rs`：HTTP 报文中的**实测间隔**权威契约为 `[0.1, 3600]`；
- `agent/src/config.rs`：配置是整数秒（最小 1），并保证 jitter 后最坏实测周期不超过 3600，
  `MIN/MAX_REPORT_INTERVAL_SECONDS` 保护生成报文；
- `server/schema/sqlite.sql`：存储层只设 `(0, 3600]` 粗粒度 CHECK，
  用于拦截损坏数据，不能代替 Rust 入口校验。

### 只维护一个当前 schema

`server/schema/sqlite.sql` 是当前版本唯一的数据定义。schema 变更直接
更新这份基线及 `infra/database/mod.rs` 中的当前指纹；不要新增用于读取、转换或回填旧
schema 的代码。Server 只接受空目录或与当前基线精确一致的数据库。

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
3. 涉及行为变更时同步更新 `DOCUMENTATION.md` 与 `docs/` 下相应文档
4. 安全相关问题请勿走 PR，见 [SECURITY.md](SECURITY.md)

## 许可

提交即表示你同意你的贡献以 MIT 与 Apache-2.0 双许可发布。
