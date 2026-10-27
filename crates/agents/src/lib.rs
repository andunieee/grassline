//! Driving local coding agents headlessly.
//!
//! Every provider is an installed CLI that runs one conversation turn as a
//! child process and prints its progress as JSON lines. A turn is started with
//! the prompt and, for later turns, the provider's own session id, so the
//! agent keeps its context between turns without a long-lived process. The
//! lines are normalised into [`Event`]s.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

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
    /// A piece of the answer. Consecutive pieces belong to the same message.
    Text(String),
    /// A tool call, in one line.
    Tool(String),
    Error(String),
    /// The turn ended.
    Done,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Overrides the binary to run (a path, or a name looked up in `PATH`).
    pub binary: Option<String>,
    /// Lets the agent act without asking: it cannot ask anyone.
    pub auto_approve: bool,
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

    /// The command for one turn. `resume` is the id from [`Event::Session`].
    pub fn command(self, opts: &Options, cwd: &Path, prompt: &str, resume: Option<&str>) -> Command {
        let bin = self
            .detect(opts)
            .unwrap_or_else(|| opts.binary.clone().unwrap_or(self.name().into()).into());
        let mut c = Command::new(bin);
        match self {
            Provider::Claude => {
                c.args(["-p", "--output-format", "stream-json", "--verbose"]);
                c.args(["--permission-mode", if opts.auto_approve { "bypassPermissions" } else { "acceptEdits" }]);
                if let Some(id) = resume {
                    c.args(["--resume", id]);
                }
                c.arg("--").arg(prompt);
            }
            Provider::Opencode => {
                c.args(["run", "--format", "json"]);
                if opts.auto_approve {
                    c.arg("--auto");
                }
                if let Some(id) = resume {
                    c.args(["-s", id]);
                }
                c.arg("--").arg(prompt);
            }
            Provider::Codex => {
                c.args(["exec", "--json", "--skip-git-repo-check"]);
                c.arg(if opts.auto_approve {
                    "--dangerously-bypass-approvals-and-sandbox"
                } else {
                    "--full-auto"
                });
                if let Some(id) = resume {
                    c.args(["resume", id]);
                }
                c.arg("--").arg(prompt);
            }
        }
        c.current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        c
    }

    /// Interprets one output line.
    pub fn parse_line(self, line: &str) -> Vec<Event> {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return vec![];
        };
        match self {
            Provider::Claude => parse_claude(&v),
            Provider::Opencode => parse_opencode(&v),
            Provider::Codex => parse_codex(&v),
        }
    }
}

fn str_of<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_str()
}

fn parse_claude(v: &Value) -> Vec<Event> {
    let mut out = vec![];
    if let Some(id) = str_of(v, &["session_id"]) {
        out.push(Event::Session(id.into()));
    }
    match str_of(v, &["type"]) {
        Some("assistant") => {
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                match str_of(block, &["type"]) {
                    Some("text") => out.extend(str_of(block, &["text"]).map(|t| Event::Text(t.into()))),
                    Some("tool_use") => {
                        let name = str_of(block, &["name"]).unwrap_or("tool");
                        out.push(Event::Tool(tool_line(name, &block["input"])));
                    }
                    _ => {}
                }
            }
        }
        Some("result") => {
            if v["is_error"].as_bool() == Some(true) {
                let why = str_of(v, &["result"]).or(str_of(v, &["subtype"])).unwrap_or("the agent failed");
                out.push(Event::Error(why.into()));
            }
            out.push(Event::Done);
        }
        _ => {}
    }
    out
}

fn parse_opencode(v: &Value) -> Vec<Event> {
    let mut out = vec![];
    if let Some(id) = str_of(v, &["sessionID"]) {
        out.push(Event::Session(id.into()));
    }
    match str_of(v, &["type"]) {
        Some("text") => out.extend(str_of(v, &["part", "text"]).map(|t| Event::Text(t.into()))),
        Some("tool_use") | Some("tool") => {
            let part = &v["part"];
            let name = str_of(part, &["tool"]).unwrap_or("tool");
            out.push(Event::Tool(tool_line(name, &part["state"]["input"])));
        }
        Some("error") => {
            let why = str_of(v, &["error", "data", "message"])
                .or(str_of(v, &["error", "name"]))
                .unwrap_or("the agent failed");
            out.push(Event::Error(why.into()));
        }
        _ => {}
    }
    out
}

fn parse_codex(v: &Value) -> Vec<Event> {
    match str_of(v, &["type"]) {
        Some("thread.started") => str_of(v, &["thread_id"]).map(|i| Event::Session(i.into())).into_iter().collect(),
        Some("item.completed") => {
            let item = &v["item"];
            match str_of(item, &["type"]) {
                Some("agent_message") => str_of(item, &["text"]).map(|t| Event::Text(t.into())).into_iter().collect(),
                Some("command_execution") => {
                    vec![Event::Tool(format!("shell: {}", str_of(item, &["command"]).unwrap_or("")))]
                }
                Some("file_change") => vec![Event::Tool("edit files".into())],
                _ => vec![],
            }
        }
        Some("turn.completed") => vec![Event::Done],
        Some("turn.failed") | Some("error") => {
            let why = str_of(v, &["error", "message"]).or(str_of(v, &["message"])).unwrap_or("the agent failed");
            vec![Event::Error(why.into())]
        }
        _ => vec![],
    }
}

/// `Read: src/main.rs`, `Bash: cargo test`.
fn tool_line(name: &str, input: &Value) -> String {
    let arg = ["file_path", "filePath", "path", "command", "pattern", "url", "description"]
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
    provider: Provider,
    stderr: tokio::task::JoinHandle<String>,
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
        let mut child = provider.command(opts, cwd, prompt, resume).spawn()?;
        let stdout = child.stdout.take().expect("piped");
        let mut err = child.stderr.take().expect("piped");
        let stderr = tokio::spawn(async move {
            let mut s = String::new();
            let _ = tokio::io::AsyncReadExt::read_to_string(&mut err, &mut s).await;
            s
        });
        Ok(Turn { child, lines: BufReader::new(stdout).lines(), provider, stderr, done: false })
    }

    /// The next events; `None` when the turn is over. A process that exits
    /// without finishing the turn yields an error with its stderr, then `Done`.
    pub async fn next(&mut self) -> Option<Vec<Event>> {
        if self.done {
            return None;
        }
        loop {
            match self.lines.next_line().await {
                Ok(Some(line)) => {
                    let events = self.provider.parse_line(&line);
                    if events.is_empty() {
                        continue;
                    }
                    if events.contains(&Event::Done) {
                        self.done = true;
                    }
                    return Some(events);
                }
                _ => {
                    self.done = true;
                    let status = self.child.wait().await.ok();
                    let mut events = vec![];
                    if status.is_some_and(|s| !s.success()) {
                        let err = (&mut self.stderr).await.unwrap_or_default();
                        let err = err.trim();
                        let tail: String = err.chars().rev().take(500).collect::<Vec<_>>().into_iter().rev().collect();
                        events.push(Event::Error(if tail.is_empty() { "the agent exited with an error".into() } else { tail }));
                    }
                    events.push(Event::Done);
                    return Some(events);
                }
            }
        }
    }

    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_lines() {
        let p = Provider::Claude;
        assert_eq!(
            p.parse_line(r#"{"type":"system","subtype":"init","session_id":"s1"}"#),
            vec![Event::Session("s1".into())]
        );
        assert_eq!(
            p.parse_line(r#"{"type":"assistant","session_id":"s1","message":{"content":[{"type":"text","text":"Hi"},{"type":"tool_use","name":"Read","input":{"file_path":"a.rs"}}]}}"#),
            vec![Event::Session("s1".into()), Event::Text("Hi".into()), Event::Tool("Read: a.rs".into())]
        );
        assert_eq!(
            p.parse_line(r#"{"type":"result","is_error":true,"result":"boom"}"#),
            vec![Event::Error("boom".into()), Event::Done]
        );
        assert!(p.parse_line("not json").is_empty());
    }

    #[test]
    fn opencode_lines() {
        let p = Provider::Opencode;
        assert_eq!(
            p.parse_line(r#"{"type":"text","sessionID":"ses_1","part":{"type":"text","text":"Hi there."}}"#),
            vec![Event::Session("ses_1".into()), Event::Text("Hi there.".into())]
        );
        assert_eq!(
            p.parse_line(r#"{"type":"tool_use","part":{"tool":"bash","state":{"input":{"command":"ls"}}}}"#),
            vec![Event::Tool("bash: ls".into())]
        );
    }

    #[test]
    fn codex_lines() {
        let p = Provider::Codex;
        assert_eq!(p.parse_line(r#"{"type":"thread.started","thread_id":"t1"}"#), vec![Event::Session("t1".into())]);
        assert_eq!(
            p.parse_line(r#"{"type":"item.completed","item":{"type":"agent_message","text":"ok"}}"#),
            vec![Event::Text("ok".into())]
        );
        assert_eq!(p.parse_line(r#"{"type":"turn.completed"}"#), vec![Event::Done]);
    }

}
