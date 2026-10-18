# shared/presentation

P0Presenter 维护 StateFlow、忙碌互斥、错误和最多 100 条临时显示消息；不承担加密状态或可靠投递。

`display/` 提供 `MessageContent` 只读展示模型、安全文字回退、错误码文案与共享文案表；`ConversationDisplayMerger.kt` 提供与正式服务解耦的列表/消息分页纯展示合并器（迟到结果、重复 ID、追加/前置/重置）。`preview/` 提供正式聊天界面的显式样例状态。两者均不建立真实会话或投递状态。

- [P0 运行与实际验证范围](../../docs/P0.md)
- [后续完整实施计划](../../docs/implementation/presentation.md)

P0 局部验证不表示该模块全部功能已完成。
