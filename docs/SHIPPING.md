# 交付说明（可安装/可运行版本）

版本：**0.1.0**（便携包，无需安装器）

## 产物

```
build/dist/Chat-0.1.0.zip          交付压缩包（约 49 MB）
build/dist/Chat-0.1.0/
  app.jar                          全部运行期 classpath（含 shared/* 与 desktop 模块）
  native/chat_ffi.dll              Rust host 动态库（本机库解析的目标）
  run.bat                          启动脚本（java -Djna.library.path=native -jar app.jar）
  README.txt                       运行前提与数据位置
```

## 构建

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
.\gradlew.bat :apps:desktop:portableZip --offline --max-workers=1
# 产物：build/dist/Chat-0.1.0.zip
# 需要换版本号：加 -PchatVersion=0.1.1
```

## 运行前提

1. **JDK 21**（`java -version` 能输出 21.x）；
2. Windows x64；
3. 首次运行会在用户目录下创建加密数据库（口令由密钥槽封装）。

## 已验证（打包产物本身，不是源码构建）

从 `build/dist/Chat-0.1.0/` 用打包出来的 `app.jar` + `native/` 直接运行冒烟：

```powershell
java -Djna.library.path=<dir>\native -cp <dir>\app.jar chat.desktop.PlatformSmokeKt
# 输出: PASS platform slice: notifications honest-unavailable, lifecycle transitions, single exit
```

GUI 启动：双击 `run.bat`（或在同目录执行）。

## 为什么是便携包而不是 MSI

Compose 的 `nativeDistributions` 在本机离线环境下会在 `downloadWix` 任务上失败（WiX 需要联网下载），
因此交付形态选择**免安装目录 + zip**：不依赖 WiX、不写注册表、删目录即卸载。
需要 MSI 时：装好 WiX 3.14 并允许联网，然后在 `apps/desktop/build.gradle.kts` 的
`nativeDistributions` 里打开 `TargetFormat.Msi`（当前该块被移除，避免离线构建失败）。

## 已知限制（交付时必须告知使用者）

- 仅 Windows x64 验证过；macOS/Linux 打包未做。
- 需要本机 JDK 21（未内置 jlink 运行时）。
- 插件无沙箱（同 JVM 加载执行），`trusted` 只是确认，不是安全边界。
- 传输仅回环/LAN 验证过，未做真实中继/NAT 穿透。
- 系统通知在无托盘环境会明确提示不可用（不是静默失败）。
