# 人工测试指南（2026-10-19）

> 口径：本指南只覆盖**当前真的有后端实现**的路径。标注"诚实失败"的地方是设计行为，不是 bug。

## 0. 准备

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -File tools\check-disk.ps1          # 可用 < 6 GB 会非零退出，先清理
```

两个窗口：

| 窗口 | 命令 | 说明 |
|---|---|---|
| 节点 | `cargo run --offline -p chat-node` | 默认 http://127.0.0.1:8080；启动日志打印监听地址与配置摘要（不含密钥） |
| 桌面预览 | `.\gradlew.bat :apps:desktop:previewApp --offline --max-workers=1` | 左侧栏 5 个入口：真实身份 / 本机存储 / 附件 / 群 / 插件 |

可选：`cargo run --offline -p chat-node -- --require-capability`（附件块端点强制能力令牌）、`--no-audit`（关闭审计）。

## 1. 身份（真实 crates/identity）

1. 打开「真实身份」→ 首次运行自动建身份（口令由密钥槽封装）。
2. 记下 **账户 ID / 设备 ID / 指纹**。
3. 关闭应用 → 重开 → 再次打开面板。

**预期**：三项完全一致；若指纹变化说明密钥槽或库被换掉（真问题）。

## 2. 身份恢复文件（IDENTITY-013/014）

1. 面板「身份恢复」→ 输入口令（>=8 字节）→「导出恢复文件」→ 选路径保存。
   - **预期提示**包含"不含主密钥""旧库数据仍不可读"。
2. 关掉应用，换一个工作目录（或换库），重开 → 同一口令 →「导入恢复文件」→ 选刚导出的文件。
   - **预期**：账户/设备 ID 与指纹与第 1 步一致。
3. 在**已有身份**的库上再导入一次。
   - **预期**：明确报 `RECOVERY_TARGET_NOT_EMPTY`，不覆盖。

## 3. 发送链路（草稿 -> 节点 -> 回执）

1. 节点窗口确认监听正常。
2. 本机存储面板：填节点地址与节点 ID → 建草稿 → 发送。
3. 观察草稿状态与事件列表（拉取式）。

**预期**：

| 场景 | 期望 |
|---|---|
| 节点在线、参数正确 | 草稿转 accepted，事件里可看到提交 |
| 节点地址错/未启动 | 稳定错误码（如 `NODE_UNAVAILABLE`），**不会显示已送达** |
| 结果不明（unknown） | 只进入待对账，**不会自动重发** |

## 4. 附件（导入 -> 上传 -> 下载 -> 导出）

1. 附件面板：选文件 → 导入（加密落库）→ 上传（需节点 + 群）。
2. 本机存储面板「消息附件卡片」→「读取附件」查看卡片（名称/大小/状态/是否可下载）。
3. 卡片「从节点下载」→ 填目标路径 → 下载；再「导出」比对内容。

**预期**：进度显示真实块数；下载成功后提示写入字节数；未上传的附件显示"不可下载"且下载按钮禁用。

## 5. 群治理（注意：改名/禁言现在会准备真实提交）

1. 群面板：建群 → 记 epoch（应为 1）。
2. 改名 → **必须点"接受"** 才会 epoch+1；再点改名同名 → 幂等（版本不涨）。
3. 禁言某人 → 接受 → 解除 → 接受。
4. 移除成员 → 接受；「加载群列表」查看 `#epoch · role · 状态 · 禁言 N 人`。

**预期**：每步 pending 显示 base->target；未接受就继续操作会报 `INVALID_STATE_TRANSITION`（a control commit is already pending）。

## 6. 自移除提案（跨设备才完整）

非群主设备：prepare leave → 提交到节点 → 对端 process proposal → 对端 commit proposals → 本机合并该提交。
**预期**：合并后本机 `self_removed`，之后发送/解密显式失败。

## 7. 常驻同步

本机存储面板「启动同步」→ 观察 state / 已应用条数 / 轮次 / 错误码 / 下次重试 → 「停止同步」。
**预期**：`OUTBOX_NOT_CONFIGURED` 属正常（未注入发送端口时的显式状态，不是失败）。

## 8. 插件（P4，真加载真执行）

1. 准备合规 JAR：内含 `manifest.json`（**stored，不压缩**）、`entryClass` 对应 `.class`、`META-INF/services/chat.plugin.DesktopPlugin`（内容 = entryClass）。
2. 插件面板：选 JAR →「校验结构」→ 勾「确认信任」→「加载」→「执行动作」→「卸载」。

**预期**：

| 操作 | 期望 |
|---|---|
| 未勾信任就加载 | `PLUGIN_TRUST_REQUIRED` |
| 包不合规 | `PLUGIN_PACKAGE_*` 之一（缺清单/缺入口/SPI 不符/路径穿越…） |
| 加载后执行 | 返回插件输出（受 256 KiB 上限） |
| 卸载后执行 | 显式失败（不是静默成功） |

## 9. 系统通知

本机存储面板「测试通知」：
- 有托盘 → 期望弹窗；
- **无托盘（headless）→ 明确提示"系统通知不可用"并回退应用内提示**。

## 10. 节点 HTTP（curl）

```powershell
curl.exe -i http://127.0.0.1:8080/v1/health
# 期望 200，响应头带 x-chat-request-id
```

缺失鉴权头 → **400 `INVALID_REQUEST`**（本仓既有约定）；有头但签名/绑定不符 → **401**。

## 11. 已知不能人工测（别浪费时间）

- 打包安装的成品应用（当前只能从源码跑预览）
- 真实中继 / 跨网段 / NAT 穿透
- 插件沙箱隔离（同 JVM 无沙箱，trusted 只是确认）
- 后台收消息通知
- OS 凭据库（当前 DPAPI 文件封装 + 演示口令）

## 12. 出问题时的排查顺序

1. `powershell -File tools\check-disk.ps1`（磁盘满会导致构建/写文件异常）
2. 用 `--info` 重跑 Gradle，看第一个 `e: ` 报错
3. Rust 侧：`$env:CARGO_TARGET_DIR="C:\Users\35828\AppData\Local\Temp\chat-verify-target"; cargo test --offline -p <crate>`
4. 数据库被占用：同库同一时刻只能一个 FFI 对象（`STORAGE_LOCKED`）——先关其它面板/对象

## 13. 群邀请闭环（GROUPS-004，R-169/R-170 后可用）

前提：节点在线；双方各自已有真实身份。

1. **B 侧准备 KeyPackage**：B 在自己的预览实例里导出自己的 KeyPackage 字节（当前可用 `group_create_with_invitee_key_package` 的同源 API；若面板尚无导出按钮，用自动化路径 `crates/crypto` 的 `key_package()` 生成后把字节存成 `.kp` 文件）。
   - 注意：**KeyPackage 单次使用**，同一份只能用于建一次群。
2. **A 侧建群**：群面板「邀请投递」→「选 KeyPackage」挑那个 `.kp` → 填被邀请者凭据 →「用它建群」。
   - 预期：提示"已用对方 KeyPackage 建群：epoch 1，Welcome N 字节（可用于提交邀请）"。
3. **A 侧提交邀请**：确认节点地址/节点 ID 已填 →「提交邀请」。
   - 预期：显示真实回执 `seq` + `submissionId`；节点不可达 → `NODE_UNAVAILABLE`（不是假成功）。
4. **B 侧取回并接受**：群面板「取回邀请」→ 看到群名/epoch/被邀请者/Welcome 字节数 →（接受动作见 GROUPS-017）。
   - 预期：B 入群后 epoch 与成员与 A 一致。
5. **验证审计**：`GET /v1/audit?kind=invite` 能看到这次邀请（含 group_id/request_id）。

**已知限制**：面板暂无"导出自己 KeyPackage"按钮（需用底层 API 生成 `.kp`）；邀请一次只带**一个**被邀请者。


## 14. 收一轮（syncOnce）与导出 KeyPackage（R-188/R-190）

**收一轮**（本机存储面板 → 同步区 →「收一轮」）：
1. 先填好节点地址/节点 ID（并确保群已「登记到节点」）；
2. 点「收一轮」→ 结果显示 `拉取 N 条 · 落库 N 条 · 跳过 N 条 · 游标 N · 已追平/未追平`；
3. 若同步服务未创建 → 明确提示 `CORE_CLOSED`（不是静默无反应）；
4. 收完会自动重载当前会话消息（需要先选中会话）。

**导出 KeyPackage**（真实身份面板 →「导出 .kp」）：
1. 点按钮 → 选保存路径 → 得到 `.kp` 文件；
2. **每次导出都是新的**（单次使用）；只含公钥材料；
3. 把这个文件给对方 → 对方在群面板「选 KeyPackage」→「用它建群」→「提交邀请」→ 你再「取回邀请」→ 接受入群。

**已知限制**：被邀请方接受邀请后，双方收发还需要各自「收一轮」或常驻同步；当前界面展示的是计数与游标，消息列表在收一轮后自动刷新。

## 15. 双设备互通（A ↔ B，自动化脚本）

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -ExecutionPolicy Bypass -File tools\demo-two-devices.ps1
# 只看结果不看构建：加 -SkipBuild
```

脚本会做（全部真实：真节点 + 两份独立身份 + 两个独立 ChatCore）：

1. 起本地节点（127.0.0.1:8099，随机种子、独立数据目录）
2. 建两份身份库（alice / bob）
3. bob 导出 KeyPackage（.kp）并在节点自注册设备
4. alice 用 bob 的 .kp 建群 → 登记群到节点 → **把 bob 加为节点成员**
5. alice 提交邀请（Welcome 经节点投递）→ 创建草稿 → 发送 → 期望 accepted
6. bob 凭 Welcome 入群 → 收一轮 → 期望 applied >= 1

预期输出（关键行）：

```
PASS alice added bob as node member: seq=1
PASS alice submitted invite: seq=2
PASS alice message accepted by node
PASS bob joined: grp:1:... epoch=1
PASS bob applied 1 entries from the node
two-device demo: PASS
```

脚本产物：`build/two-device-logs/<时间戳>/`（node.out / alice.log / bob.log）；工作目录 `build/two-device/<时间戳>/`（两份身份库、两个消息库、bob.kp、a2b.txt 握手文件）。

**手工等价路径**（不经脚本）：

1. 两台机器各自启动预览应用，各自建身份；
2. B：真实身份面板「导出 .kp」→ 把文件发给 A；
3. A：群面板「选 KeyPackage」→「用它建群」→ 填节点地址 → 「登记到节点」；
4. A：群面板填「对方设备 ID（dev:1:...）」→「加为节点成员」（返回 seq 即成功；设备须已在节点注册）；
5. A：草稿 → 发送；B：本机存储面板「收一轮」→ 消息出现。

**已知限制**：对方设备必须**先在节点注册**（否则节点会拒绝，界面会显示稳定错误码）；KeyPackage 单次使用；两边都要有同一节点的地址与节点 ID。


## 16. 选区格式化与界面缩放（R-207/R-208）

**选区格式化**（本机存储面板 → 草稿正文）：
1. 选中一段文字 → 点工具条「加粗/斜体/删除线/代码」；
2. 或按 `Ctrl+B` / `Ctrl+I`（桌面）；
3. **再点一次同一按钮 = 取消**（toggle）；不选文字直接点 → 插入成对标记并把光标放中间；
4. 跨行选区会**逐行**包裹；首尾空格留在标记外面。

**界面缩放**（「关于」面板 → 界面缩放）：
1. 选 小 0.9 / 标准 1.0 / 大 1.15 / 特大 1.3；
2. 立即生效，并提示「已保存到本机」；
3. 偏好文件：`%APPDATA%\Chat\ui-preferences.json`（非机密，只含缩放与入口显隐）；
4. 关掉重开仍是上次的缩放。

## 17. 密钥槽（主密钥保护）与迁移（R-212 / R-213 / R-214）

打开「关于」面板 -> 「密钥槽（主密钥保护）」：

| 行 | 含义 |
| --- | --- |
| 当前后端 | Windows 凭据管理器 / DPAPI 文件 / 未探测 |
| 探测依据 | 为什么选了这个后端（例：磁盘上存在 DPAPI blob（老版本写入）） |
| 保护说明 | 换用户/换机器是否可解包 |

**迁移到凭据管理器**：
1. 点「迁移到凭据管理器」-> 出现红字确认（旧文件会删除、换用户/换机器不可读）；
2. 点「确认迁移」-> 成功提示「已迁移： ... （重启后生效）」，失败显示稳定错误码；
3. 重启应用后，「当前后端」应为 **Windows 凭据管理器**。

**怎么算迁移成功**：迁移后再启动，本地库能正常解密（消息/身份都在）即成功；若旧文件已删而新密钥读不到，会显式报 `KEY_INVALIDATED` / `UNAVAILABLE`，**不会静默丢数据**。
