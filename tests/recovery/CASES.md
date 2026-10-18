# recovery 验证场景

状态：场景设计，尚无自动化实现或运行结果。使用实际库/进程/目标平台；允许隔离测试双，但发布路径不可使用。

|用例|场景|预期|函数|
|---|---|---|---|
|RE-01|C01/C02/C03|T1与T2每步真实终止进程，恢复旧完整或新完整状态；不重用消费密钥。|[recover_incomplete_writes](../../docs/implementation/storage.md#storage-011)|
|RE-02|C04|commit后回调前终止，重启可查草稿/状态，不重复加密。|[publish_committed_state](../../docs/implementation/crypto.md#crypto-010)|
|RE-03|C05|节点commit后响应丢失，同Submission恢复同seq/回执。|[append_group_entry](../../docs/implementation/node.md#node-006)|
|RE-04|C06|本地T3后信箱ACK前终止，重复页一次有效消息/回执。|[apply_verified_entry](../../docs/implementation/storage.md#storage-008)|
|RE-05|C07/C09|本人pending接受/并发Commit拒绝/清理重建，密码状态不回滚。|[discard_rejected_pending](../../docs/implementation/crypto.md#crypto-008)|
|RE-06|缺控制与分叉|跳号先补齐，同seq异摘要暂停该群，不跳epoch。|[catch_up_group](../../docs/implementation/sync.md#sync-008)|
|RE-07|C08|块文件与DB登记之间终止，不发布半对象，可恢复/清理。|[complete_blob_upload](../../docs/implementation/node.md#node-014)|
|RE-08|C10|撤销部分群完成后终止，重启保留每群进度。|[revoke_device](../../docs/implementation/identity.md#identity-012)|
|RE-09|Android kill/桌面睡眠|无退出回调直接杀进程；恢复前台/唤醒重建连接并补齐。|[handle_platform_lifecycle](../../docs/implementation/platform.md#platform-009)|
|RE-10|迁移/系统密钥失败|迁移中断、错误密钥、系统保护无服务，不覆盖库、不落明文。|[open_store](../../docs/implementation/storage.md#storage-001)|
|RE-11|取消竞争|取消和append接受并发，最终显示正式事实，不重发双消息。|[cancel_queued](../../docs/implementation/core.md#core-009)|
|RE-12|租约恢复|worker发送后结果落盘前终止，过期租约原字节重发；旧worker结果不覆盖。|[run_outbox_once](../../docs/implementation/sync.md#sync-002)|

执行记录需保存：版本、设备、命令、注入点、观察值、证据、PASS/FAIL。当前结构检查未执行这些场景。
