# 08. 数据库生命周期

当前架构以所有权而不是“统一一种数据库”为目标：

- Union core：自己的 SQLite；运行时只读写审计等平台状态，0.4.0 仍可物理保留只读旧域表；
- Sunshine/Host：独立 PostgreSQL role/schema/migration/backup；
- Sentinel/Photo：各自专用 PostgreSQL database/role/migration/backup；
- Dufs：模块私有 SQLite 与文件根。

旧版 `unionc.db` 中的 Sunshine/Host 表只作为切换前只读迁移/回滚来源；当前 schema 保留它们
不表示 core 仍拥有这些业务。离线 importer 导入后必须 verify；切换后不双写。只有验收和回滚
窗口关闭后才能删除旧表。rollback 需要同时考虑 PostgreSQL 新写入，不能只切回旧文件。
