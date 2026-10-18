# P0-03：OpenMLS / SQLite / outbox 原子持久化实验

状态：**Windows x64 独立实验通过；P0-03 整体验收未通过**。日期：2026-10-18（Pacific/Auckland）。复现命令：

```powershell
. .\tools\activate.ps1
cargo test --locked --offline -p chat-crypto --test p0_persistence -- --nocapture
```

追加 T1/C01 实验：独立文件 SQLite 中持久化 draft、operation ID、请求摘要与 prepare job，并让 T2 从该意图读取正文。2026-10-18 13:24（Pacific/Auckland），`tools/verify-p0.ps1` 不带锁刷新参数完整通过：7 项专项测试、11 个进程 `abort()` 断点，Rust workspace、Kotlin/原生链路和 Android arm64 构建同时通过。产物哈希见 [机器可读结果](p0-validation-latest.json)，原始专项输出位于 `build/p0-evidence/persistence-crash.log`。

实现位于 [`crates/storage/src/store.rs`](../crates/storage/src/store.rs)、[`crates/crypto/src/persistence.rs`](../crates/crypto/src/persistence.rs)；子进程故障注入见 [`crash_worker.rs`](../crates/crypto/src/crash_worker.rs)，断言见 [`p0_persistence.rs`](../crates/crypto/tests/p0_persistence.rs)。一键回归 `tools/verify-p0.ps1` 会运行这些测试，并将原始输出写入 `build/p0-evidence/persistence-crash.log`。

## 验证方式与边界

两个独立测试设备分别使用文件 SQLite 数据库、Ed25519 签名密钥和真实 OpenMLS 群状态，完成 KeyPackage、Welcome、应用消息收发。`openmls_sqlite_storage 0.3.0` 的 provider 借用与业务表相同的 `rusqlite 0.37.0` 连接。一次操作只持有一笔 `BEGIN IMMEDIATE` 事务：OpenMLS 读写、固定密文 attempt、outbox，或 OpenMLS 接收状态、inbox 去重，最终一起 commit。`journal_mode=WAL`、`synchronous=FULL`；启动时执行 OpenMLS 自带 migration，业务试验表单独创建。

T1 在单笔事务写入 `p0_intents` 与 `p0_prepare_jobs`，仅 commit 后返回；重复 draft/operation 与正文必须完全一致。重启查询 `waiting_intents` 可恢复待准备任务。T2 在同一笔事务从意图读取正文，重新 `MlsGroup::load`，写入 OpenMLS 状态、attempt、outbox，并把意图与任务同时改为已准备。提交后同一 SubmissionId 只返回已保存密文，其他 SubmissionId 被拒绝。失败时 SQLite 回滚并丢弃本次加载的群对象；重启再从库加载。接收端相同 SubmissionId 与密文摘要、发送者绑定，重复调用直接返回已提交明文。本实验的 outbox 只验证固定密文持久化，尚无租约、网络投递和节点回执。

子进程在指定断点向 stderr 输出标记后调用 `std::process::abort()`；父进程要求其异常终止，重新打开文件库，执行 `PRAGMA integrity_check` 并检查 attempt/outbox/inbox 行数及实际 MLS 可用性。测试在系统临时目录创建数据库，用后删除；不使用 in-memory SQLite 替代崩溃试验。

| 场景 | 实际断点与重启后结果 |
|---|---|
| C01 局部：T1 draft 写入后、commit 后各 abort | commit 前 draft/job 均为 0；commit 后两表均为 1 且任务可查询；相同意图幂等，冲突摘要/operation 拒绝 |
| T1→T2：MLS 写入后、attempt 写入后、commit 后各 abort | commit 前意图仍 queued、任务 waiting、attempt/outbox 均为 0；commit 后意图 prepared、任务 complete、attempt/outbox 均为 1；恢复后沿用同一密文 |
| C02 局部：发送 MLS 写入后 abort | attempt/outbox 均为 0；重新加密后接收端成功解密；下一 generation 可继续使用 |
| C03 局部：attempt 写入后、outbox/commit 前 abort | attempt/outbox 均为 0；重新准备后两表同时出现且可解密 |
| C04 局部：commit 后、返回前 abort | attempt/outbox 均为 1；同 SubmissionId 返回完全相同的密文字节；不同正文同 ID 被拒绝；下一 generation 可继续使用 |
| 接收侧 C06 局部：MLS 写入后、inbox 写入后、commit 后各 abort | 前两点回滚，提交后保留；重启处理相同密文均得到一条 inbox，重复调用不再次消费接收密钥 |
| 拒绝路径 | 篡改应用密文导致 OpenMLS 处理失败；回滚、重新加载后原始合法密文仍可解密；同 ID 换密文或发送者被拒绝 |
| SQL 写失败 | 用触发器使 outbox INSERT 报错；事务回滚后 attempt 不存在，解除故障后合法发送和解密成功 |
| T1 SQL 写失败 | 用触发器使 prepare job INSERT 报错；draft 一并回滚，解除故障后可提交完整意图 |

以上断言验证了本地进程异常终止和 SQL 错误条件下的**同一 SQLite 连接事务边界**。没有模拟断电、电源缓存丢失或文件系统故障，因此不能从 `synchronous=FULL` 推出断电耐久保证。实验库**未加密**，包含 OpenMLS secret、消息明文/摘要/索引；不得用于真实账户或生产数据。Android/Linux 上的 SQLite/provider 行为尚未实测。现有 Kotlin/iroh P0 页面仍使用内存 `MlsDevice`，尚未接入此实验。

## 未完成与下一步

- C01 只在独立试验表跑通；正式 T1 草稿/准备任务、正式 C05 节点提交与 HTTP 回执丢失、C06 完整 Mailbox ACK、C07 pending Commit merge、C08 附件 staging、C09 控制冲突和 C10 撤销任务均未运行。[独立节点接受实验](P0_NODE_ACCEPTANCE.md) 已覆盖 C05 的本地事务与固定密文重试，但无 HTTP/签名；上表“C06 局部”仅表示接收侧本地事务与去重。
- 生产 schema、并发群执行器、CAS outbox 租约、未知结果同 SubmissionId 节点对账及经认证拒绝后的处理尚未实现；未证明跨进程多写者或 SQLite busy/磁盘满时的完整恢复策略。
- 需要验证 pending Commit、外来 Commit、拒绝输入和身份校验在 provider 中的全部写入路径，并给出本机候选与正式已验证分支的事务约束。
- 选择全库加密与系统密钥保护，覆盖 DB/WAL/备份/临时文件；之后将相同故障测试接入正式存储 API 和三端运行环境。

此结果支持继续采用“单连接事务 + 失败丢弃内存群对象”作为 P0-03 候选方案；生产方案仍以完整 C01..C09 和加密存储验证为决策条件。
