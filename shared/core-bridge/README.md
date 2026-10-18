# shared/core-bridge

JVM 与 Android 共用 UniFFI 绑定适配；所有阻塞原生操作在 Dispatchers.IO 中执行，访问串行化，构造失败/取消与关闭路径释放原生句柄。Gradle 自动生成绑定及原生库。

- [P0 运行与实际验证范围](../../docs/P0.md)
- [后续完整实施计划](../../docs/implementation/bridge.md)

P0 局部验证不表示该模块全部功能已完成。
