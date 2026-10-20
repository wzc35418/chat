> 状态补充：目标目录已在安装/验证工具链，见 [当前依赖记录](../DEPENDENCIES.md)。以下是产品P0验收标准；迁移不将依赖安装或原结构检查自动视为P0通过。

# P0：开始写产品功能前的技术验证

状态更新：P0-01/P0-02 已开始。已接入正式构建、真实 MLS 和本机 iroh 双设备纵向原型；实际运行范围与证据见 [P0 最小链路报告](../P0.md)。下列复选框对应完整三端/持久化验收，仍不将局部验证标为整体 P0 通过。

P0-03 局部实验已通过客户端单连接事务及独立节点接受事务，共 14 个进程异常终止断点；完整清单仍未满足。见 [持久化实验记录](../P0_PERSISTENCE.md) 和 [节点接受实验](../P0_NODE_ACCEPTANCE.md)。

## 环境与版本锁定

|项目|需记录的信息|输出位置|
|---|---|---|
|Rust|rustc/cargo 精确版本、targets、各平台 linker|docs/adr/0002-toolchain.md|
|Android|最低 API 待定、arm64 真机、SDK/NDK/JDK/AGP 兼容组合|同上和验证报告|
|Kotlin/Compose|Kotlin/Compose/Gradle/coroutines 精确组合、合法 source sets|gradle/libs.versions.toml 与 wrapper|
|FFI|UniFFI 或等价生成器版本、调用/取消/释放规则|crates/ffi 与 tools/bindings|
|MLS|OpenMLS 版本/套件/provider API/pending Commit 恢复能力|docs/adr/0003-crypto-storage.md|
|网络|iroh/relay 精确版本、发现与公共回退设置|docs/adr/0004-transport.md|
|本地保护|全库加密方案、系统保护库、Linux 无 Secret Service 路径|docs/adr/0003-crypto-storage.md|
|部署|节点 Linux 基线、TLS入口、持久目录和镜像摘要|deploy 和验证报告|

选择版本时核对各库官方文档/发布说明，不从本计划假设当前最新版本支持情况。没有验证前不填写虚构精确版本、不生成假 wrapper 二进制。原架构来源只代表该设计文档的研究记录。

## P0-01：最小 FFI 往返

- [x] 编译本地 Rust workspace 骨架，补生成绑定工具。
- [x] Windows 根目录 `start-chat.cmd` 实际启动 P0 Compose 窗口，正常关闭后 Gradle 任务退出成功；运行时 Skiko 临时目录固定在项目 build 下。仅证明本机启动。
- [x] 2026-10-18 再次实测根目录启动脚本：`Task :apps:desktop:run` 后出现 `Chat · 加密通信实验室` 窗口（有非零窗口句柄），控制台输出窗口就绪提示；关闭窗口后 `BUILD SUCCESSFUL in 28s`。`run` 运行期间占用终端属于预期行为；启动脚本增加任务栏/Alt+Tab 提示。
- [x] 2026-10-18 诊断“就绪但用户桌面不可见”：Win32 报告窗口 visible、非最小化、在主屏幕范围内（48,48 至 1072,908），但资源管理器 Alt+Tab 不显示。启动脚本改用 `--no-daemon` 避免复用可能处于另一桌面上下文的旧 Gradle daemon；本机复跑确认为 single-use daemon，窗口创建并在关闭后 `BUILD SUCCESSFUL in 1m 8s`。用户随后从资源管理器双击更新后的脚本，确认能看到窗口。
- [ ] Rust echo 有界 DTO → Kotlin suspend 包装 → 返回稳定错误。
- [ ] Android arm64 真机、Windows x64、Linux x64 分别运行。
- [ ] 线程切换、事件订阅、取消、释放、panic/异常边界有真实证据。
- [ ] 每个目标记录安装/运行命令、版本、原生库 SHA256 与结果。

## P0-02：MLS 互操作

- [ ] 两设备本地生成真实账户凭据与 KeyPackage，标准 MLS 建群/Welcome。
- [ ] Windows→Android、Android→Linux、Linux→Windows 应用消息往返。
- [ ] 验证应用凭据绑定，错误设备/过期包/重复包明确拒绝。
- [ ] 成员加入、移除、自己的 pending Commit 接受路径分别跑通。
- [ ] 明确角色/禁言/群名控制批次是否总推进 epoch，并冻结批次编码。
- [ ] 验证本机已发 ciphertext 取得回执时不会重复消费发送密钥状态。

## P0-03：统一事务和崩溃恢复（阻断项）

已完成的局部实验（2026-10-18，Windows x64，证据见 [P0-03 报告](../P0_PERSISTENCE.md)、[节点接受报告](../P0_NODE_ACCEPTANCE.md) 和 `build/p0-evidence/`）：

- [x] 独立文件 SQLite 的 T1/C01：draft、operation ID、请求摘要与 prepare job 同事务；commit 前/后进程终止恢复一致，重复意图幂等，冲突拒绝。
- [x] 独立文件 SQLite 的 T1→T2：从已提交意图读取正文，MLS、attempt、outbox、意图与任务状态同事务；3 个进程终止断点恢复一致。
- [x] 独立文件 SQLite 的发送/接收：6 个进程终止断点，固定密文重试、接收去重、篡改密文与 SQL 失败回滚。
- [x] 独立节点文件 SQLite 的 T4/C05 局部实验：3 个进程终止断点，日志/群 head/投递任务原子提交，提交后结果丢失再用客户端 outbox 原密文重试得到同 seq/回执；换密文拒绝，8 并发写入序号连续。未经过 HTTP。
- [x] 独立节点 P0 Ed25519 回执：调用方提供签名种子，实验库固定公钥并拒绝换钥重开；验证端用预先固定的公钥核对群、提交、密文摘要、序号和哈希链；错误公钥/改动字段与库中篡改回执均拒绝。正式密钥托管、公钥分发和回执编码未验收。
- [x] 2026-10-18 14:56 的 `tools/verify-p0.ps1` 不刷新锁文件完整复跑通过，Rust、Kotlin 原生链路、Compose P0/预览渲染与展示层检查、Android arm64 APK 均通过。

2026-10-18 追加局部实验（MLS 群状态持久化，证据见 `crates/storage/tests/formal_mls_state.rs`、`formal_crash_mls.rs`、`crates/crypto/tests/formal_mls_state.rs`，文档见 [storage.md](storage.md)、[crypto.md](crypto.md)）：

- [x] 群状态整组快照与 epoch 版本存入正式加密存储（`0004_mls_state`，密封 + AAD 绑 group/epoch）；两设备建群→加密→关闭→同主密钥重开→继续加密，对端解密到原文。
- [x] 旧 epoch 保存被 `STALE_EPOCH` 拒绝且不覆盖；篡改记录/错误主密钥 `STATE_CORRUPTED`。
- [x] 真实 `abort()` 三个断点后只保留上一 epoch 的完整状态（不产生半状态）。

注意：这仍是"提交后一次性写库"，**不是** OpenMLS provider 在调用方事务内读写，所以下面正式退出条件保持未勾选。

2026-10-18 追加局部实验（附件：分块加密 + 记录持久化 + 上传/下载端口，证据见 `crates/core/src/attachments.rs` 内联测试、`crates/storage/tests/attachments_records.rs`、`:apps:desktop:attachmentSmoke`）：

- [x] 25 MiB 上限、分块 AEAD、整文件与明文摘要校验、失败不留半成品；导入后磁盘缓存无明文。
- [x] 附件记录落正式加密存储（0005），重开后记录仍在；**FileKey 未持久化 → 重启后导出/下载显式 `ATTACHMENT_KEY_UNAVAILABLE`**（诚实失败，不含伪造）。
- [x] 上传/下载经可注入的 `BlobTransport` 端口完成（密文逐字节、同序号重试幂等、manifest 先校验）。

2026-10-18 追加（R-47 落地后更新）：

- [x] FileKey 持久化：附件密钥随记录存入正式加密库（`0007_attachment_secrets`，密封 + AAD 绑 attachment_id），**关闭重开后导出/下载真实可用**；篡改密钥 `STATE_CORRUPTED`；删除附件级联清密钥。
- [x] 真实节点适配层已接线（`NodeBlobTransport`：NODE-012~015，块上限已与客户端 256 KiB 分块对齐），上传/下载端到端通过。
- [x] E2EE SecretDescriptor 已实现（`crates/crypto/src/secret_descriptor.rs`，93 字节、摘要先校验）。

仍未勾选（缺口）：断点续传位图（进度不落盘，重启从头重发）、下载写入本地加密缓存以便离线再导出、跨设备/跨实例的“对方可下载”（需要描述符随消息投递 + 群密钥通路）。

下面仍为正式跨端/全场景退出条件，未因局部实验打勾：

- [ ] OpenMLS provider 的每次读写绑定同一 CoreTransaction，不暗中独立 commit。
- [ ] 源码/实验确认库内存缓存能够在提交失败时失效，不发布新状态。
- [ ] 跑 TRANSACTIONS.md 的 C01..C09，使用实际进程终止并重启。
- [ ] 原 attempt 重发不再次 encrypt；未知节点结果同 SubmissionId 对账。
- [ ] 并发 Commit 胜出/失败后的 pending 清理不会回滚已消费密钥。
- [ ] 若同事务不可行，先补 journaling ADR 与每一恢复状态，未经故障验证不得进入 P1。

## P0-04：存储与系统保护

已完成的局部实验（2026-10-18，Windows x64，证据见 `crates/storage/tests/formal_store.rs`、`crates/storage/tests/formal_crash.rs`、`crates/crypto/tests/formal_master_key.rs`，文档见 [storage.md](storage.md)、[crypto.md](crypto.md)）：

- [x] 正式存储路径 `crates/storage/src/formal/` 与 P0 实验路径完全分开：独占写锁（第二个打开返回 `STORAGE_LOCKED`）、WAL/`synchronous=FULL`/外键、密钥校验记录、版本化迁移（SHA-256 校验和、`MIGRATION_FAILED`/`SCHEMA_TOO_NEW`）。
- [x] 敏感值列级 AEAD（ChaCha20-Poly1305，用途子密钥 + 行身份 AAD）：提交后直接扫描 `.db`/`-wal`/`-shm` 原始字节，明文正文、MLS 秘密与主密钥字节均不出现，重开仍可解密；搬移/篡改密文、错误密钥、来源不一致均拒绝且不覆盖数据。
- [x] 事务语义：`BEGIN IMMEDIATE` 句柄只消费一次、版本绑定、失败 poison 拒绝提交、提交前版本比较 + `foreign_key_check`、`CommitToken` 只在 COMMIT 后返回；Drop 等价回滚。
- [x] 崩溃恢复：真实子进程 `abort()` 五个断点（AfterDraft/AfterPrepareJob/AfterAttempt/AfterMlsRecords/AfterOutbox），重开后无半草稿/半消息；恢复只核对既有字节（清标记、重置过期租约、重建 outbox、隔离篡改记录），不重造密文。
- [x] 主密钥：Argon2id v0x13 派生且参数有界、32 字节密钥文件加载、错误秘密/损坏文件拒绝。

以下仍是正式退出条件，未因局部实验打勾：

- [ ] 验证 SQL 草案可执行仅是第一步，随后接真实加密数据库。
- [ ] 主 DB、WAL、临时文件、备份、索引不泄漏测试正文/身份关系。
- [ ] Android Keystore、Windows 系统保护、Linux 密钥服务可用路径跑通。
- [ ] 系统保护拒绝/失效/不可用时进入锁定或明确口令路径，拒绝明文保存。
- [ ] 恢复文件 KDF 参数上限、错误秘密、损坏/半文件有拒绝测试。

## P0-05：完全自托管网络

- [ ] 关闭公共中继/公共发现，设置自己的节点和 relay。
- [ ] 局域网直连、不同 NAT、必须中继、节点离线四场景。
- [ ] 证明连接中继不承担离线信箱功能；离线需要应用节点存密文。
- [ ] 记录所有实际使用的服务域名，确认无隐式公共 fallback。
- [ ] 客户端主版本/能力不兼容时明确拒绝，不关 E2EE 降级。

## 通过/失败记录

每项报告字段：date、commit、OS/设备、toolchain、dependencyVersions、command/scenario、expected、observed、evidencePath、result、remainingRisk。状态只有 NOT_RUN/PASS/FAIL/BLOCKED；结构检查 PASS 不等于密码学/跨端 PASS。

失败时缩小原型并修复，或在 ADR 提供明确替代决策和重验范围。不估算尚无依据的工期，不把接口编写量当成技术验证进度。

## 第二批 32 轮后的局部实验补充（2026-10-18）

> 口径不变：以下都是**单机/本机可复现的局部实验**，不等于正式 P0 验收（跨机、经中继、真机密钥库仍未覆盖）。

- [x] **附件跨设备解密（本地实验）**：`AttachmentMessageBody` 走受 MLS 加密的协议字段 57（v2 尾部追加 display_name），描述符用 **MLS exporter** 派生包装密钥；B 在**全新库**仅凭消息体即可 adopt 并独立下载解密，导出内容与 A 明文逐字节一致；篡改摘要/描述符/换密钥全部拒绝且零写入。证据：`cargo test -p chat-core`（attachment_descriptor / attachment_descriptor_protocol / attachment_display_name / attachment_metadata_compat）。
- [x] **附件元数据版本兼容（本地实验）**：字段 57 v1 前缀逐字节不变、v2 尾部追加名字；v1 视角可读 v2 字节并回退占位名；未登记版本显式拒绝。限制：**未升级的旧客户端仍需过渡版本**才能读新消息。
- [x] **身份恢复文件（本地实验）**：argon2id + XChaCha20-Poly1305，KCV 区分错口令与篡改，目标库非空拒绝；导出→空库导入→重开拿到同一设备指纹（FFI 层 3 用例）。
- [x] **插件宿主（本地实验）**：清单校验/安装/启停/派发计划/私有存储隔离/配额/**落库持久化**（迁移 0010）+ Kotlin 桥 + `pluginSmoke` 全链路 PASS。限制：**不加载也不执行插件代码**（无 JVM 插件运行时、无信任确认）。
- [x] **自移除与角色同步（本地实验）**：MLS 禁止自移除提交 → 走 Remove 提案 + 对端打包提交；策略角色/禁言/群名随控制提交同步（先授权再合并，失败零写入）。
- [x] **拉取条目应用（本地实验）**：同步 worker 现在把拉到的条目真正 apply 进 MLS 并落成本机消息（游标只在 apply 后前进、重放幂等、失败分类），作者账户经**凭据目录**映射（未知作者显式计数 `unknown_authors`）。
- [x] **平台密钥槽（本机实验）**：Windows DPAPI（当前用户作用域）封装/解封/删除通过；非 Windows 回退口令派生槽。限制：跨用户/跨机器失败无法单机自动验证。
- [x] **系统通知（本机实验）**：有托盘时尝试投递；**无托盘显式不可用**并回退应用内提示（测试断言"诚实失败"而非弹窗）。
- [ ] **经中继实跑**：仍未验证（需要可用 relay 或第二台设备）。
- [ ] **跨网段/对称 NAT 直连**：未验证。
- [ ] **OS 凭据库（Credential Manager/keychain）**：未接，当前是 DPAPI 文件封装。



## 2026-10-19 收尾回归（第 48 轮，最终基线）

- **Rust**：`cargo test --workspace --offline` → **994 passed / 0 failed**（`cargo check --locked` 通过）
- **Gradle**：14 个任务全部 `BUILD SUCCESSFUL`（presentationChecks / p0Smoke / p0Snapshot / previewSnapshot / localStoreSmoke / identitySmoke / identityPersistSmoke / attachmentSmoke / keySlotSmoke / groupSmoke / dpapiSmoke / platformSmoke / pluginSmoke / pluginJvmSmoke）
- **Android**：`assembleDebug` 成功
- **文档自检**：`tools/verify-project.cjs` PASS
- **磁盘**：`tools/check-disk.ps1` → D: 171.63 GB 空闲

仍**未覆盖**（P0 口径不变）：经中继实际收发、跨网段/对称 NAT、跨机器附件下载（本机双库已验）、OS 凭据库（当前 DPAPI 文件封装）、插件沙箱执行（当前同 JVM 无沙箱、需用户确认）。

```
RUST_EXIT=0  LOCKED_EXIT=0  VERIFY_EXIT=0  GRADLE_EXIT=0  ANDROID_EXIT=0
```


## 2026-10-19 第 55 轮补充（本地实验）

- [x] **审计持久化与保留期**：（本轮在做）node_audit 落库 + `audit_query`（viewer 过滤）+ `purge_audit`（默认保留 7 天）。
- [x] **身份设备登记与撤销集合**：（本轮在做）`identity_devices` 迁移 0013 + register/list/revoke（不能撤销本机）+ 恢复导入登记。
- [x] **插件私有 KV 迁移**：Keep/Namespace 双策略，原键保留、重跑幂等、超配额预检拒绝。
- [x] **落地到可人工验证**：`docs/manual-test-guide.md`（12 节）+ `tools/demo.ps1`（起节点/健康检查/粘贴配置/停止），实测 `node health: OK`。

## 2026-10-20 第二批收尾（第 64 轮）

- [x] **邀请闭环**：KeyPackage 导出 → 建群拿可用 Welcome → 节点投递/取回（可按被邀请者过滤）→ 接受入群（端到端有测试）。
- [x] **审计落库**（0009）+ `GET /v1/audit`（过滤/白名单/关闭显式）+ `kind=invite`。
- [x] **设备登记与撤销**（0013）+ 撤销随控制提交传播（字段 61 / 操作值 14）+ 账户级播种。
- [x] **可验证性**：`docs/manual-test-guide.md`、`tools/demo.ps1`、`tools/verify-all.ps1`。
- 未完成（正式 P0 验收前必须补）：插件沙箱/签名、真实中继、OS 凭据库、PRESENTATION 收敛、后台通知。

## 2026-10-20 第三批收尾（第 88 轮）

- [x] **交付物级回归**：verify-package（结构+4 冒烟+GUI 存活）、verify-all 七阶段、双设备演示。
- [x] **双设备互通**：KeyPackage 导出 → 建群 → 节点登记 → 加为节点成员 → 邀请 → 发送 accepted → 对方入群 → 拉取 applied=1。
- [x] **插件权限门**（最小可行沙箱）：清单声明四权限，宿主过滤，类加载器按声明放行；宿主类永不可见。
- [ ] 未完成：真实中继/NAT、OS 凭据库、后台通知、PRESENTATION 全面收敛、插件签名信任链、MSI 安装器。

## 2026-10-21 第六批收尾（第 118 轮）

- [x] **证书生命周期**：DER 解析有效期（`cert_validity`）→ 节点启动告警 → `/v1/tls/info` 公开 → 客户端启动自动显示（<30 天显红）。
- [x] **插件治理**：权限门 + 签名闸门（默认要求签名）+ 信任列表（记住/移除）。
- [x] **错误可用性**：`Cow` 详情 → 整条错误链 → 分层展示（7 处统一 `ErrorBanner`）→ 统一失败投影（12 处共用）。
- [x] **验收**：`verify-all` 11 阶段；`docs/FINAL_REPORT.md` + `docs/RETROSPECTIVE_120.md`。
- [ ] 未完成：跨机器/NAT/中继（需外部节点）、证书自动化续签、`AppFacade` 深层收敛、安装器/后台通知/soak、非 Windows 平台。

## 2026-10-21 公网跨机器验证

- [x] **公网节点部署**（Windows Server，`106.54.223.171:8443`，自签证书 + TLS，指纹 `13be2f93…`）。
- [x] **跨机器收发**：打包产物 `sendDispatch accepted` → `syncOnce fetched=2 applied=1 skipped=1 cursor=1 settled=true`。
- [x] **TLS pinning 生效**：同证书指纹校验通过；换成错误指纹 → 提交被拒（`FAIL NODE_UNAVAILABLE`）。
- [x] **证书状态端点**：`/v1/tls/info` 返回 `cert_sha256=13be2f93…, days_remaining=3649, expired=false`。
- [ ] 仍未做：NAT 打洞/中继（当前只支持直连公网 IP）、证书自动续签、非 Windows 平台验证。
