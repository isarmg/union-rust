# 07. Web 前端

生产发行只构建一份 Core Web Shell，并放入 `share/union/web`。Builder profile 决定哪些
模块包随发行交付，但不把模块页面编译进 Shell，也不记录模块的运行期启用状态。
不能手工混用其他 revision 的 `dist` 或模块前端。

登录后，Shell 从 `GET /api/platform/modules` 读取运行时 catalog。设置页可显示发行已包含但尚未
启用或未配置的模块；导航和 ESM loader 只处理同时满足三个条件的模块：

1. Builder 已将它纳入当前不可变发行；
2. 管理员已在运行期启用它；
3. 当前用户至少拥有一条模块页面所需权限。

模块 ESM 通过 `activate(hostSdk)` 复用 Shell 的 React 和受模块 API base 限制的客户端。这个 Web
`hostSdk` 不是 Rust 进程模块的远程 Platform SDK；v0.5 尚没有面向 worker 的 Event Bus、任务、
通知或 SDK 审计线协议。前端权限过滤只改善交互，Core 或拥有领域凭据的 worker 仍须逐请求授权。

Shell 自带的总览只展示模块生命周期和服务状态。Core 不再采集整机 CPU、内存、网络、磁盘和
挂载点；这些能力属于 Host Monitoring 模块。

开发可运行 Vite，正式公网仍只有 Union origin。浏览器不能直连回环 worker；模块页面使用
固定 `/modules/<id>` 前缀，API 使用 `/api/modules/<id>` 前缀。
