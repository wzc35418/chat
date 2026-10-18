# crates/node-services

独立节点服务的正式实现（`p0_accept` 仍是独立的 P0 T4/C05 局部事务实验）。当前状态：P1-03 的挑战/鉴权/设备注册，以及本轮 NODE-005~008 群登记/排序日志与 NODE-009~011 密文信箱。

- [节点服务、排序日志、密文信箱与文件](../../docs/implementation/node.md)
- [节点 HTTP、鉴权、限流与运行入口](../../docs/implementation/api.md)
- [P0 节点接受与恢复实验](../../docs/P0_NODE_ACCEPTANCE.md)

## 模块

|文件|内容|
|---|---|
|src/services.rs|NodeService：节点身份固定、迁移、issue_auth_challenge（API-002）、authenticate_request / verify_device_signature（API-003）、register_device（NODE-001）|
|src/group.rs|register_group（NODE-005）、append_group_entry（NODE-006，单事务 + seq/哈希链 + 幂等）、fetch_group_log（NODE-007）、get_group_head（NODE-008）、audit_group_log（运维自检，无 HTTP 端点）|
|src/mailbox.rs|store_mailbox_item（NODE-009）、read_mailbox_page（NODE-010，PROTOCOL-012/013 认证游标）、ack_mailbox_items（NODE-011）|
|src/blob.rs|begin_blob_upload（NODE-012）、put_blob_chunk（NODE-013，按序/幂等/边界）、complete_blob_upload（NODE-014，整体摘要+原子发布）、read_blob_chunk（NODE-015，成员窗口授权）|
|src/auth.rs|一次性挑战、RequestMeta 规范化、请求签名原文|
|src/registration.rs|设备凭据、注册能力、注册回执的类型与签名原文|
|src/canonical.rs|域分隔规范编码、十六进制、SHA-256、Ed25519 严格验签|
|src/error.rs|稳定错误码（INVALID_SIGNATURE / AUTH_REPLAY / SUBMISSION_ID_CONFLICT / STALE_CONTROL_HEAD / INVALID_CURSOR / QUOTA_EXCEEDED 等）|
|src/limits.rs|全部上限（标识长度、挑战 TTL、正文/查询上限、设备上限、群成员与分页上限、信箱配额、ACK 批量）|
|src/p0_accept.rs|既有 P0 事务/崩溃恢复实验，未改动|
|src/append_crash_worker.rs|仅测试用子进程：在 NODE-006 追加事务的检查点 abort，用于“无半条日志”的证据|
|migrations/0001_initial.sql|独立节点库初始 schema（既有，未改动）|
|migrations/0002_node_auth.sql|P1-03：挑战绑定字段、节点身份、设备公钥、能力消费、注册回执（未改动）|
|migrations/0003_group_mailbox.sql|群登记回执与信箱条目回执两表（复用 0001 的 groups/group_log/mailbox_items）|
|migrations/0004_blob_attachments.sql|附件 blob：上传会话字段、块内容表、发布回执表、已发布字节数（复用 0001 的 blob_uploads/blob_chunks/blob_objects）|

## 验证

```
. .\tools\activate.ps1
cargo test --offline -p chat-node-services -p chat-node-api
```

结果：11 个 lib 单元测试 + 18 个 `tests/auth_registration.rs` + 13 个 `tests/blob.rs` + 10 个 `tests/group_log.rs` + 7 个 `tests/mailbox.rs` + 4 个既有 `tests/p0_accept.rs` 全部通过（共 63 个）；`chat-node-api` 另有 16 个端到端测试。
`tests/p0_accept.rs` 仍需要 dev-dependency `chat-crypto`（用真实 MLS 设备产生密文/Welcome），它是 NODE-006 既有 P0 T4/C05 验收证据，属刻意保留。

## 本阶段未实现

NODE-002～004（发现记录与密钥包）、NODE-016（TTL 回收）、API-004 能力校验（含签名下载能力令牌）、API-005 逐设备限流、API-009 事件套接字、API-010 run_node；附件密文块存在节点 SQLite（单库单事务）而不是独立文件系统，节点目前仍是单进程、单 SQLite 连接串行的服务形态，不等同于可部署节点。