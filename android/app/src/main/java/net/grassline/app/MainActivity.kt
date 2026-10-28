package net.grassline.app

import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.mikepenz.markdown.m3.Markdown
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext
import org.json.JSONObject

class MainActivity : ComponentActivity() {
    /** A pairing code the app was opened with (a scanned `grassline:` link). */
    private val link = mutableStateOf<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        RustBridge.init(filesDir.absolutePath)
        if (savedInstanceState == null) link.value = pairingLink(intent)
        setContent { MaterialTheme { Surface(Modifier.fillMaxSize()) { App(link.value) { link.value = null } } } }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        pairingLink(intent)?.let { link.value = it }
    }

    private fun pairingLink(intent: Intent?): String? =
        intent?.takeIf { it.action == Intent.ACTION_VIEW }?.dataString?.takeIf { it.startsWith("grassline:") }
}

fun deviceName(): String = Build.MODEL ?: "Android"

/** The state of the core, updated whenever it changes. */
@Composable
fun rememberCoreState(): State<AppState> {
    val state = remember { mutableStateOf(AppState()) }
    LaunchedEffect(Unit) {
        withContext(Dispatchers.IO) {
            var seen = -1L
            while (isActive) {
                val next = RustBridge.await(seen, 30_000)
                if (next != seen) {
                    seen = next
                    state.value = AppState.parse(RustBridge.snapshot())
                }
            }
        }
    }
    return state
}

@Composable
fun App(link: String?, onLinkUsed: () -> Unit) {
    val state by rememberCoreState()
    if (state.status == "unpaired" || state.status == "refused") {
        PairScreen(state, link, onLinkUsed)
        return
    }
    if (link != null) {
        AlertDialog(
            onDismissRequest = onLinkUsed,
            title = { Text("Pair with another daemon?") },
            text = { Text("This pairing code is for a daemon; this phone forgets the one it is paired with.") },
            confirmButton = { TextButton({ onLinkUsed(); RustBridge.pair(link, deviceName()) }) { Text("Pair") } },
            dismissButton = { TextButton(onLinkUsed) { Text("Cancel") } },
        )
    }
    var current by remember { mutableStateOf<String?>(null) }
    // A session we just asked for opens once the daemon announces it.
    var waitingFor by remember { mutableStateOf<Set<String>?>(null) }
    // Unless the daemon could not start it.
    LaunchedEffect(state.error) { if (state.error != null) waitingFor = null }
    LaunchedEffect(state.sessions, waitingFor) {
        val known = waitingFor ?: return@LaunchedEffect
        state.sessions.firstOrNull { it.id !in known }?.let {
            waitingFor = null
            RustBridge.open(it.id)
            current = it.id
        }
    }
    val session = state.sessions.firstOrNull { it.id == current }
    if (session != null) {
        BackHandler { current = null }
        ChatScreen(state, session, onBack = { current = null })
    } else {
        SessionsScreen(
            state,
            onOpen = { RustBridge.open(it.id); current = it.id },
            onCreate = { project, agent ->
                waitingFor = state.sessions.map { it.id }.toSet()
                RustBridge.create(project.path, agent)
            },
        )
    }
}

@Composable
fun PairScreen(state: AppState, link: String?, onLinkUsed: () -> Unit) {
    var ticket by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }
    var ownScanner by remember { mutableStateOf(false) }
    fun pair(code: String) {
        ticket = code.trim()
        error = RustBridge.pair(ticket, deviceName())
    }
    // Opened from a link: the code is filled in, pairing is up to the user.
    LaunchedEffect(link) {
        if (link != null) {
            ticket = link
            error = null
            onLinkUsed()
        }
    }
    val scan = rememberQrScan(onFallback = { ownScanner = true }, onResult = ::pair)
    if (ownScanner) {
        BackHandler { ownScanner = false }
        QrScannerScreen(onResult = { ownScanner = false; pair(it) }, onCancel = { ownScanner = false })
        return
    }
    Column(Modifier.fillMaxSize().padding(24.dp).systemBarsPadding(), verticalArrangement = Arrangement.Center) {
        Text("Grassline", style = MaterialTheme.typography.headlineLarge)
        Spacer(Modifier.height(8.dp))
        Text("Open the daemon's admin interface, go to Access, press “New pairing code”, then scan it or paste the code here.")
        Spacer(Modifier.height(16.dp))
        Button(onClick = scan, Modifier.fillMaxWidth()) { Text("Scan QR code") }
        Spacer(Modifier.height(16.dp))
        OutlinedTextField(ticket, { ticket = it; error = null }, Modifier.fillMaxWidth(), label = { Text("Pairing code") }, singleLine = true)
        val shown = error ?: state.reason?.takeIf { state.status == "refused" }
        if (shown != null) Text(shown, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(top = 8.dp))
        Spacer(Modifier.height(16.dp))
        OutlinedButton(
            onClick = { pair(ticket) },
            enabled = ticket.isNotBlank(),
        ) { Text("Pair") }
    }
}

/** The last error the daemon reported; tapping it dismisses it. */
@Composable
fun ErrorLine(state: AppState) {
    state.error?.let {
        Row(Modifier.fillMaxWidth().clickable { RustBridge.dismissError() }.padding(horizontal = 16.dp, vertical = 4.dp)) {
            Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionsScreen(state: AppState, onOpen: (SessionInfo) -> Unit, onCreate: (Project, String) -> Unit) {
    var picking by remember { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
    var agentMenu by remember { mutableStateOf(false) }
    // The agent the next session gets; the default until another is picked.
    var agent by remember(picking, state.defaultAgent) { mutableStateOf(state.defaultAgent) }
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("Sessions")
                        if (state.status != "connected") {
                            Text(
                                if (state.status == "offline") "offline: ${state.reason ?: ""}" else "connecting…",
                                style = MaterialTheme.typography.labelSmall,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                },
                actions = {
                    if (state.agents.isNotEmpty()) {
                        Box {
                            TextButton({ agentMenu = true }) { Text(state.defaultAgent.ifEmpty { "agent" } + " ▾") }
                            AgentMenu(agentMenu, state.agents, state.defaultAgent, { agentMenu = false }) {
                                agentMenu = false
                                RustBridge.setDefaultAgent(it)
                            }
                        }
                    }
                    TextButton({ menu = true }) { Text("⋮") }
                    DropdownMenu(menu, { menu = false }) {
                        DropdownMenuItem(text = { Text("Refresh") }, onClick = { menu = false; RustBridge.refresh() })
                        DropdownMenuItem(text = { Text("Unpair") }, onClick = { menu = false; RustBridge.unpair() })
                    }
                },
            )
        },
        floatingActionButton = {
            ExtendedFloatingActionButton(onClick = { picking = true }) { Text("New session") }
        },
    ) { pad ->
        Column(Modifier.fillMaxSize().padding(pad)) {
            ErrorLine(state)
            if (state.sessions.isEmpty()) {
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Text("No sessions yet.")
                }
            } else {
                LazyColumn(Modifier.fillMaxSize()) {
                    items(state.sessions, key = { it.id }) { s ->
                        ListItem(
                            headlineContent = { Text(s.project) },
                            supportingContent = {
                                Text(s.title.ifEmpty { "(empty)" }, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            },
                            overlineContent = { Text("${s.provider} · ${ago(s.updated)}") },
                            trailingContent = when {
                                s.waiting -> ({ Badge(containerColor = MaterialTheme.colorScheme.tertiary) { Text("needs you", Modifier.padding(horizontal = 4.dp)) } })
                                s.running -> ({ CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp) })
                                else -> null
                            },
                            modifier = Modifier.clickable { onOpen(s) },
                        )
                        HorizontalDivider()
                    }
                }
            }
        }
    }
    if (picking) {
        AlertDialog(
            onDismissRequest = { picking = false },
            confirmButton = { TextButton({ picking = false }) { Text("Cancel") } },
            title = { Text("Project") },
            text = {
                if (state.projects.isEmpty()) {
                    Text("The daemon has no projects. Add a project directory in its admin interface.")
                } else {
                    Column {
                        AgentChips(state.agents, agent) { agent = it }
                        LazyColumn {
                            items(state.projects, key = { it.path }) { p ->
                                ListItem(
                                    headlineContent = { Text(p.name) },
                                    modifier = Modifier.clickable { picking = false; onCreate(p, agent) },
                                )
                            }
                        }
                    }
                }
            },
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatScreen(state: AppState, session: SessionInfo, onBack: () -> Unit) {
    val messages = state.messages[session.id].orEmpty()
    var draft by remember { mutableStateOf("") }
    var agentMenu by remember { mutableStateOf(false) }
    // The agent to switch to, once the user confirms.
    var switchTo by remember { mutableStateOf<String?>(null) }
    val list = rememberLazyListState()
    LaunchedEffect(messages.size, messages.lastOrNull()) {
        if (messages.isNotEmpty()) list.animateScrollToItem(messages.size - 1)
    }
    Scaffold(
        modifier = Modifier.imePadding(),
        topBar = {
            TopAppBar(
                title = { Text(session.project) },
                navigationIcon = { TextButton(onBack) { Text("←") } },
                actions = {
                    Box {
                        TextButton({ agentMenu = true }, enabled = !session.running && state.agents.isNotEmpty()) {
                            Text(session.provider + " ▾")
                        }
                        AgentMenu(agentMenu, state.agents, session.provider, { agentMenu = false }) {
                            agentMenu = false
                            if (it != session.provider) switchTo = it
                        }
                    }
                },
            )
        },
        bottomBar = {
            Column(Modifier.navigationBarsPadding()) {
                ErrorLine(state)
                Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.Bottom) {
                    OutlinedTextField(
                        draft, { draft = it }, Modifier.weight(1f),
                        placeholder = { Text("Message") }, maxLines = 5,
                    )
                    Spacer(Modifier.width(8.dp))
                    if (session.running) {
                        Button({ RustBridge.stop(session.id) }, colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error)) { Text("Stop") }
                    } else {
                        Button(
                            onClick = { RustBridge.send(session.id, draft); draft = "" },
                            enabled = draft.isNotBlank() && state.status == "connected",
                        ) { Text("Send") }
                    }
                }
            }
        },
    ) { pad ->
        LazyColumn(Modifier.fillMaxSize().padding(pad), state = list, contentPadding = PaddingValues(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            items(messages.size) { Bubble(session.id, messages[it]) }
            if (session.running && !session.waiting) item { CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp) }
        }
    }

    switchTo?.let { to ->
        AlertDialog(
            onDismissRequest = { switchTo = null },
            title = { Text("Switch to $to?") },
            text = { Text("$to starts a new conversation: it does not see the messages so far, which stay here.") },
            confirmButton = { TextButton({ switchTo = null; RustBridge.setAgent(session.id, to) }) { Text("Switch") } },
            dismissButton = { TextButton({ switchTo = null }) { Text("Cancel") } },
        )
    }
}

/** The agents to pick from; the ones the daemon cannot find are greyed out. */
@Composable
fun AgentMenu(open: Boolean, agents: List<Agent>, current: String, onDismiss: () -> Unit, onPick: (String) -> Unit) {
    DropdownMenu(open, onDismiss) {
        for (a in agents) {
            DropdownMenuItem(
                text = { Text(if (a.installed) a.name else "${a.name} (not installed)") },
                leadingIcon = { Text(if (a.name == current) "✓" else "") },
                enabled = a.installed,
                onClick = { onPick(a.name) },
            )
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun AgentChips(agents: List<Agent>, picked: String, onPick: (String) -> Unit) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        for (a in agents) {
            FilterChip(selected = a.name == picked, onClick = { onPick(a.name) }, label = { Text(a.name) }, enabled = a.installed)
        }
    }
}

@Composable
fun Bubble(sessionId: String, m: ChatMessage) {
    when (m.role) {
        "user" -> Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
            Surface(shape = RoundedCornerShape(16.dp), color = MaterialTheme.colorScheme.primaryContainer, modifier = Modifier.widthIn(max = 320.dp)) {
                Text(m.text, Modifier.padding(12.dp))
            }
        }
        "tool" -> {
            val t = m.tool
            when {
                t == null -> Text(m.text, style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace, color = MaterialTheme.colorScheme.outline, maxLines = 2, overflow = TextOverflow.Ellipsis)
                t.ask != null && t.ask.questions.isNotEmpty() -> QuestionsCard(sessionId, t.ask)
                t.ask != null -> PermissionCard(sessionId, m.text, t, t.ask)
                else -> ToolLine(m.text, t)
            }
        }
        "error" -> Text(m.text, color = MaterialTheme.colorScheme.error)
        else -> Markdown(m.text)
    }
}

/** A tool call in one line; tapping it shows its input and output. */
@Composable
fun ToolLine(summary: String, t: ToolCall) {
    var open by remember { mutableStateOf(false) }
    val mono = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace)
    Column(Modifier.fillMaxWidth().clickable { open = !open }) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            when (t.status) {
                "running" -> CircularProgressIndicator(Modifier.size(10.dp), strokeWidth = 1.5.dp)
                else -> Text(
                    when (t.status) { "done" -> "✓"; "denied" -> "⊘"; else -> "✗" },
                    style = mono,
                    color = if (t.status == "done") MaterialTheme.colorScheme.outline else MaterialTheme.colorScheme.error,
                )
            }
            Spacer(Modifier.width(6.dp))
            Text(summary, style = mono, color = MaterialTheme.colorScheme.outline, maxLines = if (open) 10 else 1, overflow = TextOverflow.Ellipsis)
        }
        if (open) {
            Code(t.input)
            t.output?.takeIf { it.isNotBlank() }?.let { Code(it) }
        }
    }
}

@Composable
fun Code(text: String, maxLines: Int = 30) {
    Surface(shape = RoundedCornerShape(8.dp), color = MaterialTheme.colorScheme.surfaceVariant, modifier = Modifier.fillMaxWidth().padding(top = 4.dp)) {
        Text(
            text, Modifier.horizontalScroll(rememberScrollState()).padding(8.dp),
            style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace,
            maxLines = maxLines, overflow = TextOverflow.Ellipsis, softWrap = false,
        )
    }
}

fun decide(sessionId: String, request: String, allow: Boolean, always: Boolean = false, answers: Map<String, String> = emptyMap(), message: String? = null) {
    val d = JSONObject()
        .put("allow", allow)
        .put("always", always)
        .put("answers", JSONObject(answers))
        .put("message", message ?: JSONObject.NULL)
    RustBridge.decide(sessionId, request, d.toString())
}

/** The agent asks to run a tool. */
@Composable
fun PermissionCard(sessionId: String, summary: String, t: ToolCall, ask: Ask) {
    var note by remember(ask.request) { mutableStateOf("") }
    // A plan reads better as text than as JSON.
    val plan = if (t.name == "ExitPlanMode") runCatching { JSONObject(t.input).optString("plan") }.getOrNull()?.takeIf { it.isNotBlank() } else null
    ElevatedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            Text(if (plan != null) "Approve the plan?" else "Allow ${t.name}?", style = MaterialTheme.typography.titleSmall)
            if (plan != null) {
                Markdown(plan, modifier = Modifier.padding(top = 4.dp))
            } else {
                Text(summary, style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace, color = MaterialTheme.colorScheme.outline)
                Code(t.input, maxLines = 12)
            }
            OutlinedTextField(
                note, { note = it }, Modifier.fillMaxWidth().padding(top = 8.dp),
                placeholder = { Text("Why not (optional)") }, maxLines = 3, textStyle = MaterialTheme.typography.bodySmall,
            )
            Row(Modifier.fillMaxWidth().padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End)) {
                TextButton({ decide(sessionId, ask.request, false, message = note.ifBlank { null }) }) { Text("Deny") }
                if (plan == null) OutlinedButton({ decide(sessionId, ask.request, true, always = true) }) { Text("Always") }
                Button({ decide(sessionId, ask.request, true) }, enabled = note.isBlank()) { Text("Allow") }
            }
        }
    }
}

/** The agent asks questions with options to pick. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun QuestionsCard(sessionId: String, ask: Ask) {
    val picked = remember(ask.request) { mutableStateMapOf<String, Set<String>>() }
    val other = remember(ask.request) { mutableStateMapOf<String, String>() }
    fun answer(q: Question): String? =
        other[q.question]?.takeIf { it.isNotBlank() }
            ?: picked[q.question]?.takeIf { it.isNotEmpty() }?.let { set -> q.options.map { it.label }.filter { it in set }.joinToString(", ") }
    ElevatedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            for (q in ask.questions) {
                Column {
                    if (q.header.isNotBlank()) Text(q.header, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
                    Text(q.question, style = MaterialTheme.typography.titleSmall)
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        for (o in q.options) {
                            val on = o.label in picked[q.question].orEmpty()
                            FilterChip(
                                selected = on,
                                onClick = {
                                    val cur = picked[q.question].orEmpty()
                                    picked[q.question] = when {
                                        q.multi && on -> cur - o.label
                                        q.multi -> cur + o.label
                                        else -> setOf(o.label)
                                    }
                                    other.remove(q.question)
                                },
                                label = { Text(o.label) },
                            )
                        }
                    }
                    q.options.firstOrNull { it.label in picked[q.question].orEmpty() && it.description.isNotBlank() }?.let {
                        Text(it.description, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline)
                    }
                    OutlinedTextField(
                        other[q.question].orEmpty(),
                        { other[q.question] = it; if (it.isNotBlank()) picked.remove(q.question) },
                        Modifier.fillMaxWidth(), placeholder = { Text("Something else") }, maxLines = 3,
                        textStyle = MaterialTheme.typography.bodySmall,
                    )
                }
            }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End)) {
                TextButton({ decide(sessionId, ask.request, false, message = "I'd rather not answer.") }) { Text("Skip") }
                Button(
                    onClick = { decide(sessionId, ask.request, true, answers = ask.questions.associate { it.question to answer(it)!! }) },
                    enabled = ask.questions.all { answer(it) != null },
                ) { Text("Answer") }
            }
        }
    }
}

fun ago(ms: Long): String {
    val s = (System.currentTimeMillis() - ms) / 1000
    return when {
        s < 60 -> "just now"
        s < 3600 -> "${s / 60}m ago"
        s < 86400 -> "${s / 3600}h ago"
        else -> "${s / 86400}d ago"
    }
}
