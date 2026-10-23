
## 零、一页验收（第 179 轮定稿）

```powershell
cd D:\AgentProject\chat; . .\tools\activate.ps1
# 1) 12 阶段（含公网跨机器）
powershell -ExecutionPolicy Bypass -File tools\verify-all.ps1 `
  -RemoteEndpoint https://106.54.223.171:8443 -RemoteNodeId node-public `
  -RemoteFingerprint 13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053
# 2) 应用真实链路（就是点发送走的那条路）
powershell -ExecutionPolicy Bypass -File tools\verify-app-flow.ps1
# 3) 双人配对（本地节点）
powershell -ExecutionPolicy Bypass -File tools\verify-pairing.ps1
# 4) soak（冷启动 + 公网稳态）
powershell -ExecutionPolicy Bypass -File tools\soak.ps1 -Iterations 10
powershell -ExecutionPolicy Bypass -File tools\soak.ps1 -Reuse 10 -Endpoint https://106.54.223.171:8443 `
  -NodeId node-public -Fingerprint 13be2f93…
# 5) 三种故障注入
powershell -ExecutionPolicy Bypass -File tools\soak-faults.ps1
powershell -ExecutionPolicy Bypass -File tools\soak-partition.ps1
powershell -ExecutionPolicy Bypass -File tools\soak-disk.ps1
# 6) 交付包完整性
powershell -ExecutionPolicy Bypass -File tools\verify-zip.ps1
```

当前产物：`Chat.exe` `644E1672…5BD36`；`app.jar` `FA29A6EA…DCC33`；`Chat-app-0.1.0.zip` `9BF370CD…405D5`（82.8 MB）。

最近一次全绿：**第 194 轮**（`build/verify-all-194b.log` + `build/final-rest-194.log`），8 类套件全 PASS（12 阶段 / appflow / pairing / diagnose / 2 soak / 3 故障 / 真实库）。

新增诊断入口与真机验证操作单：`Chat.exe --diagnose`（报告）、`--diagnose --json`（机器读）、`docs/TWO_DEVICE_CHECKLIST.md`（两台机器 6 步配对）。
# 交付清单（一页看完）

最后更新：2026-10-21（第 139 轮）。这份文件只放**能立刻执行**的东西。

## 一、交付物

| 产物 | 路径 | SHA-256 | 大小 |
| --- | --- | --- | --- |
| 免 JDK 启动器 | `build/dist/app-image/Chat/Chat.exe` | `644E16725B6F5E61B693158F25877DB1A7B04827BE27912C12A9B9FC8FE5BD36` | — |
| 完整分发包 | `build/dist/Chat-app-0.1.0.zip` | `996E799E7D12F3C1623FC469261A4D6B02C95A8961FC9AFC99C5DB05B7D923C7` | 81.79 MB |
| 服务器部署包 | `build/node-bundle/`（`prepare-node-bundle.ps1` 生成） | 每次生成含新证书，指纹见包内 `fingerprint.txt` | ~5.6 MB |

## 二、验收（复制粘贴即可）

### 1. 全量 12 阶段（含公网节点）

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -ExecutionPolicy Bypass -File tools\verify-all.ps1 `
  -RemoteEndpoint https://106.54.223.171:8443 -RemoteNodeId node-public `
  -RemoteFingerprint 13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053
```

期望：`overall: PASS`（12 阶段；`remote` 阶段 7 条断言）。

### 2.1 打包产物是否新鲜（10 秒，防“旧 jar”）

```powershell
powershell -ExecutionPolicy Bypass -File tools\check-artifact-freshness.ps1
```

期望 `PASS freshness`。过期会打印双方时间与修复命令（`tools\verify-package.ps1`）；`verify-remote` 与 `soak` 已内置这项检查。

### 2. 只验公网节点（35 秒左右）

```powershell
powershell -ExecutionPolicy Bypass -File tools\verify-remote.ps1 `
  -Endpoint https://106.54.223.171:8443 -NodeId node-public `
  -Fingerprint 13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053
```

期望 7 行 `PASS`，最后 `CROSS-MACHINE VERIFICATION: PASS`。

### 2.5 节点巡检（30 秒，一条命令）

```powershell
powershell -ExecutionPolicy Bypass -File deploy\inspect-node.ps1 `
  -Endpoint https://106.54.223.171:8443 `
  -Fingerprint 13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053
```

期望 `INSPECT: HEALTHY`（退出码 0）。

### 2.6 Soak（可选，本机 20 轮约 1 分钟）

```powershell
powershell -ExecutionPolicy Bypass -File tools\soak.ps1 -Iterations 20
```

期望 `SOAK: PASS`（100% 成功，p50≈2 s）。数据与方法见 `docs/SOAK_2026-10-23.md`。

### 2.7 交付 zip 是否完整可运行（30 秒）

```powershell
powershell -ExecutionPolicy Bypass -File tools\verify-zip.ps1
```

期望 `ZIP CHECK: PASS`：解压到临时目录并从**解压副本**跑一次打包冒烟，同时校验 exe 与 app-image 一致。

### 2.8 故障注入（节点被杀时必须如实失败，40 秒）

```powershell
powershell -ExecutionPolicy Bypass -File tools\soak-faults.ps1
```

期望 `FAULT INJECTION: PASS`（A 正常→PASS；B 杀节点→如实失败；C 恢复→PASS）。

### 2.9 网络分区（半死链路必须如实失败，约 40 秒）

```powershell
powershell -ExecutionPolicy Bypass -File tools\soak-partition.ps1
```

期望 `PARTITION: PASS`；注意 B 阶段会等约 **26 秒**才报 `NODE_UNAVAILABLE`（语义正确、体验待优化）。

### 2.10 存储只读（节点不得谎报成功，约 40 秒）

```powershell
powershell -ExecutionPolicy Bypass -File tools\soak-disk.ps1
```

期望 `DISK FAULT: PASS`（B 阶段节点启动失败并报 `STORAGE_FAILURE`，客户端如实失败）。

### 3. 应用本体冒烟

```powershell
powershell -ExecutionPolicy Bypass -File tools\verify-package.ps1    # 结构 + 4 冒烟 + GUI 存活
.\gradlew.bat :apps:desktop:presentationChecks --offline            # 展示层断言
```

## 二点五、终局回归套件（7 个，一次跑齐）

```powershell
powershell -ExecutionPolicy Bypass -File tools\verify-all.ps1 `
  -RemoteEndpoint https://106.54.223.171:8443 -RemoteNodeId node-public `
  -RemoteFingerprint 13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053   # 12 阶段
powershell -ExecutionPolicy Bypass -File tools\soak.ps1 -Iterations 10                        # 冷启动 soak
powershell -ExecutionPolicy Bypass -File tools\soak.ps1 -Reuse 10 -Endpoint https://106.54.223.171:8443 `
  -NodeId node-public -Fingerprint 13be2f93…                                                 # 稳态 soak
powershell -ExecutionPolicy Bypass -File tools\soak-faults.ps1                               # 杀节点
powershell -ExecutionPolicy Bypass -File tools\soak-partition.ps1                            # 半死链路
powershell -ExecutionPolicy Bypass -File tools\soak-disk.ps1                                 # 数据库只读
```

最近一次结果（2026-10-23，`build/final-regression-20261023.log`）：**7 个套件全 PASS**；remote 64.6s、冷启动 p50 2075ms、公网稳态 p50 3855ms、三种故障注入全部如实失败。

### 2.11 应用真实链路对公网节点（约 40 秒，需节点在线）

```powershell
powershell -ExecutionPolicy Bypass -File tools\verify-app-flow.ps1
```

期望 `APP FLOW: PASS`（指纹固定 → 建群/登记 → `sendDispatch: status=accepted settled=true` → 同步）。最近一次实测：`sync: fetched=2 applied=1 skipped=1`。

### 2.12 用真实本地库验证发送（约 40 秒，需节点在线）

```powershell
$body = (curl.exe -s --insecure https://106.54.223.171:8443/v1/health | Out-String)
$key = [regex]::Match($body, '"node_public_key"\s*:\s*"([0-9a-f]{64})"').Groups[1].Value
cmd /c (".\gradlew.bat :apps:desktop:userDbSmoke --offline -Puser.endpoint=https://106.54.223.171:8443 -Puser.nodeId=node-public -Puser.nodeKey=$key -Puser.fingerprint=13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053")
```

期望 `USER DB: PASS`（`dispatch: status=accepted`）。注意：先关掉正在运行的 Chat.exe，否则会 `storage is locked`。

### 2.13 入群配对包（本地节点，约 1 分钟）

```powershell
powershell -ExecutionPolicy Bypass -File tools\verify-pairing.ps1
```

期望 `PAIRING: PASS`（KeyPackage → 建群 → 邀请包 → 入群，两端同群同 epoch）。

## 三、手工检查点（4 个，5 分钟）

1. 双击 `build/dist/app-image/Chat/Chat.exe` → 能启动；
2. 「本机存储」点**「发送（一步完成）」** → 步骤变 `queued → prepared → accepted`；点「收一轮」→ 入库；
3. 「关于」→ 「节点连通性自检」→ 5~6 步全 PASS（health / node_id / 证书指纹 / tls_info / 延迟）；
4. 把传输指纹**改错** → 再发送**必须失败**（这是 pinning 真正生效的证据）。

## 四、文档索引

| 文档 | 用途 |
| --- | --- |
| `docs/FINAL_REPORT.md` | 对外说明：能做什么 / 不能做什么 / 设计选择 / 跨机器验证 |
| `docs/SHIPPING.md` | 基线记录、限制清单、12 阶段全绿记录 |
| `docs/BATCH_121_140.md` | 最近 20 轮总结（里程碑 / 7 个错误 / 验证矩阵 / 未做项） |
| `docs/RETROSPECTIVE_120.md` | 前 120 轮总账（六批节奏 / 12 条缺陷教训 / 我的 7 条失误） |
| `docs/manual-test-guide.md` | 21 节手工测试指引 |
| `docs/DEEPSEEK_WORK_PLAN.md` | 每轮真实记录（R-140…R-263） |
| `docs/implementation/P0_CHECKLIST.md` | 计划勾选与未完成项 |
| `deploy/README-WINDOWS.md` | Windows Server 部署（复制 / 防火墙 / 自检 / 启动） |
| `docs/OPS_RUNBOOK.md` | **节点运维手册**（巡检 / 自启 / 日志 / 备份 / 换证书 / 故障对照表 / 事故处置） |
| `deploy/README.md` | Linux 部署脚本与公网验证说明 |

## 五、公网节点（当前唯一外部依赖）

| 项 | 值 |
| --- | --- |
| endpoint | `https://106.54.223.171:8443` |
| node_id | `node-public` |
| 证书指纹（SHA-256） | `13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053` |
| 证书剩余 | 3649 天（自签 10 年） |
| 开机自启 | 未确认；建议在服务器上跑一次 `register-service.ps1` |

## 六、已知限制（不要当成 bug）

1. **只能直连节点**：消息走自建 HTTP 节点；iroh P2P 中继已有配置与测试但未接入消息路径（“穿透 NAT”不成立）；
2. **证书不会自动续签**：只有到期告警（<30 天进错误条），轮换需人工；
3. **无 MSI 安装器**：离线环境拉不到 WiX，只有 app-image + zip；
4. **无后台通知**：应用关掉就不收消息；
5. **只有 Windows 凭据实现**：macOS/Linux 走口令派生但未验证；
6. **`AppFacade` 未完整收敛**：面板仍各自调 service（已收敛错误码与发送链路）；
7. **无 soak 数据**：没有连续运行/压力测试结论。


## 目标状态（第 199 轮判定）

**保持 `active`**：A 口径 100% / B 口径 ≈90%。判定依据与“改判条件”见 [docs/GOAL_STATUS_199.md](GOAL_STATUS_199.md)。
