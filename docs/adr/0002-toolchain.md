# ADR 0002-toolchain: 工具链与FFI锁定

状态：已采纳当前 P0 Windows 工具链基线；三端运行矩阵待完成。日期：2026-10-18。

使用 Rust 1.99.1、JDK 21.0.12.1、Gradle 8.14.3、Kotlin/Compose compiler 2.2.20、Compose 1.9.3、AGP 8.11.1、UniFFI 0.32.2、Android SDK 36 / NDK 28.2.13676358 / minSdk 26。完整依赖版本和官方兼容来源见 [依赖记录](../DEPENDENCIES.md)。

正式共享模块采用 KMP desktop JVM + Android target，原生桥接源码共用于两端；Android JNA 使用 AAR，桌面使用 JAR。Gradle 自动生成 host 原生库与 Kotlin 绑定，Android JNI 任务另行依赖 NDK 构建。窗口运行和 IO 线程业务调用分开；UniFFI 业务关闭名为 shutdown，以免与生成对象 AutoCloseable.close 冲突。

Windows 的 Kotlin → Rust → MLS → iroh 双向往返已实际通过；Android APK 构建与三端剩余证据见 [P0 报告](../P0.md)。保留对 Android 真机、Linux 运行、取消边界和系统密钥保护的待验收状态。

决策模板：背景、候选与官方证据、选择、代价、失败替代方案、验证命令/设备/实际结果、影响函数与回滚策略。没有填写证据前不得标已采纳。
