# 节点运维手册（chat-node on Windows Server）

对象：`106.54.223.171:8443` 这台自建节点。目标：**任何人拿到这份文档 + 一台能连的机器，都能判断它是好是坏、并把它弄回来。**

## 一、现状（2026-10-21 实测）

| 项 | 值 |
| --- | --- |
| 部署目录 | `C:\chat-node`（脚本用 `$here` 定位，目录名可改） |
| 端口 | TCP **8443**（云安全组 + Windows 防火墙都要放行） |
| endpoint | `https://106.54.223.171:8443` |
| node_id | `node-public` |
| 证书指纹（SHA-256） | `13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053` |
| 证书有效期 | 自签 10 年（`/v1/tls/info` 可查剩余天数） |
| 数据 | `C:\chat-node\data\node.db`（SQLite） |
| 机密 | `seed.hex`（节点身份）、`key.pem`（TLS 私钥） |

## 二、日常巡检（30 秒）

在任意机器（需要本机装了 OpenSSL）：

```powershell
powershell -ExecutionPolicy Bypass -File deploy\inspect-node.ps1 `
  -Endpoint https://106.54.223.171:8443 `
  -Fingerprint 13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053
```

期望输出：

```
tcp                   reachable
fingerprint (sha256)  13be2f93…
pinned fingerprint    matches served certificate
node_id               node-public
certificate expiry    3648 days left
INSPECT: HEALTHY
```

判据：出现任何 `NEEDS ATTENTION` → 查第七节对照表；退出码 `0` 才是健康。

在服务器本机巡检（进程/端口/自启/文件/证书一致性）：

```powershell
powershell -ExecutionPolicy Bypass -File C:\chat-node\inspect-node.ps1 -Local -BundleDir C:\chat-node
```

## 三、启动与开机自启

```powershell
# 前台启动（关窗口就停）
cd C:\chat-node
powershell -ExecutionPolicy Bypass -File .\run-node.ps1

# 注册开机自启（管理员，一次即可）
powershell -ExecutionPolicy Bypass -File .\register-service.ps1
```

**注意**：当前**没有**注册成 Windows 服务（exe 不实现服务控制接口），用的是**计划任务（SYSTEM / AtStartup）**。想确认：

```powershell
Get-ScheduledTask -TaskName chat-node | Select-Object TaskName, State
```

## 四、日志

- 前台运行时：日志直接打在 `run-node.ps1` 的窗口里（启动时会打印证书剩余天数）；
- 用计划任务运行时：日志在任务历史里，或改用重定向（把 `run-node.ps1` 的调用改成 `... *>> C:\chat-node\node.log`）；
- 关键行样式：`chat-node started node_id=node-public address=0.0.0.0:8443 database=... capability=off`。

## 五、备份与恢复

**必须备份**（丢了就回不来）：

| 文件 | 丢了会怎样 |
| --- | --- |
| `seed.hex` | 节点身份变了：所有客户端看到的节点公钥变化，需要重新登记 |
| `data\node.db` | 消息/成员/提交记录全丢 |
| `cert.pem` + `key.pem` | 证书指纹变化，客户端要重新配指纹 |

```powershell
# 冷备份（先停节点，避免 SQLite 写到一半）
Stop-Process -Name chat-node -Force -ErrorAction SilentlyContinue
Copy-Item C:\chat-node\seed.hex,C:\chat-node\cert.pem,C:\chat-node\key.pem D:\backup\node\ -Force
Copy-Item C:\chat-node\data D:\backup\node\data -Recurse -Force
# 恢复：把文件放回去，重新 run-node.ps1
```

备份**不要**放进 git（`.gitignore` 已忽略 `build/`，但备份目录要自己确保不在仓库里）。

## 六、换证书（到期 / 换 IP）

```powershell
# 在开发机上对部署包执行（保留 seed，只换证书）
powershell -ExecutionPolicy Bypass -File deploy\rotate-certificate.ps1 `
  -BundleDir build\node-bundle -PublicIp 106.54.223.171 -Port 8443
```

然后：

1. 把新的 `cert.pem` + `key.pem` + `fingerprint.txt` 拷到服务器，重启节点；
2. 客户端**先同时配置新旧指纹**（轮换窗口，内核按集合匹配，已实证）；
3. 全部客户端切到新指纹后，再把旧的去掉。

## 七、故障对照表

| 现象 | 最可能原因 | 处理 |
| --- | --- | --- |
| `tcp NEEDS ATTENTION` | 云安全组 / Windows 防火墙 / 进程没起 | 放行 8443；`Get-NetTCPConnection -LocalPort 8443`；看 run-node 窗口 |
| `pinned fingerprint MISMATCH` | 证书换过 / 客户端指纹抄错 | 核对 `fingerprint.txt`；必要时 `rotate-certificate.ps1` 重新分发 |
| `certificate expiry` 报红 | 证书到期（<30 天也算告警） | 第六节轮换 |
| `node_id` 与配置不一致 | 连错节点 | 核对 endpoint 与 `-NodeId` |
| 客户端 `NODE_UNAVAILABLE` | 节点没起 / 网络不通 / 提交被拒 | 先巡检，再看客户端错误码 |
| 数据库锁（`STORAGE_LOCKED`） | 同一库被两个进程打开 | 保证只有一个节点实例；它是**可重试**错误 |
| 计划任务没生效 | 注册时不是管理员 / 路径变了 | 重新跑 `register-service.ps1`；`Get-ScheduledTaskInfo` 看上次结果 |

## 八、安全事故处置

**`seed.hex` 泄露**（等于节点身份被盗）：

1. 立刻停节点；
2. 换种子：`openssl rand -hex 32 > C:\chat-node\seed.hex`（**旧数据里的签名记录会失效**，按需要重建）；
3. 重启后把新的 `node_public_key` 告知客户端（客户端需要重新登记设备）；
4. 复盘：谁拿到过这个文件、备份是否也泄露。

**`key.pem` 泄露**（TLS 私钥）：走第六节换证书即可（因为客户端是**按指纹固定**的，换证书比换 CA 更简单）。

## 九、变更记录

| 日期 | 变更 | 验证 |
| --- | --- | --- |
| 2026-10-21 | 部署节点（Windows Server，自签证书，TLS） | `verify-remote` 7 条断言 PASS |
| 2026-10-21 | 加入运维巡检脚本与本文档 | 远程/本机两种模式均实跑 |
