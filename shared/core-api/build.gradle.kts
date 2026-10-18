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
            
        }
    }
}
android {
    namespace = "dev.chat.shared.coreapi"
    compileSdk = 36
    buildToolsVersion = "36.0.0"
    defaultConfig { minSdk = 26 }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }
}
