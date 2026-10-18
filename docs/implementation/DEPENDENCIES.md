# 代码依赖与逻辑协作关系

函数章节中的调用关系描述业务协作。写源码时必须保持以下依赖，不能为了直接调用同名函数引入循环依赖。

|模块|允许依赖|边界|
|---|---|---|
|protocol|标准类型和锁定后的编码/验签原语|无数据库、UI、identity 服务或 node-api|
|domain|protocol|纯权限规则，消费已验证凭据和上下文|
|storage|protocol|只管持久化与事务，不 import crypto/core|
|identity|protocol、storage、注入平台保护端口|账户生命周期；公有凭据验证原语可共享给节点|
|crypto|protocol、identity、storage|OpenMLS/provider，不能依赖 sync/core|
|transport|protocol、注入 RequestSigner|只传字节/签请求，不调用服务器 node-api|
|sync|protocol、domain、storage、transport、注入任务执行端口|不反向依赖 core；加密准备和应用由 core 注入编排适配器|
|core|上述客户端模块|唯一组合根，构造服务和适配器，不 import ffi|
|ffi|core、protocol|生成绑定和句柄；关闭时调用 core，并释放 FFI 回调|
|node-services|protocol、domain、节点自己的 DB 适配|不复用客户端含秘密的 storage/identity 服务|
|node-api|protocol、node-services|鉴权、路由、限额与 HTTP/WS|

## 四个需要明确分开的端口

**CredentialVerifier**：不访问私钥、数据库或 UI 的公共凭据签名/绑定验证原语应放 protocol。identity.verify_credential 是带本地信任/撤销上下文的包装；节点通过自己的信任/撤销查询使用同一个纯原语。protocol.verify_candidate 接收已验证凭据，不反向调用 identity 服务。domain 对实际 MLS 成员结果比对前，应已经取得通过该原语验证的凭据集合。

**RequestSigner**：客户端 transport 获取一次性挑战，使用自己的 DeviceKeyRef 对规范 method/path/query/body hash/node/challenge/expiry 签名。服务端 authenticate_request 做验签和挑战消费，两者不互相 import。它们必须采用同一份 protocol 中的规范签名原文格式。私钥从不传给服务端。

**PrepareExecutor / ApplyExecutor**：sync 的 prepare_draft_job、apply_log_page 等是工作流位置；通过端口调用 core 注入的编排实现，再由 core 调 crypto 与 storage。sync 自身不 import core。端口参数只包含 ID、上下文和受限事务作用域，所有 MLS 操作仍在单群串行执行器中。

**EventSink / SubscriptionRegistry**：core 发布有界变更事件并关闭自己的队列；ffi 管理外部回调注册和释放。core.close_core 不调用 ffi.stop_event_bridge；由 FFI 拥有者安排停止回调、等待 core 关闭和释放句柄，避免 core→ffi 循环依赖。

Kotlin 同样保持 ui→presentation→core-api，bridge 实现 core-api 并持有生成绑定。插件 SDK 不 import host/shared内部实现；宿主经正常应用用例提出草稿。UI 渲染只使用已验证视图，函数章节中指向协议校验的链接是规则来源，不代表每次重组都跨 FFI 校验。

## 邀请范围

联系人邀请与群邀请使用不同 capability scope。invite_group_member 不直接复用 create_contact_invite 的 token；联系人接受仅是群邀请前置条件，不能授权自动加入群。群邀请必须绑定 group、target、inviter、期限与当前上下文。

现有 Cargo.toml 只登记本地路径依赖，外部库待 P0 锁定。本文与工作区依赖方向一致；实际接口类型、端口 trait 和 Kotlin DI 在对应工作项实现。
