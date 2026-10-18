package chat.bridge

import chat.api.SyncService

/** 创建常驻同步服务桥；独占单写者，运行期间其它 FFI 对象不能打开同一库。 */
expect fun createSyncService(
    databasePath: String,
    masterKeyHex: String,
    endpointUrl: String,
    nodeId: String,
    nodePublicKeyHex: String,
    groupId: String,
): SyncService
