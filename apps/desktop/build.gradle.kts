plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.compose)
}
kotlin { jvmToolchain(21) }
dependencies {
    implementation(project(":shared:ui"))
    implementation(project(":shared:presentation"))
    implementation(project(":shared:core-api"))
    implementation(project(":shared:core-bridge"))
    implementation(project(":shared:platform-api"))
    implementation(project(":desktop:platform"))
    implementation(project(":desktop:plugin-host"))
    // P4 起步：JVM 插件宿主需要 SDK 的 DesktopPlugin/PluginContext 接口
    implementation(project(":desktop:plugin-sdk"))
    implementation(compose.desktop.currentOs)
    implementation(compose.material3)
    implementation(libs.coroutines.core)
    // DPAPI 密钥槽需要直接调用 crypt32（与 core-bridge 使用同一 JNA 版本）
    implementation(libs.jna)
}
compose.desktop {
    application {
        mainClass = "chat.desktop.MainKt"
        jvmArgs += "-Djna.library.path=${rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath}"
        val runTemp = rootProject.layout.buildDirectory.dir("tmp/desktopRun").get().asFile.absolutePath
        jvmArgs += "-Djava.io.tmpdir=$runTemp"
        jvmArgs += "-Dskiko.data.path=$runTemp"
    }
}
tasks.matching { it.name == "run" }.configureEach {
    doFirst { rootProject.layout.buildDirectory.dir("tmp/desktopRun").get().asFile.mkdirs() }
}
tasks.register<JavaExec>("p0Smoke") {
    group = "verification"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.P0SmokeKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
}
tasks.register<JavaExec>("p0Snapshot") {
    group = "verification"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.P0SnapshotKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
    systemProperty("java.awt.headless", "true")
    val snapshotTemp = rootProject.layout.buildDirectory.dir("tmp/p0Snapshot")
    systemProperty("java.io.tmpdir", snapshotTemp.get().asFile.absolutePath)
    systemProperty("skiko.data.path", snapshotTemp.get().asFile.absolutePath)
    doFirst { snapshotTemp.get().asFile.mkdirs() }
    workingDir(rootProject.projectDir)
}

tasks.register<JavaExec>("presentationChecks") {
    group = "verification"
    dependsOn("classes")
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.PresentationChecksKt")
}

tasks.register<JavaExec>("previewSnapshot") {
    group = "verification"
    // 真实身份面板需要原生库与 JNA 路径（与 p0Snapshot 一致）。
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.PreviewSnapshotKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
    systemProperty("java.awt.headless", "true")
    val previewTemp = rootProject.layout.buildDirectory.dir("tmp/previewSnapshot")
    systemProperty("java.io.tmpdir", previewTemp.get().asFile.absolutePath)
    systemProperty("skiko.data.path", previewTemp.get().asFile.absolutePath)
    doFirst { previewTemp.get().asFile.mkdirs() }
    workingDir(rootProject.projectDir)
}

tasks.register<JavaExec>("localStoreSmoke") {
    group = "verification"
    description = "P2-01 竖直链路冒烟：Kotlin → UniFFI → Rust core → formal store。"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.LocalStoreSmokeKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
    systemProperty("file.encoding", "UTF-8")
    systemProperty("stdout.encoding", "UTF-8")
    val storeTemp = rootProject.layout.buildDirectory.dir("tmp/localStoreSmoke")
    systemProperty("java.io.tmpdir", storeTemp.get().asFile.absolutePath)
    doFirst { storeTemp.get().asFile.mkdirs() }
}

tasks.register<JavaExec>("identitySmoke") {
    group = "verification"
    description = "P1-02 竖直链路冒烟：Kotlin → UniFFI → crates/identity（真实口令/联系人状态机）。"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.IdentitySmokeKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
    systemProperty("file.encoding", "UTF-8")
    systemProperty("stdout.encoding", "UTF-8")
}

tasks.register<JavaExec>("identityPersistSmoke") {
    group = "verification"
    description = "P1-02 身份持久化冒烟：重开后账户/设备/指纹与联系人均保留，错误主密钥拒绝。"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.IdentityPersistSmokeKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
    systemProperty("file.encoding", "UTF-8")
    systemProperty("stdout.encoding", "UTF-8")
}

tasks.register<JavaExec>("attachmentSmoke") {
    group = "verification"
    description = "ATTACHMENTS 本机链路冒烟：导入/状态/导出/取消 + 25MiB 上限。"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.AttachmentSmokeKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
    systemProperty("file.encoding", "UTF-8")
    systemProperty("stdout.encoding", "UTF-8")
}

tasks.register<JavaExec>("keySlotSmoke") {
    group = "verification"
    description = "PLATFORM-001/002/003 桌面竖切：口令派生主密钥封装、无明文落盘、失败锁定。"
    dependsOn("classes")
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.KeySlotSmokeKt")
    systemProperty("file.encoding", "UTF-8")
    systemProperty("stdout.encoding", "UTF-8")
}

tasks.register<JavaExec>("groupSmoke") {
    group = "verification"
    description = "GROUP 竖直链路：Kotlin -> UniFFI -> core groups -> 真 OpenMLS（建群/改名/禁言/移除/接受）。"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.GroupSmokeKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
    systemProperty("file.encoding", "UTF-8")
    systemProperty("stdout.encoding", "UTF-8")
}

tasks.register<JavaExec>("appFlowSmoke") {
    group = "verification"
    description = "应用真实链路：设置 → 建群/登记 → 发送（accepted）→ 关闭/同步/重开；用 -Dapp.* 传节点参数。"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.AppFlowSmokeKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
    // Gradle 不会把命令行 -D 转发给 forked JVM，用 -Papp.* 转成系统属性。
    listOf("endpoint", "nodeId", "nodeKey", "fingerprint").forEach { key ->
        providers.gradleProperty("app." + key).orNull?.let { systemProperty("app." + key, it) }
    }
    systemProperty("file.encoding", "UTF-8")
    systemProperty("stdout.encoding", "UTF-8")
}

tasks.register<JavaExec>("pairingSmoke") {
    group = "verification"
    description = "配对包往返：KeyPackage 包 → 邀请包 → Welcome 入群（copy/paste 文本，不依赖网络）。"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.PairingSmokeKt")
    listOf("endpoint", "nodeId", "nodeKey", "fingerprint").forEach { key ->
        providers.gradleProperty("pairing." + key).orNull?.let { systemProperty("pairing." + key, it) }
    }
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
    systemProperty("file.encoding", "UTF-8")
    systemProperty("stdout.encoding", "UTF-8")
}

tasks.register<JavaExec>("dpapiSmoke") {
    group = "verification"
    description = "PLATFORM-001/002 Windows DPAPI 竖切：当前用户作用域封装/解封、无明文落盘、删除后不可解。"
    dependsOn("classes")
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.DpapiSmokeKt")
    systemProperty("file.encoding", "UTF-8")
    systemProperty("stdout.encoding", "UTF-8")
}

tasks.register<JavaExec>("pluginJvmSmoke") {
    group = "verification"
    description = "PLUGINS P4：真实加载并执行本地插件 JAR（无沙箱，需显式信任）。"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.PluginJvmSmokeKt")
    systemProperty("file.encoding", "UTF-8")
    systemProperty("stdout.encoding", "UTF-8")
}

tasks.register<JavaExec>("pluginSmoke") {
    group = "verification"
    description = "PLUGINS 竖直冒烟：清单校验/安装/启动/派发计划/私有存储/停止/卸载（不执行插件代码）。"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.PluginSmokeKt")
    systemProperty("file.encoding", "UTF-8")
    systemProperty("stdout.encoding", "UTF-8")
}

tasks.register<JavaExec>("platformSmoke") {
    group = "verification"
    description = "PLATFORM-007~010 竖切：通知诚实不可用、参数校验、生命周期与单次退出。"
    dependsOn("classes")
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.PlatformSmokeKt")
    systemProperty("file.encoding", "UTF-8")
    systemProperty("stdout.encoding", "UTF-8")
    systemProperty("java.awt.headless", "true")
}

tasks.register<JavaExec>("previewApp") {
    group = "application"
    dependsOn("classes")
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.PreviewAppKt")
    val previewTemp = rootProject.layout.buildDirectory.dir("tmp/previewApp")
    systemProperty("java.io.tmpdir", previewTemp.get().asFile.absolutePath)
    systemProperty("skiko.data.path", previewTemp.get().asFile.absolutePath)
    doFirst { previewTemp.get().asFile.mkdirs() }
}


/**
 * 可交付便携包（离线可用，不依赖 WiX / jpackage）：
 *   dist/Chat-<version>/app.jar        —— 所有运行期 classpath（含 shared/desktop 模块）
 *   dist/Chat-<version>/native/        —— Rust host 动态库（含 chat_ffi.dll）
 *   dist/Chat-<version>/run.bat        —— 启动脚本（要求本机 JDK 21）
 *   dist/Chat-<version>.zip           —— 压缩后的交付物
 */
// 交付版本号：可用 -PchatVersion=0.1.1 覆盖；默认 0.1.0。
val appVersion = providers.gradleProperty("chatVersion").getOrElse("0.1.0")

val portableJar = tasks.register<Jar>("portableJar") {
    group = "distribution"
    dependsOn(tasks.named("classes"), rootProject.tasks.named("prepareRustHost"))
    archiveFileName.set("app.jar")
    destinationDirectory.set(rootProject.layout.buildDirectory.dir("portable"))
    duplicatesStrategy = DuplicatesStrategy.EXCLUDE
    manifest {
        attributes(
            "Main-Class" to "chat.desktop.MainKt",
        )
    }
    from(sourceSets["main"].output)
    from(configurations.runtimeClasspath.get().filter { it.exists() }.map { if (it.isDirectory) it else zipTree(it) })
}

val portableDist = tasks.register<Sync>("portableDist") {
    group = "distribution"
    dependsOn(portableJar)
    into(rootProject.layout.buildDirectory.dir("dist/Chat-$appVersion"))
    from(rootProject.layout.buildDirectory.dir("portable/app.jar"))
    from(rootProject.layout.buildDirectory.dir("native/host")) { into("native") }
    doLast {
        val dir = rootProject.layout.buildDirectory.dir("dist/Chat-$appVersion").get().asFile
        dir.resolve("run.bat").writeText(
            "@echo off\r\n" +
                "rem Chat $appVersion portable launcher (JDK 21 required)\r\n" +
                "set DIR=%~dp0\r\n" +
                "java -Djna.library.path=%DIR%native -Dfile.encoding=UTF-8 -jar %DIR%app.jar %*\r\n",
        )
        dir.resolve("README.txt").writeText(
            "Chat $appVersion portable build\r\n\r\n" +
                "1. Requires JDK 21 on PATH (java -version).\r\n" +
                "2. Double-click run.bat (or: run.bat)\r\n" +
                "3. Data is stored per-user; see docs/manual-test-guide.md\r\n",
        )
    }
}

tasks.register<Zip>("portableZip") {
    group = "distribution"
    dependsOn(portableDist)
    archiveFileName.set("Chat-$appVersion.zip")
    destinationDirectory.set(rootProject.layout.buildDirectory.dir("dist"))
    from(rootProject.layout.buildDirectory.dir("dist/Chat-$appVersion")) { into("Chat-$appVersion") }
}

/**
 * 自包含 app-image（jlink 运行时 + jpackage），不依赖 Compose 的 nativeDistributions
 * （后者在离线环境会在 downloadWix 上失败）。产物：build/dist/Chat-app/Chat.exe
 */
val jlinkRuntimeDir = rootProject.layout.buildDirectory.dir("jlink/chat-runtime")

val jlinkRuntime = tasks.register<Exec>("jlinkRuntime") {
    group = "distribution"
    val javaHome = System.getProperty("java.home")
    val outDir = jlinkRuntimeDir.get().asFile
    inputs.property("javaHome", javaHome)
    outputs.dir(outDir)
    commandLine(
        "$javaHome/bin/jlink.exe",
        "--add-modules", "java.base,java.desktop,java.logging,java.management,java.naming,java.sql,java.xml,jdk.crypto.ec,jdk.unsupported,jdk.zipfs",
        "--strip-debug", "--no-man-pages", "--no-header-files", "--compress=2",
        "--output", outDir.absolutePath,
    )
    doFirst { if (outDir.exists()) outDir.deleteRecursively() }
}

/**
 * 自包含 app-image（jlink + jpackage）。
 *
 * **原子替换**：jpackage 先写 `app-image-next`，成功后才把它换成 `app-image`。
 * 原因：以前先删 `app-image` 再生成，中间有几秒窗口——此时双击 Chat.exe 会报
 * “Error opening ... app\Chat.cfg: 没有那个文件或目录”（用户实际撞到过）。
 */
val jpackageAppImage = tasks.register<Exec>("jpackageAppImage") {
    group = "distribution"
    dependsOn(portableJar, tasks.named("jlinkRuntime"))
    val appVersion = providers.gradleProperty("chatVersion").getOrElse("0.1.0")
    val javaHome = System.getProperty("java.home")
    val inputDir = rootProject.layout.buildDirectory.dir("portable").get().asFile
    val destDir = rootProject.layout.buildDirectory.dir("dist/app-image").get().asFile
    val stageDir = rootProject.layout.buildDirectory.dir("dist/app-image-next").get().asFile
    inputs.dir(inputDir)
    outputs.dir(destDir)
    commandLine(
        "$javaHome/bin/jpackage.exe",
        "--type", "app-image",
        "--name", "Chat",
        "--app-version", appVersion,
        "--input", inputDir.absolutePath,
        "--main-jar", "app.jar",
        "--main-class", "chat.desktop.MainKt",
        "--runtime-image", jlinkRuntimeDir.get().asFile.absolutePath,
        "--dest", stageDir.absolutePath,
        "--java-options", "-Dfile.encoding=UTF-8",
        // 打包产物用 Chat.exe 启动时，必须让 JNA 找到 app\native 下的 chat_ffi.dll，
        // 否则 UniffiLib 静态初始化失败（现象：界面报 "Could not initialize class uniffi.chat_ffi.UniffiLib"）。
        "--java-options", "-Djna.library.path=\$APPDIR\\native",
    )
    doFirst {
        if (stageDir.exists()) stageDir.deleteRecursively()
        stageDir.mkdirs()
    }
    doLast {
        // 只有新的镜像完整生成后，才替换旧目录（替换窗口在毫秒级，且不再出现“半成品目录”）。
        if (destDir.exists()) destDir.deleteRecursively()
        destDir.mkdirs()
        stageDir.copyRecursively(destDir, overwrite = true)
        stageDir.deleteRecursively()
    }
}

// 把 Rust host 库塞进 app-image 的 app/ 目录，供 JNA 相对路径查找。
val appImageWithNative = tasks.register<Copy>("appImageWithNative") {
    group = "distribution"
    dependsOn(jpackageAppImage)
    from(rootProject.layout.buildDirectory.dir("native/host"))
    into(rootProject.layout.buildDirectory.dir("dist/app-image/Chat/app/native"))
}

tasks.register<Zip>("appImageZip") {
    group = "distribution"
    dependsOn(appImageWithNative)
    archiveFileName.set("Chat-app-" + providers.gradleProperty("chatVersion").getOrElse("0.1.0") + ".zip")
    destinationDirectory.set(rootProject.layout.buildDirectory.dir("dist"))
    from(rootProject.layout.buildDirectory.dir("dist/app-image/Chat")) { into("Chat") }
}


// OS 凭据库（Windows Credential Manager）冒烟：真写/真读/真删。
tasks.register<JavaExec>("credentialManagerSmoke") {
    group = "verification"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.CredentialManagerSmokeKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
}


// 密钥槽探测与迁移（DPAPI 文件 <-> Windows 凭据管理器）冒烟。
tasks.register<JavaExec>("keySlotSelectorSmoke") {
    group = "verification"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.KeySlotSelectorSmokeKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
}


// 节点连通性自检冒烟：对一个真实运行中的节点跑 NodeSelfCheck（参数由脚本/手工给）。
tasks.register<JavaExec>("nodeSelfCheckSmoke") {
    group = "verification"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.NodeSelfCheckSmokeKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
}


// 一步发送（界面路径）冒烟：对真实节点跑 sendWithRecovery（参数走环境变量，避免 Gradle --args 引号问题）。
tasks.register<JavaExec>("sendFlowSmoke") {
    group = "verification"
    dependsOn("classes", rootProject.tasks.named("prepareRustHost"))
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.PackageE2eSmokeKt")
    systemProperty("jna.library.path", rootProject.layout.buildDirectory.dir("native/host").get().asFile.absolutePath)
    environment("CHAT_SENDFLOW", "1")
    val env = System.getenv()
    val endpoint = env["SENDFLOW_ENDPOINT"] ?: ""
    val nodeId = env["SENDFLOW_NODE_ID"] ?: ""
    val fingerprint = env["SENDFLOW_FP"] ?: "-"
    if (endpoint.isNotEmpty() && nodeId.isNotEmpty()) {
        args = listOf(endpoint, nodeId, "-", fingerprint)
    }
}
