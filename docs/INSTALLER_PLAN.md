# 安装器方案（MSI）与当前替代方案

## 一、现状：没有 MSI，只有便携 zip

| 交付形态 | 路径 | 能否安装 |
| --- | --- | --- |
| 自包含 app-image | `build/dist/app-image/Chat/Chat.exe` | 免安装，双击即用（内置 jlink 运行时，不需要 JDK） |
| 便携 zip | `build/dist/Chat-app-0.1.0.zip`（81.79 MB） | 解压即用 |
| MSI 安装包 | **无** | 需要联网拉 WiX 工具链 |

为什么没有：本项目全程**离线构建**（`--offline`），而 jpackage 生成 MSI 需要 WiX Toolset；离线环境拿不到，所以没有硬做。**这不是“忘了”，是当前环境的硬限制。**

已实证 zip 是完整可运行的：`tools/verify-zip.ps1` 会把 zip 解压到临时目录并**从解压副本**跑一次打包冒烟：

```
size                        81.79 MB
sha256                      996E799E7D12F3C1623FC469261A4D6B02C95A8961FC9AFC99C5DB05B7D923C7
app/app.jar                 present (38567.6 KB)
app/native/chat_ffi.dll     present (43253.5 KB)
runtime/bin/java.exe        present (49.1 KB)
exe matches app-image       yes (644E16725B6F...)
packaged smoke (from zip)   PASS
exe file version            0.1.0
ZIP CHECK: PASS (delivered zip is complete and runs)
```

## 二、如果要出 MSI（需要一台能联网的机器）

### 步骤

1. **装 WiX Toolset 3.14**（jpackage 目前只认 3.x）：下载 `wix314.exe` 安装，确保 `candle.exe` / `light.exe` 在 PATH；
2. 在能联网的机器上，用**同一份源码**构建：
   ```bash
   . .\tools\activate.ps1
   .\gradlew.bat :apps:desktop:jpackageAppImage --offline   # 先确认 app-image 能出
   ```
3. 直接把 app-image 打成 MSI（jpackage 支持从已有 app-image 生成安装包，避免重复构建）：
   ```powershell
   $javaHome = $env:JAVA_HOME
   & "$javaHome\bin\jpackage.exe" --type msi `
     --name Chat --app-version 0.1.0 `
     --app-image build\dist\app-image\Chat `
     --win-shortcut --win-menu --win-dir-chooser `
     --dest build\dist\msi
   ```
4. 产物：`build/dist/msi/Chat-0.1.0.msi`。

### 必须验证的 5 件事（缺一不可）

| # | 验证 | 判据 |
| --- | --- | --- |
| 1 | 安装 | 双击 MSI → 完成 → 开始菜单出现 Chat |
| 2 | 首次启动 | 能启动，且**不再弹首启引导**（若之前配过） |
| 3 | 数据落点 | 数据写到用户目录（不是安装目录，安装目录通常无写权限） |
| 4 | 卸载 | 卸载后程序目录清空，**用户数据是否保留要明确说明** |
| 5 | 升级 | 覆盖安装同版本/新版本，配置与数据不丢 |

### 已知坑

- jpackage 的 `--win-dir-chooser` 在静默安装（`msiexec /qn`）时会被忽略；
- WiX 3.x 与 4.x 不兼容（4.x 改了 CLI），jpackage 只支持 3.x；
- 签名：未签名 MSI 会触发 SmartScreen 警告，正式分发需要代码签名证书（本项目没有）。

## 三、不装 MSI 也能用的方式（推荐给当前场景）

1. 把 `Chat-app-0.1.0.zip` 拷到目标机器；
2. 解压到任意目录（例如 `D:\Apps\Chat`）；
3. 双击 `Chat.exe`；
4. 想要开机自启：给 `Chat.exe` 建快捷方式放进 `shell:startup`。

这条路径没有安装器依赖、不需要管理员权限，也没有注册表残留——对**内部自用**已经够。

## 四、什么时候必须做 MSI

- 要给**不熟悉电脑的人**用（他们不会解压/建快捷方式）；
- 要**企业分发**（组策略推送、集中卸载）；
- 要**自动更新**（需要安装器接管文件替换）。

以上三条现在都不成立，所以优先级排在「长稳测试 / 多平台」之后。
