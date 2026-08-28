# Agent 生命周期

Agent 是 [`host-monitoring`](https://github.com/isarmg/host-monitoring) 仓库产出的跨平台远端
companion artifact，Union supervisor 不管理它，Builder `full` 服务器发行也不包含它。
Agent 的官方产物由 Union Builder Release 从锁定 Host revision 集中构建；组织再通过
可信包管理/MDM 渠道独立安装与更新，并按照 compatibility matrix 选择版本。

## 安装与配对

1. 在 Union Web 创建待激活实例和短时授权值；
2. 在目标主机安装兼容 Agent；
3. 执行 `unionc-agent pair --server https://union.example.com`；
4. 核对设备摘要并激活；
5. 用 `status`、`doctor` 和 Union 主机页验证上报。

不要把已配对状态目录制作进镜像，也不要让 Agent 连接任何 worker loopback 地址；它只能访问
Union 的 `/api/modules/host-monitoring/agent/*` 网关。

## 卸载与退役

普通卸载可按平台政策保留本地身份，方便误卸载后的同版本重装。永久退役必须同时：

1. 在 Union Web 永久删除主机，使服务端 credential 失效；
2. 停止并卸载远端 Agent；
3. 显式清除本地配置、secret、配对状态和 spool；
4. 验证服务、程序和状态目录均消失。

主机丢失时至少完成第 1 步。服务端不会远程执行卸载或擦除。

## Release 证据

Union Release 的 compatibility matrix 应记录每个平台 Agent 版本、协议版本与验证日期。
目标平台的安装器与生命周期测试在 Host 仓库中维护，Agent 可以作为 companion artifact 发布，
但不能被描述成独立公网服务、服务器模块 Release 或 Core 私有 worker。文档中的通用流程不代表
Linux DEB/RPM、Windows MSI 或 macOS pkg 已建立生产签名、公证或 MDM 信任链。
