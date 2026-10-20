# 0.1.0 验收清单（10 分钟）

验收分两部分：**自动验收**（命令，我这边已实跑）与**手工验收**（你在界面上点，按预期核对）。

## 〇、本次实跑结果（2026-10-20，commit `19ff699`）

```
disk             PASS      0s
rust-check       PASS   2.9s
verify-project   PASS   0.9s
gradle           PASS  33.9s
android          PASS   5.6s
package          PASS  16.4s   build/verify-package-20261020-220253.md
two-device       PASS   5.9s   A/B exchange (send accepted + pull applied)
overall: PASS                     ← EXIT=0
report: build/verify-all-20261020-220205.md
```

- 单机发收（package 阶段内部）：`sendDispatch accepted` + `syncOnce fetched=2 applied=1`
- 双设备（two-device 阶段）：A 建群→登记→加成员→提交邀请→发送 accepted，B 入群→拉取 applied=1

## 一、自动验收（3 分钟，一条命令）

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -ExecutionPolicy Bypass -File tools\verify-all.ps1
```

**期望**：末尾输出 7 行阶段表，`overall: PASS`，退出码 0。

| 阶段 | 含义 | 不通过说明什么 |
| --- | --- | --- |
| disk | 磁盘空间 | 空间不足（清理 `target/` 或 `build/`） |
| rust-check | `cargo check --locked` | 代码不可编译或依赖被改 |
| verify-project | 文档锚点/结构校验 | 文档链接断了 |
| gradle | 11 个桌面冒烟 | 某条真实链路坏了（会点名） |
| android | `assembleDebug` | Android 侧编译断 |
| package | 交付物结构 + 包内 4 冒烟 + GUI 存活 | 打包产物坏了 |
| two-device | A/B 互通 | 群/成员/发送/拉取链路退化 |

## 二、手工验收（7 分钟，按顺序）

### 0. 启动

```powershell
powershell -ExecutionPolicy Bypass -File tools\demo.ps1     # 起节点（占住窗口）
```

另开一个窗口：

```powershell
. .\tools\activate.ps1
.\gradlew.bat :apps:desktop:previewApp --offline --max-workers=1
```

或者直接双击**免 JDK** 的交付物：`build\dist\app-image\Chat\Chat.exe`。

### 1. 首启引导（30 秒）

- 期望：弹出三步引导「数据在哪 → 连接节点（可跳过）→ 可以开始了」。
- 填 `http://127.0.0.1:8080` 与节点 ID（任意，演示用），点「保存并继续」→「完成」。
- **核对**：关掉应用重开，**不再弹引导**（配置已持久化到 `%APPDATA%\Chat\node-config.json`）。

### 2. 身份（30 秒）

- 左侧「真实身份」：应有账户 ID / 设备 ID / 指纹（16 组 4 位大写 hex）。
- 点「导出 .kp」→ 选保存路径 → 提示「已导出 KeyPackage（N 字节）」。
- **核对**：再点一次，字节数可能不同（**单次使用**，每次都是新包）。

### 3. 本机存储 + 消息（2 分钟）

- 「本机存储」→ 建草稿 → 填节点地址/节点 ID → 「发送」。
- **期望**：草稿状态从 `queued` → `prepared` → `accepted`（节点在线时）；节点没起时是 `NODE_UNAVAILABLE`（**不是假成功**）。
- 点「收一轮」→ 显示 `拉取 N 条 · 落库 N 条 · 跳过 N 条 · 游标 N · 已追平`。
- **核对**：收一轮后消息区自动刷新（提示「收一轮：拉取 N / 落库 N；当前会话共 N 条消息」）。

### 4. 群 + 节点登记 + 成员（2 分钟）

- 「群」→ 建群 → 记下 epoch（应为 1）。
- 改名 → **必须点「接受」** 才 epoch+1；不点就再改 → 报 `INVALID_STATE_TRANSITION`（符合预期）。
- 「登记到节点」→ 期望显示 `节点登记：seq 0 · grp:1:...`。
- 填「对方设备 ID（dev:1:...）」→「加为节点成员」→ 期望显示 `成员回执：seq 1`。

### 5. 收附件 / 插件 / 通知（1.5 分钟）

- 附件：导入文件 → 上传（需群）→ 卡片「从节点下载」。
- 插件：选 JAR → 校验 → **不勾信任直接加载 → 必须被拒** `PLUGIN_TRUST_REQUIRED` → 勾信任 → 加载 → 执行 → 卸载 → 再执行**必须失败**。
- 通知：点「测试通知」；无托盘环境应显示**「系统通知不可用」并回退应用内提示**。

### 6. 关于 / 诊断（30 秒）

- 左侧「关于」→ 期望看到 版本 / 构建类型 / FFI 契约版本 / 平台 / host 库路径 / 数据目录。
- 若 host 库路径显示「解析失败」，那是**真实错误**（会把尝试过的路径列出来）。

## 三、验收结论表

| # | 项目 | 判定 |
| --- | --- | --- |
| 1 | 免 JDK 启动（`Chat.exe`） | 通过 / 受限（需 Win x64） |
| 2 | 首启引导 + 配置持久化 | 通过 |
| 3 | 身份创建 + KeyPackage 导出 | 通过（单次使用） |
| 4 | 草稿 → 发送 → 节点回执 | 通过（需节点） |
| 5 | 收一轮 → 消息入库 | 通过 |
| 6 | 群治理（改名/禁言/移除/接受） | 通过（必须点接受） |
| 7 | 登记到节点 + 加节点成员 | 通过 |
| 8 | 附件导入/上传/下载 | 通过（需群 + 节点） |
| 9 | 插件加载/执行/卸载 + 权限门 | 通过（**非安全边界**） |
| 10 | 双设备互通 | 通过（**同机双进程**；跨机器未做） |
| 11 | 关于/诊断 | 通过 |
| 12 | 跨机器 / 中继 / NAT | **不适用（未实现）** |
| 13 | 插件隔离安全性 | **不适用（只有门槛）** |
| 14 | OS 凭据保护 | **不适用（DPAPI 文件封装 + 演示口令）** |

## 四、验收不过时怎么报

提供四样东西即可定位：**版本号**（关于面板）、**平台**、**host 库路径**（关于面板）、**错误码 + 提示文案**（界面会显示稳定码）。

对应日志：`build/verify-logs/<时间戳>/`（自动验收）、`build/two-device-logs/<时间戳>/`（双设备）、`build/e2e-logs/<时间戳>/`（单机发收）。
