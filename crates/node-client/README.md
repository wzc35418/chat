# crates/node-client

节点 HTTP 客户端：一次性挑战 + Ed25519 设备签名 + 有界收发 + PROTOCOL-014 稳定错误映射。
端点为 [docs/implementation/api.md](../../docs/implementation/api.md) 已落地的挑战/注册/群日志/信箱端点。

## 模块

|文件|内容|
|---|---|
|src/endpoint.rs|`NodeEndpoint`：URL + 固定 node_id（+ 可选固定节点公钥）；拒绝 http(s) 之外、带凭据/查询/片段/路径前缀的 URL|
|src/client.rs|`NodeClient`：fetch_challenge / send_with_challenge（低层单发）/ register_device / create_group / append_entry / fetch_log / get_head / store_mailbox_item / read_mailbox_page / ack_mailbox_items / begin_blob_upload / put_blob_chunk / complete_blob_upload / read_blob_chunk；`ClientLimits`（响应上限 1 MiB、请求上限 64 KiB、超时 10s、尝试预算 ≤2）|
|src/error.rs|`NodeClientError`：PROTOCOL-014 码 + 分类（network/timeout/protocol/authentication/permission/conflict/not_found/invalid_request/limit/internal/config）|

签名原文完全复用 `chat_node_services::auth::request_signing_bytes` 与 `RequestMeta::canonical`，不存在第二套 canonical 规则。
高层方法每次调用都重新取一个一次性挑战；只在“挑战不可用（INVALID_CHALLENGE/AUTH_EXPIRED/AUTH_REPLAY）”或传输失败时重取一次，其余错误立即返回。

## 验证

```
. .\tools\activate.ps1
cargo test --offline -p chat-node-client
```

结果：13 个 lib 单元测试 + 11 个 `tests/client_limits.rs`（脚本化最小 HTTP 服务：重试预算、响应上限、超时、网络失败、不跟随重定向）+ 9 个 `tests/http_flow.rs`（进程内 `axum::serve` 起真实 node-api，端到端跑 challenge→注册→建群→append 幂等→log/head→信箱 store/read/ack 与错误场景）+ 5 个 `tests/blob_flow.rs`（附件 blob 上传/发布/读取往返、重复块幂等、乱序/缺块/摘要不符/越权拒绝）全部通过（共 38 个），0 警告。

## 本阶段未实现

API-005 逐设备限流、API-009 事件套接字；客户端也不构造 MLS 内容或附件密文（密文由上游加密层提供），附件块以原始字节收发、摘要由调用方按清单校验。
