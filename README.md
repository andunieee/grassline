# grassline

A daemon that runs local coding agents (Claude Code, opencode, Codex) headlessly, and an Android
app to talk to them from anywhere over iroh.

- `crates/daemon` → `grasslined`: agents, sessions, the iroh endpoint and the admin interface
  ([hypereffect](../hypereffect) served over HTTP and/or iroh, so any [hyperb](../hyperb) browser
  opens it). It starts on `127.0.0.1:7878` only; every setting is edited from that interface.
- `crates/agents`: one driver per agent CLI and its protocol on stdin/stdout.
- `crates/proto`: the wire protocol between daemon and phone (JSON lines on an iroh stream).
- `crates/mobile` + `android/`: the Rust core (iroh client, JNI) and the Kotlin Compose UI.

```
just daemon            # run it; open hyperb://127.0.0.1:7878/
just daemon-install    # cargo install + systemd user service (deploy/grasslined.service)
just android           # build the Rust core + APK and install on adb devices
```

The app streams every tool call (tap one for its input and output). Before an agent runs a tool
that needs permission, or when it asks a question, the call waits in the app for Allow / Always
(this tool, for the rest of the session) / Deny, or for the answers; "Let agents run tools without
asking" skips the permission prompts. Each agent is driven through its interactive protocol:
Claude Code's stream-json, `opencode acp` and `codex app-server`. The app shows each session's
agent and the default one (top of the sessions list), and changes either: a new session can pick
its agent, and switching an existing session to another agent starts a new conversation with it.

Pairing: admin interface → Remote control → "New pairing code", then "Scan QR code" in the app
(it uses a scanner app or Google's code scanner when the phone has one, else its own) or paste the
code. Scanning the QR code with the phone's camera app opens Grassline with the code filled in. Projects:
Projects tab → add a root (each subdirectory is a project) or a single project directory.
`cargo run -p grassline-mobile --example smoke -- <dir> <ticket|-> <project> <prompt>` is a
command-line controller that allows everything it is asked (add `deny` to deny instead).
