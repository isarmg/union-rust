# Agent 配对协议

管理员在 Union 管理台预留主机并生成短时一次性授权值。远端 Agent 本地生成长期 secret 和
polling secret，向固定 `/api/agent/v2/...` 创建请求；用户核对设备摘要后完成激活。激活后
Agent 使用固定 `/api/agent/v1/report` 数据面。

```text
管理员浏览器 --登录/CSRF--> Union --gateway-v1--> Host worker
远端 Agent --一次性配对/设备凭据--> Union --gateway-v1--> Host worker
                                                    |
                                                    +--> PostgreSQL host_monitoring
```

gateway token 只用于 Union 到 worker，绝不交给 Agent。浏览器也不会读取长期 Agent secret。
请求中断可依靠 request id 和 polling secret 幂等恢复；授权值只消费一次。永久删除主机时，
实例、credential、邀请和历史作为 Host 域事务删除，再次加入必须创建新实例。

安全不变量：

- 非回环配对与上报默认要求 HTTPS；
- Agent 零入站端口；
- 服务端不提供远程执行或自动更新；
- report 的 host id 必须与设备 credential 绑定；
- 邀请、速率限制、配对和报告由 Host worker/其 PostgreSQL 持有，不由 Union 核心数据库持有。

具体 JSON 字段由 `protocol/src/` 和合同测试定义；本文不复制易漂移的完整 payload。
