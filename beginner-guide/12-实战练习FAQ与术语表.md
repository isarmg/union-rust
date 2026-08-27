# 12. 练习、FAQ 与术语

练习：比较 `minimal` 与 `full` 的 Builder plan；验证未选模块没有路由；向 worker 直接
发送缺少 gateway token 的请求；画出 Agent→Union→Host→PostgreSQL 链路。

- **profile**：确定 feature、revision、binary 和资源的构建清单。
- **worker**：由 supervisor 管理的私有业务进程，不是独立产品。
- **gateway-v1**：Union 到 worker 的内部身份合同。
- **companion**：部署在远端设备、随兼容矩阵管理但不在 server 进程树中的客户端。
- **rollback evidence**：证明离线导入前状态和导入映射的只读资料，不是在线数据库。

常见问题：为什么不把 Dufs 改成 PostgreSQL？因为它的本地文件/SQLite 边界更简单且没有共享
业务查询收益。为什么 Photo 服务端明文？需求只保护传输；服务器必须读取内容完成媒体处理。
