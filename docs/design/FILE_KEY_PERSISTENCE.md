# ADR：附件 FileKey 的持久化与 E2EE SecretDescriptor

状态：提议（R-36 任务落地中）
影响：ATTACHMENTS-003/005/006、CRYPTO-011/012、STORAGE 附件记录、PLATFORM 密钥槽

## 背景

附件分块用每文件随机 FileKey 做 AEAD，FileKey 目前只存在于进程内存：
- 重启后 export/download 显式返回 ATTACHMENT_KEY_UNAVAILABLE（诚实但功能缺失）；
- 另一实例（甚至同一用户另一设备）无法下载自己上传过的附件；
- E2EE SecretDescriptor 未定义，附件无法作为消息内容安全传递。

## 决策（分三步）

1. **本地持久化（本轮）**：新增 attachment_secrets 表，FileKey 用主密钥派生的独立用途子密钥密封，AAD 绑定 attachment_id + 格式版本。附件导入时写入、导出/下载时读取；删除附件同时删 secret。任何解封失败（错主密钥、挪行、篡改）→ STATE_CORRUPTED，绝不降级为“没有密钥”。
2. **描述符化（下一步）**：把 FileKey 包装成 E2EE SecretDescriptor（版本字节 + 密文 + 摘要），使其可作为应用事件内层载荷随消息传递；接收方用群密钥解密描述符再下载分块。此时“对方能下载”才成为可验证的事实，界面才能显示“可下载”。
3. **平台密钥槽（依赖 PLATFORM）**：主密钥进入系统凭据库/TPM，secret 的用途子密钥由平台密钥派生，不再由调用方传 master_key_hex。

## 后果

- 好处：重启后可导出成为真实行为；为跨实例/跨设备下载铺路；secrets 与附件记录同一事务域，可审计。
- 代价：多一张表与一次密封开销；master_key 生命周期仍由上层提供，直到第 3 步落地。
- 明确不做：绝不把 FileKey 明文写盘；绝不在缺少密钥时假装可导出；绝不把“已上传”等同于“对方可下载”（需要描述符真正送达）。

## 验收口径

- 导入 → 关闭 → 同主密钥重开 → 导出逐字节一致；
- 错误主密钥 / 篡改 secret → STATE_CORRUPTED；
- 删除附件后 secret 不存在；
- 全盘扫描无 FileKey 明文。
