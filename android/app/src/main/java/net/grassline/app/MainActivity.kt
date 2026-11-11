package net.grassline.app

import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.mikepenz.markdown.m3.Markdown
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
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
        val theme = ThemeSetting(this)
        setContent { GrasslineTheme(theme) { Surface(Modifier.fillMaxSize()) { App(link.value) { link.value = null } } } }
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
    // The About screen, reachable from the pairing screen and the sessions list.
    var about by remember { mutableStateOf(false) }
    if (about) {
        BackHandler { about = false }
        AboutScreen(state, onBack = { about = false })
        return
    }
    if (state.status == "outdated") {
        OutdatedScreen(state.reason)
        return
    }
    // Pairing another daemon, until the code is sent.
    var adding by remember { mutableStateOf(false) }
    LaunchedEffect(link) { if (link != null) adding = true }
    // Then until the daemon lets us in.
    LaunchedEffect(state.pairing) { if (state.pairing) adding = false }
    if (state.status == "unpaired" || state.pairing || adding) {
        // Back to the daemon the app was on.
        val back = state.daemon?.let { previous -> { adding = false; if (state.pairing) RustBridge.switchTo(previous) } }
        PairScreen(state, link, back, onLinkUsed, onAbout = { about = true })
        return
    }
    var current by remember { mutableStateOf<String?>(null) }
    // The archive is showing, under the open session if there is one.
    var archive by remember { mutableStateOf(false) }
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
    val session = (state.sessions + state.archived.orEmpty()).firstOrNull { it.id == current }
    val open = { s: SessionInfo -> RustBridge.open(s.id); current = s.id }
    if (session != null) {
        val leave = {
            // A voice message that is still uploading is not a message yet:
            // closing an empty session would delete it before it is sent, so it closes after.
            val unsent = voiceBusy(session.id) && state.messages[session.id].isNullOrEmpty() && session.queue.isEmpty()
            if (unsent) closeAfterVoice(session.id) else RustBridge.close(session.id)
            current = null
        }
        BackHandler(onBack = leave)
        // A fresh screen for each session, so what it remembers is that session's.
        key(session.id) { ChatScreen(state, session, onBack = leave) }
    } else if (archive) {
        BackHandler { archive = false }
        ArchiveScreen(state, onOpen = open, onBack = { archive = false })
    } else {
        SessionsScreen(
            state,
            onOpen = open,
            onArchive = { archive = true },
            onAbout = { about = true },
            onAddDaemon = { adding = true },
            onCreate = { project, agent ->
                waitingFor = state.sessions.map { it.id }.toSet()
                RustBridge.create(project.path, agent)
            },
            onCreateProject = { root, name, agent ->
                waitingFor = state.sessions.map { it.id }.toSet()
                RustBridge.createProject(root, name, agent)
            },
        )
    }
}

@Composable
fun PairScreen(state: AppState, link: String?, onBack: (() -> Unit)?, onLinkUsed: () -> Unit, onAbout: () -> Unit) {
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
    if (onBack != null) BackHandler(onBack = onBack)
    if (ownScanner) {
        BackHandler { ownScanner = false }
        QrScannerScreen(onResult = { ownScanner = false; pair(it) }, onCancel = { ownScanner = false })
        return
    }
    Box(Modifier.fillMaxSize()) {
        PairForm(state, ticket, error, scan, { ticket = it; error = null }) { pair(ticket) }
        if (onBack != null) {
            IconButton(onBack, Modifier.systemBarsPadding().padding(4.dp)) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
        }
        TextButton(onClick = onAbout, Modifier.align(Alignment.TopEnd).systemBarsPadding().padding(4.dp)) { Text("About") }
    }
}

@Composable
private fun PairForm(state: AppState, ticket: String, error: String?, onScan: () -> Unit, onTicket: (String) -> Unit, onPair: () -> Unit) {
    Column(Modifier.fillMaxSize().padding(24.dp).systemBarsPadding(), verticalArrangement = Arrangement.Center) {
        Image(painterResource(R.drawable.logo), null, Modifier.size(72.dp))
        Spacer(Modifier.height(16.dp))
        Text("Grassline", style = MaterialTheme.typography.headlineLarge)
        Text("Your coding agents, from anywhere.", style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.outline)
        Spacer(Modifier.height(16.dp))
        Text("Open the daemon's admin interface, go to Access, press “New pairing code”, then scan it or paste the code here.")
        Spacer(Modifier.height(16.dp))
        Button(onClick = onScan, Modifier.fillMaxWidth()) { Text("Scan QR code") }
        Spacer(Modifier.height(16.dp))
        OutlinedTextField(ticket, onTicket, Modifier.fillMaxWidth(), label = { Text("Pairing code") }, singleLine = true)
        when {
            error != null -> Text(error, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(top = 8.dp))
            !state.pairing -> {}
            state.status == "refused" -> Text(state.reason ?: "refused", color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(top = 8.dp))
            else -> Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                Spacer(Modifier.width(8.dp))
                Text(
                    if (state.status == "offline") "Can't reach the daemon yet: ${state.reason ?: ""}" else "Connecting…",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline,
                )
            }
        }
        Spacer(Modifier.height(16.dp))
        OutlinedButton(onClick = onPair, enabled = ticket.isNotBlank()) { Text("Pair") }
    }
}

/** The daemon speaks a newer protocol than this app. */
@Composable
fun OutdatedScreen(version: String?) {
    Column(Modifier.fillMaxSize().padding(24.dp).systemBarsPadding(), verticalArrangement = Arrangement.Center) {
        Image(painterResource(R.drawable.logo), null, Modifier.size(72.dp))
        Spacer(Modifier.height(16.dp))
        Text("Update the app", style = MaterialTheme.typography.headlineLarge)
        Spacer(Modifier.height(16.dp))
        Text("The daemon on your computer is newer than this app (protocol version ${version ?: "?"}), so they can't talk. Install the latest app to connect again.")
    }
}

/** This build and the phone's pairing key. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AboutScreen(state: AppState, onBack: () -> Unit) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("About") },
                navigationIcon = { IconButton(onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
            )
        },
    ) { pad ->
        Column(
            Modifier.fillMaxSize().padding(pad).verticalScroll(rememberScrollState()).padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            Image(painterResource(R.drawable.logo), null, Modifier.size(72.dp))
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text("Grassline", style = MaterialTheme.typography.headlineMedium)
                Text(
                    "Your coding agents, from anywhere.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.outline,
                )
            }
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                AboutValue("Version", state.version)
                AboutValue("Wire version", state.wireVersion.takeIf { it > 0 }?.toString().orEmpty())
                AboutValue("Pairing key", state.key)
            }
            Text(
                "The pairing key identifies this phone to its daemon; the daemon lists it with its paired devices.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline,
            )
        }
    }
}

@Composable
private fun AboutValue(label: String, value: String) {
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(label, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.outline)
        SelectionContainer {
            Text(
                value.ifEmpty { "unknown" },
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
            )
        }
    }
}

/** The last error the daemon reported; it goes away after a while, or when tapped. */
@Composable
fun ErrorLine(state: AppState) {
    state.error?.let {
        LaunchedEffect(it) {
            delay(8_000)
            RustBridge.dismissError()
        }
        Row(Modifier.fillMaxWidth().clickable { RustBridge.dismissError() }.padding(horizontal = 16.dp, vertical = 4.dp)) {
            Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionsScreen(state: AppState, onOpen: (SessionInfo) -> Unit, onArchive: () -> Unit, onAbout: () -> Unit, onAddDaemon: () -> Unit, onCreate: (Project, String) -> Unit, onCreateProject: (root: String, name: String, agent: String) -> Unit = { _, _, _ -> }) {
    var picking by remember { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
    var daemonMenu by remember { mutableStateOf(false) }
    var agentMenu by remember { mutableStateOf(false) }
    // The agent the next session gets; the default until another is picked.
    var agent by remember(picking, state.defaultAgent) { mutableStateOf(state.defaultAgent) }
    val theme = LocalThemeSetting.current
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    // The daemon the app is on; tapping it switches to another or pairs a new one.
                    Box {
                        Column(Modifier.clickable { daemonMenu = true }) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(state.daemons.firstOrNull { it.endpoint == state.daemon }?.label ?: "Sessions", Modifier.weight(1f, fill = false), maxLines = 1, overflow = TextOverflow.Ellipsis)
                                DropArrow()
                            }
                            if (state.status != "connected") {
                                Text(
                                    when (state.status) {
                                        "offline" -> "offline: ${state.reason ?: ""}"
                                        "refused" -> "refused: ${state.reason ?: ""}"
                                        else -> "connecting…"
                                    },
                                    style = MaterialTheme.typography.labelSmall,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                        DropdownMenu(daemonMenu, { daemonMenu = false }) {
                            for (d in state.daemons) {
                                DropdownMenuItem(
                                    text = { Text(d.label) },
                                    leadingIcon = { Tick(d.endpoint == state.daemon) },
                                    onClick = { daemonMenu = false; if (d.endpoint != state.daemon) RustBridge.switchTo(d.endpoint) },
                                )
                            }
                            HorizontalDivider()
                            DropdownMenuItem(
                                text = { Text("Pair a new daemon") },
                                leadingIcon = { Icon(Icons.Filled.Add, null) },
                                onClick = { daemonMenu = false; onAddDaemon() },
                            )
                        }
                    }
                },
                actions = {
                    if (state.agents.isNotEmpty()) {
                        Box {
                            TextButton({ agentMenu = true }) { Text(state.defaultAgent.ifEmpty { "agent" }); DropArrow() }
                            AgentMenu(agentMenu, state.agents, state.defaultAgent, { agentMenu = false }) {
                                agentMenu = false
                                RustBridge.setDefaultAgent(it)
                            }
                        }
                    }
                    IconButton({ menu = true }) { Icon(Icons.Filled.MoreVert, "Menu") }
                    DropdownMenu(menu, { menu = false }) {
                        DropdownMenuItem(text = { Text("Archive") }, onClick = { menu = false; onArchive() })
                        DropdownMenuItem(text = { Text("Refresh") }, onClick = { menu = false; RustBridge.refresh() })
                        DropdownMenuItem(text = { Text("Unpair this daemon") }, onClick = { menu = false; RustBridge.unpair() })
                        DropdownMenuItem(text = { Text("About") }, onClick = { menu = false; onAbout() })
                        HorizontalDivider()
                        for (m in ThemeMode.entries) {
                            DropdownMenuItem(
                                text = { Text(m.label) },
                                leadingIcon = { Tick(m == theme.mode) },
                                onClick = { menu = false; theme.set(m) },
                            )
                        }
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
            if (state.status == "refused") {
                Box(Modifier.fillMaxSize().padding(24.dp), contentAlignment = Alignment.Center) {
                    Text("The daemon does not let this phone in: ${state.reason ?: "refused"}. Pair it again from the menu at the top left.")
                }
            } else if (state.sessions.isEmpty()) {
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Text("No sessions yet.")
                }
            } else {
                SessionList(state.sessions, "Archive", onOpen, { RustBridge.archive(it.id, true) }, { RustBridge.delete(it.id) })
            }
        }
    }
    if (picking) {
        // The new project's name while one is being made.
        var naming by remember { mutableStateOf<String?>(null) }
        var root by remember(state.roots) { mutableStateOf(state.roots.firstOrNull().orEmpty()) }
        val name = naming
        AlertDialog(
            onDismissRequest = { picking = false },
            dismissButton = when {
                name != null -> { { TextButton({ naming = null }) { Text("Back") } } }
                state.roots.isNotEmpty() -> { { TextButton({ naming = "" }) { Text("New project") } } }
                else -> null
            },
            confirmButton = {
                if (name == null) {
                    TextButton({ picking = false }) { Text("Cancel") }
                } else {
                    Button({ picking = false; onCreateProject(root, name.trim(), agent) }, enabled = name.isNotBlank() && root.isNotEmpty()) { Text("Create") }
                }
            },
            title = { Text(if (name == null) "Project" else "New project") },
            text = {
                if (state.projects.isEmpty() && state.roots.isEmpty()) {
                    Text("The daemon has no projects. Add a project directory in its admin interface.")
                } else {
                    var query by remember { mutableStateOf("") }
                    val projects = state.projects.filter { matches(query, "${it.name} ${it.path}") }
                    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                        AgentChips(state.agents, agent) { agent = it }
                        when {
                            name != null -> NewProjectForm(name, { naming = it }, state.roots, root) { root = it }
                            projects.isEmpty() && query.isEmpty() -> Text("No projects yet.", color = MaterialTheme.colorScheme.outline)
                            else -> {
                                // Only worth it when the list is long.
                                if (state.projects.size > 5) FilterField(query) { query = it }
                                PickerList(projects.map { PickerItem(it.path, it.name, it.path) }) { path ->
                                    picking = false
                                    onCreate(state.projects.first { it.path == path }, agent)
                                }
                            }
                        }
                    }
                }
            },
        )
    }
}

/** The name of a new project, and which of the daemon's folders it goes in when there are several. */
@Composable
fun NewProjectForm(name: String, onName: (String) -> Unit, roots: List<String>, root: String, onRoot: (String) -> Unit) {
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        OutlinedTextField(
            name, onName, Modifier.fillMaxWidth().focusRequester(focus),
            label = { Text("Name") }, singleLine = true, shape = RoundedCornerShape(12.dp),
        )
        if (roots.size > 1) {
            var open by remember { mutableStateOf(false) }
            Box {
                OutlinedTextField(
                    root, {}, Modifier.fillMaxWidth(), readOnly = true, singleLine = true,
                    label = { Text("In") }, trailingIcon = { DropArrow() }, shape = RoundedCornerShape(12.dp),
                )
                // The field only shows the choice; a tap anywhere on it opens the menu.
                Box(Modifier.matchParentSize().clickable { open = true })
                DropdownMenu(open, { open = false }) {
                    for (r in roots) {
                        DropdownMenuItem(text = { Text(r) }, leadingIcon = { Tick(r == root) }, onClick = { open = false; onRoot(r) })
                    }
                }
            }
        } else {
            Text("In $root", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline)
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ArchiveScreen(state: AppState, onOpen: (SessionInfo) -> Unit, onBack: () -> Unit) {
    // Only asked for here, and again after a reconnect.
    val connected = state.status == "connected"
    LaunchedEffect(connected) { if (connected) RustBridge.archived() }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Archive") },
                navigationIcon = { IconButton(onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
            )
        },
    ) { pad ->
        Column(Modifier.fillMaxSize().padding(pad)) {
            ErrorLine(state)
            val archived = state.archived
            when {
                archived == null -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
                archived.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Text("No archived sessions. Swipe one away to archive it.")
                }
                else -> SessionList(archived, "Unarchive", onOpen, { RustBridge.archive(it.id, false) }, { RustBridge.delete(it.id) })
            }
        }
    }
}

/** Sessions to open; swiping one either way does [onSwipe], which [swipeLabel] tells, and holding it asks to [onDelete] it. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun SessionList(sessions: List<SessionInfo>, swipeLabel: String, onOpen: (SessionInfo) -> Unit, onSwipe: (SessionInfo) -> Unit, onDelete: (SessionInfo) -> Unit) {
    // The session a long press asks to delete.
    var deleting by remember { mutableStateOf<SessionInfo?>(null) }
    LazyColumn(Modifier.fillMaxSize()) {
        items(sessions, key = { it.id }) { s ->
            Swiped(swipeLabel, { onSwipe(s) }) {
                ListItem(
                    headlineContent = { Text(s.project) },
                    supportingContent = {
                        Text(s.preview.ifEmpty { s.title }.ifEmpty { "(empty)" }, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    },
                    overlineContent = { Text("${s.provider} · ${ago(s.updated)}") },
                    trailingContent = when {
                        s.waiting -> ({ Badge(containerColor = MaterialTheme.colorScheme.tertiary) { Text("needs you", Modifier.padding(horizontal = 4.dp)) } })
                        s.running -> ({ CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp) })
                        else -> null
                    },
                    modifier = Modifier.combinedClickable(onClick = { onOpen(s) }, onLongClick = { deleting = s }),
                )
            }
            HorizontalDivider()
        }
    }
    deleting?.let { s ->
        AlertDialog(
            onDismissRequest = { deleting = null },
            title = { Text("Delete session?") },
            text = {
                Text("“${s.preview.ifEmpty { s.title }.ifEmpty { s.project }}” and its messages will be deleted from the daemon. This cannot be undone.")
            },
            dismissButton = { TextButton({ deleting = null }) { Text("Cancel") } },
            confirmButton = {
                TextButton(
                    { deleting = null; onDelete(s) },
                    colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
                ) { Text("Delete") }
            },
        )
    }
}

/** [content] that can be swiped away either way to do [onSwipe]; it leaves the list once the daemon moves it. */
@Composable
fun Swiped(label: String, onSwipe: () -> Unit, content: @Composable () -> Unit) {
    val swipe = rememberSwipeToDismissBoxState(confirmValueChange = {
        if (it != SwipeToDismissBoxValue.Settled) onSwipe()
        true
    })
    // Back in place when the daemon did not take it, e.g. offline.
    LaunchedEffect(swipe.currentValue) {
        if (swipe.currentValue != SwipeToDismissBoxValue.Settled) {
            delay(3000)
            swipe.reset()
        }
    }
    SwipeToDismissBox(
        swipe,
        backgroundContent = {
            val start = swipe.dismissDirection == SwipeToDismissBoxValue.StartToEnd
            Box(
                Modifier.fillMaxSize().background(MaterialTheme.colorScheme.secondaryContainer).padding(horizontal = 24.dp),
                contentAlignment = if (start) Alignment.CenterStart else Alignment.CenterEnd,
            ) { Text(label, color = MaterialTheme.colorScheme.onSecondaryContainer) }
        },
    ) { content() }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatScreen(state: AppState, session: SessionInfo, onBack: () -> Unit) {
    val messages = state.messages[session.id].orEmpty()
    var draft by remember { mutableStateOf("") }
    // A voice message goes out as text once it is transcribed.
    val voice = rememberVoice(session.id) { RustBridge.send(session.id, it) }
    var agentMenu by remember { mutableStateOf(false) }
    // The agent to switch to, once the user confirms.
    var switchTo by remember { mutableStateOf<String?>(null) }
    var pickingModel by remember { mutableStateOf(false) }
    var permissionsMenu by remember { mutableStateOf(false) }
    val list = rememberLazyListState()
    // The list is laid out top-down, so a reply that streams in grows below what is on screen
    // instead of pushing it up. It only moves on opening, when the user sends, and with its height.
    // Opening the session reads it on the daemon, so what was unread is taken before that comes back.
    val unread = remember { session.unread }
    var opened by remember { mutableStateOf(false) }
    LaunchedEffect(messages.isNotEmpty()) {
        if (!opened && messages.isNotEmpty()) {
            if (unread != null && unread < messages.size) {
                // At the start of the agent's message not seen yet.
                list.scrollToItem(unread)
            } else {
                // An offset past the end is clamped, so this lands at the very bottom of the last message.
                list.scrollToItem(messages.size - 1, Int.MAX_VALUE)
            }
            opened = true
        }
    }
    LaunchedEffect(messages.size) {
        if (opened && messages.lastOrNull()?.role == "user") list.animateScrollToItem(messages.size - 1)
    }
    // A top-down list keeps its top when its height changes, so the keyboard or a longer draft would
    // cover the end of what is on screen: keep the bottom edge where it is instead.
    LaunchedEffect(list) {
        var height = 0
        snapshotFlow { list.layoutInfo.viewportSize.height }.collect { now ->
            val grown = now - height
            // Growing at the very end, the list has already moved by itself.
            if (height != 0 && (grown < 0 || list.canScrollForward)) list.dispatchRawDelta(-grown.toFloat())
            height = now
        }
    }
    Scaffold(
        modifier = Modifier.imePadding(),
        topBar = {
            Column {
                TopAppBar(
                    title = { Text(session.project) },
                    navigationIcon = { IconButton(onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                )
                // What the next message runs with.
                Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Box {
                        AssistChip({ agentMenu = true }, { Text(session.provider) }, enabled = !session.running && state.agents.isNotEmpty(), trailingIcon = { DropArrow() })
                        AgentMenu(agentMenu, state.agents, session.provider, { agentMenu = false }) {
                            agentMenu = false
                            when {
                                it == session.provider -> {}
                                // Nothing to lose yet.
                                state.messages[session.id]?.isEmpty() == true -> RustBridge.setAgent(session.id, it)
                                else -> switchTo = it
                            }
                        }
                    }
                    // A long name is cut so the chips after it stay on screen.
                    AssistChip({ pickingModel = true }, { Text(modelLabel(state, session), maxLines = 1, overflow = TextOverflow.Ellipsis) }, Modifier.weight(1f, fill = false).widthIn(max = 200.dp), trailingIcon = { DropArrow() })
                    Box {
                        AssistChip({ permissionsMenu = true }, { Text(PERMISSIONS.firstOrNull { it.first == session.permissions }?.second.orEmpty()) }, trailingIcon = { DropArrow() })
                        DropdownMenu(permissionsMenu, { permissionsMenu = false }) {
                            for ((value, label, about) in PERMISSIONS) {
                                DropdownMenuItem(
                                    text = { Column { Text(label); Text(about, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline) } },
                                    leadingIcon = { Tick(value == session.permissions) },
                                    onClick = { permissionsMenu = false; RustBridge.setPermissions(session.id, value) },
                                )
                            }
                        }
                    }
                }
            }
        },
        bottomBar = {
            Column(Modifier.navigationBarsPadding()) {
                ErrorLine(state)
                voice.error?.let {
                    Row(Modifier.fillMaxWidth().clickable { voice.error = null }.padding(horizontal = 16.dp, vertical = 4.dp)) {
                        Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                    }
                }
                if (session.queue.isNotEmpty()) {
                    val steers = state.agents.firstOrNull { it.name == session.provider }?.steers == true
                    Column(Modifier.heightIn(max = 160.dp).verticalScroll(rememberScrollState())) {
                        for (q in session.queue) {
                            QueuedLine(session, q, steers) { draft = if (draft.isBlank()) it else draft + "\n\n" + it }
                        }
                    }
                }
                if (voice.phase == VoicePhase.Captured) {
                    Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            "Captured recording of ${voice.capturedMinutes} minutes",
                            Modifier.weight(1f),
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        TextButton(voice::discard) { Text("Discard") }
                        Spacer(Modifier.width(8.dp))
                        Button(
                            onClick = { state.transcription?.let(voice::send) },
                            enabled = state.status == "connected" && state.transcription != null,
                        ) { Text("Send") }
                    }
                } else {
                    Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
                        if (voice.phase == VoicePhase.Recording) {
                            IconButton(voice::cancel) { Icon(Icons.Filled.Close, "Cancel") }
                            RecordingTime(voice.started, Modifier.weight(1f))
                        } else {
                            OutlinedTextField(
                                draft, { draft = it }, Modifier.weight(1f),
                                placeholder = {
                                    Text(
                                        when (voice.phase) {
                                            VoicePhase.Uploading -> "Uploading…"
                                            VoicePhase.Transcribing -> "Transcribing…"
                                            else -> if (session.running) "Queue a message" else "Message"
                                        },
                                    )
                                },
                                maxLines = 5, enabled = voice.phase == VoicePhase.Idle,
                            )
                        }
                        Spacer(Modifier.width(8.dp))
                        when {
                            voice.phase == VoicePhase.Recording -> Button(
                                onClick = { state.transcription?.let(voice::finish) ?: voice.cancel() },
                                enabled = state.status == "connected",
                            ) { Text("Send") }
                            voice.phase == VoicePhase.Uploading -> BusyLabel("Uploading…")
                            voice.phase == VoicePhase.Transcribing -> BusyLabel("Transcribing…")
                            // A voice message sent now waits in the queue.
                            session.running && draft.isBlank() -> Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                if (state.transcription != null) FilledTonalIconButton(voice::start) { Icon(Glyphs.Mic, "Record") }
                                Button({ RustBridge.stop(session.id) }, colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error, contentColor = MaterialTheme.colorScheme.onError)) { Text("Stop") }
                            }
                            draft.isBlank() && state.transcription != null -> FilledTonalIconButton(voice::start) { Icon(Glyphs.Mic, "Record") }
                            else -> Button(
                                onClick = { RustBridge.send(session.id, draft); draft = "" },
                                enabled = draft.isNotBlank() && state.status == "connected",
                            ) { Text("Send") }
                        }
                    }
                }
            }
        },
    ) { pad ->
        LazyColumn(Modifier.fillMaxSize().padding(pad), state = list, contentPadding = PaddingValues(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            items(messages.size, key = { it }) { Bubble(session.id, messages[it]) }
            if (session.running && !session.waiting) item(key = "running") { CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp) }
        }
    }

    if (pickingModel) {
        ModelPicker(state, session, onDismiss = { pickingModel = false }) {
            pickingModel = false
            RustBridge.setModel(session.id, it)
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

/** A message waiting for the agent; tapping it offers to send it now, edit it or take it out. */
@Composable
fun QueuedLine(session: SessionInfo, q: Queued, steers: Boolean, onEdit: (String) -> Unit) {
    var menu by remember { mutableStateOf(false) }
    val (now, about) = when {
        !session.running -> "Send" to "Starts the agent with this message"
        steers -> "Send now" to "The agent reads it while it works"
        else -> "Interrupt and send" to "Stops the agent and starts it again with this message"
    }
    Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 2.dp), horizontalArrangement = Arrangement.End) {
        Box {
            Surface(
                shape = RoundedCornerShape(8.dp),
                color = MaterialTheme.colorScheme.surfaceVariant,
                border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                modifier = Modifier.widthIn(max = 320.dp).clickable(enabled = !q.steering) { menu = true },
            ) {
                Column(Modifier.padding(horizontal = 12.dp, vertical = 6.dp)) {
                    Text(if (q.steering) "sending…" else "queued", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.outline)
                    Text(q.text, style = MaterialTheme.typography.bodySmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
                }
            }
            DropdownMenu(menu, { menu = false }) {
                DropdownMenuItem(
                    text = { Column { Text(now); Text(about, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline) } },
                    onClick = { menu = false; RustBridge.sendNow(session.id, q.id) },
                )
                DropdownMenuItem(text = { Text("Edit") }, onClick = { menu = false; RustBridge.unqueue(session.id, q.id); onEdit(q.text) })
                DropdownMenuItem(text = { Text("Remove") }, onClick = { menu = false; RustBridge.unqueue(session.id, q.id) })
            }
        }
    }
}

/** What a session's agent may do without asking: the value, its label, and what it means. */
val PERMISSIONS = listOf(
    Triple("ask", "Ask first", "Asks before every tool that needs it"),
    Triple("auto", "Auto", "Runs what looks safe, asks for the rest"),
    Triple("all", "Don't ask", "Runs every tool without asking"),
)

fun modelLabel(state: AppState, session: SessionInfo): String {
    val known = state.models[session.provider]?.items?.firstOrNull { it.id == session.model }
    return when {
        session.model.isEmpty() -> "default model"
        known != null -> known.name
        else -> session.model
    }
}

/** The models of the session's agent, filtered by what is typed: every word must appear. */
@Composable
fun ModelPicker(state: AppState, session: SessionInfo, onDismiss: () -> Unit, onPick: (String) -> Unit) {
    LaunchedEffect(session.provider) { RustBridge.models(session.provider) }
    val list = state.models[session.provider]
    var query by remember { mutableStateOf("") }
    val items = list?.items.orEmpty().let { items ->
        // There is always a way back to the agent's own choice.
        if (items.any { it.id.isEmpty() }) items else listOf(AgentModel("", "Default", "Whatever ${session.provider} picks")) + items
    }.filter { matches(query, "${it.id} ${it.name} ${it.description}") }
    val typed = query.trim()
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = { TextButton(onDismiss) { Text("Cancel") } },
        title = { Text("Model for ${session.provider}") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                FilterField(query) { query = it }
                if (list?.error != null) {
                    Column {
                        Text(list.error, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                        TextButton({ RustBridge.models(session.provider) }) { Text("Retry") }
                    }
                }
                if (list == null || list.loading && list.items.isEmpty()) {
                    CircularProgressIndicator(Modifier.align(Alignment.CenterHorizontally).size(24.dp), strokeWidth = 2.dp)
                } else {
                    val entries = items.map { PickerItem(it.id, it.name, it.description.ifEmpty { it.id.takeIf { id -> id != it.name } }, it.id == session.model) }
                    // A model the agent did not list.
                    val other = if (typed.isNotEmpty() && ' ' !in typed && items.none { it.id == typed }) listOf(PickerItem(typed, "Use “$typed”")) else emptyList()
                    PickerList(entries + other, onPick = onPick)
                }
            }
        },
    )
}

/** The agents to pick from; the ones the daemon cannot find are greyed out. */
@Composable
fun AgentMenu(open: Boolean, agents: List<Agent>, current: String, onDismiss: () -> Unit, onPick: (String) -> Unit) {
    DropdownMenu(open, onDismiss) {
        for (a in agents) {
            DropdownMenuItem(
                text = { Text(if (a.installed) a.name else "${a.name} (not installed)") },
                leadingIcon = { Tick(a.name == current) },
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
            Surface(shape = RoundedCornerShape(8.dp), color = MaterialTheme.colorScheme.primaryContainer, border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant), modifier = Modifier.widthIn(max = 320.dp)) {
                SelectionContainer { Text(m.text, Modifier.padding(12.dp)) }
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
        "error" -> SelectionContainer { Text(m.text, color = MaterialTheme.colorScheme.error) }
        else -> SelectionContainer { Markdown(m.text, components = chatMarkdown) }
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
                else -> Icon(
                    when (t.status) { "done" -> Icons.Filled.Check; "denied" -> Glyphs.Block; else -> Icons.Filled.Close },
                    t.status,
                    Modifier.size(14.dp),
                    tint = if (t.status == "done") MaterialTheme.colorScheme.outline else MaterialTheme.colorScheme.error,
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
    Surface(shape = RoundedCornerShape(4.dp), color = MaterialTheme.colorScheme.surfaceVariant, border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant), modifier = Modifier.fillMaxWidth().padding(top = 4.dp)) {
        SelectionContainer {
            Text(
                text, Modifier.horizontalScroll(rememberScrollState()).padding(8.dp),
                style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace,
                maxLines = maxLines, overflow = TextOverflow.Clip, softWrap = false,
            )
        }
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
    OutlinedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            Text(if (plan != null) "Approve the plan?" else "Allow ${t.name}?", style = MaterialTheme.typography.titleSmall)
            if (plan != null) {
                Markdown(plan, modifier = Modifier.padding(top = 4.dp), components = chatMarkdown)
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
    OutlinedCard(Modifier.fillMaxWidth()) {
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
