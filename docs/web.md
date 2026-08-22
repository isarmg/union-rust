# 管理前端

UnionC 的 React 管理前端，提供总览、只读主机监控、Sunshine、Sunshine 日志和设置页面。
主机页显示 CPU、内存、GPU、逐接口网络、逐挂载磁盘、温度、采集能力和历史趋势；
缺失能力显示 `N/A`，没有任何远程控制按钮。

“创建 Agent”流程预留稳定实例并签发默认 15 分钟有效的一次性激活码。Agent 软件由用户
通过平台包管理器、MDM 或组织软件中心独立安装；管理台不托管安装包、不识别客户端平台，
也不生成 shell、PowerShell 或 pkg 安装命令。

已安装的 Agent 会给出 `/agent/activate/{requestId}` 页面。该页面无需管理员会话，只读取
有限的设备摘要供用户核对，并把一次性激活码提交给 Server。页面永远不会收到长期 Agent
secret。管理页还能取消待激活邀请、为同一实例重新配对，以及把实例持久标记为 `revoked`、
吊销其全部 credential，同时保留 tombstone 和历史。
当前 Windows Agent 中，管理台将该一次性值标为“授权密钥”，并引导用户到目标设备的
本机配置页同时填写 Server 地址和密钥；`/agent/activate/{request_id}` 仍保留给 CLI 和其他平台。
管理台不会向目标机器发起 SSH、WinRM 或任何远程命令。

## 开发运行

先启动 UnionC，然后运行：

```bash
cd web
npm ci
npm run dev
```

开发服务器先尝试监听 `127.0.0.1:3001`，并把 `/api` 代理到 `127.0.0.1:8081`。当 3001 已被占用时，Vite 默认会尝试后续端口，以终端打印的实际 URL 为准。
后端使用其他端口时，可通过 `UNIONC_DEV_API_TARGET=http://127.0.0.1:18081 npm run dev`
覆盖代理目标。

## 构建

```bash
npm run build     # 等价于 tsc -b && vite build && 原子发布到 dist/
```

构建产物位于 `web/dist/`。发布步骤是原子的：先构建到 `dist.next`，
成功后才替换 `dist`，失败会回滚到上一版本（见 `scripts/publish-static.mjs`），
因此可以直接把 `dist` 作为线上目录而不会出现"构建到一半的站点"。

## 部署

**前端是纯静态产物，由反向代理提供；UnionC 服务端只提供 API，不托管静态文件。**

这不是疏漏而是刻意的边界：服务端在生产环境强制绑定回环、强制经 HTTPS 反代访问，
反代本来就在链路上，由它直接 `file_server` 比让 API 进程多担一个静态文件服务器更简单，
也少一类路径穿越风险。

```bash
cd web && npm ci && npm run build
sudo rsync -a --delete dist/ /var/www/unionc/
```

反代配置见 [Caddyfile.console.example](examples/caddy/Caddyfile.console.example)。两个要点：

1. **SPA 回落**：未命中的路径要 `try_files {path} /index.html`，
   否则刷新子路由会 404；
2. **SSE 不缓冲**：`/api/events` 是长连接流，反代必须关闭响应缓冲
   （Caddy 用 `flush_interval -1`，nginx 用 `proxy_buffering off`），
   否则状态更新会被攒住直到缓冲区满才下发；
3. **静态文档安全头**：HTML/JS/CSS 由反代直接返回，不经过后端安全中间件，因此
   CSP、HSTS、`nosniff`、frame 限制等必须在静态文件的 `handle` 中设置。

### 版本匹配

前端与服务端同属一个仓库、同一版本号发布。混用不同版本的前后端不受支持——
接口契约（如 `MonitoringHostDetailResponse` 的字段）没有版本协商机制。
