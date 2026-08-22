# 12. 实战练习、FAQ 与术语表

这一章用于检验你是否真正建立了系统模型。练习从只读观察开始，逐渐进入测试与小改动。所有运行练习都应使用第 00/10 章的临时目录，绝不能指向生产数据。

## 1. 练习一：口述系统

不看前文，用 3 分钟回答：

1. Server、Agent、protocol、Web 各自是什么；
2. Server 本机资源、Agent 主机、Sunshine 主机的区别；
3. 为什么 Server 不能向 Agent 下命令；
4. 为什么 Web 不能直接访问 SQLite，也不能读回 Server 已存储的 Sunshine 密码明文；
5. 一份报告为什么要有 host ID 和 report ID 两种 ID。

验收：能讲出组件、调用方向和 credential 类型，而不只是重复目录名。

## 2. 练习二：追踪 Server 本机 CPU

目标：把页面上的 CPU 数值追到采样源。

按顺序查找：

```bash
rg -n "systemResources" web/src
rg -n '"/api/system/resources"' server/src
rg -n "ResourceMonitor" server/src/system server/src/startup.rs server/src/state.rs
```

画出：

```text
后台 ResourceMonitor → AppState 快照 → HTTP handler
→ api.ts → App useQuery → OverviewView
```

验收问题：HTTP 请求是否会临时执行一次系统采样？页面曲线是否来自 SQLite？正确答案都是否。

## 3. 练习三：追踪 Agent CPU

目标：理解它为何与练习二不同。

查找：

```bash
rg -n "CpuSnapshot" protocol agent server web/src
rg -n "store_monitoring_report" server
rg -n "monitoringHistory" web/src
```

画出：采集器 → AgentReport → spool → Reporter → Server handler → 摘要列/latest payload → 前端。

验收：指出哪一步计算速率、哪一步计算页面摘要、完整 per-core 历史是否长期保存。

## 4. 练习四：观察断线 spool

前提：已按第 00 章使用临时 Server data，并复用同一份隔离的 Agent config 和 state 完成配对。最稳妥的做法是在第 00 章同一个 Agent 终端执行；若换了 shell，必须先把第 00 章实际创建的精确 `/tmp/unionc-tutorial-agent.XXXXXX` 目录重新赋给 `UNIONC_TUTORIAL_AGENT_STATE`。不要在变量为空时继续；先执行 `printf '%s\n' "$UNIONC_TUTORIAL_AGENT_STATE"` 并确认它是正确目录，再让本练习的所有命令继承同一环境：

```bash
export UNIONC_AGENT_CONFIG="$UNIONC_TUTORIAL_AGENT_STATE/config.json"
export UNIONC_AGENT_STATE_DIR="$UNIONC_TUTORIAL_AGENT_STATE"
```

1. 在 Server 终端按 `Ctrl+C` 停止开发 Server；
2. 保持相同 Agent state，执行一次：

```bash
cargo run -p unionc-agent --bin unionc-agent -- once
```

3. 命令应报告投递失败，当前报告留在 spool；
4. 用同一 state 执行 `status`，观察 pending；
5. 在第 00 章的 Server 终端使用同一个非空 `UNIONC_TUTORIAL_SERVER_DATA` 重新启动：`UNIONC_DATA_DIR="$UNIONC_TUTORIAL_SERVER_DATA" cargo run -p unionc`；
6. 再执行 `once`：它会先排空旧积压，再发送当前采样；
7. `status` 应回到 0 pending，管理台历史出现补传点。

验收：解释为什么暂时网络失败保留，而永久非法报告必须丢弃队首。

## 5. 练习五：验证幂等概念

阅读：

```bash
rg -n "accepted.*false|accepted: false|duplicate" server/tests agent/src
```

找到同一 `report_id` 重放测试，写下四条不变量：

- 不增加第二行；
- ACK `accepted=false`；
- 复用首次 received_at；
- 不刷新 last_seen/身份/latest。

验收：能解释“网络响应丢失”为什么要求该行为，而不是简单把重复当 409。

## 6. 练习六：纯前端小功能

实现第 07 章的“总览新增网络上传卡片”。要求：

1. 先确认 API type 已有 `transmitted_bytes_per_second`；
2. 复用 `Metric` 和格式化函数；
3. 不修改 Server/protocol/schema；
4. 新建 `web/src/views/OverviewView.test.tsx` 覆盖该视图；
5. 通过 lint、test、typecheck、build；
6. 用窄屏检查响应式布局。

验收：`git diff --stat` 只出现合理的前端源码/测试，不出现 `dist` 或依赖目录。

## 7. 练习七：分析一个安全端点

选择 `POST /api/events/ticket`，回答：

- 路由在哪个模块；
- 是否公开；
- 使用什么 session/CSRF；
- ticket 存在哪里；
- 有效多久、能用几次；
- 如何与会话绑定；
- 为什么不是 GET；
- 登出后已建立 SSE 如何结束；
- 哪些测试守护它。

验收：能从代码和测试给出证据，不只凭安全常识猜测。

## 8. 练习八：阅读一个数据库事务

选择 `store_monitoring_report`，给每段 SQL 标注：

1. 读取当前状态；
2. 判断 latest；
3. 幂等 insert；
4. duplicate 分支；
5. 条件更新 host；
6. 清空旧 payload；
7. commit。

然后回答：网络调用是否发生在事务内？为什么 identity 未变时不更新？相同采样时间怎样决胜？

## 9. 练习九：模拟需求评审

需求：“在管理台增加按钮，让 Server 远程重启 Agent。”

不要开始写代码。写一份评审：

- 它违反哪条明确产品边界；
- 会引入什么入站/轮询命令通道；
- credential 被盗后的权限如何扩大；
- 审计、授权、重放、幂等、超时和结果证明有哪些新问题；
- 如果业务确实必须做，为什么应作为独立产品/代理协议重新设计，而不是普通端点。

验收：能拒绝“看似简单”的实现捷径，并具体说明原因。

## 10. 练习十：跨层设计题

假设要新增“系统负载 load average”监控。先只做设计，不提交代码：

1. 哪些平台有可靠来源，其他平台 capability 怎么表达；
2. protocol 字段是必选还是可选，数值范围和数量上限；
3. Server 是否需要摘要历史列；
4. 若改 schema，当前版本/全新部署策略如何处理；
5. 列表、详情、历史分别展示什么；
6. OTLP 是否导出以及 metric 语义；
7. 哪些单元、合同、乱序、前端、feature 和平台测试需要新增；
8. 容量和向后兼容代价。

验收：设计覆盖 Agent、protocol、Server、SQLite、Web、测试和文档，而不是只给一个字段名。

---

## 11. FAQ

### Q1：UnionC 是远程管理/RMM 工具吗？

不是。Agent 监控严格只读、只出站，没有命令、脚本、进程、文件和自更新通道。Sunshine 管理是 Server 对已配置 Sunshine API 的独立代理。

### Q2：Server 能运行在 Windows 或 macOS 吗？

不能，源码显式限制 Linux。Agent 支持 Linux、Windows、macOS；Web 在浏览器运行。

### Q3：为什么页面有两类监控曲线？

总览是 Server 本机资源，在浏览器内存保留短期点；“主机”页是 Agent 上报，摘要历史持久化在 SQLite。

### Q4：为什么 GPU 显示 N/A，不显示 0？

N/A 表示不可采集，0 表示成功采集且当前空闲。混用会让故障看起来像健康空闲。

### Q5：为什么 Server/Agent 版本必须完全相同，schema_version 都是 1 还不行吗？

schema_version 只代表顶层报告 schema；同一 schema 编号内的校验、capability 和行为也可能变化。当前项目选择同仓库精确版本匹配，不做版本协商。

### Q6：为什么 protocol crate 不做所有校验？

共享 crate 只保证线上形状。Agent 负责平台构造，Server 负责不可信输入的业务范围；把 Server 安全策略编进共享 DTO 会模糊职责。

### Q7：为什么 Agent 常驻模式每份报告都先写磁盘？

这样进程崩溃或网络失败不会丢失尚未确认报告，采样节拍也不被网络 I/O 拖慢。spool 是事实来源，通知只负责唤醒 worker。

### Q8：spool 满了会怎样？

在固定预算内先淘汰最老的 `.invalid`，再淘汰最老待发报告，避免占满系统盘。应通过监控提前发现持续积压。

### Q9：收到 401 后 Agent 为什么不自动注册新身份？

自动换身份会绕过管理员撤销或造成重复主机。必须为同一实例创建新邀请并完成明确重新配对。
常驻 `run` 得到 401 后会标记 `reauth_required`；`once` / `doctor --delivery` 则只把可重试
报告入队并失败，不修改授权状态。

### Q10：403 与 421 有何区别？

403 表示主机生命周期已撤销，或一份有效 credential 与报告 `host_id` 绑定不匹配；未知/失效
credential，以及主机仍 active 但已被重配替换的 credential，则是 401。当前常驻 `run`
收到两者之一后写 `reauth_required`、停止投递并继续采样到有界 spool；重启后会在采样前因
没有 authorized reporter 而退出并等待重新配对。一次性投递命令只入队并失败。421 是请求
没有走预期反代，修复部署后可原样重试。

### Q11：为什么登录后写请求还需要 CSRF？

浏览器会自动带 session Cookie。CSRF token 证明发起写请求的脚本能读取 UnionC 同源随机 cookie，而非第三方网站诱导浏览器发送。

### Q12：为什么当前前端不只依赖 SSE 自动携带的 Cookie？

同源 `EventSource` 可以自动携带 Cookie，Server 也保留了这条认证路径。当前前端选择先通过受 CSRF 保护的 POST 签发单次短效 ticket，用显式 capability 引导建连；ticket 仍绑定 Cookie 会话并支持会话撤销，同时不把长期 session token 放入 URL。

### Q13：为什么不让每个浏览器连接自己探测 Sunshine？

那会使上游负载随浏览器和标签页数线性增长。唯一后台探测、快照、多订阅者把成本固定为主机数。

### Q14：为什么历史不保留完整 JSON？

页面只需要摘要曲线，完整报文体量大。每台主机只保留 latest 完整详情，显著降低主库、WAL 和备份体积。

### Q15：可以直接用 sqlite3 修一行吗？

不建议，尤其不能改运行中的生产库。手工写会绕过事务、审计、latest/payload、加密和内存快照不变量。先使用 API、测试库和内置维护命令。

### Q16：为什么当前运行时没有逐版本 migration 链？

当前交付策略只支持全新当前版本数据和同版本恢复，明确不读取或转换旧布局。schema 变化直接更新唯一当前基线；仓库保留的 `server/migrations/` 目录骨架没有 SQL migration 文件，也不被运行时用于升级。

### Q17：运行中可以复制 `unionc.db` 备份吗？

不可以依赖简单复制；WAL 下可能得到跨时点或缺页快照。使用 `unionc backup` 和配套 manifest。

### Q18：OTLP 能替代 UnionC SQLite 吗？

不能。OTLP 是可选、尽力而为的时序旁路，不包含完整资产快照或完整 capability 列表，也不
驱动管理台权威状态；它仍会携带识别时间序列所需的主机与设备属性。

### Q19：为什么前端不用 React Router？

当前普通管理导航只需要单页本地切换，只有公开激活路径需要 URL。若未来需要可分享的多路由，才应重新评估；现在不要假设路由库存在。

### Q20：页面收到 200 为什么仍报错？

前端按每个 API 的精确状态与 JSON 媒体类型验证。200 HTML 常表示反代误路由到 SPA，不能当成 API 成功。

### Q21：服务运行正常但 Agent 主机没出现？

安装/启动与配对是两步。先用 `status` 看 credential 与 pairing，再确认邀请、激活和严格 202 ACK。

### Q22：Web 撤销后为什么本机文件还在？

Server 撤销与本地 purge 是两个管理域。完整退役需要两边都做，避免远程操作能删除本机文件这一危险能力。
撤销不会主动推送到本机；运行中的 Agent 要到下一次报告收到拒绝时才会更新本地诊断状态。

---

## 12. 术语表

| 术语 | 小白解释 |
|---|---|
| Agent | 安装在被监控主机上的只读采集程序 |
| Server | Linux 中心 API、SQLite 和管理协调进程 |
| Web/Console | 浏览器中的 React 管理台 |
| protocol/DTO | 两个进程对 JSON 字段和类型的共同约定 |
| workspace | 一个根 Cargo 项目管理多个 Rust package |
| crate | Rust 编译单元/库名 |
| feature | 编译时启用的可选代码，例如 OTLP |
| Axum | Server 使用的 Rust HTTP 框架 |
| Tokio | 执行异步任务、网络和定时器的 runtime |
| handler | 某个 HTTP 路由最终调用的 Rust 函数 |
| middleware | handler 前后执行的认证、日志、安全头等检查层 |
| AppState | Server 各请求/后台任务共享的状态入口 |
| React component | 返回一块页面 UI 的函数 |
| hook | React 中复用 state/effect/查询行为的函数 |
| React Query | 管理 Server 数据请求、缓存与刷新的前端库 |
| query key | React Query 中某份缓存的稳定地址 |
| mutation | 会改变 Server 状态的前端操作 |
| optimistic update | Server 返回前先更新界面，失败再回滚 |
| SSE | Server 通过一个 HTTP 长连接向浏览器单向推事件 |
| polling | 客户端按周期重复 GET |
| schema | 数据结构与约束；本项目同时有 JSON 和 SQLite schema |
| SQLite | 单机文件型关系数据库，本项目内嵌使用 |
| WAL | SQLite 先写日志、再 checkpoint 到主库的模式 |
| transaction | 一组写要么全部提交、要么全部回滚 |
| idempotent | 同一操作重试多次，最终效果与一次相同 |
| latest | 按采样时间选出的主机当前报告 |
| summary | 从完整报告派生、适合列表/历史的小数值集合 |
| payload | 当前最新完整 AgentReport JSON |
| spool | Agent 本地有界、持久、按顺序的未确认报告队列 |
| ACK | Server 明确确认某 host/report 已持久化的响应 |
| backoff | 失败越多，重试等待逐渐变长 |
| jitter | 周期加入随机偏移，避免所有客户端同时请求 |
| capability | 指标能否采集及不能采集的结构化原因 |
| instance_id | Server 预留、贯穿主机历史的稳定 ID |
| credential | 用于证明调用者身份的秘密或其记录 |
| activation code | 管理员创建、短时、单次的配对授权密钥 |
| polling secret | Agent 私密查询配对结果的临时 secret |
| Bearer secret | Agent 正常上报使用的长期每实例 secret |
| hash | 单向摘要；Server 用它验证 secret 而不存明文 |
| bcrypt | 适合低熵用户密码的慢哈希算法 |
| AES-GCM | 可解密的认证加密，用于 Sunshine password |
| CSRF | 第三方网站诱导已登录浏览器发写请求的攻击 |
| HttpOnly | 禁止 JavaScript 读取 Cookie 的属性 |
| SameSite | 控制跨站请求何时携带 Cookie 的属性 |
| reverse proxy | 对外终止 TLS、提供静态页并转发 API 的入口 |
| mTLS | Server 与客户端都提供证书的双向 TLS |
| liveness | 进程是否活着 |
| readiness | 当前是否具备接收业务流量的依赖条件 |
| audit | 记录谁在何时改变了什么状态 |
| retention | 历史数据保留多久以及何时清理 |
| tombstone | 不硬删除的撤销实例记录，用来保留历史语义 |
| purge | 永久删除目标机本地 Agent 状态和 secret |
| OTLP | OpenTelemetry 传输协议；本项目的可选旁路输出 |

## 13. 结业自检

若你能独立完成以下事项，就已经具备接手本项目的基础：

- [ ] 从空临时目录启动 Server 与 Web；
- [ ] 用 `probe` 解释本机 capability；
- [ ] 完成邀请、配对、一次报告和撤销；
- [ ] 画出报告从采集到页面的调用链；
- [ ] 区分 session、CSRF、activation、polling 和 Bearer secret；
- [ ] 解释幂等、乱序、latest 与摘要存储；
- [ ] 找到一个 API 的路由、handler、store、前端调用和测试；
- [ ] 用 request ID 和日志定位一次失败；
- [ ] 完成一个纯前端小改动并通过全部前端门禁；
- [ ] 说明新指标为何是跨 Agent/protocol/Server/DB/Web 的改动；
- [ ] 解释生产反代、备份、恢复和密钥轮换的安全顺序；
- [ ] 遇到远程命令需求时识别它越过项目明确边界。

完成后，继续把根目录 `DOCUMENTATION.md`、`PROJECT_CAPABILITIES.md` 和 `docs/` 作为日常参考手册；源码与测试始终是最终事实。
