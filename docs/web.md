# Union Web Shell

Web 是 Union 发行的一部分，不是独立部署物。Core 是浏览器唯一连接的服务端入口；同一 Union
origin 提供 Shell、平台 API、模块 API、SSE 和模块静态资源。

## Shell 与模块前端边界

Builder 2.1 只构建一次 Core Web Shell，并将其放入 `share/union/web`。Shell 只包含基础布局、
登录状态、核心导航、权限门、设置、动态模块加载器和错误边界，不导入 Sunshine、Host、Sentinel、
Photo 或 Dufs 的业务页面。

每个发行模块包独立提供：

- `frontend.entry` 和同源样式资源；
- `components` 白名单；
- `/modules/<id>/*` 页面 route 与 menu；
- `/api/modules/<id>` API base 和所需 permission。

Core 从 Manifest 生成 `/modules/<id>/assets/*` 资源映射。Shell 校验运行 catalog 后加载模块 ESM，
并调用 `activate(hostSdk)`；模块复用 Shell 提供的 React runtime 和以模块 API base 为默认前缀的
客户端，不能注册 Manifest 未声明的页面或把自己的 React/ReactDOM 副本带入运行时。单模块下载、
激活或渲染失败由模块错误边界隔离，不应破坏登录、设置页或其他模块。

模块可以为某个已声明组件注册一个 Shell 主操作按钮。该贡献必须声明组件、标签以及可选的
模块自有 permission；Shell 只向拥有该 permission 的用户显示按钮，再以单调递增的
`actionRequest` 通知组件。模块完成消费后调用 `onActionRequestHandled`，避免路由往返或
重新渲染重复执行创建操作。

同源 ESM 是 Builder 验证并纳入发行的受信任供应链代码，不是浏览器沙箱。`hostSdk` 只是支持的
编程接口，不能阻止脚本直接调用同源 `fetch` 或读取非 HttpOnly 的 CSRF token；前端 route/API base/
permission 检查也只改善兼容性和界面行为。安全授权必须始终由 Core 的会话、RBAC、CSRF、Manifest
路由和 Gateway 在服务端执行。

## Catalog、导航与模块管理

`GET /api/platform/modules` 返回当前发行内模块的 Manifest 投影、enabled 状态、生命周期、健康
消息、PID、重启次数和已解析资源。设置页始终列出所有有效发行模块，包括 disabled 和
unconfigured；主导航只为已启用且当前用户拥有 permission 的模块生成 menu，并只加载这些模块
的 ESM。

管理员可以在设置页：

- 重新扫描当前发行的只读模块目录；
- 查看配置 Schema 和脱敏后的当前值；
- 保存完整 JSON 配置；
- 在配置完成后启用模块，或停用正在运行的模块。

服务器把 Manifest 声明的敏感配置显示为 `***`。Shell 不允许把该占位符原样 PUT 回去；管理员
必须替换全部隐藏值并明确确认。界面没有模块代码安装、升级、卸载、上传或商店入口。升级模块或
改变发行包含必须构建和激活新的完整 Union 发行，但启停现有模块不需要重建 Shell。

前端权限过滤只改善界面可用性，不是安全边界。Core 必须对每次平台认证路由执行 RBAC/CSRF；
Manifest 标记为模块领域认证的端点仍由相应 worker 校验 Agent、移动端、ACL 或媒体凭据。

## 构建与开发

本地 Shell 反馈：

```bash
cd web
npm ci
npm run lint
npm run typecheck
npm test
npm run dev
```

Vite 只代理 `/api`。完整模块资源加载必须通过组装发行或等价的同源开发代理验证。正式构建由
Builder 锁定依赖并调用 `npm ci`/`npm run build`；模块自有前端也由 Builder 分别构建或复制其
版本化 ESM，再进入 `modules/<id>/frontend`。不得把开发目录中的 `dist` 手工复制到生产。

公网 TLS 反代只能指向 Union，不能为任一 worker 建独立站点或 upstream。示例见
[Caddyfile.console.example](examples/caddy/Caddyfile.console.example)。源码测试和本地 bundle
扫描通过不代表浏览器矩阵、真实反代、CSP、大文件或生产升级已经验收。
