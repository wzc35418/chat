# integration 验证场景

状态：场景设计，尚无自动化实现或运行结果。使用实际库/进程/目标平台；允许隔离测试双，但发布路径不可使用。

|用例|场景|预期|函数|
|---|---|---|---|
|IN-01|文字纵向|两真实客户端+节点完成QUEUED→ACCEPTED→DELIVERED，三种ACK区分。|[send_message](../../docs/implementation/core.md#core-007)|
|IN-02|离线信箱|B离线A发送，节点持久存，B恢复拉取并仅显示一次。|[consume_mailbox_page](../../docs/implementation/sync.md#sync-007)|
|IN-03|双设备|加入后新消息到达两设备，无加入前旧历史。|[complete_device_link](../../docs/implementation/identity.md#identity-010)|
|IN-04|群邀请和治理|三账户邀请/接受/角色/禁言/群名/移除/转让/退出/解散。|[propose_group_operation](../../docs/implementation/groups.md#groups-003)|
|IN-05|禁言与消息竞争|控制与应用按同seq排序，各端最终状态一致。|[set_group_muted](../../docs/implementation/group-actions.md#group-actions-003)|
|IN-06|自托管网络|关闭公共设施，测试直连/穿透/中继/信箱四场景。|[connect_peer](../../docs/implementation/transport.md#transport-003)|
|IN-07|附件完整性|交换块/文件/index、截断/缺最后块/错误原文摘要均失败。|[export_attachment](../../docs/implementation/attachments.md#attachments-006)|
|IN-08|插件正常行为|两个JAR真实加载→选中操作/草稿→用户确认；stop清注册；安全启动。|[start_plugin](../../docs/implementation/plugins.md#plugins-004)|
|IN-09|插件包攻击输入|ZIP穿越/符号链接/重复路径/炸弹/摘要替换/SDK不兼容拒绝。|[inspect_plugin_package](../../docs/implementation/plugins.md#plugins-001)|
|IN-10|容量与流式内存|20账户40设备/2设备/64KiB/25MiB边界和实际峰值内存记录。|[validate_capacity](../../docs/implementation/domain.md#domain-003)|
|IN-11|节点隐私|节点DB/blob/日志不出现测试正文、群名或用户密钥；元数据泄漏范围记录。|[run_node](../../docs/implementation/api.md#api-010)|
|IN-12|送达聚合|同账户2设备只计1账户，本人其他设备不计。|[aggregate_delivery](../../docs/implementation/domain.md#domain-009)|

执行记录需保存：版本、设备、命令、注入点、观察值、证据、PASS/FAIL。当前结构检查未执行这些场景。
