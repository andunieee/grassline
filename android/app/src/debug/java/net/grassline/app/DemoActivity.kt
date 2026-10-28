package net.grassline.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier

/** The app's screens with made-up data, for screenshots; debug builds only. */
class DemoActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val state = demoState()
        val screen = intent.getStringExtra("screen") ?: "sessions"
        setContent {
            MaterialTheme {
                Surface(Modifier.fillMaxSize()) {
                    when (screen) {
                        "pair" -> PairScreen(AppState(status = "unpaired"), null) {}
                        "chat" -> ChatScreen(state, state.sessions[0]) {}
                        "questions" -> ChatScreen(state, state.sessions[1]) {}
                        else -> SessionsScreen(state, {}, { _, _ -> })
                    }
                }
            }
        }
    }
}

private fun demoState(): AppState {
    val now = System.currentTimeMillis()
    val min = 60_000L
    fun tool(id: String, name: String, summary: String, input: String, status: String = "done", ask: Ask? = null) =
        ChatMessage("tool", summary, ToolCall(id, name, input, status, null, ask))
    val agents = listOf(Agent("claude", true), Agent("opencode", true), Agent("codex", true))
    val sessions = listOf(
        SessionInfo("s1", "weather-api", "claude", "the forecast endpoint fails for cities with accents", now - 1 * min, running = true, waiting = true),
        SessionInfo("s2", "garden-blog", "opencode", "add an RSS feed", now - 4 * min, running = true, waiting = true),
        SessionInfo("s3", "dotfiles", "codex", "make the fish prompt show the git branch", now - 35 * min, running = true, waiting = false),
        SessionInfo("s4", "weather-api", "claude", "write tests for the rate limiter", now - 3 * 60 * min, running = false, waiting = false),
        SessionInfo("s5", "invoice-cli", "claude", "export to CSV", now - 26 * 60 * min, running = false, waiting = false),
        SessionInfo("s6", "garden-blog", "opencode", "dark mode", now - 3 * 24 * 60 * min, running = false, waiting = false),
    )
    val chat = listOf(
        ChatMessage("user", "/forecast returns 500 for São Paulo and Zürich, can you fix it?"),
        tool("t1", "Grep", "Grep city_slug", """{"pattern": "city_slug"}"""),
        tool("t2", "Read", "Read src/cities.rs", """{"file_path": "src/cities.rs"}"""),
        ChatMessage(
            "assistant",
            "Found it: `city_slug` slices the name by **bytes**, so `ã` and `ü` get cut in half and the lookup panics. " +
                "I'll normalize the name and slice by characters instead.",
        ),
        tool("t3", "Edit", "Edit src/cities.rs", """{"file_path": "src/cities.rs"}"""),
        tool(
            "t4", "Bash", "Bash cargo test forecast",
            """{
  "command": "cargo test forecast",
  "description": "Run the forecast tests"
}""",
            status = "waiting", ask = Ask("r1", emptyList()),
        ),
    )
    val questions = listOf(
        ChatMessage("user", "add an RSS feed"),
        tool("u1", "Glob", "Glob content/posts/*.md", """{"pattern": "content/posts/*.md"}"""),
        ChatMessage("assistant", "There are 48 posts. Before I write the feed:"),
        tool(
            "u2", "question", "question", "{}", status = "waiting",
            ask = Ask(
                "r2",
                listOf(
                    Question(
                        "How much of each post should the feed carry?", "Content",
                        listOf(
                            Choice("Full text", "Readers never leave their feed reader."),
                            Choice("Summary", "The first paragraph and a link."),
                        ),
                        multi = false,
                    ),
                    Question(
                        "Which posts?", "Posts",
                        listOf(Choice("Latest 20", ""), Choice("All 48", ""), Choice("Skip drafts", "")),
                        multi = true,
                    ),
                ),
            ),
        ),
    )
    return AppState(
        status = "connected",
        projects = listOf(Project("weather-api", "/home/me/code/weather-api"), Project("garden-blog", "/home/me/code/garden-blog")),
        agents = agents,
        defaultAgent = "claude",
        sessions = sessions,
        transcription = Transcription("", "", ""),
        messages = mapOf("s1" to chat, "s2" to questions),
    )
}
