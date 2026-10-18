# crates/ffi

通过 UniFFI 生成 P0Core 对象、结果 DTO 和稳定输入错误。原生业务关闭方法为 shutdown，Kotlin 桥接再 destroy 句柄，避免与生成对象的 close 冲突。

- [P0 运行与实际验证范围](../../docs/P0.md)
- [后续完整实施计划](../../docs/implementation/bridge.md)

P0 局部验证不表示该模块全部功能已完成。
