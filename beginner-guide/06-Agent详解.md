# 06. Agent

Agent 是远端、只出站、零入站 companion。它本地采样、生成设备 secret、离线时写有界 spool，
恢复后按顺序补传。服务端不向它执行命令、传文件或推送更新。

Agent 源码和安装制品由
[`host-monitoring`](https://github.com/isarmg/host-monitoring) 仓库维护。Agent 版本必须出现在
Union Release compatibility matrix，但它独立安装，不进入 Builder 服务器发行，supervisor 也不
启动 Agent。
