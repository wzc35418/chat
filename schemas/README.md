# Schema草案

plugin-manifest用于可信JVM插件，theme用于声明式默认主题，message-types/extension用于结构事件的内容字段；它们均非完整线协议。包大小/解压量/ZIP路径、实际完整消息64KiB、签名/权限需要业务层校验，JSON schema不能替代。jarSha256在最终打包产物中必须与实际JAR匹配；当前示例未打包故不填虚构摘要。
