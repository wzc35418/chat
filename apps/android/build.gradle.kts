import org.gradle.api.artifacts.dsl.LockMode

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
}
// AGP's artifact view omits kotlin-stdlib-common when it is only a variant
// constraint, while dependency resolution still records it in the lockfile.
dependencyLocking { lockMode.set(LockMode.LENIENT) }
kotlin { jvmToolchain(21) }
android {
    namespace = "chat.android"
    compileSdk = 36
    buildToolsVersion = "36.0.0"
    ndkVersion = "28.2.13676358"
    defaultConfig {
        applicationId = "dev.chat.p0"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0-p0"
        ndk { abiFilters += "arm64-v8a" }
    }
    buildFeatures { compose = true }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }
    packaging { resources.excludes += setOf("META-INF/AL2.0", "META-INF/LGPL2.1") }
}
dependencies {
    implementation(project(":shared:ui"))
    implementation(project(":shared:presentation"))
    implementation(project(":shared:core-api"))
    implementation(project(":shared:core-bridge"))
    implementation(libs.activity.compose)
}
