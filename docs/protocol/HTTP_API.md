# 节点路由与服务函数映射

状态：路由计划，尚未实现。设备鉴权覆盖方法、规范路径和查询、body摘要、节点身份、一次性挑战、有效期。每个变更资源带业务幂等键，业务重试取新鉴权挑战但保持原业务键。

|路由|主要服务函数|鉴权与边界|
|---|---|---|
|GET /v1/capabilities|negotiate_capabilities 对应公开声明|匿名限流，节点key最终仍须用户固定信任|
|POST /v1/auth/challenges|issue_auth_challenge|匿名库存/IP/全局预算|
|POST /v1/devices/register|register_device|主管凭据+设备持有证明+注册能力|
|PUT /v1/discovery/{deviceId}|publish_discovery|本人设备签名/版本/期限|
|GET /v1/discovery/{deviceId}|verify_discovery_record 对应查询|联系人或发现能力；不公开任意设备目录|
|POST /v1/key-packages|publish_key_packages|只能本人，库存/批次限额|
|POST /v1/key-packages/claim|claim_key_package|受限群加入/联系人能力，原子消费|
|POST /v1/mailbox/items|store_mailbox_item|持目标投递能力，TTL/配额/密文限制|
|GET /v1/mailbox/items|read_mailbox_page|当前设备，游标认证绑定scope|
|POST /v1/mailbox/acks|ack_mailbox_items|当前设备自己的条目|
|POST /v1/groups|register_group|创建能力，初始角色/设备签名|
|POST /v1/groups/{id}/entries|append_group_entry|群作者设备，head/policy/epoch/角色|
|GET /v1/groups/{id}/entries|fetch_group_log|成员可见内容窗口与控制恢复规则|
|GET /v1/groups/{id}/head|get_group_head|有权成员，签名一致头|
|POST /v1/blobs/uploads|begin_blob_upload|设备/上传能力，实际密文字节配额|
|PUT /v1/blobs/uploads/{id}/chunks/{index}|put_blob_chunk|会话owner能力，同index同摘要|
|POST /v1/blobs/uploads/{id}/complete|complete_blob_upload|owner/完整密文manifest|
|GET /v1/blobs/{id}/chunks/{index}|read_blob_chunk|对象/设备/群消息窗口/期限能力|
|GET /v1/events WebSocket|upgrade_event_socket|短期单次token，慢消费者有界断开|

错误 envelope：code、retryable、retry_after_ms?、operation_id?、detail_key；不回显明文或原始签名秘密。HTTP 400表示畸形，401鉴权，403权限，409前置状态/幂等冲突，413超长，429限流，503暂不可用。具体错误码在函数章定义，码表冻结时避免同名不同义。

管理接口默认关闭且独立绑定/授权，不复用“普通群主”作为节点管理员。节点镜像完成前不开放此草案端口。
