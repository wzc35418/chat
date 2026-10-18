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
    implementation(project(":desktop:platform"))
    implementation(project(":desktop:plugin-host"))
    implementation(compose.desktop.currentOs)
    implementation(compose.material3)
    implementation(libs.coroutines.core)
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
    dependsOn("classes")
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("chat.desktop.PreviewSnapshotKt")
    systemProperty("java.awt.headless", "true")
    val previewTemp = rootProject.layout.buildDirectory.dir("tmp/previewSnapshot")
    systemProperty("java.io.tmpdir", previewTemp.get().asFile.absolutePath)
    systemProperty("skiko.data.path", previewTemp.get().asFile.absolutePath)
    doFirst { previewTemp.get().asFile.mkdirs() }
    workingDir(rootProject.projectDir)
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
