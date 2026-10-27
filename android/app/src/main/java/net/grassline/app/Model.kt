package net.grassline.app

import org.json.JSONArray
import org.json.JSONObject

data class Project(val name: String, val path: String)

data class SessionInfo(
    val id: String,
    val project: String,
    val provider: String,
    val title: String,
    val updated: Long,
    val running: Boolean,
)

data class ChatMessage(val role: String, val text: String)

/** What the Rust core reports (`Controller::snapshot`). */
data class AppState(
    /** unpaired, connecting, connected, offline or refused. */
    val status: String = "connecting",
    val reason: String? = null,
    val projects: List<Project> = emptyList(),
    /** Most recent first. */
    val sessions: List<SessionInfo> = emptyList(),
    val messages: Map<String, List<ChatMessage>> = emptyMap(),
    val error: String? = null,
) {
    companion object {
        fun parse(json: String): AppState {
            val o = JSONObject(json)
            fun <T> JSONArray.map(f: (JSONObject) -> T) = (0 until length()).map { f(getJSONObject(it)) }
            val msgs = o.getJSONObject("messages")
            return AppState(
                status = o.getString("state"),
                reason = o.optString("reason").takeIf { !o.isNull("reason") && it.isNotEmpty() },
                projects = o.getJSONArray("projects").map { Project(it.getString("name"), it.getString("path")) },
                sessions = o.getJSONArray("sessions").map {
                    SessionInfo(
                        it.getString("id"), it.getString("project"), it.getString("provider"),
                        it.getString("title"), it.getLong("updated"), it.getBoolean("running"),
                    )
                },
                messages = msgs.keys().asSequence().associateWith { k ->
                    msgs.getJSONArray(k).map { ChatMessage(it.getString("role"), it.getString("text")) }
                },
                error = if (o.isNull("error")) null else o.getString("error"),
            )
        }
    }
}
