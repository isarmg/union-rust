# 需求、能力与边界

## 产品需求

Union 必须作为唯一服务端产品交付，并满足：

1. `minimal`、`storage`、`monitoring`、`full` profile 在构建期形成确定模块图；
2. 五个模块在运行时使用独立进程，由 Union supervisor 管理；
3. 唯一公网入口实施 TLS、核心登录/CSRF、请求清洗和固定路由；
4. worker 只绑定回环，并验证 `gateway-v1` protocol/audience/token/prefix；
5. 发行包包含 Union、所选 worker、前端、模块静态资源和 SHA-256 manifest；
6. `union-builder` v1.0.0 是唯一组合、校验、安装和回滚入口；
7. 数据 owner 清晰，迁移可核验且不在线双写。

## 模块能力

| 模块 | 核心职责 | 持久层 |
|---|---|---|
| Sunshine | 多主机配置、状态、受控代理、凭据审计 | PostgreSQL `sunshine` |
| Host monitoring | Agent 配对、认证、上报、最新状态与历史查询 | PostgreSQL `host_monitoring` |
| Sentinel | 摄像头/流配置、状态协调、受限媒体入口 | 专用 PostgreSQL database/role |
| Photo | TLS 传输、分片、哈希/去重、元数据、缩略图、Range/ETag | 专用 PostgreSQL database/role + 明文内容 |
| Dufs | 通用文件浏览、上传下载和目录权限 | 模块私有 SQLite + 文件根 |

Dufs 与 Photo 共享 blob-transfer 合同、错误 envelope、哈希、Range 等基础语义，不共享业务
表或强行合并领域。Photo 的资产、相册、时间线和媒体派生语义不应进入 Dufs；通用目录浏览
和任意文件树不应进入 Photo。

## 数据规则

- Sunshine/Host 使用独立 runtime role、migration role、schema 和 migration history。
- Sentinel/Photo 各自使用专用 PostgreSQL database/role 和 migration history。
- 禁止跨 owner 外键、JOIN、写事务和“公共业务表”。
- Dufs SQLite 是明确例外，只服务自身文件索引/配置，不成为上游共享数据库。
- Union 核心运行时只读写审计等平台状态；0.4.0 schema 可物理保留 Sunshine/Host 旧表，
  但只供离线导入与 rollback evidence 使用，在线 route/repository 不得访问。
- 切换后禁止双写；回滚前必须确认新 PostgreSQL 写入如何保留或导出。
- Photo 服务端文件为未加密内容；HTTPS 只保护传输。静态/备份加密是部署层能力。

## 安全边界

- 外部只可连接 Union；18101–18105 是内部实现细节。
- gateway token 是进程间 capability，不是用户、设备或模块管理员身份。
- Agent 零入站端口，服务端不执行远程命令、脚本、文件传输或 Agent 自更新。
- Photo 手机客户端与 Agent 是远端 companion，不进入服务端 feature 图。
- 配置不能改变 worker executable、port、prefix 或 audience。

## 非目标

多租户/RBAC、动态插件市场、热加载、模块独立部署/Release、多 Server active-active、跨模块
事务、自动接管任意旧数据库、应用层端到端照片加密和内置 PostgreSQL/MediaMTX 运维均不是
本阶段承诺。

## 完成判定

不能用“代码已写”代替验收。发布候选至少要记录：

- 四个官方 profile 的 `check`、`plan`、`build`、`verify`；
- 未选择模块在 binary/layout/catalog/route 中均缺席；
- 五个 worker 的 gateway 拒绝、健康回显、崩溃退避和优雅关机测试；
- PostgreSQL migration、离线 import/verify 以及回滚证据演练；
- Photo HTTPS 与服务器明文读取验证；
- Builder 两个完整 release 间 install/rollback 和 manifest 篡改拒绝；
- 目标 Linux、PostgreSQL、反代和 companion 版本。

本文定义要求，不声称当前机器已完成上述生产验收。
