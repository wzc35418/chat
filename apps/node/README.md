# apps/node

Rust 节点入口。main 当前明确以78退出，避免误以为节点已监听。真正启动函数 run_node 在 node-api 计划；锁定版本后接配置、持久库和 HTTP 服务。

- [详细实施总计划](../../docs/implementation/README.md)

阶段完成需实际实现和验证，当前空源码位置不计完成。
