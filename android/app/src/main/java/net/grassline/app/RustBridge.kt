package net.grassline.app

/** The Rust core (`crates/mobile`): one controller per process. */
object RustBridge {
    init {
        System.loadLibrary("grassline_mobile")
    }

    /** Opens the controller kept in [filesDir]; returns this device's key. */
    @JvmStatic external fun init(filesDir: String): String

    /** The whole state as JSON; see `Controller::snapshot`. */
    @JvmStatic external fun snapshot(): String

    /** Blocks until the state is newer than [seen] or the timeout passes; returns its version. */
    @JvmStatic external fun await(seen: Long, timeoutMs: Long): Long

    /** Pairs with the daemon a ticket names, and goes to it once it lets us in; returns an error message, or null. */
    @JvmStatic external fun pair(ticket: String, deviceName: String): String?
    /** Goes to another paired daemon, one of [AppState.daemons]. */
    @JvmStatic external fun switchTo(endpoint: String)
    /** Forgets the daemon the app is on, and goes to another one if there is. */
    @JvmStatic external fun unpair()
    @JvmStatic external fun refresh()
    /** Creates a session in [project] with [provider], or the default agent when it is empty. */
    @JvmStatic external fun create(project: String, provider: String)

    /** Makes project [name] in [root] and creates a session in it, like [create]. */
    @JvmStatic external fun createProject(root: String, name: String, provider: String)
    @JvmStatic external fun setDefaultAgent(provider: String)

    /** Switches a session to another agent; it starts a new conversation. */
    @JvmStatic external fun setAgent(id: String, provider: String)
    /** Sets a session's model (an [AgentModel.id]) from its next message on. */
    @JvmStatic external fun setModel(id: String, model: String)

    /** Sets what a session's agent may do without asking: ask, auto or all. */
    @JvmStatic external fun setPermissions(id: String, permissions: String)

    /** Asks for an agent's models; they arrive in [AppState.models]. */
    @JvmStatic external fun models(provider: String)
    /** Puts a session in the archive, or takes it out. */
    @JvmStatic external fun archive(id: String, archived: Boolean)
    /** Asks for the archived sessions; they arrive in [AppState.archived]. */
    @JvmStatic external fun archived()
    @JvmStatic external fun open(id: String)
    /** Leaves a session; the daemon deletes it when nothing was sent to it. */
    @JvmStatic external fun close(id: String)
    /** Deletes a session from the daemon, with its stored messages. */
    @JvmStatic external fun delete(id: String)
    /** Sends a message; while the agent works, it is queued. */
    @JvmStatic external fun send(id: String, text: String)
    /** Sends a [Queued] message now: into the running turn when the agent [Agent.steers], otherwise by stopping it. */
    @JvmStatic external fun sendNow(id: String, queued: String)
    @JvmStatic external fun unqueue(id: String, queued: String)
    @JvmStatic external fun stop(id: String)

    /** Answers the [Ask] whose request is [request]; [decision] is a `Decision` as JSON. */
    @JvmStatic external fun decide(id: String, request: String, decision: String)
    @JvmStatic external fun dismissError()
}
