# 数据模型草案与实现约束

SQL 位于 [客户端初始迁移](../../crates/storage/migrations/0001_initial.sql) 和 [节点初始迁移](../../crates/node-services/migrations/0001_initial.sql)。仅做 SQLite DDL 可执行检查；尚未被 Rust 加载，未实现事务/provider或加密数据库。禁止把此DDL单独上线当成安全存储。

客户端：accounts/device_credentials/trusted_contacts 管身份信任；conversations/conversation_devices/policy/log/pending 管会话；mls_state_records/key_packages 管秘密材料；drafts/message_attempts/messages/receipts 分开意图、尝试、正式视图和送达；outbox/inbox/cursors 管可靠性；attachments/chunks 管受保护缓存；revocation父子任务和mailbox消费支持恢复。

节点：devices/discovery/key_packages 为公开设备材料；groups/members/control_heads/log 管公开排序授权；mailbox 存授权密文；uploads/chunks/objects 分离文件提交阶段；quotas/request_dedup/auth_challenges/node_jobs 支持配额、重放防护与恢复。节点字段不存明文消息、用户私钥或文件密钥。

## 实施前必须解决的 schema 细节

1. messages.submission_id 引用统一 message_attempts；origin 区分 local_application/local_control/remote。本地尝试必须绑定draft或operation，远端认证尝试两者为空。T3 收件先写认证后的 remote attempt 再物化消息；不得为收件伪造本地draft。
2. 表内 BLOB 的 protected 前缀只是数据契约，SQL不会自动加密。P0选全库加密后覆盖ID索引、WAL、临时文件和备份；必要记录再加应用层封装。
3. 加入/退出窗口、重复退出再加入、KeyPackage claim消费、origin/故障码、附件引用关系、恢复 journal、插件独立私有存储要在相应工作项补完整字段/表与迁移。不在第一迁移伪称已经支持所有行为。
4. 控制head/epoch变化规则冻结后补状态版本CAS与更强约束，不能由SQL猜测MLS协议。
5. node_jobs租约扩展、challenge库存上限、能力发行登记、附件下载范围绑定、邀请消费登记随实际实现补充。能力不是数据库里一个未签名布尔字段。
6. schema_migrations checksum由实际迁移工具计算，当前不写虚构已应用记录。

## 必须验证的约束

- group/seq 和 group/submission 唯一；同Submission不同摘要由服务拒绝。
- operationId/requestHash绑定，重复请求只一个意图。
- quota used/reserved不负且不超过limit；重复ACK/上传不得重复加减。
- MLS provider记录、消息、游标、outbox原子更新，不靠多笔事务“通常能完成”。
- 真正收件者集合按消息时点固定；当前device表不能替代历史范围。
- GC文件路径只能是受控cache/blob根；不能用上传文件名拼接系统路径。

这份模型是可审查起点。每次改变约束需补迁移和相应故障场景，不能直接手改用户库。
