# 事务、锁与恢复约定

这些是不变量，所有函数实施时必须保持。事务分组是本计划定义的编排边界，不表示 SQLite 能与系统密钥槽、文件系统或另一数据库自动原子化。OpenMLS provider 能否同事务工作仍是 P0 阻断验证项。

## T1：持久化用户意图

局部实测（2026-10-18）：独立 P0 文件库的 draft 与 prepare job 在同一事务，C01 的 commit 前/后真实进程终止均恢复到一致状态；T2 从已提交意图读取正文并同时更新任务、MLS 状态和 outbox。见 [实验结果](../P0_PERSISTENCE.md)。下列仍是正式实现约定。

写入 draft/pending operation、唯一 operation ID 与请求摘要、加密内容、来源/回复、初始状态、prepare job。在一笔客户端事务中完成。只有 commit 成功才向 Kotlin 返回票据。失败不清编辑区；回调丢失时按 operationId/draftId 查询。

## T2：准备密码学尝试

1. 取得该群串行执行权，读取最后已确认业务和 MLS 状态。
2. 短事务内校验 expectedVersion，provider 的读/写均绑定同 tx。
3. 为合法草稿调用一次 MLS encrypt，或建立 pending Commit。
4. 保存已消费密钥状态、密文字节、摘要、SubmissionId、草稿关联、outbox。
5. commit 后才换内存状态/通知；失败使该事务 provider 的全部临时缓存失效。
6. 网络重试只读已保存字节，不再次 MLS encrypt。

控制 pending 分支与正式 MLS 状态分别持久化。库如果不能安全隔离 pending、恢复发送状态或合并并发胜出 Commit，P0 必须证明替代流程，不能将应用快照回滚当作密码学回滚。结果未知时先对账旧 SubmissionId；仅有明确且认证的拒绝证据才允许废弃旧候选重新构造。

## T3：应用正式已验证事件

同事务提交：group_log_entries、messages 物化记录、group_policy_snapshots、conversation_devices、MLS provider records、inbox_dedup、verified cursor、pending 状态确认、必要送达回执任务、mailbox 消费记录。正式条目必须依次验证 seq/hash→作者身份→业务权限→MLS→内外绑定→控制实际结果。

本机 pending Commit 被接受时用 merge_accepted_pending；普通自身已发送密文被接受时使用本机已保存内容与候选绑定验证，不重复解密/再次消费发送状态。接收远端 Commit 使用 stage/verify/merge 路径。公开角色和 MLS 设备集合不一致时停止该群分支，游标不前进。

若当前条目验证失败，之前已提交条目保持；该条目及之后暂停。不得为了“尽量显示消息”越过坏控制事件。其他群可以继续工作。

## T4：节点接受事件

局部实测（2026-10-18）：独立节点文件库验证密文、seq/哈希链、签名回执记录、耐久投递任务和群 head 同事务；commit 前/后进程终止、固定密文重试与并发分配通过。P0 回执已用调用方提供的 Ed25519 签名种子生成，并用预先固定的公钥验证；密钥托管、正式编码、HTTP 和鉴权未完成，正式 T4/C05 仍未完成。见 [节点实验报告](../P0_NODE_ACCEPTANCE.md)。

在 SQLite 写事务内：请求/业务幂等检查→当前 group/control_head/epoch/权限检查→分配 seq→形成哈希链和节点签名回执→日志/公开控制状态/dedup/耐久投递任务一起写→commit→返回回执。不要在 DB 事务中请求网络或 fanout。

同 group/submission_id 同摘要返回原回执；不同摘要拒绝；重试窗口内幂等结果不能提前被 GC。group_seq 单调分配且接受后不复用。节点对业务公开元数据负责，不能声称已经验证 MLS Commit 的解密结果。

## 锁顺序与并发

- core 生命周期锁只用于句柄状态，不持锁跨回调或网络。
- 群执行器→短存储事务；不得先持 DB 写锁再等待另一个群锁。
- 每群应用日志/prepare/merge/rebuild 串行，多群有界并行。
- 跨群设备撤销先持久化父任务和子任务，再逐群推进，无跨群“大原子事务”。
- outbox 租约使用 CAS 和版本，超时 worker 的迟到结果不得覆盖新状态。
- UI 订阅有界，溢出以 ResyncRequired 合并；持久真相靠查询恢复。

## 外部副作用边界

系统密钥槽创建→包装→DB 引用建立：记录意图，失败清理无引用槽，绝不删除在用槽。文件导入/上传：staging→文件同步→DB记录→published；崩溃后恢复或清孤儿。导出：受控临时目标→逐块认证→完整摘要→完成目标；无法原子 rename 的 Android provider 必须明确能力和清理策略。插件安装：staging + 待安装记录，完整后下次启动加载。

## 取消与接受竞争

未准备草稿可直接取消；已准备未投递可停止任务；已在途必须保留候选并对账。取消动作只承诺停止后续主动投递，不保证节点尚未接受。在途超时为 AcceptanceUnknown，不能立刻删除记录或把草稿换一个新 MessageId 再发。若正式回执后到，以正式接受事实更新界面。

## P0 故障注入点

|点|故障位置|恢复后必须成立|
|---|---|---|
|C01|T1 commit 前/后|没有草稿，或完整草稿+准备任务|
|C02|MLS encrypt/provider 写中|旧完整状态，或可重发的新完整状态；不能复用已消费 generation|
|C03|attempt/outbox 写中|MLS 与密文尝试成对；不丢失可重发密文|
|C04|DB commit 后、内存发布前|重启从库重建同状态，不再 encrypt|
|C05|节点 commit 后、HTTP 响应前|同 SubmissionId 重试返回同 seq/回执|
|C06|客户端 T3 后、Mailbox ACK 前|重复页不产生重复消息或回执|
|C07|本机 pending 被接受后、merge 中|恢复合并一次，不把本机 Commit 当新远端事件|
|C08|节点存块后、元数据登记前|staging 恢复/清理，半对象不能下载|
|C09|控制冲突清理与同步间|不能恢复已消费历史密钥；草稿可重新授权重建|
|C10|撤销父任务提交后、部分群完成|完成与待处理群进度可追踪，不能宣称全部完成|

每个点记录注入方式、进程终止方式、库/依赖版本、前后哈希/计数和实际结果。性能报告记录机器，不先写无实测延迟保证。
