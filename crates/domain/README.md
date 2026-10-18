# crates/domain

Rust 领域规则 crate（纯函数，不访问数据库/网络），DOMAIN-001~012 全部实现：

- `model`：`GroupSnapshot`/`GroupMember`/`Actor`/`GroupOperation`/容量/授权与策略结果；
- `group`：001/002/003/006/007（发言权限、群治理、容量、群主退出与转让）；
- `control`：004/005（策略操作应用、控制结果核对；冻结的控制头/操作摘要派生公式）；
- `delivery`：008/009/010（发送状态机、送达聚合、重试分类）；
- `visibility`：011/012（回复引用、加入可见性）；
- `error`：复用 `chat_protocol::ErrorCode` 的领域错误。

实现边界与语义（快照不变量、基线限额 20/40/2、管理员只能治理普通成员、唯一 owner、策略迁移/控制结果核对、状态表与重试表）见 [领域规则、群权限与状态机](../../docs/implementation/domain.md) 的「P1-01 实现边界与语义」。

验证命令：`. .\tools\activate.ps1; cargo test --offline -p chat-domain`（当前 43 passed）。
