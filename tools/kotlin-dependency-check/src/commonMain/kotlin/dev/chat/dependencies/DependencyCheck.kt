package dev.chat.dependencies

import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

// This verifies the dependency baseline; it is not the chat application.
@Serializable
data class DependencyStatus(val status: String)

val dependencyStatus = MutableStateFlow(
    Json.encodeToString(DependencyStatus("dependencies-ready"))
)

@Composable
fun DependencyCheck() {
    Text(dependencyStatus.value)
}
