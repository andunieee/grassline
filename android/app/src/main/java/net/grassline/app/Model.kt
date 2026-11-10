package net.grassline.app

import org.json.JSONArray
import org.json.JSONObject

data class Project(val name: String, val path: String)

/** A daemon this phone is paired with; [name] is empty until it says what it calls itself. */
data class Daemon(val endpoint: String, val name: String) {
    val label get() = name.ifEmpty { endpoint.take(8) }
}

/** An agent the daemon can run; [installed] when its binary was found there, [steers] when it takes queued messages while it works. */
data class Agent(val name: String, val installed: Boolean, val steers: Boolean = false)

/** A model an agent can use; an empty [id] is the agent's default. */
data class AgentModel(val id: String, val name: String, val description: String)

/** An agent's models, once asked for with [RustBridge.models]. */
data class ModelList(val items: List<AgentModel>, val error: String?, val loading: Boolean)

/** A message waiting for the agent's turn to end; [steering] once the agent was given it but has not taken it in yet. */
data class Queued(val id: String, val text: String, val steering: Boolean)

data class SessionInfo(
    val id: String,
    val project: String,
    val provider: String,
    val title: String,
    val updated: Long,
    val running: Boolean,
    /** The agent waits for a decision. */
    val waiting: Boolean,
    /** An [AgentModel.id]; empty for the agent's default. */
    val model: String = "",
    /** ask, auto or all. */
    val permissions: String = "auto",
    /** Messages sent while the agent works, in order. */
    val queue: List<Queued> = emptyList(),
    /** The start of the agent's last message. */
    val preview: String = "",
    /** The index of the agent's first unread message; opening the session reads it. */
    val unread: Int? = null,
)

/** The speech-to-text service the daemon set up for voice messages. */
data class Transcription(
    /** "remote" (an OpenAI-style endpoint called by the app) or "local" (a daemon model). */
    val provider: String,
    val url: String,
    val model: String,
    val key: String,
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

private fun parseSession(it: JSONObject) = SessionInfo(
    it.getString("id"), it.getString("project"), it.getString("provider"),
    it.getString("title"), it.getLong("updated"), it.getBoolean("running"),
    it.optBoolean("waiting"), it.optString("model"), it.optString("permissions", "auto"),
    it.optJSONArray("queue").map { q -> Queued(q.getString("id"), q.getString("text"), q.optBoolean("steering")) },
    it.optString("preview"),
    if (it.isNull("unread")) null else it.optInt("unread"),
)

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
    /** unpaired, connecting, connected, offline, refused or outdated. */
    val status: String = "connecting",
    /** Why it is offline or refused; when outdated, the daemon's protocol version. */
    val reason: String? = null,
    val projects: List<Project> = emptyList(),
    /** The daemon's folders whose subfolders are projects; new projects go in one of them. */
    val roots: List<String> = emptyList(),
    val agents: List<Agent> = emptyList(),
    /** The agent new sessions use by default. */
    val defaultAgent: String = "",
    /** By agent; only the ones asked for. */
    val models: Map<String, ModelList> = emptyMap(),
    /** Most recent first; not the archived ones. */
    val sessions: List<SessionInfo> = emptyList(),
    /** The archived sessions, most recent first; null until asked for with [RustBridge.archived]. */
    val archived: List<SessionInfo>? = null,
    /** Null when voice messages are not set up. */
    val transcription: Transcription? = null,
    val messages: Map<String, List<ChatMessage>> = emptyMap(),
    val error: String? = null,
    /** The paired daemons. */
    val daemons: List<Daemon> = emptyList(),
    /** The [Daemon.endpoint] of the one the app is on. */
    val daemon: String? = null,
    /** The connection is a pairing the daemon has not let in yet; once it does, the app is on that daemon. */
    val pairing: Boolean = false,
    /** This build's version, with its git commit. */
    val version: String = "",
    /** The version of the wire protocol this app speaks. */
    val wireVersion: Int = 0,
    /** This phone's pairing key. */
    val key: String = "",
) {
    companion object {
        fun parse(json: String): AppState {
            val o = JSONObject(json)
            val msgs = o.getJSONObject("messages")
            return AppState(
                status = o.getString("state"),
                reason = o.optString("reason").takeIf { !o.isNull("reason") && it.isNotEmpty() },
                projects = o.getJSONArray("projects").map { Project(it.getString("name"), it.getString("path")) },
                roots = o.optJSONArray("roots")?.let { a -> List(a.length()) { a.getString(it) } }.orEmpty(),
                agents = o.optJSONArray("agents").map { Agent(it.getString("name"), it.optBoolean("installed"), it.optBoolean("steers")) },
                defaultAgent = o.optString("default_agent"),
                models = o.optJSONObject("models")?.let { m ->
                    m.keys().asSequence().associateWith { k ->
                        val l = m.getJSONObject(k)
                        ModelList(
                            l.optJSONArray("items").map { AgentModel(it.getString("id"), it.getString("name"), it.optString("description")) },
                            l.optStringOrNull("error"),
                            l.optBoolean("loading"),
                        )
                    }
                } ?: emptyMap(),
                sessions = o.getJSONArray("sessions").map(::parseSession),
                archived = o.optJSONArray("archived")?.map(::parseSession),
                transcription = o.optJSONObject("transcription")?.let {
                    Transcription(
                        it.optString("provider", "remote"),
                        it.optString("url"),
                        it.getString("model"),
                        it.optString("key"),
                    )
                },
                messages = msgs.keys().asSequence().associateWith { k ->
                    msgs.getJSONArray(k).map {
                        ChatMessage(it.getString("role"), it.getString("text"), it.optJSONObject("tool")?.let(::parseTool))
                    }
                },
                error = if (o.isNull("error")) null else o.getString("error"),
                daemons = o.optJSONArray("daemons").map { Daemon(it.getString("endpoint"), it.optString("name")) },
                daemon = o.optStringOrNull("daemon"),
                pairing = o.optBoolean("pairing"),
                version = o.optString("version"),
                wireVersion = o.optInt("wire_version"),
                key = o.optString("key"),
            )
        }
    }
}
