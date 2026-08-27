# 本地开发运行手册

开发的目的不是模拟模块独立部署，而是验证 Union 组装边界。

## 前置依赖

- Rust toolchain、Node/npm；
- PostgreSQL（Sunshine/Host 各自 role/schema；Sentinel/Photo 各自专用 database/role）；
- 含 Sentinel 时需要配置受限 MediaMTX 伴随进程；
- Dufs 使用 `.runtime/server/modules/dufs/dufs.yaml` 和自己的 SQLite。

创建私有数据目录并从仓库根运行：

```bash
install -d -m 0700 .runtime/server
export UNIONC_DATA_DIR="$PWD/.runtime/server"
export UNIONC_ENV=development
# 按所选 feature 设置 unionc.env.example 中的 UNIONC_*。
cargo run -p unionc --no-default-features \
  --features module-sunshine,module-host-monitoring
```

worker binary 必须位于 Union 预期的发行布局。日常端到端开发建议先用 Builder profile 生成
调试发行，再从该发行运行；直接 `cargo run` 适合核心单元调试，不能证明 supervisor 布局
或安装回滚。

前端开发：

```bash
cd web
npm ci
npm run dev
```

迁移实验只能使用旧版 `unionc.db` 的只读副本和隔离 PostgreSQL schema。不要把旧域表连接
到新 Union 在线路径，不要在线双写，也不要用开发结果宣称生产迁移已通过。新 Union 仍会为
核心审计创建自己的 core-only `unionc.db`，不要把两者混淆。
