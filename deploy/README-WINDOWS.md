# Windows Server 部署（chat-node）

服务器是 **Windows Server**，所以走这条路径（不需要在服务器上编译 Rust）。

## 一、在开发机准备部署包（已为你生成）

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -ExecutionPolicy Bypass -File deploy\prepare-node-bundle.ps1 -PublicIp <服务器公网IP> -Port 8443
```

产物在 `build\node-bundle\`（约 5.6 MB）：

| 文件 | 说明 |
| --- | --- |
| `chat-node.exe` | release 构建的节点（目标机无需 Rust） |
| `cert.pem` / `key.pem` | 自签证书，**SAN = 公网 IP**（客户端按 SHA-256 指纹固定） |
| `seed.hex` | 节点签名种子（**机密**，等于节点身份） |
| `run-node.ps1` | 前台启动脚本（保持窗口开着） |
| `register-service.ps1` | 注册开机启动的计划任务（管理员） |
| `README-SERVER.txt` | 服务器上的操作步骤 |
| `fingerprint.txt` | endpoint / node_id / 证书指纹（给客户端） |

## 二、在服务器上（复制 + 放行 + 启动）

1. 把整个 `node-bundle` 文件夹复制到服务器，例如 `C:\chat-node`；
2. **防火墙**（管理员 PowerShell）：
   ```powershell
   New-NetFirewallRule -DisplayName 'chat-node' -Direction Inbound -Protocol TCP -LocalPort 8443 -Action Allow
   ```
3. **云安全组**：入站放行 TCP **8443**（腾讯云控制台）；
4. 启动：
   ```powershell
   cd C:\chat-node
   powershell -ExecutionPolicy Bypass -File .\run-node.ps1
   ```
   或注册开机自启：
   ```powershell
   powershell -ExecutionPolicy Bypass -File .\register-service.ps1
   ```
5. 服务器自检（可选）：
   ```powershell
   curl.exe --cacert cert.pem https://<公网IP>:8443/v1/health
   curl.exe --cacert cert.pem https://<公网IP>:8443/v1/tls/info
   ```

### 也可以让它自己等（推荐）

```powershell
# 起来之后不用我们再手动跑：脚本会等端口打开，然后自动验证
powershell -ExecutionPolicy Bypass -File tools\watch-remote.ps1 `
  -Endpoint https://<公网IP>:8443 -NodeId node-public `
  -FingerprintFile build\node-bundle\fingerprint.txt -TimeoutMinutes 20
```

`watch-remote.ps1` 每 5 秒探一次端口；一旦通了就跑完整 `verify-remote.ps1`（可用 `-DiagnoseOnly` 只做快速诊断）。

## 二点五、服务器上先自检再启动（推荐）

```powershell
cd C:\chat-node
powershell -ExecutionPolicy Bypass -File .\verify-bundle.ps1 -Port 8443 -StartProbe
```

它会检查：6 个必需文件是否都在（拷贝是否完整）、**`cert.pem` 的 SHA-256 是否与 `fingerprint.txt` 一致**、`chat-node.exe` 能否执行；加 `-StartProbe` 还会临时起一个实例读 `/v1/health`。全部 PASS 打印 `BUNDLE OK`。

## 三、回到开发机验证（跨机器）

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
# 快速诊断（TCP/证书/状态端点）
powershell -ExecutionPolicy Bypass -File tools\verify-remote.ps1 `
  -Endpoint https://<公网IP>:8443 -NodeId node-public `
  -FingerprintFile build\node-bundle\fingerprint.txt -DiagnoseOnly

# 完整验证（含打包产物真实收发 + 错误指纹必须失败）
powershell -ExecutionPolicy Bypass -File tools\verify-remote.ps1 `
  -Endpoint https://<公网IP>:8443 -NodeId node-public `
  -FingerprintFile build\node-bundle\fingerprint.txt
```

## 三点五、指纹对不上时的应急验证（TOFU）

如果你的文件夹里的证书和我给你的不是同一张（比如整包重拷过），可以先用 TOFU 模式验证连通与收发：

```powershell
powershell -ExecutionPolicy Bypass -File tools\verify-remote.ps1 `
  -Endpoint https://<公网IP>:8443 -NodeId node-public -TrustServedCertificate
```

它会**醒目打印 WARNING**并接受服务端当前证书作为指纹（**这不是 pinning**，只用于首次联调）；随后照常跑完 health / 节点身份 / tls_info / 打包产物收发 / 错误指纹被拒。正式使用请核对指纹后写进配置。

## 四、注意

- **不要**把 `seed.hex` / `key.pem` 提交进 git 或发给别人；`fingerprint.txt` 可以公开。
- 证书 SAN 必须等于客户端连接的 IP，否则 TLS 校验会失败（脚本已按 `-PublicIp` 生成）。
- 换 IP 或换证书 -> 重新生成 bundle，并把**新指纹**同步给客户端（客户端支持一次填多个指纹做轮换）。
- 想用明文 HTTP 先把链路跑通：服务器上不带 `--tls-cert/--tls-key` 启动，然后本机加 `-PlainHttp`。
