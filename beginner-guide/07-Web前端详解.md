# 07. Web 前端详解

Web 管理台是一个纯静态 React 应用。开发时由 Vite 提供，生产时由反向代理提供；Rust Server 只提供 API，不托管前端文件。

## 1. 技术栈与约束

以 `web/package.json` 为准，当前主要技术：

- React 19 与 React DOM；
- TypeScript 6；
- Vite 8；
- TanStack React Query 5；
- lucide-react 图标；
- Vitest、Testing Library、jsdom；
- ESLint 与 React Hooks 规则。

前端没有 React Router，也没有 Redux、Zustand 一类通用状态库。普通导航是
`app/App.tsx` 的本地 `view` 状态；刷新后回到总览。只有公开 `/agent/activate/{uuid}` 由
`features/agent-activation/route.ts` 的手写路径解析器识别。

## 2. 从 HTML 到组件树

```text
web/index.html
  └─ <div id="root">
     └─ src/main.tsx
        └─ React.StrictMode
           └─ QueryClientProvider
              └─ ErrorBoundary
                 └─ App
                    ├─ AgentActivationPage（公开激活路径）
                    └─ AuthenticatedAppRoot
                       ├─ 会话检查中
                       ├─ LoginScreen（401）
                       ├─ 会话错误页（非 401）
                       └─ AuthedApp
                          ├─ 导航/主题/连接状态
                          └─ 当前 View
```

`main.tsx` 中 React Query 全局默认值：

- 窗口重新聚焦不自动刷新；
- 查询失败重试 1 次；
- 5 秒内数据视为 fresh。

`StrictMode` 只在开发阶段帮助发现副作用问题。effect 必须正确清理 EventSource、定时器和监听器，否则开发模式下的重复挂载会暴露泄漏。

## 3. `app/App.tsx` 的两道门

### 3.1 激活路径门

`parseAgentActivationRoute(window.location.pathname)` 先判断当前路径是否为严格的 Agent 激活路径。合法时直接渲染公开 `AgentActivationPage`，不会先要求管理员登录。

### 3.2 会话门

普通路径查询 `/api/auth/me`：

- pending：显示“正在验证会话”；
- 401：显示登录表单；
- 其他错误：显示错误和重试按钮，不能把 Server 故障误显示成“密码不对”；
- 成功：进入 `AuthedApp`。

登录成功后把用户名直接写入当前会话的 `queryKeys.auth.me` 缓存，避免立刻再请求一次。注销或
收到会话过期事件时，前端不会只重置这一条查询，而是立即清空并替换整套 `QueryClient`：旧会话
已经发出的 mutation 即使稍后执行回调，也只能写入已经脱离页面的旧 client，不能把私有快照带进
下一次登录。注销请求完成前，登录按钮和处理函数会同时拒绝新登录，避免旧 logout 在新 login
之后返回并清掉新 Cookie。

全局 `unionc:auth-expired` 浏览器事件把任何 API 的 401 传回会话门。这样各个视图不需要重复实现
“会话过期跳登录”，同时会话级缓存边界也只需在顶层维护。

## 4. 顶层管理应用

`AuthedApp` 管理：

- 当前视图；
- 浅色/深色主题与 localStorage；
- “新建 Sunshine 实例”触发器；
- 全局 query invalidation；
- 退出登录；
- SSE 连接状态；
- `/api/services` 和 `/api/system/resources` 顶层查询。

只有 `OverviewView` 直接进入首屏 bundle；Monitoring、Sunshine、Logs、Settings 使用
`React.lazy` 与 `Suspense` 按需下载。即使各视图已经拆成较小组件，按功能分块仍能避免
只看总览的用户下载其他业务代码。

## 5. 统一 API 层

`web/src/shared/api/client.ts` 是浏览器普通 `fetch` 请求的共享底座；每个业务功能在自己的
`features/<feature>/api.ts` 声明端点和 DTO。SSE 则由 `app/hooks.ts` 与 `app/realtimeApi.ts`
配合建立。共享的 `request<T>` 统一处理：

1. 默认 15 秒 AbortController 超时；
2. `credentials: "include"`，随同源请求发送 Cookie；
3. 有普通 body 时自动设置 JSON Content-Type；
4. 非 GET/HEAD/OPTIONS 请求读取 CSRF cookie，填入 `X-CSRF-Token`；
5. 401 广播 auth-expired，公开激活和登录可选择抑制；
6. 非 2xx 解析 `{code,message}` 为 `ApiError`；
7. 严格比对每个调用声明的预期状态码；
8. 非 204 响应严格要求 `application/json`；
9. 支持调用方 AbortSignal；只有调用方显式传入并一路向下转发时，请求才能据此取消。

为什么不能只写 `if (response.ok)`？因为真实 Web 契约中，创建邀请或 Sunshine 主机返回 201，logout、取消邀请、删除 Sunshine 主机和 revoke 返回 204，普通查询返回 200；某些 DELETE 操作（如删除 Sunshine app）也会按各自契约返回 200 JSON。代理错误返回 200 HTML 也不能被当作成功 JSON。

TanStack Query 会把 `AbortSignal` 交给 `queryFn`。功能 API 还必须接收该 signal 并传给
`request`；否则最后一个 observer 卸载后只是清除了轮询 timer，已经在途的 HTTP 请求仍会
跑完并写入旧缓存。监控邀请列表与 Sunshine 查询都遵守这条完整传递链。

## 6. URL、类型和缓存键

### 6.1 `shared/api/paths.ts`

所有动态 path segment 经过 `encodeURIComponent`：

```ts
monitoringHostPath(id)
```

这防止 ID 中的 `/`、`?` 或其他字符改变 URL 结构。即使当前 ID 是 UUID，也应保持统一构造规则。

### 6.2 功能内的 `types.ts`

前端按功能维护 Server 响应的 TypeScript 表达，包括：

- Server 本机资源；
- 服务状态；
- Sunshine 主机、应用、客户端、配置、日志；
- Agent 邀请与公开激活；
- 主机摘要、完整报告与历史。

这些类型不是运行时验证器。若后端类型改变，前端类型、API、视图和测试必须在同一个仓库版本一起更新。

### 6.3 功能内的 `queryKeys.ts`

缓存键在对应功能内集中定义。例如：

```text
["monitoring-hosts", limit, offset]
["monitoring-host", hostId]
["monitoring-history", hostId]
["sunshine-apps", hostId]
```

参数必须进入 key。否则第 1 页和第 2 页、主机 A 和主机 B 会错误共享同一缓存。

## 7. 顶层 SSE 与轮询兜底

`useEventStream` 的状态机：

```text
POST /api/events/ticket
  → new EventSource(/api/events?ticket=...)
    ├─ open：connected=true，停止 services 轮询
    ├─ status：解析 payload，setQueryData(services)
    ├─ 内容非法：关闭，立即恢复普通查询
    └─ error：关闭，恢复轮询，5 秒后申请新 ticket 重连
```

旧 EventSource 的排队回调不能关闭已经替换它的新连接，因此 hook 会比较回调所属实例。

`app/App.tsx` 中 services：SSE 连通时不轮询，断线时每 10 秒；Server 本机 resources 每
20 秒。SSE 只更新服务状态，不替代本机资源和 Agent 历史查询。

## 8. `useMetricHistory`

总览曲线不是 SQLite 历史。`useMetricHistory` 每次收到 Server 本机资源快照，就在浏览器内存追加 CPU、内存、网络、磁盘数据，最多 180 点。

因此：

- 刷新页面后曲线从头开始；
- 它与 Agent 主机的持久历史接口无关；
- maxPoints 变化和新数据到来触发 state 更新；
- 内存百分比由 total/used 现场计算，total 为 0 时安全回退为 0。

## 9. 六个视图

### 9.1 `OverviewView`

由 `App` 传入 services、本机 resources 和短期 history。展示：

- 服务健康数量；
- CPU、内存、网络与磁盘吞吐；
- 各磁盘容量；
- sparkline 趋势。

它本身不发请求，是“容器查询、视图展示”的简单示例。

### 9.2 `MonitoringView`

负责 Agent 主机：

- 主机列表每页 20，约 10 秒刷新；
- 选中主机详情约 10 秒刷新；
- 历史约 30 秒刷新；
- 邀请列表约 10 秒刷新；
- 创建邀请、只显示一次授权密钥、取消邀请；
- 为同一 instance 重新配对；
- 把实例生命周期持久标记为 `revoked` 并吊销其全部 credential，同时保留 tombstone 和历史；
- 展示逐网卡、逐磁盘、GPU、温度和 capability；
- `null` 明确显示 N/A，历史缺口不补成零。

这是理解“同一实体有列表摘要、详情、历史、生命周期”多缓存协调的好例子。
入口只协调状态；主机摘要、硬件详情、历史指标和 Agent 邀请分别位于
`features/monitoring/components/`，纯展示推导位于 `model.ts`。

### 9.3 `SunshineView`

最复杂的 master-detail 视图：

- 左侧多主机卡片；
- 内联创建、编辑、删除；
- 右侧应用、客户端、PIN、完整配置、系统操作五类内容；
- 主机创建、修改、删除 mutation 使用乐观更新，失败回滚；其他操作按各自契约在成功后失效查询或显示结果；
- 删除成功清理该主机 apps/clients/config/logs 子缓存；
- 探测 pending 时短轮询，稳定后降低频率。

`features/sunshine/data.ts` 放置纯数据变换，`queries.ts` 把 GET 结果与进行中的 mutation 做
屏障合并。原因是旧 GET 响应可能晚于新 PATCH 返回，不能让旧快照覆盖用户刚保存的值。
主机卡、详情面板、应用和客户端区位于 `features/sunshine/components/`；`SunshineView`
负责选择与组合，不再承载所有表单和列表实现。

### 9.4 `LogsView`

复用 Sunshine hosts 缓存，选择已持久化主机后约 30 秒读取日志。渲染只保留最新 2000 行，防止大日志拖垮 DOM。

### 9.5 `SettingsView`

修改单管理员密码。前端以 JavaScript `String.length` 做至少 12 个 UTF-16 code unit 的快速反馈，Server 则按 Unicode 字符数做最终校验；对 emoji 等非 BMP 字符，两者计数可能不同，因此不能把前端检查当成权威契约。成功后主动 logout 并触发 auth-expired，要求重新登录。

### 9.6 `AgentActivationPage`

公开页面只 GET 有限设备摘要；waiting 状态才显示授权码表单。POST 成功只收到 `instance_id` 与 `active`，不接触 Agent secret。

## 10. 通用组件与工具

`shared/components/ui.tsx` 包含卡片行、操作区、指标、sparkline、状态灯、进度条、notice、
loading 等共享原语。日志查看器和服务卡等带业务语义的组件分别留在 `features/logs/` 与
`features/overview/`，避免共享层反向依赖具体功能。

`shared/lib/format.ts` 统一格式化：

- 字节、KiB、每秒速率；
- 百分比；
- 日期时间。

格式化集中后，KB/MB/GB/TB 阈值和精度不易跨页面漂移。监控页的 N/A 规则目前由
`features/monitoring/MonitoringView.tsx` 中的局部辅助函数负责；普通字节格式化不应被
误当成统一缺失值策略。

`ErrorBoundary` 捕获 React 渲染异常并提供整页刷新恢复。它不能捕获事件 handler 或所有异步 Promise 错误，因此 API 查询仍需正常 error UI。

## 11. 样式系统

`app/styles.css` 只控制导入顺序：

1. `shared/styles/tokens.css`：颜色、字体等浅/深主题变量；
2. `shared/styles/content-cards.css`：卡片网格与内部行列；
3. `shared/styles/foundation.css`：壳、导航、按钮、表单、登录、通用结构；
4. `shared/styles/responsive.css`：通用响应式规则；
5. 各功能自己的 `settings.css`、`sunshine.css`、`monitoring.css`、`activation.css`。

卡片网格随宽度从 6 列逐步降到 1 列；业务组件不应通过硬编码像素破坏统一断点。动画还应遵守 `prefers-reduced-motion`。

## 12. 一个完整的小改动示例

需求：总览新增“网络上传”指标卡。

### 找数据

`SystemResources` 已有：

```ts
resources.network.transmitted_bytes_per_second
```

所以不需要后端、协议或数据库改动。

### 改视图

在 `OverviewView.tsx` 的网络指标附近复用 `Metric` 和 `formatBytesPerSecond`：

```tsx
<Metric
  label="上传"
  value={resources
    ? formatBytesPerSecond(resources.network.transmitted_bytes_per_second)
    : "--"}
  tone="neutral"
/>
```

### 补测试

当前没有 `OverviewView` 的相邻测试文件；新建
`web/src/features/overview/OverviewView.test.tsx`，渲染一个 `SystemResources` fixture，
断言“上传”和格式化值出现。然后运行：

```bash
cd web
npm run lint
npm test
npm run typecheck
npm run build
```

这个例子体现了先确认“数据是否已经存在”。若存在，就不要为了一个展示需求无谓修改四层协议。

## 13. 前端阅读顺序

1. `index.html`、`main.tsx`；
2. `app/App.tsx`；
3. `features/overview/`、`shared/components/ui.tsx`、`shared/lib/format.ts`；
4. `shared/api/`，再看每个功能的 `types.ts`、`api.ts` 和 `queryKeys.ts`；
5. `app/hooks.ts` 与 `app/realtimeApi.ts`；
6. 较小的 `features/logs`、`settings`、`agent-activation`；
7. `features/monitoring`；
8. `features/sunshine/data.ts`、`queries.ts`、`SunshineView.tsx`；
9. 样式与相邻测试；
10. `scripts/publish-static.mjs`。

## 14. 本章自检

1. 为什么页面刷新后普通导航会回到总览？
2. `shared/api/client.ts` 为何要严格检查预期状态和媒体类型？
3. query key 为什么必须包含 hostId 和分页参数？
4. SSE 断开时 services 如何继续更新？
5. Sunshine 乐观写入为何需要防旧 GET 覆盖？
6. 总览历史与 Agent 历史有何本质差别？

下一章：[08. SQLite、生命周期与一致性](08-SQLite生命周期与一致性.md)。
