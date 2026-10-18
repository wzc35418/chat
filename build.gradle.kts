plugins {
    alias(libs.plugins.kotlin.multiplatform) apply false
    alias(libs.plugins.kotlin.compose) apply false
    alias(libs.plugins.kotlin.serialization) apply false
    alias(libs.plugins.compose) apply false
    alias(libs.plugins.android.library) apply false
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.kotlin.jvm) apply false
    alias(libs.plugins.kotlin.android) apply false
}

val onWindows = System.getProperty("os.name").startsWith("Windows")
tasks.register<Exec>("prepareRustHost") {
    group = "build"
    description = "Build the Rust core and generate Kotlin UniFFI bindings."
    workingDir(rootDir)
    inputs.files(fileTree("crates") { include("**/*.rs", "**/Cargo.toml") })
    inputs.files("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "tools/toolchain-lock.json", "tools/activate.ps1", "tools/build-native.ps1", "tools/build-native.sh")
    outputs.dir(layout.buildDirectory.dir("native/host"))
    outputs.dir(layout.buildDirectory.dir("generated/chat-ffi"))
    if (onWindows) commandLine("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/build-native.ps1")
    else commandLine("bash", "tools/build-native.sh")
}
tasks.register<Exec>("prepareRustAndroid") {
    group = "build"
    description = "Build Android arm64 Rust libraries using the NDK."
    dependsOn("prepareRustHost")
    workingDir(rootDir)
    inputs.files(fileTree("crates") { include("**/*.rs", "**/Cargo.toml") })
    inputs.files("Cargo.toml", "Cargo.lock", "tools/toolchain-lock.json", "tools/activate.ps1", "tools/build-native.ps1", "tools/build-native.sh")
    outputs.dir(layout.buildDirectory.dir("native/android"))
    if (onWindows) commandLine("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/build-native.ps1", "-AndroidOnly")
    else commandLine("bash", "tools/build-native.sh", "--android-only")
}
subprojects { dependencyLocking { lockAllConfigurations() } }
