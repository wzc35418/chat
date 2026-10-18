# crates/protocol

Rust 线协议 crate。P0 隔离帧（`wire`）保持原公开 API；PROTOCOL-001~014 全部实现：

- `ids`：001 强类型 ID 与规范文本解析；
- `canonical`：002 确定性规范编码（TLV，字段号 1–54 冻结）；
- `decode`：003 有界解码（`DecodeLimits`，返回 `Unverified` 帧）；
- `signing`：004/006 验签原文与 Ed25519 验签原语；
- `verify`：005 候选外层验签与 007 日志回执/哈希链；
- `event`：008 应用事件规范编码与 009 内外绑定认证（`SecretBytes` 析构清零）；
- `extension`：010 扩展内容校验（未知 payload 保留 + fallback）；
- `capability`：011 能力协商（必需能力/未知能力/限额取小/会话指纹）；
- `cursor`：012/013 ChaCha20-Poly1305 认证分页游标；
- `digest`：`Digest`/`ControlHead` 类型；
- `error`：014 稳定错误码（43 个）与脱敏 `PublicError`。

冻结的字节格式、字段号、签名域、日志/事件/能力格式见 [标识符、线协议、编码与验证](../../docs/implementation/protocol.md) 的「P1-01 已冻结的字节与错误码」。固定测试向量在 `tests/vectors.rs`（字节/签名/事件/日志哈希/会话指纹 hex 断言）。

验证命令：`. .\tools\activate.ps1; cargo test --offline -p chat-protocol`（当前 73 passed）。
