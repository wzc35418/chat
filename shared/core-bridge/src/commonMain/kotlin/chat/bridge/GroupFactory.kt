package chat.bridge

import chat.api.GroupService

/** 创建群服务桥（GROUP 模块）。独占单写者：同一库不要与 LocalStoreFfi 同时打开。 */
expect fun createGroupService(databasePath: String, masterKeyHex: String, node: String?): GroupService
