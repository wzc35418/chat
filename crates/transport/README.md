# crates/transport

- `network`：P0 仅绑定 127.0.0.1 的真实 iroh/QUIC 传输实验（关闭公共 relay/发现、portmapper 与可选网络探测）。自托管 relay、跨 NAT 与节点客户端仍待实现。
- **纯逻辑核心**（无网络、无 sleep、无 `unsafe`）：
  - `session`：TRANSPORT-001 会话建立（端点/设备标识、协商结果、初始连接态、预算）；
  - `state`：TRANSPORT-002 连接状态机（5 状态 × 6 事件冻结迁移表，非法迁移拒绝、重复事件幂等）；
  - `frame`：TRANSPORT-003 帧编解码与严格有序分片重组（长度前缀、上限、乱序/重复/旧帧拒绝）；
  - `retry`：TRANSPORT-004 指数退避 + 有界确定性 jitter + 预算耗尽 Stop；
  - `wire`：可注入 `WireSink`/`WireSource` 端口（端口失败分可重试/致命）；
  - `send`：TRANSPORT-005 负载→分片→逐片写出，带队列/记录预算与可重试/致命判定；
  - `receive`：TRANSPORT-006 逐片喂入重组器，带会话/端点绑定与跨会话拒绝；
  - `pull_map`：TRANSPORT-007 把 sync `Pull { from, to }` 映射成帧请求序列与响应校验（不依赖 chat-sync，保持分层方向）；
  - `error`：传输层稳定错误码（冻结码复用，其余新增码登记在 transport.md）。

语义、迁移表、线格式、管线、错误码登记与证据见 [设备直连、中继与节点客户端](../../docs/implementation/transport.md) 的「Rust 纯逻辑传输核心」。P0 局部验证不表示该模块全部功能已完成；TRANSPORT-002、006、008~012 未实现。

验证命令：`. .\tools\activate.ps1; cargo test --offline -p chat-transport`（当前 53 passed：lib 39 + tests/transport_core.rs 8 + tests/peer_pipeline.rs 6）。
