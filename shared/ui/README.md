# shared/ui

共享的真实 Compose P0 界面：设备连接/重建、Alice/Bob 切换、输入发送、实际解密结果与可折叠验证详情，支持宽屏和窄屏。

另含 `PreviewChatScreen` 正式聊天界面交互预览（显式样例数据、900 dp 双栏、窄屏单栏、口令弹窗与锁定/身份变化状态），由独立的 `:apps:desktop:previewApp` 入口启动，不连接 P0 真链路。

P0 与预览为输入框、发送设备、错误动作、返回/关闭和身份变化提示提供语义标签、状态描述和 live region；窄屏/短窗口使用整页滚动，大字体与短窗口截图见 `build/p0-snapshots/` 与 `build/preview-snapshots/`。

- [P0 运行与实际验证范围](../../docs/P0.md)
- [后续完整实施计划](../../docs/implementation/presentation.md)

P0 局部验证不表示该模块全部功能已完成。
