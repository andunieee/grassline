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
    /** The agent waits for a decision. */
    val waiting: Boolean,
)

data class Choice(val label: String, val description: String)

data class Question(val question: String, val header: String, val options: List<Choice>, val multi: Boolean)

/** A decision the agent waits for; no questions means it asks permission to run the tool. */
data class Ask(val request: String, val questions: List<Question>)

data class ToolCall(
    val id: String,
    val name: String,
    /** The arguments, as pretty JSON. */
    val input: String,
    /** running, waiting, done, failed or denied. */
    val status: String,
    val output: String?,
    val ask: Ask?,
)

data class ChatMessage(val role: String, val text: String, val tool: ToolCall? = null)

private fun <T> JSONArray?.map(f: (JSONObject) -> T): List<T> =
    if (this == null) emptyList() else (0 until length()).map { f(getJSONObject(it)) }

private fun JSONObject.optStringOrNull(k: String): String? = if (isNull(k)) null else optString(k)

private fun parseTool(t: JSONObject) = ToolCall(
    id = t.optString("id"),
    name = t.optString("name"),
    input = t.optString("input"),
    status = t.optString("status"),
    output = t.optStringOrNull("output"),
    ask = t.optJSONObject("ask")?.let { a ->
        Ask(
            a.getString("request"),
            a.optJSONArray("questions").map { q ->
                Question(
                    q.optString("question"), q.optString("header"),
                    q.optJSONArray("options").map { Choice(it.optString("label"), it.optString("description")) },
                    q.optBoolean("multi"),
                )
            },
        )
    },
)

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
            val msgs = o.getJSONObject("messages")
            return AppState(
                status = o.getString("state"),
                reason = o.optString("reason").takeIf { !o.isNull("reason") && it.isNotEmpty() },
                projects = o.getJSONArray("projects").map { Project(it.getString("name"), it.getString("path")) },
                sessions = o.getJSONArray("sessions").map {
                    SessionInfo(
                        it.getString("id"), it.getString("project"), it.getString("provider"),
                        it.getString("title"), it.getLong("updated"), it.getBoolean("running"),
                        it.optBoolean("waiting"),
                    )
                },
                messages = msgs.keys().asSequence().associateWith { k ->
                    msgs.getJSONArray(k).map {
                        ChatMessage(it.getString("role"), it.getString("text"), it.optJSONObject("tool")?.let(::parseTool))
                    }
                },
                error = if (o.isNull("error")) null else o.getString("error"),
            )
        }
    }
}
