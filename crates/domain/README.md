# crates/domain

Rust 领域规则 crate（纯函数，不访问数据库/网络）：

- `model`：`GroupSnapshot`/`GroupMember`/`Actor`/`GroupOperation`/容量与授权结果；
- `group`：DOMAIN-001/002/003/006/007（发言权限、群治理、容量、群主退出与转让）；
- `delivery`：DOMAIN-008/009/010（发送状态机、送达聚合、重试分类）；
- `visibility`：DOMAIN-011/012（回复引用、加入可见性）；
- `error`：复用 `chat_protocol::ErrorCode` 的领域错误（不可自动重试）。

实现边界与语义（快照不变量、基线限额 20/40/2、管理员只能治理普通成员、唯一 owner、状态表与重试表）见 [领域规则、群权限与状态机](../../docs/implementation/domain.md) 的「P1-01 实现边界与语义」。DOMAIN-004/005 需要 MLS Commit 阶段结果，未实现。

验证命令：`. .\tools\activate.ps1; cargo test --offline -p chat-domain`。
