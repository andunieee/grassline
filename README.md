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

```
just daemon-install    # install the daemon as a systemd user service
just android           # build the app and install it on connected phones
```

Open `hyperb://127.0.0.1:7878/`, go to **Access**, press **New pairing code**, and scan it with the
app. Then add your projects under **Projects** and start a session from the phone.
