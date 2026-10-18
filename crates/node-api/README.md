# crates/node-api

节点 HTTP 适配层（axum 0.8）。当前状态：P1-03 的 `/v1/auth/challenge` 与 `/v1/devices`，群登记/日志与信箱的七个端点，以及附件 blob 的四个端点。

- [节点 HTTP、鉴权、限流与运行入口](../../docs/implementation/api.md)
- [节点服务、排序日志、密文信箱与文件](../../docs/implementation/node.md)

## 模块

|文件|内容|
|---|---|
|src/handlers.rs|build_router：`POST /v1/auth/challenge`（API-002）、`POST /v1/devices`（API-003 + NODE-001）、`POST /v1/groups`（NODE-005）、`POST /v1/groups/{id}/entries`（NODE-006）、`GET /v1/groups/{id}/log`（NODE-007）、`GET /v1/groups/{id}/head`（NODE-008）、`POST /v1/mailbox/items`（NODE-009）、`GET /v1/mailbox`（NODE-010）、`POST /v1/mailbox/ack`（NODE-011）、`POST /v1/blobs`（NODE-012）、`PUT /v1/blobs/{id}/chunks/{index}`（NODE-013）、`POST /v1/blobs/{id}/complete`（NODE-014）、`GET /v1/blobs/{id}/chunks/{index}`（NODE-015）|
|src/dto.rs|版本化请求/响应 DTO、鉴权头解析、正文 schema_version 校验|
|src/error.rs|NodeError → HTTP 状态与稳定错误码的唯一映射（400/401/403/404/409/413/429/503，错误体带 schema_version）|
|src/state.rs|AppState（服务 + 时钟），Clock trait、SystemClock、测试用 FixedClock|

本层只做限长、解码、鉴权、调用服务与结果映射；领域规则与事务都在 `chat-node-services`。所有已登记设备端点共用同一个 API-003 鉴权：签名绑定 method + 规范 path + 查询串 + 正文摘要 + 节点 + 一次性挑战。

## 验证

```
. .\tools\activate.ps1
cargo test --offline -p chat-node-api
```

`tests/http_contract.rs`（8 个）与 `tests/group_mailbox_http.rs`（8 个，含附件 blob 的三个契约用例）都用 `axum::serve` 绑定本地随机端口，以原始 HTTP/1.1 客户端覆盖路由、限长层、DTO、鉴权、二进制正文/响应与错误映射：16 个测试全部通过。

## 本阶段未实现

API-005 逐 IP/逐设备限流、API-009 事件套接字、API-010 run_node（含 TLS 终止与管理接口）。[apps/node](../../apps/node) 仍是明确 78 退出的骨架。
