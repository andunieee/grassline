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

    /** Pairs with the daemon a ticket names; returns an error message, or null. */
    @JvmStatic external fun pair(ticket: String, deviceName: String): String?
    @JvmStatic external fun unpair()
    @JvmStatic external fun refresh()
    /** Creates a session in [project] with [provider], or the default agent when it is empty. */
    @JvmStatic external fun create(project: String, provider: String)
    @JvmStatic external fun setDefaultAgent(provider: String)

    /** Switches a session to another agent; it starts a new conversation. */
    @JvmStatic external fun setAgent(id: String, provider: String)
    @JvmStatic external fun open(id: String)
    @JvmStatic external fun send(id: String, text: String)
    @JvmStatic external fun stop(id: String)

    /** Answers the [Ask] whose request is [request]; [decision] is a `Decision` as JSON. */
    @JvmStatic external fun decide(id: String, request: String, decision: String)
    @JvmStatic external fun dismissError()
}
