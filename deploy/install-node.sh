#!/usr/bin/env bash
# 一键部署 chat-node（Linux Ubuntu/Debian 22.04+，需要 root 或 sudo）。
#
# 用法（在服务器上）：
#   git clone <repo> /opt/chat-src && cd /opt/chat-src && bash deploy/install-node.sh
#   或把整个仓库目录拷上来后：bash deploy/install-node.sh
#
# 脚本会：装 Rust → 编译 release 节点 → 生成自签 TLS 证书 → 生成节点种子 → 写 systemd 单元 →
# 启动服务 → 打印 **证书 SHA-256 指纹** 和 **node_id**（这两样要给客户端用）。
#
# 幂等：重复执行会复用已有的证书/种子数据（不会覆盖身份）。
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DATA_DIR="${CHAT_DATA_DIR:-/var/lib/chat-node}"
CERT_DIR="${CHAT_CERT_DIR:-/etc/chat-node}"
PORT="${CHAT_PORT:-8443}"
NODE_ID="${CHAT_NODE_ID:-node-public}"
SERVICE=/etc/systemd/system/chat-node.service

echo "== chat-node deploy =="
echo "repo:     $REPO_DIR"
echo "data:     $DATA_DIR"
echo "certs:    $CERT_DIR"
echo "listen:   0.0.0.0:$PORT"

if [ "$(id -u)" -ne 0 ]; then
  echo "请用 root 或 sudo 运行" >&2
  exit 1
fi

# 1. Rust 工具链（没有才装）
if ! command -v cargo >/dev/null 2>&1; then
  echo "== 安装 Rust 工具链 =="
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
cargo --version

# 2. 编译 release 节点
echo "== 编译 chat-node（release）=="
cd "$REPO_DIR"
cargo build --release -p chat-node
NODE_BIN="$REPO_DIR/target/release/chat-node"
test -x "$NODE_BIN"
install -m 0755 "$NODE_BIN" /usr/local/bin/chat-node

# 3. 目录、证书、种子
mkdir -p "$DATA_DIR" "$CERT_DIR"
chmod 700 "$DATA_DIR" "$CERT_DIR"

if [ ! -f "$CERT_DIR/cert.pem" ] || [ ! -f "$CERT_DIR/key.pem" ]; then
  echo "== 生成自签证书（10 年）=="
  PUBLIC_IP="${CHAT_PUBLIC_IP:-$(curl -s --max-time 5 https://api.ipify.org || true)}"
  [ -n "$PUBLIC_IP" ] || PUBLIC_IP=127.0.0.1
  cat > "$CERT_DIR/openssl.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $PUBLIC_IP
[ext]
subjectAltName = IP:$PUBLIC_IP,DNS:localhost
basicConstraints = critical,CA:TRUE
keyUsage = critical,digitalSignature,keyCertSign
extendedKeyUsage = serverAuth
EOF
  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$CERT_DIR/key.pem" -out "$CERT_DIR/cert.pem" \
    -config "$CERT_DIR/openssl.cnf" -extensions ext >/dev/null 2>&1
  chmod 600 "$CERT_DIR/key.pem"
  chmod 644 "$CERT_DIR/cert.pem"
fi

if [ ! -f "$DATA_DIR/seed.hex" ]; then
  echo "== 生成节点签名种子 =="
  openssl rand -hex 32 > "$DATA_DIR/seed.hex"
  chmod 600 "$DATA_DIR/seed.hex"
fi

# 4. systemd 单元
echo "== 写 systemd 单元 =="
cat > "$SERVICE" <<EOF
[Unit]
Description=chat node (self-hosted, TLS)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/chat-node \\
  --bind 0.0.0.0:$PORT \\
  --allow-public-bind \\
  --data-dir $DATA_DIR \\
  --seed-file $DATA_DIR/seed.hex \\
  --node-id $NODE_ID \\
  --tls-cert $CERT_DIR/cert.pem \\
  --tls-key $CERT_DIR/key.pem
Restart=on-failure
RestartSec=3
User=root

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable chat-node.service
systemctl restart chat-node.service
sleep 2

# 5. 自检 + 输出客户端需要的东西
echo
echo "== 自检 =="
PUBLIC_IP="${CHAT_PUBLIC_IP:-$(curl -s --max-time 5 https://api.ipify.org || echo 127.0.0.1)}"
set +e
HEALTH=$(curl -s --max-time 10 --cacert "$CERT_DIR/cert.pem" "https://$PUBLIC_IP:$PORT/v1/health")
set -e
if [ -z "$HEALTH" ]; then
  echo "!! 自检失败：curl https://$PUBLIC_IP:$PORT/v1/health 没有返回" >&2
  echo "   排查：systemctl status chat-node ; journalctl -u chat-node -n 50 ; ss -ltnp | grep $PORT" >&2
  exit 1
fi

FINGERPRINT=$(openssl x509 -in "$CERT_DIR/cert.pem" -noout -fingerprint -sha256 | sed 's/.*Fingerprint=//' | tr -d ':' | tr 'A-Z' 'a-z')

echo "health: $HEALTH"
echo
echo "================ 给客户端的两个值 ================"
echo "endpoint    : https://$PUBLIC_IP:$PORT"
echo "node_id     : $NODE_ID"
echo "fingerprint : $FINGERPRINT"
echo "================================================"
echo
echo "提示：云厂商安全组要放行 TCP $PORT；本机防火墙 ufw allow $PORT/tcp"
