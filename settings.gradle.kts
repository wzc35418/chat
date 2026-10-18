pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}
rootProject.name = "chat"
include(":tools:kotlin-dependency-check")
include(":apps:desktop")
include(":apps:android")
include(":shared:core-api")
include(":shared:core-bridge")
include(":shared:presentation")
include(":shared:ui")
include(":shared:platform-api")
include(":desktop:plugin-sdk")
include(":desktop:plugin-host")
include(":desktop:platform")
include(":plugins:examples:hello-command")
include(":plugins:examples:message-formatter")
