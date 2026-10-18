# protocol 验证场景

状态：场景设计，尚无自动化实现或运行结果。使用实际库/进程/目标平台；允许隔离测试双，但发布路径不可使用。

|用例|场景|预期|函数|
|---|---|---|---|
|PR-01|规范编码|跨语言同值同字节，字段顺序输入差异不影响规范编码。|[encode_canonical](../../docs/implementation/protocol.md#protocol-002)|
|PR-02|作者签名覆盖|逐项改变group/device/head/policy/epoch/payload/metadata必须验签失败。|[verify_candidate](../../docs/implementation/protocol.md#protocol-005)|
|PR-03|节点回执绑定|改变seq/previous/group/candidate摘要失败，未知节点key不自动信任。|[verify_log_entry](../../docs/implementation/protocol.md#protocol-007)|
|PR-04|内外身份绑定|合法MLS但外层作者错误、伪造外层EventId不能成为可见消息。|[verify_inner_event](../../docs/implementation/protocol.md#protocol-009)|
|PR-05|幂等冲突|同Submission同字节同回执；相同Submission异摘要冲突拒绝。|[append_group_entry](../../docs/implementation/node.md#node-006)|
|PR-06|畸形输入预算|巨大长度/深嵌套/重复关键字段/溢出/截断有界拒绝。|[decode_bounded](../../docs/implementation/protocol.md#protocol-003)|
|PR-07|KeyPackage|过期/重复claim/错误设备/错误套件拒绝。|[validate_key_package](../../docs/implementation/crypto.md#crypto-003)|
|PR-08|能力和鉴权重放|跨节点/方法/path/query/body/设备/资源scope重放拒绝。|[authenticate_request](../../docs/implementation/api.md#api-003)|

执行记录需保存：版本、设备、命令、注入点、观察值、证据、PASS/FAIL。当前结构检查未执行这些场景。
