# apps/node

Rust 节点入口（可部署二进制 `chat-node`，API-010 run_node）。

- 逻辑在 [src/lib.rs](src/lib.rs)：配置解析/校验、种子加载、`NodeService` 初始化与迁移、axum 全部端点、Ctrl+C 优雅关闭；
- [src/main.rs](src/main.rs) 只把退出码翻译给操作系统。

## 启动

```powershell
cargo run -p chat-node -- --bind 127.0.0.1:8080 --data-dir .\node-data --seed-file .\node-seed.bin --init-seed
```

配置来源（命令行优先于环境变量）：

|参数|环境变量|说明|
|---|---|---|
|`--bind`|`CHAT_NODE_BIND`|监听地址，默认 `127.0.0.1:8080`；非回环地址必须再加 `--allow-public-bind`（进程内没有 TLS 终止）|
|`--data-dir`|`CHAT_NODE_DATA_DIR`|数据目录（内含 `node.db`），默认 `chat-node-data`|
|`--seed-file`|`CHAT_NODE_SEED_FILE`|节点签名种子：32 字节原文或 64 位十六进制；缺失则拒绝启动|
|`--init-seed`|`CHAT_NODE_INIT_SEED=1`|显式允许在种子文件缺失时生成一个新的随机种子|
|`--node-id`|`CHAT_NODE_ID`|节点标识，默认由公钥派生（`node-<公钥前16位>`）|
|`--expect-public-key`|`CHAT_NODE_EXPECT_PUBLIC_KEY`|固定节点公钥：与种子派生结果不一致即拒绝启动|
|—|`CHAT_NODE_RATE_LIMIT_WINDOW_SECONDS` / `CHAT_NODE_RATE_LIMIT_REQUESTS_PER_DEVICE`|API-005 限流窗口与每设备预算（账户预算自动取每设备预算的两倍）|

启动日志只有一行：`chat-node started node_id=... address=... database=...`；**密钥种子、私钥与任何正文都不进日志**。

## 验证

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-node
```

结果：6 个 lib 单元测试（配置/种子/固定公钥/派生 node_id，含一次真实 `spawn_node` + 优雅关闭）+ 2 个 `tests/smoke.rs`（进程内 `spawn_node` 的真 TCP `/v1/health`、关闭后端口不可连；真实二进制子进程解析启动日志、做健康检查，并断言日志不含种子十六进制）。

## 本阶段未实现

API-001 的 TOML 配置文件与目录权限校验、API-009 事件套接字、后台 GC worker（NODE-016）、TLS 终止、管理接口与多实例协调；完整限制见 [docs/implementation/api.md](../../docs/implementation/api.md) 的 API-010 记录。
