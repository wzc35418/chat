package chat.presentation.display

/**
 * 共享文案表。
 *
 * 这里的字符串只描述界面事实：P0 只能证明“接收设备已验证并解密”，
 * 不能写成正式产品的“已接受”“已送达”或“离线可用”。
 */
object UiCopy {
    const val P0_TITLE = "Chat"
    const val P0_SUBTITLE = "加密通信实验室 · P0"
    const val P0_HERO = "让消息安全抵达"
    const val P0_DESCRIPTION = "在本机建立两个独立设备。消息加密后通过真实连接传输，仅在接收设备解密。"
    const val P0_VERIFIED = "接收设备已验证并解密"
    const val P0_EPHEMERAL_NOTE = "本机临时验证 · 关闭后不保留历史 · 尚未接入正式节点与离线投递"
    const val P0_EMPTY_TITLE = "从一句你好开始"
    const val P0_EMPTY_WAITING = "先连接两个本机设备，再选择发送设备。"
    const val P0_EMPTY_READY = "选择 Alice 或 Bob 作为发送设备，发送第一条消息。"
    const val P0_CONNECT = "连接两个设备"
    const val P0_RECONNECT = "重建会话"
    const val P0_CONNECTING = "正在连接…"
    const val P0_SEND = "发送"
    const val P0_SENDING = "发送中…"
    const val P0_SENDER_LABEL = "发送设备"
    const val P0_COMPOSER_LABEL = "写一条消息"
    const val P0_DETAILS = "验证详情"
    const val P0_DETAILS_HIDE = "收起详情"

    const val PREVIEW_BADGE = "交互预览"
    const val PREVIEW_NOTICE = "联系人、消息、回执与状态均为显式样例数据；不连接真实账户、节点、会话或历史。"
    const val PREVIEW_SAMPLE_STATUS = "样例状态"
    const val PREVIEW_SAMPLE_SEND = "发送样例"
    const val PREVIEW_SAMPLE_HINT = "仅在预览中追加一条样例消息，不会离开本机。"
    const val PREVIEW_EMPTY_TITLE = "选择一位好友开始聊天"
    const val PREVIEW_EMPTY_DETAIL = "好友列表与消息均为样例数据。"
    const val PREVIEW_ADD_FRIEND = "加好友"
    const val PREVIEW_FRIEND_REQUESTS = "好友申请"
    const val PREVIEW_SEARCH = "搜索已接受好友（样例）"
}
