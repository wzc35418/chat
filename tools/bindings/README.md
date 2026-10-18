# 绑定生成

P0 锁定 UniFFI 或替代框架后添加生成器和一致性检查。目标：Android arm64-v8a、Windows x64、Linux x64。生成源码需可复现，回调可释放，panic 不跨越 FFI，文件使用 token/受控流。不得手写伪造生成绑定。
