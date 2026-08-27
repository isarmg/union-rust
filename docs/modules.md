# 平台模块

> 目标边界已变更：五个业务模块都必须由编译 profile 选择并以 Union 私有进程运行，Union 是
> 唯一公共入口和 Release。本文后续“内置/服务模块”的描述是当前迁移状态，不是最终承诺。
> 完整需求见 sibling `upstream/REQUIREMENTS-AND-BOUNDARIES.md`，迁移门禁见
> `platform/docs/COMPILED-PROCESS-MIGRATION.md`。

UnionC 是 `sarmg-platform` 的首个发行组装程序。模块目录来自精确 Git revision 依赖导出的
版本化 manifest，而不是本机兄弟目录或前后端各自维护的名称列表。

## 当前编译图

`unionc` 当前定义三个可选 feature：

| Cargo feature | Worker | 默认启用 |
|---|---|---|
| `module-sentinel-monitor` | `sentinel-monitor` | 是 |
| `module-photo-backup` | `photo-backup-server` | 是 |
| `module-dufs` | `dufs` | 是 |

未选择的外部模块不会进入后端 catalog。`union-builder` 总是对 Union 使用
`--no-default-features`，再按组合清单显式加入 feature，因此 Cargo 的开发默认值不会污染正式
发行图。无外部模块、每个单模块以及默认三模块组合均由编译/测试覆盖。

Sunshine 与主机监控目前仍无独立 feature：它们总是编入 Union 进程。这是 PostgreSQL 数据迁移
和进程拆分完成前的明确过渡限制，不是最终模块模型。

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

当前过渡实现中，服务 URL 只来自启动环境，管理员 API 不能修改它，避免把通用模块探测器变成
SSRF 或开放代理。
探测客户端使用严格 TLS、三秒超时且不跟随重定向。未配置模块通过
`GET /api/platform/modules` 显式报告 `unconfigured`，但不会出现在导航。

首版不做 SSO：外部模块在新标签页打开并使用自己的登录。后续身份统一必须使用带 audience
和短有效期的签名票据或 OIDC，不能共享 Cookie、session 表或用户表。

## 添加模块

1. 在 `platform/modules/` 增加通过 `module-v1` 验证的 manifest。
2. 在 Union 增加唯一 `module-<id>` feature，并让 catalog、静态路由、前端入口和 supervisor
   定义都受同一 feature 控制。
3. 在 `union-builder` profile 固定 worker 仓库、完整 revision、package、binary、回环地址和
   gateway path；模块不能定义任意构建命令。
4. 增加 manifest、路由边界、导航、健康、未选择模块缺席和发行目录合规测试。
5. 明确数据库 profile；禁止跨模块表和外键。
