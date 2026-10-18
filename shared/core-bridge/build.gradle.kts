plugins {
    alias(libs.plugins.kotlin.multiplatform)
    alias(libs.plugins.android.library)
}
kotlin {
    jvm("desktop")
    androidTarget()
    jvmToolchain(21)
    sourceSets {
        commonMain.dependencies {
            implementation(project(":shared:core-api"))
            implementation(libs.coroutines.core)
        }
        val desktopMain by getting {
            kotlin.srcDirs("src/jvmMain/kotlin", rootProject.layout.buildDirectory.dir("generated/chat-ffi"))
            dependencies { implementation(libs.jna) }
        }
        androidMain {
            kotlin.srcDirs("src/jvmMain/kotlin", rootProject.layout.buildDirectory.dir("generated/chat-ffi"))
            dependencies { implementation("net.java.dev.jna:jna:${libs.versions.jna.get()}@aar") }
        }
    }
}
android {
    namespace = "dev.chat.shared.corebridge"
    compileSdk = 36
    buildToolsVersion = "36.0.0"
    ndkVersion = "28.2.13676358"
    defaultConfig { minSdk = 26; ndk { abiFilters += "arm64-v8a" } }
    sourceSets.getByName("main").jniLibs.srcDir(rootProject.layout.buildDirectory.dir("native/android"))
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }
}
tasks.matching { it.name.startsWith("compile") && it.name.contains("Kotlin") }.configureEach {
    dependsOn(rootProject.tasks.named("prepareRustHost"))
}
tasks.matching { it.name.startsWith("merge") && it.name.endsWith("JniLibFolders") }.configureEach {
    dependsOn(rootProject.tasks.named("prepareRustAndroid"))
}
