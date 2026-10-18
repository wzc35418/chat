# crates/core

P0Session 串行编排两套 OpenMLS 状态和真实 iroh 回环连接，提供双向 send、info、幂等 close 和 chat-p0 CLI。正式协调节点、outbox、群治理和附件服务仍未实现。

- `attachments.rs`：附件本地加密缓存（`AttachmentStore`）与记录持久化入口（`PersistedAttachmentStore`，组合缓存与正式加密库的 attachments 表）。导出/下载前核对记录与缓存；**FileKey 持久化在 `attachment_secrets`**（迁移 0007），因此关掉重开仍可导出/下载；密钥记录确实缺失时才显式 `ATTACHMENT_KEY_UNAVAILABLE`。上传/下载**支持断点续传**（迁移 0008）：每块成功后持久化已确认位图，重启后只补剩余块。另提供 `export_secret_descriptor` 把 FileKey 封装成 E2EE SecretDescriptor。
- `service.rs`：`ConversationCore` 会话/草稿/正式消息/查询/身份持久化的唯一内核入口。
- `send_orchestrator.rs`：CORE-007/008/009 的发送编排与 outbox 对账（节点端口可注入）。

- [P0 运行与实际验证范围](../../docs/P0.md)
- [后续完整实施计划](../../docs/implementation/core.md)

P0 局部验证不表示该模块全部功能已完成。
