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

mod acp;
mod claude;
mod codex;

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

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Overrides the binary to run (a path, or a name looked up in `PATH`).
    pub binary: Option<String>,
    /// Lets the agent act without asking for permission. It can still ask
    /// questions.
    pub auto_approve: bool,
}

/// What a turn is started with.
struct Start {
    prompt: String,
    /// The id from [`Event::Session`].
    resume: Option<String>,
    cwd: String,
    auto_approve: bool,
}

/// One turn's conversation with the agent process, in its protocol. `w`
/// collects the lines to write to it.
trait Driver: Send {
    fn start(&mut self, w: &mut Vec<Value>);
    fn line(&mut self, v: Value, w: &mut Vec<Value>) -> Vec<Event>;
    fn answer(&mut self, request: &str, answer: Answer, w: &mut Vec<Value>);
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
                c.args(["--input-format", "stream-json"]);
                c.args(["--permission-prompt-tool", "stdio"]);
                c.args([
                    "--permission-mode",
                    if opts.auto_approve {
                        "bypassPermissions"
                    } else {
                        "default"
                    },
                ]);
                if let Some(id) = resume {
                    c.args(["--resume", id]);
                }
            }
            Provider::Opencode => {
                c.arg("acp").arg("--cwd").arg(cwd);
                // opencode allows everything unless configured otherwise.
                let ask = if opts.auto_approve { "allow" } else { "ask" };
                let perm = json!({ "permission": { "edit": ask, "bash": ask, "webfetch": ask } });
                c.env("OPENCODE_CONFIG_CONTENT", perm.to_string());
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

    fn driver(self, start: Start) -> Box<dyn Driver> {
        match self {
            Provider::Claude => Box::new(claude::Claude::new(start)),
            Provider::Opencode => Box::new(acp::Acp::new(start)),
            Provider::Codex => Box::new(codex::Codex::new(start)),
        }
    }
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
            auto_approve: opts.auto_approve,
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
        auto_approve: false,
    }
}
