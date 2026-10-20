# 120 轮回顾（总账）

生成时间：2026-10-21（第 118 轮）。这份文件回答三件事：**做成了什么、我犯过什么错、还差什么**。

## 一、批次与节奏

| 批次 | 轮次（可核对） | 主题 | 结果 |
| --- | --- | --- | --- |
| 第一/二批 | 见 `DEEPSEEK_WORK_PLAN.md` R-140…R-172 | 内核/协议/存储/节点/FFI 打通 | Rust 测试从 ~1006 起步 |
| 第三批 | R-172…R-205 | 交付物（app-image）+ 双设备互通 | `verify-all` 7 阶段；双设备 PASS |
| 第四批 | R-206…R-220 | 凭据库、LAN、TLS、MLS 设备持久化、错时投递 | 8 → 10 阶段 |
| 第五批 | R-222…R-234 | 证书轮换、信任贯通 FFI、错误诊断、插件签名 | 11 阶段；`tls-e2e` 引入 |
| 第六批 | R-235…R-241 | 证书生命周期（检测/端点/展示/自动读）、治理收尾 | 本轮收尾 |

## 二、做成了什么（按用户能感知的能力）

1. **能装、能跑**：`Chat.exe`（jlink + jpackage 自包含，免 JDK），包内冒烟 + GUI 存活。
2. **能建身份、能加密存**：master key 经 DPAPI / Windows 凭据管理器封装，库文件加密。
3. **能建群、能治理**：改名/禁言/移除/接受，epoch 语义严格（不点接受再改会显式失败）。
4. **能发、能收**：真实节点 `sendDispatch accepted` → `syncOnce applied≥1`。
5. **能两台设备互通**：A/B 建群→登记→加成员→邀请→发送→入群→拉取（脚本 PASS）。
6. **能错时投递**：接收方离线时发送，上线后仍收到（四进程顺序验证）。
7. **能走非回环**：LAN 地址上 `accepted + applied`（不再依赖 127.0.0.1）。
8. **能加密传输**：节点 `--tls-cert/--tls-key` 真监听；客户端**严格 pinning**（只认 SHA-256 指纹，系统根不参与）；打包产物端到端含**错误指纹负例**。
9. **能轮换证书**：指纹用**集合**，新旧并存；节点启动算有效期，`/v1/tls/info` 公开，客户端启动自动显示剩余天数。
10. **插件可控**：权限门 + 签名闸门（默认要求签名、篡改/未签/指纹不符全拒）+ 信任列表（能看能移除）。
11. **错误能看懂**：`Cow` 详情 + 整条错误链 + 分层展示（标题/说明/原因/建议/可重试性/稳定码），7 处共用 `ErrorBanner`。
12. **一条命令验收**：`verify-all` 11 阶段（disk/rust-check/verify-project/gradle/android/package/two-device/lan/tls/offline/tls-e2e）。

## 三、修掉的真实缺陷（挑最值钱的 12 条）

| # | 缺陷 | 教训 |
| --- | --- | --- |
| 1 | 邀请取回用**定长解码**解变长 blob → 恒返回 None | 只有端到端测试能发现这种“永远拿不到” |
| 2 | `identity` 篡改测试固定写 `0x5a`，原字节恰为 `0x5a` 时没真改 → flaky | 测试要保证**真的变了**（用 `flip_body_byte`） |
| 3 | Rust `close` 与 UniFFI `Disposable.close()` **conflicting overloads** | 跨语言命名冲突要在接口层解决（改 `shutdown`） |
| 4 | KeyPackage 凭据口径是**设备 ID** 而非账户 ID → `INVALID_CANDIDATE` | 口径要变成代码里的**事实**（`keyPackageCredential()`） |
| 5 | 节点成员行把 `account_id` 写成作者账户 → 跨账户成员被拒 | 安全边界（设备已注册）与记录字段（设备自己的账户）是两件事 |
| 6 | 打包节点二进制**过期** → 邀请 `INVALID_CANDIDATE` | 构建产物要能追溯（同坑后来在 dll/绑定上又犯两次） |
| 7 | `-Only` 只看整组名 → 单冒烟验证**整组跳过却报 PASS** | 假绿比红灯更危险 |
| 8 | JNA `@FieldOrder` 漏 `LastWrittenLow/High` → **运行时**才炸 | JNA 结构体字段列表必须与字段声明一一对应 |
| 9 | 篡改落在 deflate 流 → `ZipException` 漏出（而非签名错） | 完整性失败要收敛到**同一个稳定码** |
| 10 | `prepareRustHost` UP-TO-DATE → uniffi 绑定没重生成 | 改了 Rust FFI 必须 `--rerun-tasks` 并**验证绑定里有新符号** |
| 11 | PowerShell 调 `java.exe` 时**空字符串参数被丢弃** | 脚本传参用占位符（`-`），不要依赖空串 |
| 12 | `STORAGE_LOCKED` 其实**在**冻结可重试表里（我拿它当反例） | 断言前先查表，不要凭记忆 |

## 四、我自己的失误（不掩饰）

1. **把 Rust 源码写成带引号的字符串数组**（批量替换事故）→ 只能整文件重写；
2. **CRLF 脚本用 LF 替换** → 一个验证步骤静默丢失，靠“日志文件缺失”才发现；
3. **同名锚点改错位置**（`aboutState = aboutState` / `pluginState` / `PreviewPluginState.Empty`）→ 参数加到了调用点而不是签名（发生过 3 次）；
4. **跨语言转义**把 Kotlin 正则写坏（两次），一次是 JS 模板、一次是全角括号；
5. **对 `NodeClientError::config` 参数类型想当然**（`&'static str` vs `String`）→ 编译失败；
6. **拿“另一张证书”做负例时用手写 base64 假证书** → 换成 openssl 真生成；
7. **反复踩“旧二进制/旧绑定”**：`target/debug` 与临时 `CARGO_TARGET_DIR` 不一致，共 3 次。

共同点：**都是“看起来很对但没验证”**。唯一有效的对策就是本轮一直坚持的：**每个结论都配一条可复跑的证据**。

## 五、验证史（数字只写核对过的）

- 工作区 Rust 测试：起步约 **1006** → **约 1160**；其中已单独核对：`chat-ffi` 124+、`chat-node-client` 8 套件（含 7 例 TLS）、`cert_validity` 3 例。
- 迁移：storage **0001–0014**、node-services **0001–0009**。
- 桌面冒烟：**13** 个 Gradle 任务；Android `assembleDebug` 通过。
- 一键回归：**11 阶段**；另有 `verify-package`、`verify-package-e2e`、`verify-lan`、`verify-tls`、`verify-tls-e2e`、`verify-offline-delivery`、`demo-two-devices`。

## 六、外部限制（不是能力问题）

1. **没有可公网/VPN 可达的节点** → 跨机器/NAT 无法证明（LAN 地址仍是同一台机器）；
2. **离线构建**：MSI（WiX）需要联网下载，故只有 app-image + zip；
3. **Windows-only**：凭据管理器/DPAPI/TLS 验证都在 Windows；macOS/Linux 走口令派生但未验证；
4. **时间**：120 轮上限，剩余议题只能留作下一批。

## 七、如果重来，我会更早做的三件事

1. **先建“一条命令验收”**（`verify-all`）——它在第 64 轮才成形，之前每轮收尾都靠手工拼命令；
2. **先定“构建产物一致性”纪律**（固定 target 目录 + 绑定符号自检）——省下三次旧二进制排查；
3. **先做端到端（双设备）再做界面**——双设备打通后，界面 wiring 的方向才不会再返工。

## 八、剩余 backlog（按价值）

1. **跨机器联调**（唯一外部依赖，A 口径最后一块）；
2. **证书生命周期自动化**：定时刷新、到期弹窗、自动续签、信任列表导入导出/吊销；
3. **`AppFacade` 深层收敛**（把面板各自调 service 收掉；当前只做了失败投影收敛）；
4. **生产化**：安装器/自动更新、后台通知、soak、指标与运维巡检；
5. **多平台**：macOS/Linux 的凭据库与路径（现在只有 Windows 实现）。
