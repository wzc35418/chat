package chat.bridge

import chat.api.IdentityService

/**
 * 创建身份服务桥（P1-02）。
 *
 * 实际实现在 jvmMain：桌面与 Android 共用同一份 UniFFI 绑定，
 * 调用 crates/identity 的真实密码学（账户/设备 ID、指纹、inv1 口令、联系人状态）。
 */
expect fun createIdentityService(): IdentityService
