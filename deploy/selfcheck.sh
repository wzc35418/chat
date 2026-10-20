#!/usr/bin/env bash
# 部署后自检：把「服务状态 / 监听 / 健康 / 证书 / 指纹」一次列清楚。
# 用法：sudo bash deploy/selfcheck.sh [端口, 默认 8443]
set -uo pipefail

PORT="${1:-${CHAT_PORT:-8443}}"
CERT_DIR="${CHAT_CERT_DIR:-/etc/chat-node}"
CERT="$CERT_DIR/cert.pem"
PUBLIC_IP="${CHAT_PUBLIC_IP:-$(curl -s --max-time 5 https://api.ipify.org || echo 127.0.0.1)}"
fail=0

line() { printf '%-28s %s\n' "$1" "$2"; }

echo "== chat-node 自检（$PUBLIC_IP:$PORT）=="

# 1. 服务
if systemctl is-active --quiet chat-node; then
  line "service" "active"
else
  line "service" "NOT active（journalctl -u chat-node -n 60 看原因）"
  fail=1
fi

# 2. 监听
if ss -ltn 2>/dev/null | grep -q ":$PORT "; then
  line "listen" "0.0.0.0:$PORT ok"
else
  line "listen" "端口 $PORT 没有监听"
  fail=1
fi

# 3. 证书与指纹
if [ -f "$CERT" ]; then
  FP=$(openssl x509 -in "$CERT" -noout -fingerprint -sha256 2>/dev/null | sed 's/.*Fingerprint=//' | tr -d ':' | tr 'A-Z' 'a-z')
  NOT_AFTER=$(openssl x509 -in "$CERT" -noout -enddate 2>/dev/null | sed 's/notAfter=//')
  line "certificate" "$CERT"
  line "fingerprint (sha256)" "$FP"
  line "notAfter" "$NOT_AFTER"
else
  line "certificate" "缺失：$CERT"
  fail=1
fi

# 4. 健康（带证书校验，证明 TLS 真的能通）
HEALTH=$(curl -s --max-time 10 --cacert "$CERT" "https://$PUBLIC_IP:$PORT/v1/health" 2>/dev/null)
if [ -n "$HEALTH" ]; then
  line "health" "ok"
  line "health body" "$HEALTH"
else
  line "health" "失败：curl --cacert 没拿到响应"
  fail=1
fi

# 5. 证书状态端点
INFO=$(curl -s --max-time 10 --cacert "$CERT" "https://$PUBLIC_IP:$PORT/v1/tls/info" 2>/dev/null)
if [ -n "$INFO" ]; then
  line "tls/info" "$INFO"
  DAYS=$(printf '%s' "$INFO" | sed -n 's/.*"days_remaining":\(-\{0,1\}[0-9]*\).*/\1/p')
  if [ -n "$DAYS" ] && [ "$DAYS" -le 0 ]; then
    line "expiry" "已过期（$DAYS 天）"
    fail=1
  fi
else
  line "tls/info" "读不到（不影响连通性，但客户端看不到剩余天数）"
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "SELFCHECK: PASS —— 把下面两行发给客户端配指纹："
  echo "  endpoint    : https://$PUBLIC_IP:$PORT"
  echo "  fingerprint : ${FP:-（未知）}"
  echo "  node_id     : 见 /v1/health 的 node_id"
  exit 0
else
  echo "SELFCHECK: FAIL —— 按上面标红的项排查（安全组 / ufw / journalctl）"
  exit 1
fi
