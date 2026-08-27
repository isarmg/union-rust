# 06. Agent

Agent 是远端、只出站、零入站 companion。它本地采样、生成设备 secret、离线时写有界 spool，
恢复后按顺序补传。服务端不向它执行命令、传文件或推送更新。

Agent 版本必须出现在 Union Release compatibility matrix，但 supervisor 不启动 Agent。
