# 公网节点部署与跨机器验证

目标：把 `chat-node` 部署到公网服务器（**106.54.223.171**），然后从本机验证
「**两台真机器**之间能发消息 + 拉取 + TLS 指纹校验」。

## 一、服务器上做什么（一次性，约 5–10 分钟）

前提：Ubuntu/Debian 22.04+，有 root/sudo，能访问 crates.io（编译需要联网）。

```bash
# 1. 把仓库弄上去（任选一种：git clone / scp 整个目录）
sudo -i
cd /opt && git clone <你的仓库地址> chat-src && cd chat-src

# 2. 一键部署（编译 + 证书 + 种子 + systemd + 自检）
bash deploy/install-node.sh
```

脚本结束时会打印三个值，**请把这三行发回给我**：

```
endpoint    : https://106.54.223.171:8443
node_id     : node-public
fingerprint : <64 位小写 hex>
```

### 放行端口（必须）

1. **云厂商安全组**：入站放行 TCP **8443**（腾讯云控制台 → 安全组）；
2. **本机防火墙**：`ufw allow 8443/tcp`（或 `firewall-cmd --add-port=8443/tcp --permanent && firewall-cmd --reload`）。

### 部署后自检（推荐）

```bash
sudo bash deploy/selfcheck.sh        # 服务/监听/健康/证书/指纹 一次列清，PASS 时直接打印要发给客户端的值
```

### 常用排查

```bash
systemctl status chat-node            # 是否 active
journalctl -u chat-node -n 80         # 启动日志（含证书到期天数）
ss -ltnp | grep 8443                  # 是否在监听
curl --cacert /etc/chat-node/cert.pem https://106.54.223.171:8443/v1/health
curl --cacert /etc/chat-node/cert.pem https://106.54.223.171:8443/v1/tls/info
```

## 二、本机做什么（我这边跑，你把三个值给我后自动完成）

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -ExecutionPolicy Bypass -File tools\verify-remote.ps1 `
  -Endpoint https://106.54.223.171:8443 `
  -NodeId node-public `
  -Fingerprint <64 位 hex>
```

这条脚本会依次验证：

1. **匿名健康检查**（带证书校验）能拿到 `node_public_key`；
2. `/v1/tls/info` 能读到 `days_remaining` 与 `cert_sha256`；
3. **打包产物**（`app.jar` + FFI）配置指纹后，向公网节点提交消息 → 期望 `accepted`；
4. 从公网节点**拉取** → 期望 `applied ≥ 1`；
5. **错误指纹必须失败**（证明 pinning 真在生效）。

## 二点五、如果 TLS 暂时起不来

先用明文模式验证「能不能通」，再回头修 TLS：

```powershell
powershell -ExecutionPolicy Bypass -File tools\verify-remote.ps1 `
  -Endpoint http://106.54.223.171:8080 -NodeId <node_id> -PlainHttp
```

明文模式只验证 TCP 可达 + 打包产物提交/拉取；证书与指纹相关的断言会跳过并明确打印 INFO（不会伪装成通过）。

## 三、注意事项（诚实说明）

- 这是**自签证书**：客户端靠 SHA-256 指纹固定，不依赖 CA；因此**指纹必须人工核对**（脚本会打印，别从不可信渠道拿）。
- 节点是**明文之外的唯一防线**：请只开放 8443，不要额外暴露其它端口。
- 数据目录 `/var/lib/chat-node` 里是 SQLite 库与签名种子：**要备份，别外泄**（`seed.hex` 等于节点身份）。
- 证书 10 年有效，节点启动会打印剩余天数；到期前需要换证书并把新指纹同步给客户端。
- 目前**没有**中继/NAT 打洞：客户端必须能直连这个公网 IP:端口。

## 四、如果服务器没有外网编译环境

两个替代方案：

1. **在本地交叉编译**（需要 Linux 目标工具链，未在项目里验证过，不推荐）；
2. **把本地 `target/debug/chat-node.exe` 换成 Linux 版**：需要在一台 Linux 上 `cargo build --release -p chat-node` 后把二进制拷到服务器（脚本会自己编，通常不需要）。
