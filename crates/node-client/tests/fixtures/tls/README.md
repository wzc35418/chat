# TLS 测试夹具（**仅测试用**）

这里是一张**自签证书 + 私钥**，只为 `node-client` 的 TLS/固定证书测试服务端使用，
**不得**用于任何真实部署；真实部署应各自生成自己的证书并把指纹固定到客户端。

| 文件 | 用途 |
| --- | --- |
| `test-cert.pem` | 自签证书（CN=127.0.0.1，SAN=IP:127.0.0.1，CA:TRUE，serverAuth） |
| `test-key.pem` | 对应私钥（**测试专用**） |
| `openssl.cnf` | 生成用的配置（保证 SAN 含 127.0.0.1，否则 reqwest 会拒绝主机名校验） |

重新生成：

```powershell
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout test-key.pem -out test-cert.pem -config openssl.cnf -extensions ext
```
