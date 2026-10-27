package net.grassline.app

import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        RustBridge.init(filesDir.absolutePath)
        setContent { MaterialTheme { Surface(Modifier.fillMaxSize()) { App() } } }
    }
}

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
fun App() {
    val state by rememberCoreState()
    if (state.status == "unpaired" || state.status == "refused") {
        PairScreen(state)
        return
    }
    var current by remember { mutableStateOf<String?>(null) }
    // A session we just asked for opens once the daemon announces it.
    var waitingFor by remember { mutableStateOf<Set<String>?>(null) }
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
            onCreate = { project ->
                waitingFor = state.sessions.map { it.id }.toSet()
                RustBridge.create(project.path)
            },
        )
    }
}

@Composable
fun PairScreen(state: AppState) {
    var ticket by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }
    Column(Modifier.fillMaxSize().padding(24.dp).systemBarsPadding(), verticalArrangement = Arrangement.Center) {
        Text("Grassline", style = MaterialTheme.typography.headlineLarge)
        Spacer(Modifier.height(8.dp))
        Text("Open the daemon's admin interface, go to Remote control, press “New pairing code” and paste the code here.")
        Spacer(Modifier.height(16.dp))
        OutlinedTextField(ticket, { ticket = it; error = null }, Modifier.fillMaxWidth(), label = { Text("Pairing code") }, singleLine = true)
        val shown = error ?: state.reason?.takeIf { state.status == "refused" }
        if (shown != null) Text(shown, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(top = 8.dp))
        Spacer(Modifier.height(16.dp))
        Button(
            onClick = { error = RustBridge.pair(ticket.trim(), Build.MODEL ?: "Android") },
            enabled = ticket.isNotBlank(),
        ) { Text("Pair") }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionsScreen(state: AppState, onOpen: (SessionInfo) -> Unit, onCreate: (Project) -> Unit) {
    var picking by remember { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
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
        if (state.sessions.isEmpty()) {
            Box(Modifier.fillMaxSize().padding(pad), contentAlignment = Alignment.Center) {
                Text("No sessions yet.")
            }
        } else {
            LazyColumn(Modifier.fillMaxSize().padding(pad)) {
                items(state.sessions, key = { it.id }) { s ->
                    ListItem(
                        headlineContent = { Text(s.project) },
                        supportingContent = {
                            Text(s.title.ifEmpty { "(empty)" }, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        },
                        overlineContent = { Text("${s.provider} · ${ago(s.updated)}") },
                        trailingContent = if (s.running) ({ CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp) }) else null,
                        modifier = Modifier.clickable { onOpen(s) },
                    )
                    HorizontalDivider()
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
                    LazyColumn {
                        items(state.projects, key = { it.path }) { p ->
                            ListItem(
                                headlineContent = { Text(p.name) },
                                modifier = Modifier.clickable { picking = false; onCreate(p) },
                            )
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
    val list = rememberLazyListState()
    LaunchedEffect(messages.size, messages.lastOrNull()?.text) {
        if (messages.isNotEmpty()) list.animateScrollToItem(messages.size - 1)
    }
    Scaffold(
        modifier = Modifier.imePadding(),
        topBar = {
            TopAppBar(
                title = { Text(session.project) },
                navigationIcon = { TextButton(onBack) { Text("←") } },
            )
        },
        bottomBar = {
            Column(Modifier.navigationBarsPadding()) {
                state.error?.let {
                    Row(Modifier.fillMaxWidth().clickable { RustBridge.dismissError() }.padding(horizontal = 16.dp, vertical = 4.dp)) {
                        Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                    }
                }
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
                            enabled = draft.isNotBlank(),
                        ) { Text("Send") }
                    }
                }
            }
        },
    ) { pad ->
        LazyColumn(Modifier.fillMaxSize().padding(pad), state = list, contentPadding = PaddingValues(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            items(messages.size) { Bubble(messages[it]) }
            if (session.running) item { CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp) }
        }
    }
}

@Composable
fun Bubble(m: ChatMessage) {
    when (m.role) {
        "user" -> Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
            Surface(shape = RoundedCornerShape(16.dp), color = MaterialTheme.colorScheme.primaryContainer, modifier = Modifier.widthIn(max = 320.dp)) {
                Text(m.text, Modifier.padding(12.dp))
            }
        }
        "tool" -> Text(m.text, style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace, color = MaterialTheme.colorScheme.outline, maxLines = 2, overflow = TextOverflow.Ellipsis)
        "error" -> Text(m.text, color = MaterialTheme.colorScheme.error)
        else -> Text(m.text)
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
