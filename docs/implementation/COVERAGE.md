# 原设计与函数计划覆盖

覆盖表示已有实施说明，**不表示实现完成**。对照原始 [MVP](../design/MVP.md) 和 [核心接口](../design/CORE_INTERFACES.md)。

|需求/接口|对应函数/章节|集成工作项|
|---|---|---|
|ChatCore.start/close/observeEvents|[core](core.md)、[bridge](bridge.md)|P0-01、P2-01|
|createAccount/currentAccount|[identity](identity.md)|P1-02|
|邀请/接受/确认/指纹|[identity](identity.md)|P1-02|
|begin/authorize/completeDeviceLink/listDevices|[identity](identity.md)、[group-actions](group-actions.md)|P2-04|
|revokeDevice|[identity](identity.md)、[groups](groups.md)、[sync](sync.md)|P3-04|
|export/importIdentityRecovery|[identity](identity.md)、[platform](platform.md)|P2-04|
|openDirect/listConversations/listMessages|[core](core.md)、[storage](storage.md)|P1-07、P2-01|
|send/retry/cancelQueued/getDraftStatus|[core](core.md)、[sync](sync.md)|P1-05、P2-02|
|getDelivery/账户数量|[domain](domain.md)、[core](core.md)|P3-05|
|group.create/snapshot/propose|[groups](groups.md)|P3-01、P3-02|
|邀请接受/拒绝/operationStatus|[groups](groups.md)|P3-01|
|SetRole/SetMuted/RemoveMember|[group-actions](group-actions.md)、[domain](domain.md)、[groups](groups.md)|P3-02|
|Rename/TransferOwnership/Leave/Dissolve|[groups](groups.md)|P3-02|
|离线设备授权重新加入|[group-actions](group-actions.md)|P3-03|
|import/status/download/export/cancel附件|[attachments](attachments.md)、[crypto](crypto.md)|P2-03|
|Sync.observeStatus|从 set_lifecycle/set_connectivity/sync_now 的 SyncStatus 经 bridge_event_flow 投影；Kotlin 完整 SyncService 在 P2-01补齐|P2-01|
|syncNow/lifecycle/connectivity/configureNode|[sync](sync.md)、[platform](platform.md)|P2-05|
|MLS/统一事务/单群串行|[crypto](crypto.md)、[storage](storage.md)、[事务](TRANSACTIONS.md)|P0-02、P0-03|
|直连/穿透/连接中继|[transport](transport.md)|P0-05|
|离线信箱/持久日志/节点重启|[node](node.md)、[api](api.md)|P1-04、P1-06|
|鉴权/能力/限流/配额|[api](api.md)、[node](node.md)|P1-03、P5-01|
|Android前台/恢复前台，桌面托盘/退出|[platform](platform.md)|P2-05|
|配置包/可信插件/清单|[plugins](plugins.md)|P4-01、P4-02|
|SDK命令/选中消息/设置/提出草稿|[plugins](plugins.md)、[presentation](presentation.md)|P4-03|
|start/stop/禁用/移除/安全启动|[plugins](plugins.md)|P4-04|
|无插件文本回退|[protocol](protocol.md)、[presentation](presentation.md)|P2-01、P4-03|
|默认20账户/40设备/双设备/64KiB/25MiB/30天|[domain](domain.md)、[attachments](attachments.md)、[node](node.md)|P3-05、P5-03|
|线协议/SDK/schema分别版本化|[protocol](protocol.md)、[types](TYPES.md)、[ADR](../adr/0001-bootstrap.md)|P1-01、P4-02|
|发布/部署/安装包/基线测试|[工作项](TASKS.md)、[部署说明](../../deploy/README.md)|P5-01..03|

暂缓：已读、编辑、撤回、全文搜索、旧历史自动导入、FCM、Wasm、公开插件商店、机器人、任意Compose面板、协作投票、DHT/自动联邦/自动协调者选举、大群与音视频。它们沿用原设计范围，未用空函数冒充已计划的 MVP 任务。
