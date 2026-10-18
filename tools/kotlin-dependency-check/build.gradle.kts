plugins {
    alias(libs.plugins.kotlin.multiplatform)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.compose)
    alias(libs.plugins.android.library)
}

kotlin {
    jvm("desktop")
    androidTarget()
    jvmToolchain(21)
    sourceSets {
        commonMain.dependencies {
            implementation(compose.runtime)
            implementation(compose.foundation)
            implementation(compose.material3)
            implementation(libs.coroutines.core)
            implementation(libs.serialization.json)
        }
        val desktopMain by getting {
            kotlin.srcDir(layout.buildDirectory.dir("generated/uniffi"))
            dependencies {
                implementation(compose.desktop.currentOs)
                implementation(libs.pf4j)
                implementation(libs.jna)
            }
        }
        androidMain {
            kotlin.srcDir(layout.buildDirectory.dir("generated/uniffi"))
            dependencies {
                implementation("net.java.dev.jna:jna:${libs.versions.jna.get()}@aar")
            }
        }
    }
}

android {
    namespace = "dev.chat.dependencies"
    compileSdk = 36
    buildToolsVersion = "36.0.0"
    ndkVersion = "28.2.13676358"
    defaultConfig { minSdk = 26 }
    sourceSets.getByName("main").jniLibs.srcDir(layout.buildDirectory.dir("generated/jniLibs"))
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }
}

dependencyLocking { lockAllConfigurations() }

val desktopTarget = kotlin.targets.getByName("desktop") as org.jetbrains.kotlin.gradle.targets.jvm.KotlinJvmTarget
val desktopCompilation = desktopTarget.compilations.getByName("main")
tasks.register<JavaExec>("verifyFfi") {
    group = "verification"
    description = "Call the compiled Rust DLL through generated UniFFI Kotlin bindings."
    dependsOn("desktopMainClasses")
    classpath(desktopCompilation.output.allOutputs, desktopCompilation.runtimeDependencyFiles)
    mainClass.set("dev.chat.dependencies.FfiCheckKt")
    systemProperty("jna.library.path", rootProject.file("target/debug").absolutePath)
}

tasks.register("resolveDependencies") {
    group = "verification"
    description = "Download and lock the application dependency classpaths."
    doLast {
        val classpaths = setOf(
            "commonMainResolvableDependenciesMetadata",
            "desktopCompileClasspath", "desktopRuntimeClasspath",
            "androidDebugCompileClasspath", "androidDebugRuntimeClasspath",
            "androidReleaseCompileClasspath", "androidReleaseRuntimeClasspath"
        )
        configurations.filter { it.isCanBeResolved && it.name in classpaths }.forEach {
            println("Resolving ${it.name}")
            it.resolve()
        }
    }
}
