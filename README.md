<p align="center">
  <img src="assets/logo.svg" width="128" alt="Grassline logo">
</p>

<h1 align="center">Grassline</h1>

<p align="center">Your coding agents, from anywhere.</p>

Grassline runs Claude Code, opencode and Codex on your own computer and lets you talk to them
from your phone. Start a session in any of your projects, watch the agent work, approve what it
wants to do, answer its questions, and walk away: it keeps going at home while you're out
touching grass.

The phone reaches your computer directly over [iroh](https://iroh.computer). You don't need a
server, an account, a port to open or a VPN.

<p align="center">
  <img src="screenshots/app-sessions.png" width="24%" alt="Sessions">
  <img src="screenshots/app-chat.png" width="24%" alt="A tool waiting for permission">
  <img src="screenshots/app-questions.png" width="24%" alt="An agent asking questions">
  <img src="screenshots/app-pair.png" width="24%" alt="Pairing">
</p>

## What it does

- **Every session in one place.** See all your sessions across projects and agents, newest
  first. The ones where an agent is waiting for you are marked **needs you**.
- **You decide what runs.** Before an agent runs a command or edits a file that needs your
  permission, the app shows exactly what it wants to do. You can **Allow** it, **Always** allow
  that tool for the rest of the session, or **Deny** it and say why. Each session can also run in
  **Auto**, where the agent runs what looks safe and asks only for the rest, or **Don't ask**.
- **Questions with real answers.** When an agent asks you something, its options show up as
  chips, and you can always write something else.
- **See every step.** Each file read, search, edit and command shows up as one line. Tap it to
  see its input and output.
- **Pick your agent.** Choose Claude Code, opencode or Codex for each new session, change the
  default, or switch a running conversation to another agent. Pick its model from the list the
  agent gives, and type to filter it.
- **Voice messages.** Tap 🎤, talk, and the agent gets the text. Transcription uses the
  speech-to-text service you choose (OpenAI, OpenRouter, Groq, Mistral or any compatible one).
  The phone calls it directly, so the audio never passes through your computer.
- **Pairing with a QR code.** Scan the code shown in the admin dashboard with the app, or with
  the phone's camera, and you're connected.

## The admin dashboard

The daemon on your computer is set up from a dashboard you open in a
[hyperb](https://github.com/andunieee/hyperb) browser. There you can pair phones and see who's
connected, choose which directories are projects, check which agents are installed, and set up
voice messages.

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
