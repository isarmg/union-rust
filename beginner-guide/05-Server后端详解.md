# 05. Union 后端

Union 核心负责登录/CSRF、系统接口、编译 catalog、固定反向代理和子进程监督。它不再包含
Sunshine/Host 业务 Router 或业务表。

supervisor 从同一 release 的 `libexec/union/modules/<id>` 启动所选 worker，清空继承环境，
只传允许项；执行健康握手，崩溃时退避重启，关机先 SIGTERM。动态 `SARMG_*_URL` 被拒绝。
