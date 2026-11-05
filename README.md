<p align="center">
  <img src="assets/logo.svg" width="128" alt="Grassline logo">
</p>

<h1 align="center">Grassline</h1>

<p align="center">A calm framework for controlling agents from afar.</p>

Grassline runs Claude Code, opencode, Codex and others on your own computer and lets you talk to them from your phone.

Grassline has no notifications, no file browser or command line and doesn't try to replicate your desktop workstation in your phone, only a chat interface for when you're on the go.

Voice commands are supported as long as you bring your own API keys.

Communication between phone and computer computer happens over [iroh](https://iroh.computer), so it is encrypted and works and no servers are needed.

You don't have to manually configure each project you already have in Grassline, because projects are just directories, and if you setup a base directory on Grassline all directories under it are automatically assumed to be projects.

The daemon that you run on your computer is pure Rust with no UI, it uses almost no memory and CPU, so you can keep it running always in the background.

<p align="center">
  <img src="screenshots/app-sessions.png" width="24%" alt="Sessions">
  <img src="screenshots/app-chat.png" width="24%" alt="A tool waiting for permission">
  <img src="screenshots/app-questions.png" width="24%" alt="An agent asking questions">
  <img src="screenshots/app-pair.png" width="24%" alt="Pairing">
</p>

## The admin dashboard

The daemon on your computer is headless, but it can be configured from a dashboard you open in a [hyperb](https://github.com/andunieee/hyperb) browser. There you can pair phones and see who's connected, choose which directories are projects, check which agents are installed, and set up voice messages.

<p align="center">
  <img src="screenshots/admin-agents.png" width="49%" alt="Agents">
  <img src="screenshots/admin-voice.png" width="49%" alt="Voice messages">
</p>

## Getting started

Grassline has two parts: the `grasslined` daemon, which runs on the computer where your agents
live, and the Android app. Both are attached to every
[GitHub release](https://github.com/andunieee/grassline/releases/latest):

- `.deb` for Debian, Ubuntu and derivatives
- `.rpm` for Fedora, openSUSE and derivatives
- `.pkg.tar.zst` for Arch Linux
- a `.tar.gz` with the binary, for everything else (Linux x86_64 and aarch64, macOS)
- the `.apk` for the phone

On Linux the daemon runs as a systemd user service. The packages install a unit in
`/usr/lib/systemd/user/` that starts `/usr/bin/grasslined`, and the Linux tarballs include one
that starts `~/.local/bin/grasslined`. `just daemon-install` uses
[`deploy/grasslined.service`](deploy/grasslined.service) for a `cargo install`. On macOS there
is no systemd: extract the tarball and run `grasslined` directly.

Once the daemon is running, install the app on your phone, open the admin panel and pair them.

## Daemon

Each snippet installs the latest release and starts the daemon as a user service. They are
bash: in fish, run them through `bash -c '…'` or from a bash shell.

<details>
<summary>Debian / Ubuntu (.deb)</summary>

```bash
v=$(curl -fsSL https://api.github.com/repos/andunieee/grassline/releases/latest | grep -Po '"tag_name": *"v\K[^"]+')
curl -fLO "https://github.com/andunieee/grassline/releases/download/v$v/grasslined_${v}_$(dpkg --print-architecture).deb"
sudo apt install "./grasslined_${v}_$(dpkg --print-architecture).deb"
systemctl --user daemon-reload
systemctl --user enable --now grasslined
```

</details>

<details>
<summary>Fedora / openSUSE (.rpm)</summary>

```bash
v=$(curl -fsSL https://api.github.com/repos/andunieee/grassline/releases/latest | grep -Po '"tag_name": *"v\K[^"]+')
sudo dnf install "https://github.com/andunieee/grassline/releases/download/v$v/grasslined-$v-1.$(uname -m).rpm"
systemctl --user daemon-reload
systemctl --user enable --now grasslined
```

</details>

<details>
<summary>Arch Linux (.pkg.tar.zst)</summary>

```bash
v=$(curl -fsSL https://api.github.com/repos/andunieee/grassline/releases/latest | grep -Po '"tag_name": *"v\K[^"]+')
sudo pacman -U "https://github.com/andunieee/grassline/releases/download/v$v/grasslined-$v-1-$(uname -m).pkg.tar.zst"
systemctl --user daemon-reload
systemctl --user enable --now grasslined
```

</details>

<details>
<summary>Single binary (any Linux with systemd)</summary>

```bash
v=$(curl -fsSL https://api.github.com/repos/andunieee/grassline/releases/latest | grep -Po '"tag_name": *"v\K[^"]+')
name="grasslined-$v-$(uname -m)-unknown-linux-gnu"
curl -fL "https://github.com/andunieee/grassline/releases/download/v$v/$name.tar.gz" | tar -xz
install -Dm755 "$name/grasslined" ~/.local/bin/grasslined
install -Dm644 "$name/grasslined.service" ~/.config/systemd/user/grasslined.service
rm -rf "$name"
systemctl --user daemon-reload
systemctl --user enable --now grasslined
```

</details>

<details>
<summary>From source</summary>

```bash
git clone https://github.com/andunieee/grassline
cd grassline
just daemon-install
```

</details>

Check that it is running with `systemctl --user status grasslined`, and follow its logs with
`journalctl --user -u grasslined -f`. To keep it running while you are logged out, run
`sudo loginctl enable-linger $USER`.

## App

Download the latest
[`grassline-<version>.apk`](https://github.com/andunieee/grassline/releases/latest) from the
releases page on your phone and open it to install (Android will ask you to allow installs from
your browser). To build it yourself and install it on phones connected through adb, run
`just android`.

## Admin panel

The admin panel is served by the daemon and is opened with [hyperb](https://github.com/andunieee/hyperb).
With hyperb installed, open `hyperb://127.0.0.1:7878/`, go to **Access**, press
**New pairing code**, and scan it with the app. Then add your projects under **Projects** and
start a session from the phone.

## Releases

Pushing a tag builds and publishes everything:

```
git tag v0.1.0
git push origin v0.1.0
```

The release workflow builds `grasslined` for Linux (x86_64 and aarch64) and macOS (Apple
Silicon and Intel). The Linux builds are published as tarballs and as `.deb`, `.rpm` and
`.pkg.tar.zst` packages; the macOS builds as tarballs. Each archive contains the binary and
this README, and the Linux ones also contain a systemd user unit. The workflow builds the
release APK too. All of it is attached to the GitHub release for the tag.

The APK is signed with the debug key unless these repository secrets are set:
`ANDROID_KEYSTORE_BASE64` (the base64 of a `.jks` file), `ANDROID_KEYSTORE_PASSWORD`,
`ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD`. Builds signed with different keys cannot
be installed over each other, so keep the same keystore for every release.
