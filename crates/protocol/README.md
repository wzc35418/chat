# crates/protocol

Rust 线协议 crate。P0 隔离帧（`wire`）保持原公开 API；P1-01 新增：

- `ids`：PROTOCOL-001 强类型 ID 与规范文本解析；
- `canonical`：PROTOCOL-002 确定性规范编码（TLV，字段号冻结）；
- `decode`：PROTOCOL-003 有界解码（`DecodeLimits`，返回 `Unverified` 帧）；
- `signing`：PROTOCOL-004/006 验签原文与 Ed25519 验签原语；
- `extension`：PROTOCOL-010 扩展内容校验（未知 payload 保留 + fallback）；
- `cursor`：PROTOCOL-012/013 ChaCha20-Poly1305 认证分页游标；
- `digest`：`Digest`/`ControlHead` 类型；
- `error`：PROTOCOL-014 稳定错误码（41 个）与脱敏 `PublicError`。

冻结的字节格式、字段号、签名域与游标格式见 [标识符、线协议、编码与验证](../../docs/implementation/protocol.md) 的「P1-01 已冻结的字节与错误码」。固定测试向量在 `tests/vectors.rs`（字节/签名/令牌 hex 断言）。

验证命令：`. .\tools\activate.ps1; cargo test --offline -p chat-protocol`。PROTOCOL-005/007/008/009/011 仍为设计，未实现。
