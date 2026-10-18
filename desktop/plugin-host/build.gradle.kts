plugins { alias(libs.plugins.kotlin.jvm) }
kotlin { jvmToolchain(21) }
dependencies {
    implementation(project(":desktop:plugin-sdk"))
    implementation(libs.pf4j)
}
