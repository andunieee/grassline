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
    @JvmStatic external fun create(project: String)
    @JvmStatic external fun open(id: String)
    @JvmStatic external fun send(id: String, text: String)
    @JvmStatic external fun stop(id: String)
    @JvmStatic external fun dismissError()
}
