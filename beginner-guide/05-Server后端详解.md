# 05. Union 后端

Union Core 负责登录/CSRF/RBAC、系统接口、发行 catalog、Manifest 动态网关和私有进程监管。
它不包含五个模块的 Backend、Frontend 或业务表。

Runtime 从同一发行的 `modules/<id>/backend` 启动已配置且 enabled 的 worker，清空继承环境，只传
Manifest 映射项和保留上下文；执行健康握手，崩溃时退避重启，关闭时先请求优雅终止。Runtime
只能重扫、配置和启停发行内包，不能安装、替换或下载代码。
