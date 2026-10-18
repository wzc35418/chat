plugins { alias(libs.plugins.kotlin.jvm) }
kotlin { jvmToolchain(21) }
dependencies {
    compileOnly(project(":desktop:plugin-sdk"))
}
