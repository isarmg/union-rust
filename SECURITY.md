# 安全策略

## 报告漏洞

**请不要通过公开 issue 报告安全漏洞。**

请通过 GitHub 的 [Private vulnerability reporting](https://github.com/isarmg/union-rust/security/advisories/new)
提交。若该渠道不可用，请在 issue 中仅说明"存在安全问题，请提供联系方式"，不要包含任何细节。

报告请尽量包含：受影响的组件（Core / module / agent / web）、版本或 commit、复现步骤、
以及你判断的影响范围。我们会在 **72 小时**内确认收到，并在 **7 天**内给出初步评估。

## 支持范围

仅当前发布版本接受安全修复。项目不维护旧协议、旧数据库、旧配置或旧安装布局的兼容分支。

## 安全模型

理解以下前提有助于判断某个行为是否构成漏洞。

### 部署形态是安全模型的一部分

UnionC **强制**运行在 HTTPS 反向代理之后：

- 生产环境（`UNIONC_ENV=production`）下服务端拒绝绑定非回环地址
  （`config/runtime.rs`），因此不可能被直接暴露到公网；
- 登录、改密与 Agent 接口要求 `X-Forwarded-Proto: https`、`X-Forwarded-For`，以及
  与服务端 `UNIONC_PROXY_SECRET` 恒定时间匹配的 `X-UnionC-Proxy-Secret`。反代必须覆盖
  外部同名头；缺任一项返回 **421 Misdirected Request**（不是 403——Agent 报告的 403 仅在当前有效 credential 与 body 中的
  `host_id` 绑定不匹配时返回稳定机器码；未知 credential（包括实例已删除）返回 401。
  当前常驻 `run` 仅把 UnionC 的稳定 401 视为需要创建新实例并配对；未知 401/403 保持重试）；
- 按 IP 的限流取最后一个 `X-Forwarded-For` 头的**最右**一项；该项必须是裸 IP，
  非法、为空或携带端口都会返回 421，绝不向左回退到客户端可控值。该实现假定前面
  **恰好有一层**可信反代。若你在反代之前再叠加 CDN，必须相应调整，否则限流会退化。

**不在反代之后部署、或反代未透传上述头部，属于配置错误而非漏洞。**

Union Core 是唯一允许接收公网流量的进程。Builder 纳入发行的五个业务模块只绑定 loopback，
只接受带当前进程 `gateway-v1` 身份的请求；其具体地址和端口不是公共接口。反代不得为 worker
建立独立站点，也不得绕过 `/api/modules/<id>` Manifest 网关。

### 模块代码与运行状态边界

模块代码供应链以 Builder 2.0 验证的不可变 Union 发行结束。Runtime 只读取发行根中本地、
`distribution=bundled` 的 `modules/<id>` 包；运行期 API 只能重扫、读取/写入配置和启停发行内
模块，没有安装、升级、卸载、上传、下载或公网仓库选择代码的能力。改变代码或发行包含必须生成、
验证并激活新的完整发行。

Core 在启动模块进程前清空继承环境，只传 Manifest 映射的配置和保留的 Plugin/Gateway 上下文；
可执行文件、Frontend 和 migration 必须留在包边界内。管理员不能把任意 URL、`PATH` binary 或
shell 命令变成模块 Backend。

平台管理路由由 Core 会话、RBAC 和 CSRF 保护。Manifest 标记为 `module` auth 的 Agent、Photo
设备或 Sentinel 媒体端点仍由对应领域凭据授权，但也只能通过 Union 的 Gateway 到达。Dufs 的
全部公开路由均为 `platform` auth，不保留独立 ACL 登录边界。
内部 per-process token 只证明 Core→worker 边界，不是用户或设备身份。

模块 ESM 与 worker 一样，是 Builder 校验后纳入不可变发行的**受信任代码**。同源动态加载不是
JavaScript 沙箱：`hostSdk` 的 API base、Manifest route 和前端 permission 过滤是稳定接口与界面
约束，恶意模块脚本理论上仍可直接调用同源 `fetch`、读取非 HttpOnly 的 CSRF token。真正的授权
边界始终是 Core 对每次请求执行的会话、RBAC、CSRF、Manifest 路由和 Gateway 校验。不能在没有
额外 iframe/origin 沙箱设计的情况下把第三方不可信前端加入官方发行。

### 信任边界

| 主体 | 信任级别 |
|---|---|
| 管理员会话 | 平台内高权限；仍受已授予 RBAC、CSRF、Manifest 路由和模块配置 Schema 约束 |
| Builder 验证的模块进程 | 受信任发行代码，但通过独立进程、loopback、独立数据库/目录和最小环境隔离 |
| Agent（持有 per-host token） | 半可信。只能上报自己主机的数据，不能读取任何数据，不接受任何指令 |
| Sunshine 上游主机 | **不可信**。即便由管理员配置，也可能已被攻陷——响应受 MIME 白名单与体积上限约束，HTTP 3xx 不会自动跟随 |
| 未认证请求 | 不可信。健康、登录和短时配对入口可达；后者依赖高熵 capability、限流与小请求体上限 |

“独立进程”在 v0.5 首先提供崩溃、生命周期、依赖和数据所有权边界，不等同于对恶意模块的 OS
沙箱。默认由 Core 直接启动的 worker 与 Core 使用同一操作系统身份；因此官方模块属于同一发行
信任域，不能把数据库 role、环境清理或目录约定解释成能够抵抗已攻陷 worker 的机密隔离。需要把
第三方/低信任模块纳入生产时，应先实现并审计按模块 UID、容器或独立 service adapter 的凭据与
文件 ACL，再把该模块加入 Builder profile。

Agent 是**单向只读**的：它不监听端口、不执行服务端下发的命令、不含自更新器。
服务端无法通过 Agent 在被监控主机上执行任何操作——这是刻意的设计约束。

### 凭据处理

- 管理员密码：bcrypt（DEFAULT_COST）。未知用户名走 dummy hash 以抹平时序差异；
  长度限制为 12 字符 ~ 72 **字节**（bcrypt 超出部分静默截断，不设上限会形成
  "前 72 字节相同即互相可登录"的隐蔽认证等价类）
- 浏览器配对激活码和 polling secret：Server 只存 SHA-256；激活码明文只在管理员创建
  响应出现一次，polling secret 明文只存在于 Agent 私有状态目录
- Agent report secret：由 Agent 本地生成，Server 只收到并保存 SHA-256；明文不经过
  浏览器、URL、剪贴板或 Server 响应
- Sunshine 主机密码：由 Sunshine 模块使用 AES-256-GCM 和模块配置中的专属
  `credential_key` 加密，不复用 Core `UNIONC_SECRET_KEY`
- 模块配置：Core 按 Manifest `secret_fields` 在 GET 响应中显示为 `***`；更新必须提交完整新值，
  该脱敏只保护 API/UI 显示，不能替代状态目录权限或部署层静态加密
- CSRF 令牌使用恒定时间比较

配对确认界面若由 Host 模块提供，只能显示 OS、架构、Agent 版本和 request ID，不得读取监控
数据或设备名称。输入的激活码只放在 POST body，响应带 `Cache-Control: no-store`。Agent 创建
请求在发出前先把本地 secret 与创建状态可靠落盘，避免 Host worker 已接受哈希而 Agent 丢失明文。

### 输入约束

Agent 报文的**每一个**文本字段都有长度上界并禁止控制字符——数量上限管不住内容长度，
512 KiB 的 body 之内可以把配额全部塞进任一不限长的字符串，而这些文本会落库并随主机
列表在每次查询中返回。Sunshine 代理方向同样有约束：请求体按端点限长（应用 256 KiB /
配置 1 MiB / 改密 64 KiB）、上游响应流式累计限长（JSON 4 MiB / 封面 8 MiB）。
Sunshine 返回的 HTTP 3xx 会按上游失败处理，不会请求 `Location` 或重放变更请求体。

### 已知的、经过权衡的设计取舍

以下**不**视为漏洞：

1. **CSRF cookie 不是 HttpOnly。** 双提交模式要求前端能读到它。前提是跨站页面读不到
   本站 cookie；即便 CORS 将来被误配，攻击者仍需猜出随机令牌。
2. **SSE ticket 走 URL 查询参数。** `EventSource` 不支持自定义请求头。缓解措施：
   一次性、60 秒有效、随机 UUID、`Referrer-Policy: no-referrer`。
3. **封面图片代理转发上游字节。** Content-Type 收敛到图片白名单，
   配合全局 `nosniff` 与 `default-src 'none'` 的 CSP。
4. **非生产环境允许关闭 Sunshine TLS 校验。** 生产环境下该配置会被拒绝，
   且已存在的此类主机会被拒绝使用。
5. **Photo 与 Dufs 的服务器端内容不是应用层加密。** HTTPS 保护客户端到 Union 的传输；
   服务器必须读取原始字节完成媒体处理、哈希、Range 和文件服务。磁盘、快照与备份加密由部署层负责。

### Agent 安装与退役

- 普通卸载刻意保留本机 host-id、agent-token、配对状态、配置和 spool，避免误卸载后
  重装创建第二个身份；这不等同于永久退役。
- 永久退役必须分别在 Web 永久删除实例，并显式执行平台 purge。两项操作没有远程先后
  依赖；purge 只清理本机且不会持有、请求或伪造管理员凭据去调用 Server。
- Linux/macOS 只删除 root-only ownership marker 能证明由本包创建、且属性仍匹配的专用
  账户；Windows 只删除固定 Program Files/ProgramData 子路径，并拒绝接管同名非 UnionC
  服务或不可信的预存状态目录。
- Windows 状态 ACL 使用服务专属 `NT SERVICE\HostMAgent` SID，不把长期凭据授权给共享
  LOCAL SERVICE SID；程序与状态分别位于 Program Files 和 ProgramData。
- 正式 tag 发布缺少签名凭据时失败：Windows 使用 Authenticode，macOS 使用 Developer ID、
  Hardened Runtime、notarytool 和 staple；全部制品另有 GPG 签名清单与 provenance
  attestation。开发手动构建的未签名制品不得当作正式发布。

完整流程见 [`docs/runbooks/agent-lifecycle.md`](docs/runbooks/agent-lifecycle.md)。

## 加固建议

- 用 `UNIONC_SECRET_KEY` 显式提供主密钥，不要依赖开发环境的自动生成
- 需要时为报告数据入口启用 mTLS（见 `docs/examples/caddy/Caddyfile.agent-api.example`）。不要对首次
  pairing/bootstrap 入口预先要求客户端证书；应拆分域名或路由，并在配对后另行签发证书
- 首次部署后立即移除 `UNIONC_ALLOW_BOOTSTRAP` 与 `UNIONC_BOOTSTRAP_PASSWORD`
- 为 Sunshine、Host、Sentinel、Photo 分别创建专用 PostgreSQL database/role，不授予访问其他
  模块 database 的权限；Core 与 Dufs SQLite/目录也使用相互独立的最小文件权限
- 退役主机使用管理台“删除”或
  `DELETE /api/modules/host-monitoring/managed-instances/{id}`；不要直接
  删除数据库行。Server 会在单一事务中清理实例、历史、credential、配对请求和邀请并记录审计
- credential 丢失或失效后，在管理台创建新实例并再次执行配对。当前版本不提供同实例
  credential 轮换、主机撤销状态或恢复已删除实例的兼容入口
