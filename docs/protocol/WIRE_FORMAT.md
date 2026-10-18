# 线协议冻结前清单与字段语义

状态：协议草案。不能作为可互操作发布规范。逻辑字段来自原 CORE_INTERFACES，具体字节格式、算法和域分隔仍需 P0/P1-01 冻结。

## 必须冻结的内容

1. ID类型前缀/版本/长度/编码；整数字节序、最大值、空值和可选字段表示。
2. 帧长度头、最大帧、集合/嵌套预算；unknown 字段分类；不允许重复关键字段。
3. Candidate签名原文：版本、SubmissionId、GroupId、设备、kind、head/policy/epoch、密文摘要、单操作控制元数据与用途域。
4. 节点entry回执：群、seq、前序日志摘要、完整候选摘要；日志自身摘要计算、genesis定义。
5. SignedLogHead的独立用途域及control_head/policy/epoch绑定；ControlHead更新公式。
6. MLS凭据与设备主管签名格式、撤销版本、有效期/时钟偏差；KeyPackage单次claim规范。
7. 控制批次：公开单操作描述 + E2EE业务内容 + Commit关联，是否每控制推进epoch及如何证明实际增删一致。
8. 应用内层EventId/作者/群/类型/回复/扩展格式；本人已发消息确认路径。
9. 附件随机文件钥、块nonce唯一构造、AAD/结束边界/总块数/原文完整摘要；公共密文manifest分开。
10. 请求签名方法/规范path/query/body hash/node/challenge/期限；能力签名范围；WS短期token。

## 兼容与拒绝

协议主版本不兼容或安全必需能力缺失：拒绝。未知结构化应用内容：保留受保护payload并显示fallback。未知控制操作：停止该群分支；不得跳过继续推进epoch。数据库schema、SDK、内容schema独立演进。

## 必须具备的互操作向量

见 tests/protocol/CASES.md：规范编码同值同字节、修改任一签名字段失败、同Submission不同密文拒绝、节点回执改seq/group失败、内外作者不一致、畸形长度/重复字段/溢出、缺控制链、错误Welcome、附件交换/截断。

本次不生成假的密码学 expected bytes；真实向量须在算法和编码冻结后以两种独立实现交叉验证。测试私钥仅在明确测试fixtures中，发布路径不可加载。
