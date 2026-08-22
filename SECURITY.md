# 安全策略

## 报告漏洞

**请不要通过公开 issue 报告安全漏洞。**

请通过 GitHub 的 [Private vulnerability reporting](https://github.com/sarmg/unionc/security/advisories/new)
提交。若该渠道不可用，请在 issue 中仅说明"存在安全问题，请提供联系方式"，不要包含任何细节。

报告请尽量包含：受影响的组件（server / agent / web）、版本或 commit、复现步骤、
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
  外部同名头；缺任一项返回 **421 Misdirected Request**（不是 403——Agent 报告的 403
  表示主机生命周期已撤销，或当前有效 credential 与 body 中的 `host_id` 绑定不匹配；
  未知、失效或被重配替换的 credential 返回 401。当前常驻 `run` 对 401/403 都会停止
  投递并等待新的浏览器配对）；
- 按 IP 的限流取 `X-Forwarded-For` 的**最右**一项——该实现假定前面**恰好有一层**
  可信反代。若你在反代之前再叠加 CDN，必须相应调整，否则限流会退化。

**不在反代之后部署、或反代未透传上述头部，属于配置错误而非漏洞。**

### 信任边界

| 主体 | 信任级别 |
|---|---|
| 管理员会话 | 完全可信。可配置 Sunshine 主机、读取日志、代理上游 API |
| Agent（持有 per-host token） | 半可信。只能上报自己主机的数据，不能读取任何数据，不接受任何指令 |
| Sunshine 上游主机 | **不可信**。即便由管理员配置，也可能已被攻陷——响应受 MIME 白名单与体积上限约束 |
| 未认证请求 | 不可信。健康、登录和短时配对入口可达；后者依赖高熵 capability、限流与小请求体上限 |

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
- Sunshine 主机密码：AES-256-GCM，密钥经 `UNIONC_SECRET_KEY` 提供，
  支持密钥环轮换（见 `infra/secrets.rs`）
- CSRF 令牌使用恒定时间比较

浏览器激活页面只显示主机名、OS、架构、Agent 版本和 request ID，不读取监控数据。输入的
激活码只放在 POST body，响应带 `Cache-Control: no-store`。Agent 创建请求在发出前先把
本地 secret 与创建状态可靠落盘，避免 Server 已接受哈希而 Agent 丢失明文。

### 输入约束

Agent 报文的**每一个**文本字段都有长度上界并禁止控制字符——数量上限管不住内容长度，
512 KiB 的 body 之内可以把配额全部塞进任一不限长的字符串，而这些文本会落库并随主机
列表在每次查询中返回。Sunshine 代理方向同样有约束：请求体按端点限长（应用 256 KiB /
配置 1 MiB / 改密 64 KiB）、上游响应流式累计限长（JSON 4 MiB / 封面 8 MiB）。

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

### Agent 安装与退役

- 普通卸载刻意保留本机 host-id、agent-token、配对状态、配置和 spool，避免误卸载后
  重装创建第二个身份；这不等同于永久退役。
- 永久退役必须先在 Web 撤销实例，再显式执行平台 purge。purge 只清理本机且不会持有、
  请求或伪造管理员凭据去调用 Server。
- Linux/macOS 只删除 root-only ownership marker 能证明由本包创建、且属性仍匹配的专用
  账户；Windows 只删除固定 Program Files/ProgramData 子路径，并拒绝接管同名非 UnionC
  服务或不可信的预存状态目录。
- Windows 状态 ACL 使用服务专属 `NT SERVICE\UnionCAgent` SID，不把长期凭据授权给共享
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
- 退役主机使用 `POST /api/monitoring/hosts/{id}/revoke` 持久撤销；不要直接删除数据库行，
  否则会丢失身份 tombstone、凭据吊销状态和审计关联
- 恢复或轮换凭据时，为同一 `instance_id` 创建新邀请并完成浏览器重新配对，旧 credential
  会在激活事务中撤销，历史身份保持不变
