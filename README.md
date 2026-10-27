# grassline

A daemon that runs local coding agents (Claude Code, opencode, Codex) headlessly, and an Android
app to talk to them from anywhere over iroh.

- `crates/daemon` → `grasslined`: agents, sessions, the iroh endpoint and the admin interface
  ([hypereffect](../hypereffect) served over HTTP and/or iroh, so any [hyperb](../hyperb) browser
  opens it). It starts on `127.0.0.1:7878` only; every setting is edited from that interface.
- `crates/agents`: one adapter per agent CLI (command line, resume, output parser).
- `crates/proto`: the wire protocol between daemon and phone (JSON lines on an iroh stream).
- `crates/mobile` + `android/`: the Rust core (iroh client, JNI) and the Kotlin Compose UI.

```
just daemon            # run it; open hyperb://127.0.0.1:7878/
just daemon-install    # cargo install + systemd user service (deploy/grasslined.service)
just android           # build the Rust core + APK and install on adb devices
```

Pairing: admin interface → Remote control → "New pairing code", paste it in the app. Projects:
Projects tab → add a root (each subdirectory is a project) or a single project directory.
`cargo run -p grassline-mobile --example smoke -- <dir> <ticket|-> <project> <prompt>` is a
command-line controller.
