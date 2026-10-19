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
