# crates/identity

Rust identity 模块：账户根密钥与账户/设备 ID 派生、账户签名的设备凭据与生命周期、
一次性邀请口令、联系人状态机与人工核验指纹（P1-02：不接网络；持久化只经
chat-storage 的正式加密存储）。

已实现 IDENTITY-001~012：

- 001~007：账户/设备/指纹、inv1 邀请口令、联系人状态机（纯密码学与状态层）；
- 008~012：设备配对、授权、完成、列表、撤销（[registry.rs](src/registry.rs)，经
  迁移 0006 的 identity_device_* 表持久化，含过期/撤销/版本落后的发送前拒绝）；
- 013/014（导出/导入恢复文件）未开始。

验收：`cargo test --offline -p chat-identity`（34 lib + 9 device_registry +
9 identity_contract = 52 passed），`cargo test --offline -p chat-storage`（79 passed），
`cargo clippy --offline -p chat-storage -p chat-identity --all-targets` 新代码无警告。

- [账户、联系人、双设备与恢复](../../docs/implementation/identity.md)
- [存储契约](../../docs/implementation/storage.md)
