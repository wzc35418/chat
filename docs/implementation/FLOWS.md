# 关键用例的函数调用链

这里按实际处理次序拆开单函数之间的关系；具体契约从 [函数索引](FUNCTION_INDEX.md) 进入。每条链标出何时改变正式状态、何时持久提交、何时允许重试。

## 1. 应用启动与锁定

平台初始化 → bootstrap_application → ffi_create_core → start_core → unwrap_storage_key → open_store → migrate_schema → recover_incomplete_writes → 群执行器加载 → start_endpoint → sync_now。无网络可停在本地 Ready；密钥保护不可用进入 Unlock；数据库损坏进入 RecoveryRequired，不能把它误认为首次启动。

core 成功后 bridge_event_flow 注册有界回调，UI 查询会话第一页。页面退出仅释放页面订阅，应用显式退出才 close_core。桌面关窗口转托盘，Android 后台可靠性来自耐久 outbox/信箱。

## 2. 一条文字消息

1. submit_composer 冻结输入与 operationId，交 send_message。
2. send_message 校验会话、内容、回复、当前权限，T1 persist_send_intent + enqueue_job + commit_transaction。
3. 返回 SendTicket(QUEUED)。UI 此时才清掉对应输入；不显示 ACCEPTED。
4. run_outbox_once 租用 Prepare 任务，prepare_draft_job 重查最新群权限。
5. T2 prepare_application → persist_prepared_attempt → crypto_provider_write + 原密文/SubmissionId/outbox → commit_transaction → publish_committed_state。
6. submit_attempt_job → append_candidate；可同时 P2P 预传，但预传不是正式接受。
7. 节点 authenticate_request → append_group_entry，在 T4 内查幂等、检查 head/epoch/权限、分配 seq、签回执并落盘。
8. 发送方 apply_log_page 验证正式条目，将本机尝试关联 MessageId，T3 保存 ACCEPTED。本人已发 application 不重复消费发送密钥。
9. 接收方拉取、验证、解密并 T3 提交后生成端到端回执；发送方 process_delivery_receipt → aggregate_delivery，才显示 DELIVERED。

网络超时后不清理 attempt、不生成新密文；以相同 SubmissionId 和 bytes 查询/重发。若节点明确拒绝旧 epoch，先补齐，再按当前权限新建尝试，保持 DraftId。

~~~mermaid
sequenceDiagram
    participant UI as Kotlin UI
    participant Core as Rust Core
    participant DB as Client SQLite
    participant Node as Coordinator
    participant Peer as Recipient
    UI->>Core: send(request, operationId)
    Core->>DB: T1 intent + prepare job
    DB-->>Core: committed
    Core-->>UI: QUEUED ticket
    Core->>DB: T2 MLS + immutable ciphertext + outbox
    Core->>Node: append same SubmissionId / bytes
    Node->>Node: T4 permission + seq + log + receipt
    Node-->>Core: signed accepted entry
    Core->>DB: T3 verified acceptance
    Node-->>Peer: hint / mailbox availability
    Peer->>Node: fetch by durable cursor
    Peer->>Peer: verify chain / permission / MLS
    Peer->>Peer: T3 persist and queue receipt
    Peer-->>Core: authenticated delivery receipt
    Core->>DB: save receipt and aggregate accounts
    Core-->>UI: DELIVERED via requery
~~~

## 3. 收件、去重与确认

pull_mailbox → consume_mailbox_page。若页里是 seq 8 而本地只有 seq 5，catch_up_group 先拉6、7，再处理8。每条 verify_log_entry → 领域授权 → process_verified_entry → verify_inner_event / verify_control_result → apply_verified_entry → commit_transaction → publish_committed_state。

inbox_dedup 只接受认证后的 EventId；外层 SubmissionId 可用于已验签候选的尝试去重，不能使用未认证 UI message ID 预先吞掉消息。重复同 seq/hash 直接确认已应用；同 seq 不同 hash 暂停群。T3 内排队 delivery receipt；receipt 类型不再生成 receipt，避免循环。Mailbox ACK 只能在本地提交或安全持久隔离后发送。缺控制日志仍等待补齐，不能 ACK 丢弃后无法恢复。

## 4. 新群与邀请

create_group → T1 创建意图 → create_crypto_group(T2) → register_group(T4) → 验证节点初始回执(T3)。群主邀请 B：propose_group_operation(InviteMember) → B 收到签名邀请 → accept_group_invite → generate_key_package → 发布接受响应 → 管理方 claim_key_package/validate_key_package → prepare_group_operation → prepare_control(T2) → append(T4) → 双方验证已接受控制事件 → 管理方 merge_accepted_pending → Welcome 投递到 B 授权信箱 → B join_from_welcome(T3)。

邀请未接受不加入。Welcome 的群/设备/KeyPackage/正式控制锚必须匹配。新成员只获加入后的内容能力；恢复控制链材料与可见旧正文分开。

## 5. 两名管理员并发 Commit

A、B 基于 head H/epoch E 准备不同 pending Commit。节点 T4 先接受 A，使控制前置状态改变；B 收明确 STALE_CONTROL_HEAD/STALE_EPOCH。

B 先确认旧 SubmissionId 未被接受 → discard_rejected_pending（只走库支持的分支清理，不回滚已消费密码学状态）→ catch_up_group 应用 A → authorize_group_operation 用新角色和集合复核 B 的原意图 → 仍合法则新 SubmissionId/new ciphertext → T2/T4/T3。若 A 已撤销 B 权限，B 原意图直接失败。

超时不等于明确拒绝。节点回执异常或结果不清晰时先同 ID 对账；不能为了快速重试并行提交旧/新两份可能同时接受的同一业务意图。

## 6. 禁言与消息竞争

禁言操作和普通消息进同一 group_seq。若消息 seq 在禁言前，按先前策略验证；若消息后到，以新的 head/policy/epoch 要求拒绝。P2P 先收到了密文也不能先触发正式消息行为。客户端使用前序已验证策略重算，不能只相信节点“合法”标记。

## 7. 撤销第二台设备

revoke_device → T1 撤销版本、父任务、每会话子任务、能力失效发布一起耐久保存 → 逐群 propose_group_operation(SystemDeviceRemove) → MLS Remove 全部目标叶子 → 节点确认 → verify_control_result → T3 状态更新 → 子任务 done。

账户撤销记录改变与各群 epoch 更新是两个完成条件。节点离线的群显示 pending，不宣称后续全部不可解密。设备已经得到的旧内容不能撤销。撤销通知、设备变更证明和 KeyPackage 发布控制必须由主管账户授权，不能让普通群成员伪造 System 操作。

## 8. 附件发送与导出

choose_import_file → open_file_token → import_file：逐块检查原文累计大小、encrypt_attachment_chunk、写受保护缓存 → upload_attachment：begin_blob_upload / put_blob_chunk / complete_blob_upload → build_attachment_descriptor → send_message(E2EE Attachment)。文件密钥只在 E2EE descriptor，不在公开 blob manifest。

接收方 download_attachment 用能力拉密文 → 每块认证 → 完整规则/摘要检查 → 本地 available。用户选择导出目标 → export_attachment：临时受控目标、逐块认证解密、检查 total/final/size/hash，全部通过才完成目标。Android URI provider 不支持 rename 时要用已验证的等价完成/清理策略。

## 9. 插件操作

inspect_plugin_package（不执行代码）→ 用户选择 trusted-jvm → install_trusted_plugin → 重启 discover_plugins → 首次执行 start_plugin → register_plugin_command/register_message_action → 宿主签发短期 UserActionToken → selected_content → propose_plugin_send → 填编辑区并标来源 → 用户确认 submit_composer → 正常 send_message。

禁用：配置先保存 disabled → stop_plugin 取消 scope/清 registration/token → 下次启动不再加载。可信 JVM 插件与应用同权限，SDK 并非恶意代码隔离；本链只描述正常插件行为。插件 init/stop 异常要记录脱敏故障，安全启动可以跳过全部第三方代码。

## 10. 节点重启和 Android 恢复前台

节点启动恢复数据库与 staged blobs 后才监听。append 已返回接受的日志必须仍存在，信箱未过期数据可继续拉。客户端恢复前台：handle_platform_lifecycle → set_lifecycle → 重建 endpoint/WS → sync_now → 按持久 cursor 拉取，不依赖系统此前有没有发退出回调。
