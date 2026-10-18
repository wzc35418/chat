# crates/transport

- `network`：P0 仅绑定 127.0.0.1 的真实 iroh/QUIC 传输实验（关闭公共 relay/发现、portmapper 与可选网络探测）。自托管 relay、跨 NAT 与节点客户端仍待实现。
- 本轮新增**纯逻辑核心**（无网络、无 sleep、无 `unsafe`）：
  - `session`：TRANSPORT-001 会话建立（端点/设备标识、协商结果、初始连接态、预算）；
  - `state`：TRANSPORT-002 连接状态机（5 状态 × 6 事件冻结迁移表，非法迁移拒绝、重复事件幂等）；
  - `frame`：TRANSPORT-003 帧编解码与严格有序分片重组（长度前缀、上限、乱序/重复/旧帧拒绝）；
  - `retry`：TRANSPORT-004 指数退避 + 有界确定性 jitter + 预算耗尽 Stop；
  - `error`：传输层稳定错误码（`MALFORMED_FRAME`/`CONTENT_TOO_LARGE`/`PROTOCOL_UNSUPPORTED` 复用冻结码，其余为新增码，登记在 transport.md）。

语义、迁移表、线格式、错误码登记与证据见 [设备直连、中继与节点客户端](../../docs/implementation/transport.md) 的「Rust 纯逻辑传输核心」。P0 局部验证不表示该模块全部功能已完成；TRANSPORT-002、006~012 未实现。

验证命令：`. .\tools\activate.ps1; cargo test --offline -p chat-transport`（当前 33 passed：lib 25 + tests/transport_core.rs 8）。
