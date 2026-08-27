# UnionC PostgreSQL target migrations

这些 migration 固定平台化后的数据库所有权边界，但当前 UnionC 运行时仍使用 SQLite；在以下
条件完成前不得切换生产默认值：

1. SQLite → PostgreSQL 导入器能在生产数据副本上验证行数、外键、JSON 和密文原样迁移。
2. PostgreSQL 备份、恢复、readiness 和 migration rollback runbook 已演练。
3. Sunshine 与主机监控已经通过模块注册运行，且没有跨 schema SQL/外键。
4. 全部 SQLite 专用测试有等价 PostgreSQL 集成测试，切换后删除 SQLite 运行时而不是长期双栈。

部署预先创建 `core`、`sunshine`、`host_monitoring` 三个 schema。每个目录由自己的 migrator
执行，并在各 schema 中维护独立 `_sqlx_migrations` 表。模块之间通过平台 API 和不透明 ID
通信；禁止增加跨 schema 外键。
