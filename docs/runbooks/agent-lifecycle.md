# Agent 生命周期

Agent 是远端 companion，supervisor 不管理它。组织通过可信包管理/MDM 渠道安装与更新，并
按照 Union Release compatibility matrix 选择版本。

## 安装与配对

1. 在 Union Web 创建待激活实例和短时授权值；
2. 在目标主机安装兼容 Agent；
3. 执行 `unionc-agent pair --server https://union.example.com`；
4. 核对设备摘要并激活；
5. 用 `status`、`doctor` 和 Union 主机页验证上报。

不要把已配对状态目录制作进镜像，也不要让 Agent 直接连接 18105。

## 卸载与退役

普通卸载可按平台政策保留本地身份，方便误卸载后的同版本重装。永久退役必须同时：

1. 在 Union Web 永久删除主机，使服务端 credential 失效；
2. 停止并卸载远端 Agent；
3. 显式清除本地配置、secret、配对状态和 spool；
4. 验证服务、程序和状态目录均消失。

主机丢失时至少完成第 1 步。服务端不会远程执行卸载或擦除。

## Release 证据

Union Release 的 compatibility matrix 应记录每个平台 Agent 版本、协议版本与验证日期。
目标平台的安装器测试仍可在 Agent 工程中维护，但不应产生一个与 Union 服务端独立演进的
模块 Release。文档中的通用流程不代表 Linux DEB/RPM、Windows MSI 或 macOS pkg 已在当前
发布候选上完成验收。
