# 08. 数据库生命周期

当前架构以所有权而不是“统一一种数据库”为目标：

- Union Core：自己的 SQLite；运行时只读写审计等平台状态，当前文件必须精确匹配 Core schema；
- Sunshine/Host/Sentinel/Photo：各自专用 PostgreSQL database/role/migration/backup；
- Dufs：模块私有 SQLite 与文件根。

旧版 `unionc.db` 中的 Sunshine/Host 表只能作为**单独保存、离线只读**的迁移/回滚快照，不能
继续作为 v0.5 Core 的当前数据库；当前 schema 校验会拒绝多出的旧业务表。离线 importer 导入后
必须 verify，切换后不双写。只有验收和回滚窗口关闭后才能删除旧快照。rollback 需要同时考虑
PostgreSQL 新写入，不能只切回旧文件。
