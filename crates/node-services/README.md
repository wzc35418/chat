# crates/node-services

独立节点服务的正式实现（`p0_accept` 仍是独立的 P0 T4/C05 局部事务实验）。当前状态：P1-03 的挑战/鉴权/设备注册已落地。

- [节点服务、排序日志、密文信箱与文件](../../docs/implementation/node.md)
- [节点 HTTP、鉴权、限流与运行入口](../../docs/implementation/api.md)
- [P0 节点接受与恢复实验](../../docs/P0_NODE_ACCEPTANCE.md)

## 模块

|文件|内容|
|---|---|
|src/services.rs|NodeService：节点身份固定、迁移、issue_auth_challenge（API-002）、authenticate_request / verify_device_signature（API-003）、register_device（NODE-001）|
|src/auth.rs|一次性挑战、RequestMeta 规范化、请求签名原文|
|src/registration.rs|设备凭据、注册能力、注册回执的类型与签名原文|
|src/canonical.rs|域分隔规范编码、十六进制、SHA-256、Ed25519 严格验签|
|src/error.rs|稳定错误码（INVALID_SIGNATURE / AUTH_REPLAY / AUTH_EXPIRED / DEVICE_LIMIT_EXCEEDED 等）|
|src/limits.rs|全部上限（标识长度、挑战 TTL、未消费容量、正文上限、设备上限、有效期上限）|
|src/p0_accept.rs|既有 P0 事务/崩溃恢复实验，未改动|
|migrations/0001_initial.sql|独立节点库初始 schema（既有）|
|migrations/0002_node_auth.sql|P1-03：挑战绑定字段、节点身份、设备公钥、能力消费、注册回执|

## 验证

```
. .\tools\activate.ps1
cargo test --offline -p chat-node-services
```

结果：11 个 lib 单元测试 + 18 个 `tests/auth_registration.rs` 集成测试 + 4 个既有 P0 测试全部通过。
`tests/p0_accept.rs` 仍需要 dev-dependency `chat-crypto`（用真实 MLS 设备产生密文/Welcome），它是 NODE-006 既有 P0 T4/C05 验收证据，属刻意保留；本轮新增的鉴权/注册测试只用 `ed25519-dalek` + `serde_json`。该测试目标会因此间接编译 `chat-crypto` 与 `chat-storage`。

## 本阶段未实现

NODE-002～NODE-016、逐 IP 限流（API-005）、run_node（API-010）与事件套接字（API-009）。节点目前只提供挑战与设备注册服务，不等同于可部署节点。
