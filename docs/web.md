# Union 管理前端

React 前端是 Union release 的组成部分，不是独立部署物。`union-builder` 在构建同一 profile
时执行锁定的 `npm ci` 与 `npm run build`，把产物安装到 `share/union/web`；所选模块静态
资源安装到 `share/union/modules/<id>`。前后端和 worker 必须来自同一 release manifest。

本地开发可以单独启动 Vite：

```bash
cd web
npm ci
npm run dev
```

Vite 只用于开发反馈；正式制品不得把某个开发目录的 `dist` 手工复制到生产。导航由编译期
catalog 决定，未选择模块不会出现。Agent 激活页属于远端 companion 配对流程；浏览器不会
接收长期 Agent secret。

公网反代只指向 Union，不能分别反代 worker。SPA、API、SSE 与 `/modules/<id>` 都处于同一
Union origin。示例见 [Caddyfile.console.example](examples/caddy/Caddyfile.console.example)。
