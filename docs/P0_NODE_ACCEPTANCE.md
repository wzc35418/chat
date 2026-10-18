# P0 节点接受事务与未知结果恢复实验

状态：**Windows x64 独立本地实验通过；正式 T4 / C05 未通过**。日期：2026-10-18（Pacific/Auckland）。一键回归于 14:56 通过，结果见 [机器可读报告](p0-validation-latest.json)，专项原始输出在 `build/p0-evidence/node-acceptance.log`（4 项测试全部通过，随后补跑含非 ASCII 伪造签名的针对性断言）。

```powershell
. .\tools\activate.ps1
cargo test --locked --offline -p chat-node-services --test p0_accept -- --nocapture
```

实现位于 [`p0_accept.rs`](../crates/node-services/src/p0_accept.rs)，子进程断点位于 [`p0_crash_worker.rs`](../crates/node-services/src/p0_crash_worker.rs)，测试位于 [`p0_accept.rs`](../crates/node-services/tests/p0_accept.rs)。该代码只供 P0 事务实验，未接入 `apps/node` 或桌面 UI。

节点使用独立文件 SQLite，启用 WAL、`synchronous=FULL`、外键和 `BEGIN IMMEDIATE`。在同一事务内检查 `(group_id, submission_id)`，为新密文分配递增 seq，写入不可变密文、SHA-256 摘要、前项哈希、链哈希、序列化回执与待投递任务，并更新群 head。相同 ID 和相同密文字节返回原回执；相同 ID 换密文拒绝。重复查询还核对已存摘要、回执字段、签名和投递任务。

本轮补入 **P0 专用 Ed25519 回执签名**。节点签名种子由调用方提供，不保存在节点 SQLite；实验库只保存对应公钥，并拒绝以另一把密钥重开继续追加。测试夹具在临时目录生成种子供崩溃子进程重开。签名覆盖域分隔字符串、长度分隔的 group/submission ID、seq、密文摘要、前项哈希与链哈希。验证端必须预先固定节点公钥，不能仅相信回执里自报的公钥。测试证明错误公钥/种子、改动签名/序号/密文、非 ASCII 伪造签名和篡改数据库回执均被拒绝且不 panic。此编码尚未冻结为正式线协议；测试种子以明文临时文件传给子进程，**没有生产密钥保护或节点身份分发**。

| 实测场景 | 结果 |
|---|---|
| 写入日志后、commit 前 `abort()` | 重启后日志、head、投递任务均为旧状态；重试得到 seq 1 |
| 写入投递任务和 head 后、commit 前 `abort()` | 三者一起回滚；重试得到 seq 1 |
| commit 后、函数返回前 `abort()` | 重启仍有 seq 1 和完整投递任务；客户端从 T1/T2 outbox 读取原 MLS 密文重试，返回相同回执和 seq；换密文拒绝 |
| 同群第二条消息 | seq 2，`previous_hash` 等于第一条链哈希；接收设备实际用 OpenMLS 解密第一条密文 |
| 8 个并发 SQLite 连接 | 得到唯一连续的 seq 1..8，日志审计通过 |
| 回执签名与存储篡改 | 固定公钥验证通过；错误公钥、另一把签名种子、签名、序号或密文拒绝；回执行被篡改后重开库查询、重试和审计均拒绝 |

父进程确认子进程异常终止及断点标记，重开库后运行 `PRAGMA integrity_check`、`foreign_key_check` 与日志审计。三个断点计入本次 P0 回归总计 14 个进程终止断点。本实验用进程终止模拟“节点已提交但调用方没有拿到结果”；**尚未实现 HTTP 请求/响应，因此不等同正式 C05 的 HTTP 响应丢失验证**。也未模拟断电或文件系统失效。

剩余：正式节点请求鉴权、作者签名与权限/epoch/control-head 校验、生产签名钥托管/公钥固定及正式回执编码、真实 HTTP 重试与客户端对账、投递 worker 与 Mailbox ACK、生产存储迁移/加密、磁盘满与超时等错误路径、跨进程和跨平台部署验证。实验数据库未加密，不用于真实用户数据。正式 [NODE-006](implementation/node.md#node-006) 与 [T4/C05](implementation/TRANSACTIONS.md) 继续保持未完成。
