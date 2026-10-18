# 分阶段工作项与依赖

状态更新：2026-10-18 起开发由 DeepSeek 全栈接管。P1-01 线协议冻结与 DOMAIN 规则、P1-03 节点鉴权与设备注册、P0-04 本地加密存储基础已交付真实代码与自动化测试（见 [protocol](protocol.md)、[domain](domain.md)、[node](node.md)、[api](api.md)、[storage](storage.md)、[crypto](crypto.md) 的实现状态与证据；P0-04 局部实验记录在 [P0 清单](P0_CHECKLIST.md)）；完整退出条件仍未全部满足。以下保留原状态说明：P0-01 / P0-02 已开始，已实现 Windows 本机双设备 Kotlin → Rust → MLS → iroh 最小链路及 Android 构建入口，详见 [实际 P0 记录](../P0.md)。P0-03 的 T1/C01、T1→T2、发送/接收、独立节点 T4/C05 与 P0 签名回执局部实验已在 [P0 清单](P0_CHECKLIST.md) 勾选并记录证据；正式节点鉴权、HTTP、生产签名钥托管/协议编码及三端运行和 P0-03/04/05 整体阻断项仍未通过。以下按可验证的纵向链路分包，不是工期承诺。函数章提供每函数细节，阶段条目提供集成顺序与退出条件。

|工作项|内容|依赖|函数章节|必须交付|退出条件|
|---|---|---|---|---|---|
|P0-01|锁定三端工具链与 FFI 往返|无|[bridge](bridge.md)、[platform](platform.md)|版本矩阵、可复现生成绑定、三端最小运行证据|三个目标实际调用、取消、释放均通过|
|P0-02|真实 MLS 与设备凭据互操作|P0-01|[protocol](protocol.md)、[identity](identity.md)、[crypto](crypto.md)|账户/设备/KeyPackage/Welcome 最小实现、固定测试向量|三端往返和移除设备后的新 epoch 通过|
|P0-03|OpenMLS provider 与 SQLite 原子性|P0-02|[storage](storage.md)、[crypto](crypto.md)|统一事务 provider 或 journal ADR、C01..C09 实验|crash 后不重用消费密钥，无半消息/半状态|
|P0-04|本地加密存储与系统密钥|P0-01|[platform](platform.md)、[storage](storage.md)|三端主密钥保护、加密 DB/WAL 验证|拒绝/失效/无服务路径不会落明文|
|P0-05|自托管直连与中继|P0-01|[transport](transport.md)|受限 NAT 报告、自托管配置|关闭公共设施后受支持场景成功|
|P1-01|冻结线协议和编码|P0-02|[protocol](protocol.md)、[domain](domain.md)|Wire DTO、签名原文、限额、错误码、向量|跨语言规范字节相同，畸形输入有界拒绝|
|P1-02|建立身份与联系人握手|P0-03,P0-04,P1-01|[identity](identity.md)、[storage](storage.md)|账户创建、邀请、确认、指纹 UI/CLI 链路|双向接受后能开 direct，身份变化明确警告|
|P1-03|实现节点鉴权与设备注册|P1-01|[api](api.md)、[node](node.md)|challenge、签名鉴权、能力、注册服务|跨方法/节点/请求重放拒绝，第三设备限额有效|
|P1-04|实现群日志 T4 和签名回执|P1-03|[node](node.md)、[domain](domain.md)、[protocol](protocol.md)|创建群、append、fetch、head、耐久幂等|同 submission 同结果；相异摘要拒绝；断电不丢已确认|
|P1-05|实现发送意图与 T2/outbox|P0-03,P1-02,P1-04|[core](core.md)、[sync](sync.md)、[storage](storage.md)、[crypto](crypto.md)|send ticket、租约、退避、固定密文尝试|重复 UI 操作一意图，未知接受结果不重新加密|
|P1-06|实现接收 T3 与信箱|P1-05|[sync](sync.md)、[node](node.md)、[transport](transport.md)|连续拉取、MLS验证、一次有效插入、ACK|收件离线恢复/重复/乱序不重复消息|
|P1-07|跑两客户端纵向闭环|P1-06,P0-05|[core](core.md)、[sync](sync.md)、[api](api.md)|命令行或最小界面端到端报告|文字→接受→送达、节点重启、双方重启成功|
|P2-01|Kotlin 服务映射与页面状态|P1-07|[bridge](bridge.md)、[presentation](presentation.md)|完整 core-api DTO、Flow 包装、列表/会话页面|丢事件重查正确，页面取消不误取消已提交发送|
|P2-02|消息编辑区/回复/失败处理|P2-01|[presentation](presentation.md)、[core](core.md)|防双击、状态证据、可操作错误、分页|新输入不被迟到响应清除，取消竞争显示真实结果|
|P2-03|附件流式链路|P1-07,P2-01|[attachments](attachments.md)、[crypto](crypto.md)、[node](node.md)|导入/上传/descriptor/下载/导出/GC|块交换/截断/损坏拒绝，25MiB边界与峰值内存记录|
|P2-04|双设备与恢复文件|P2-01,P0-04|[identity](identity.md)、[groups](groups.md)|扫码配对、主管签发、逐会话加入、恢复向导|最多2设备；新设备无加入前历史；错误恢复秘密拒绝|
|P2-05|三端生命周期与包装|P2-02|[platform](platform.md)、[bridge](bridge.md)|托盘、退出、Android恢复前台、原生库打包|睡眠重连、直接 kill 恢复、三端可安装原型|
|P3-01|群邀请与正式加入|P2-04,P1-04|[groups](groups.md)、[crypto](crypto.md)、[node](node.md)|邀请/接受/拒绝/claim/Commit/Welcome|未接受不加入；一次性包不复用；加入后内容边界有效|
|P3-02|群权限与治理|P3-01|[groups](groups.md)、[domain](domain.md)|角色/禁言/群名/移除/转让/退出/解散|管理员不能改群主或同级；唯一owner始终成立|
|P3-03|并发控制与缺口恢复|P3-02|[sync](sync.md)、[crypto](crypto.md)、[node](node.md)|旧head/epoch重建、分支暂停、缺控制补齐|两管理员竞争一致；非法Commit不推进游标|
|P3-04|跨群设备撤销|P3-03|[identity](identity.md)、[groups](groups.md)、[sync](sync.md)|父子任务、撤销传播、全部相关叶子移除|部分节点离线显示pending，完成群不能解密后续内容|
|P3-05|送达明细与群容量|P3-03|[domain](domain.md)、[core](core.md)、[presentation](presentation.md)|每设备/账户送达统计、20/40基线报告|同账户2设备计1，本人设备排除，超限明确拒绝|
|P4-01|声明式配置包|P2-05|[plugins](plugins.md)、[presentation](presentation.md)|主题/快捷键schema、预览/应用/回滚|不执行代码，非法配置不破坏当前设置|
|P4-02|插件清单与可信安装|P4-01|[plugins](plugins.md)|包检查、可信提示、摘要/版本/路径限制|ZIP穿越/压缩炸弹/检查后替换拒绝|
|P4-03|SDK和懒加载宿主|P4-02|[plugins](plugins.md)|命令/选中消息操作/设置、短期用户token|没有用户动作不能读选中内容或直接发消息|
|P4-04|示例、禁用和安全启动|P4-03|[plugins](plugins.md)|两个真实JAR示例、清理注册、故障报告|stop 后正常插件无注册；安全模式不执行第三方入口|
|P5-01|节点配置、镜像与部署|P3-05,P4-04|[api](api.md)、[node](node.md)、[transport](transport.md)|真实容器镜像、TLS入口、持久卷、relay文档|节点重启已确认数据仍在；关闭公共设施演示通过|
|P5-02|发布包装与隐私检查|P5-01|[platform](platform.md)、[attachments](attachments.md)|三端安装包、恢复/备份说明、日志脱敏检查|无正文/私钥出现在节点和诊断；平台限制明确|
|P5-03|完整预览验收|P5-02|[core](core.md)、[groups](groups.md)、[plugins](plugins.md)|原MVP12步演示、故障报告、性能基线|记录真实机器/版本/容量；未通过项不得标完成|

## 每次实现一个工作项的步骤

1. 记录依赖已通过证据，选择本项最小纵向场景。
2. 从函数章把签名和真实类型写入源码，补实际库适配，保留模块边界。
3. 先通过纯规则/事务关键用例，再跑必要目标平台或进程故障用例。
4. 更新 function-catalog.json 与函数章状态；附实现提交、用例路径和结果。
5. 集成链路复验本次影响范围，最后才把工作项标为完成。

## 下一步精确到最初五个动作

1. 安装或选择实际 Rust/JDK/Gradle/Android 工具链，记录 P0-01 矩阵。
2. 对已有 Cargo 骨架运行 cargo check --workspace --locked，修正实际工具链问题并保存输出。
3. 核实并锁定 FFI 生成器与 Kotlin/Compose/AGP 兼容版本；补 Gradle wrapper 与模块真实插件。
4. 用 ffi_create_core 的最小非业务实验验证 DTO 返回、异常、回调释放三端路径。
5. 接入真实 MLS/provider 最小实例开始 P0-02/P0-03；在验证前不实现大批 UI/插件。
