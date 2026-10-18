# crates/identity

Rust identity 模块：账户根密钥与账户/设备 ID 派生、账户签名的设备凭据、一次性邀请
口令、联系人状态机与人工核验指纹（P1-02，纯密码学与状态层：不接网络、不写数据库）。

已实现 IDENTITY-001~007 的本地部分；IDENTITY-008~014（双设备配对、设备列表、
撤销任务、导出/导入恢复）未开始。持久化由 storage/core 集成任务补齐。

验收：`cargo test --offline -p chat-identity`（34 个单元测试 + 9 个集成测试全绿），
`cargo clippy --offline -p chat-identity --all-targets` 无警告。

- [账户、联系人、双设备与恢复](../../docs/implementation/identity.md)
