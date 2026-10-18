package chat.bridge

import chat.api.LocalStore

/**
 * 创建本地存储桥（P2-01）。
 *
 * 实际实现在 jvmMain：桌面与 Android 共用同一份 UniFFI 绑定，
 * 通过 Rust formal store 读写加密数据库。原生对象不会被 Compose 直接持有。
 */
expect fun createLocalStore(): LocalStore
