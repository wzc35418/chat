plugins {
    alias(libs.plugins.kotlin.multiplatform)
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.compose)
}
kotlin {
    jvm("desktop")
    androidTarget()
    jvmToolchain(21)
    sourceSets {
        commonMain.dependencies {
            implementation(project(":shared:core-api"))
            implementation(project(":shared:presentation"))
            implementation(compose.runtime)
            implementation(compose.foundation)
            implementation(compose.material3)
            implementation(libs.coroutines.core)
        }
    }
}
android {
    namespace = "dev.chat.shared.ui"
    compileSdk = 36
    buildToolsVersion = "36.0.0"
    defaultConfig { minSdk = 26 }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }
}
