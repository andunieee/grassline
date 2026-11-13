//! Driving local coding agents headlessly.
//!
//! Every provider is an installed CLI that runs one conversation turn as a
//! child process speaking JSON lines on stdin and stdout, each in its own
//! protocol: Claude Code's stream-json ([`claude`]), the Agent Client Protocol
//! of `opencode acp` ([`acp`]) and the Codex app server ([`codex`]). A turn is
//! started with the prompt and, for later turns, the provider's own session
//! id, so the agent keeps its context between turns without a long-lived
//! process. The lines are normalised into [`Event`]s; when the agent asks
//! before it runs a tool ([`Event::Ask`]), it waits for [`Turn::answer`].
//! Agents that can take more messages while they work get them with
//! [`Turn::steer`]. The conversations an agent had before can be listed and
//! read back ([`history`]), to take one up again.

mod acp;
mod claude;
mod codex;
mod history;

pub use history::{Entry, Logged, Past};

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Claude,
    Opencode,
    Codex,
}

pub const ALL: [Provider; 3] = [Provider::Claude, Provider::Opencode, Provider::Codex];

/// What the agent did, as far as a chat cares.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The provider's id for this conversation, to resume it later.
    Session(String),
    /// A piece of the answer: the start of a message, or a paragraph of the
    /// current one.
    Text(String),
    /// More of the piece of text before it.
    Delta(String),
    /// A tool call started, or its input became known: the same `id` is the
    /// same call. `summary` is one line.
    Tool {
        id: String,
        name: String,
        summary: String,
        input: Value,
    },
    /// A tool call ended.
    ToolResult {
        id: String,
        output: String,
        error: bool,
    },
    /// The agent waits for [`Turn::answer`] before it goes on. `tool_id` is
    /// the [`Event::Tool`] it is about, when it says. With `questions`, it asks
    /// for their answers; without, for permission to run the tool.
    Ask {
        request: String,
        tool_id: Option<String>,
        name: String,
        input: Value,
        questions: Vec<Question>,
    },
    /// The agent no longer waits for that answer.
    AskCancelled(String),
    /// The agent took in the message given to [`Turn::steer`] with this key.
    Steered(String),
    Error(String),
    /// The turn ended.
    Done,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub question: String,
    /// A short label for it.
    pub header: String,
    pub options: Vec<Choice>,
    /// Several options may be picked.
    pub multi: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub label: String,
    pub description: String,
}

/// The answer to an [`Event::Ask`].
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// Go ahead. `answers` has the answer to each question, by its text: the
    /// picked labels joined with ", ", or free text.
    Allow { answers: BTreeMap<String, String> },
    /// Do not; the message tells the agent why, when its protocol can.
    Deny(String),
}

impl Answer {
    pub fn allow() -> Answer {
        Answer::Allow {
            answers: BTreeMap::new(),
        }
    }
}

/// What the agent may do without asking. It can still ask questions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Permissions {
    /// It asks before every tool that needs permission.
    Ask,
    /// It decides by itself what is safe, and asks (or refuses) the rest:
    /// Claude's auto mode, Codex's sandbox, and the project directory only for
    /// opencode.
    #[default]
    Auto,
    /// It never asks.
    All,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Overrides the binary to run (a path, or a name looked up in `PATH`).
    pub binary: Option<String>,
    pub permissions: Permissions,
    /// A model id from [`Provider::models`]; empty for the agent's default.
    pub model: String,
}

/// A model an agent can use.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// What [`Options::model`] takes; empty for the agent's default.
    pub id: String,
    pub name: String,
    pub description: String,
}

/// What a turn is started with.
struct Start {
    prompt: String,
    /// The id from [`Event::Session`].
    resume: Option<String>,
    cwd: String,
    permissions: Permissions,
    model: String,
}

/// One turn's conversation with the agent process, in its protocol. `w`
/// collects the lines to write to it.
trait Driver: Send {
    fn start(&mut self, w: &mut Vec<Value>);
    fn line(&mut self, v: Value, w: &mut Vec<Value>) -> Vec<Event>;
    fn answer(&mut self, request: &str, answer: Answer, w: &mut Vec<Value>);
    /// Gives the agent another message while it works; false when it cannot
    /// take one. It says when it took it in with [`Event::Steered`].
    fn steer(&mut self, _key: &str, _text: &str, _w: &mut Vec<Value>) -> bool {
        false
    }
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Opencode => "opencode",
            Provider::Codex => "codex",
        }
    }

    pub fn from_name(name: &str) -> Option<Provider> {
        ALL.into_iter().find(|p| p.name() == name)
    }

    /// It takes more messages while it works ([`Turn::steer`]).
    pub fn steers(self) -> bool {
        matches!(self, Provider::Claude | Provider::Codex)
    }

    /// The installed binary, if any.
    pub fn detect(self, opts: &Options) -> Option<PathBuf> {
        which(opts.binary.as_deref().unwrap_or(self.name()))
    }

    /// The command that runs a turn; what it is about goes to its stdin.
    fn command(self, opts: &Options, cwd: &Path, resume: Option<&str>) -> Command {
        let bin = self
            .detect(opts)
            .unwrap_or_else(|| opts.binary.clone().unwrap_or(self.name().into()).into());
        let mut c = Command::new(bin);
        match self {
            Provider::Claude => {
                c.args(["-p", "--output-format", "stream-json", "--verbose"]);
                c.args(["--input-format", "stream-json", "--replay-user-messages"]);
                c.args(["--permission-prompt-tool", "stdio"]);
                c.args([
                    "--permission-mode",
                    match opts.permissions {
                        Permissions::Ask => "default",
                        Permissions::Auto => "auto",
                        Permissions::All => "bypassPermissions",
                    },
                ]);
                if !opts.model.is_empty() {
                    c.args(["--model", &opts.model]);
                }
                if let Some(id) = resume {
                    c.args(["--resume", id]);
                }
            }
            Provider::Opencode => {
                c.arg("acp");
                // opencode allows everything unless configured otherwise.
                // The old `permission` object, which opencode 2 still reads.
                let permission = match opts.permissions {
                    Permissions::Ask => json!({ "edit": "ask", "bash": "ask", "webfetch": "ask" }),
                    // Free inside the project, asks before files outside it.
                    Permissions::Auto => json!({}),
                    Permissions::All => {
                        json!({ "edit": "allow", "bash": "allow", "webfetch": "allow" })
                    }
                };
                let mut cfg = json!({ "permission": permission });
                if !opts.model.is_empty() {
                    cfg["model"] = json!(opts.model);
                }
                c.env("OPENCODE_CONFIG_CONTENT", cfg.to_string());
            }
            Provider::Codex => {
                c.arg("app-server");
            }
        }
        // Some read the directory from `PWD`, which is the daemon's.
        c.current_dir(cwd)
            .env("PWD", cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        c
    }

    /// The models the agent offers, asked of its binary.
    pub async fn models(self, opts: &Options) -> Result<Vec<Model>, String> {
        let bin = self.detect(opts).ok_or("not installed")?;
        let mut c = Command::new(bin);
        let mut ask = vec![];
        match self {
            Provider::Claude => {
                c.args(["-p", "--output-format", "stream-json", "--verbose"]);
                c.args(["--input-format", "stream-json"]);
                ask.push(json!({
                    "type": "control_request",
                    "request_id": "init",
                    "request": { "subtype": "initialize", "hooks": null },
                }));
            }
            Provider::Opencode => {
                c.arg("models");
            }
            Provider::Codex => {
                c.arg("app-server");
                ask.push(codex::initialize());
            }
        }
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = c.spawn().map_err(|e| e.to_string())?;
        let mut stdin = child.stdin.take().expect("piped");
        let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
        let read = async {
            for v in ask {
                stdin.write_all(format!("{v}\n").as_bytes()).await?;
            }
            let mut models = vec![];
            while let Some(line) = lines.next_line().await? {
                if self == Provider::Opencode {
                    models.extend(opencode_model(&line));
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let found = match self {
                    Provider::Claude => claude::models(&v),
                    _ => match codex::models(&v)? {
                        codex::Listing::Next(w) => {
                            for w in w {
                                stdin.write_all(format!("{w}\n").as_bytes()).await?;
                            }
                            None
                        }
                        codex::Listing::Page(page, more) => {
                            models.extend(page);
                            if let Some(w) = more {
                                stdin.write_all(format!("{w}\n").as_bytes()).await?;
                                None
                            } else {
                                Some(std::mem::take(&mut models))
                            }
                        }
                        codex::Listing::None => None,
                    },
                };
                if let Some(found) = found {
                    return Ok(found);
                }
            }
            Ok::<_, std::io::Error>(models)
        };
        let models = tokio::time::timeout(Duration::from_secs(30), read)
            .await
            .map_err(|_| "it took too long to answer".to_string())?
            .map_err(|e| e.to_string())?;
        let _ = child.kill().await;
        if models.is_empty() {
            return Err("it listed no models".into());
        }
        Ok(models)
    }

    fn driver(self, start: Start) -> Box<dyn Driver> {
        match self {
            Provider::Claude => Box::new(claude::Claude::new(start)),
            Provider::Opencode => Box::new(acp::Acp::new(start)),
            Provider::Codex => Box::new(codex::Codex::new(start)),
        }
    }
}

/// A line of `opencode models`: `provider/model`.
fn opencode_model(line: &str) -> Option<Model> {
    let id = line.trim();
    (id.contains('/') && !id.contains(' ')).then(|| Model {
        id: id.into(),
        name: id.into(),
        description: String::new(),
    })
}

fn str_of<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_str()
}

fn tool(id: &str, name: &str, input: Value) -> Event {
    Event::Tool {
        id: id.into(),
        name: name.into(),
        summary: tool_line(name, &input),
        input,
    }
}

/// A tool result's text: a string, or the text blocks of a list.
fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

/// `Read: src/main.rs`, `Bash: cargo test`.
pub fn tool_line(name: &str, input: &Value) -> String {
    let arg = [
        "file_path",
        "filePath",
        "path",
        "command",
        "pattern",
        "url",
        "query",
        "description",
    ]
    .iter()
    .find_map(|k| input.get(k).and_then(Value::as_str));
    match arg {
        Some(a) => {
            let a: String = a.lines().next().unwrap_or("").chars().take(120).collect();
            format!("{name}: {a}")
        }
        None => name.to_string(),
    }
}

fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        return Path::new(name).is_file().then(|| name.into());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// A running turn.
pub struct Turn {
    child: tokio::process::Child,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    driver: Box<dyn Driver>,
    stderr: tokio::task::JoinHandle<String>,
    /// Lines for the agent's stdin; dropping it closes stdin.
    stdin: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    done: bool,
}

impl Turn {
    pub fn start(
        provider: Provider,
        opts: &Options,
        cwd: &Path,
        prompt: &str,
        resume: Option<&str>,
    ) -> std::io::Result<Turn> {
        let mut child = provider.command(opts, cwd, resume).spawn()?;
        let stdout = child.stdout.take().expect("piped");
        let mut err = child.stderr.take().expect("piped");
        let stderr = tokio::spawn(async move {
            let mut s = String::new();
            let _ = tokio::io::AsyncReadExt::read_to_string(&mut err, &mut s).await;
            s
        });
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(write_lines(child.stdin.take().expect("piped"), rx));
        let driver = provider.driver(Start {
            prompt: prompt.into(),
            resume: resume.map(Into::into),
            cwd: cwd.to_string_lossy().into_owned(),
            permissions: opts.permissions,
            model: opts.model.clone(),
        });
        let mut turn = Turn {
            child,
            lines: BufReader::new(stdout).lines(),
            driver,
            stderr,
            stdin: Some(tx),
            done: false,
        };
        let mut w = vec![];
        turn.driver.start(&mut w);
        turn.write(w);
        Ok(turn)
    }

    fn write(&self, w: Vec<Value>) {
        if let Some(tx) = &self.stdin {
            for v in w {
                let _ = tx.send(format!("{v}\n"));
            }
        }
    }

    /// Answers an [`Event::Ask`].
    pub fn answer(&mut self, request: &str, answer: Answer) {
        let mut w = vec![];
        self.driver.answer(request, answer, &mut w);
        self.write(w);
    }

    /// Gives the agent another message while it works, to be taken in before
    /// the turn ends: [`Event::Steered`] with `key` says when it was. False
    /// when the agent cannot take it (see [`Provider::steers`]) or the turn
    /// is ending.
    pub fn steer(&mut self, key: &str, text: &str) -> bool {
        if self.done || self.stdin.is_none() {
            return false;
        }
        let mut w = vec![];
        let ok = self.driver.steer(key, text, &mut w);
        self.write(w);
        ok
    }

    /// The next events; `None` when the turn is over. A process that exits
    /// without finishing the turn yields an error with its stderr, then `Done`.
    /// Cancel-safe.
    pub async fn next(&mut self) -> Option<Vec<Event>> {
        if self.done {
            // Give the agent a moment to exit by itself.
            let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
            return None;
        }
        loop {
            match self.lines.next_line().await {
                Ok(Some(line)) => {
                    let Ok(v) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    let mut w = vec![];
                    let events = self.driver.line(v, &mut w);
                    self.write(w);
                    if events.is_empty() {
                        continue;
                    }
                    if events.contains(&Event::Done) {
                        self.done = true;
                        // The agents wait for more otherwise.
                        self.stdin = None;
                    }
                    return Some(events);
                }
                _ => {
                    let status = self.child.wait().await.ok();
                    let mut events = vec![];
                    if status.is_some_and(|s| !s.success()) {
                        let err = (&mut self.stderr).await.unwrap_or_default();
                        let err = err.trim();
                        let tail: String = err
                            .chars()
                            .rev()
                            .take(500)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        events.push(Event::Error(if tail.is_empty() {
                            "the agent exited with an error".into()
                        } else {
                            tail
                        }));
                    } else {
                        events.push(Event::Error("the agent stopped before answering".into()));
                    }
                    events.push(Event::Done);
                    self.done = true;
                    return Some(events);
                }
            }
        }
    }

    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

async fn write_lines(mut w: ChildStdin, mut rx: tokio::sync::mpsc::UnboundedReceiver<String>) {
    while let Some(line) = rx.recv().await {
        if w.write_all(line.as_bytes()).await.is_err() || w.flush().await.is_err() {
            return;
        }
    }
}

/// Feeds lines to a driver, for the tests: the events and the lines written.
#[cfg(test)]
fn feed(d: &mut dyn Driver, lines: &[&str]) -> (Vec<Event>, Vec<Value>) {
    let (mut events, mut w) = (vec![], vec![]);
    for l in lines {
        events.extend(d.line(serde_json::from_str(l).unwrap(), &mut w));
    }
    (events, w)
}

#[cfg(test)]
fn test_start(resume: Option<&str>) -> Start {
    Start {
        prompt: "hi".into(),
        resume: resume.map(Into::into),
        cwd: "/p".into(),
        permissions: Permissions::Ask,
        model: String::new(),
    }
}
