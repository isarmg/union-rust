# 平台模块

UnionC 是 `/mnt/sarmg.org/platform` 的首个发行组装程序。模块目录来自版本化 manifest，
而不是前后端各自维护的名称列表。

## 内置模块

- `sunshine`：编译期注册 console Router 和 React 视图；运行状态由自己的探测任务贡献。
- `host-monitoring`：编译期注册 console Router、独立认证的 Agent Router 和 React 视图；
  Agent 协议继续由 `protocol` crate 所有。

组装只发生在 `server/src/platform/mod.rs` 和 `web/src/app/moduleRegistry.tsx`。平台核心、认证、
系统健康和数据库基础设施不得反向导入模块 DTO。

## 服务模块

| 模块 | 环境变量 | liveness | 数据所有权 |
|---|---|---|---|
| Sentinel | `SARMG_SENTINEL_URL` | `/health/live` | 独立 PostgreSQL database/role |
| Photo Backup | `SARMG_PHOTO_BACKUP_URL` | `/health/live` | 独立 PostgreSQL database/role + 对象存储 |
| Dufs | `SARMG_DUFS_URL` | `/__dufs__/health` | 与共享根绑定的本地 SQLite |

服务 URL 只来自启动环境，管理员 API 不能修改它，避免把通用模块探测器变成 SSRF 或开放代理。
探测客户端使用严格 TLS、三秒超时且不跟随重定向。未配置模块通过
`GET /api/platform/modules` 显式报告 `unconfigured`，但不会出现在导航。

首版不做 SSO：外部模块在新标签页打开并使用自己的登录。后续身份统一必须使用带 audience
和短有效期的签名票据或 OIDC，不能共享 Cookie、session 表或用户表。

## 添加模块

1. 在 `platform/modules/` 增加通过 `module-v1` 验证的 manifest。
2. 进程内模块在后端组装根注册 console/public Router，并在前端注册懒加载视图。
3. 服务模块只声明固定健康路径和部署环境变量，不添加任意 URL 代理。
4. 增加 manifest、路由边界、导航和合规测试。
5. 明确数据库 profile；禁止跨模块表和外键。
