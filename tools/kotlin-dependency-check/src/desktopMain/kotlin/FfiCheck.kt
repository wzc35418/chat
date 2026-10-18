package dev.chat.dependencies

import uniffi.chat_dependency_check.dependencyStatus

fun main() {
    val status = dependencyStatus()
    check(status == "Rust/Kotlin FFI dependencies ready") { "Unexpected native response: $status" }
    println(status)
}
