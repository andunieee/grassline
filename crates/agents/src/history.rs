//! The conversations an agent had before in a directory, from wherever it
//! keeps them, so a session can take one up again: Claude Code's transcripts
//! in `~/.claude/projects`, opencode's `session list` and `session export`,
//! and the Codex rollouts in `~/.codex/sessions`. Each is read as a list of
//! [`Entry`]s, which is all a chat shows.

use crate::{content_text, str_of, tool_line, Options, Provider};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, UNIX_EPOCH};
use tokio::process::Command;

/// How many of an agent's transcripts are looked into for the newest ones
/// that have something in them.
const SCAN_MAX: usize = 60;

/// A conversation the agent had before.
#[derive(Debug, Clone, PartialEq)]
pub struct Past {
    /// The agent's id for it, to resume it with.
    pub id: String,
    pub title: String,
    /// Milliseconds since the Unix epoch.
    pub updated: u64,
}

/// A step of a past conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User(String),
    /// A piece of the agent's answer.
    Text(String),
    Tool {
        id: String,
        name: String,
        summary: String,
        input: Value,
    },
    ToolResult {
        id: String,
        output: String,
        error: bool,
    },
}

/// An [`Entry`] and when it happened, in milliseconds since the Unix epoch
/// (0 when the agent did not say).
#[derive(Debug, Clone, PartialEq)]
pub struct Logged {
    pub ts: u64,
    pub entry: Entry,
}

impl Provider {
    /// The agent's conversations in `cwd`, newest first, leaving out the ids
    /// in `skip`; at most `max` of them.
    pub async fn past(
        self,
        opts: &Options,
        cwd: &Path,
        skip: &HashSet<String>,
        max: usize,
    ) -> Result<Vec<Past>, String> {
        let (cwd, skip) = (cwd.to_path_buf(), skip.clone());
        match self {
            Provider::Claude => blocking(move || claude_past(&claude_dir(&cwd)?, &skip, max)).await,
            Provider::Codex => blocking(move || codex_past(&codex_dir(), &cwd, &skip, max)).await,
            Provider::Opencode => {
                let out = run(self, opts, &cwd, &["session", "list", "--format", "json"]).await?;
                Ok(opencode_past(&out, &cwd, &skip, max))
            }
        }
    }

    /// What happened in conversation `id` of the agent in `cwd`.
    pub async fn history(
        self,
        opts: &Options,
        cwd: &Path,
        id: &str,
    ) -> Result<Vec<Logged>, String> {
        let (cwd, id) = (cwd.to_path_buf(), id.to_string());
        match self {
            Provider::Claude => {
                blocking(move || {
                    // It is a file name.
                    if id.is_empty() || id.contains(['/', '\\', '.']) {
                        return Err("no such conversation".into());
                    }
                    let file = claude_dir(&cwd)?.join(format!("{id}.jsonl"));
                    let text = std::fs::read_to_string(&file)
                        .map_err(|_| "no such conversation".to_string())?;
                    Ok(claude_history(&text))
                })
                .await
            }
            Provider::Codex => {
                blocking(move || {
                    let file = codex_files(&codex_dir())
                        .into_iter()
                        .find(|(f, _)| {
                            codex_meta(f).is_some_and(|(i, c)| i == id && same_dir(&c, &cwd))
                        })
                        .ok_or("no such conversation")?
                        .0;
                    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
                    Ok(codex_history(&text))
                })
                .await
            }
            Provider::Opencode => {
                let out = run(self, opts, &cwd, &["session", "export", &id]).await?;
                opencode_history(&out, &cwd)
            }
        }
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

/// Runs the agent's binary with `args` in `cwd`; its stdout.
async fn run(
    provider: Provider,
    opts: &Options,
    cwd: &Path,
    args: &[&str],
) -> Result<String, String> {
    let bin = provider.detect(opts).ok_or("not installed")?;
    // To a file: opencode exits before a pipe takes more than 256 KiB of it.
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).expect("system random number generator");
    let name: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let path = std::env::temp_dir().join(format!("grassline-{}-{name}", provider.name()));
    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let status = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env("PWD", cwd)
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    let status = tokio::time::timeout(Duration::from_secs(30), status).await;
    let out = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    let status = status
        .map_err(|_| "it took too long to answer".to_string())?
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("{} {} failed", provider.name(), args.join(" ")));
    }
    Ok(String::from_utf8_lossy(&out.map_err(|e| e.to_string())?).into_owned())
}

fn home() -> PathBuf {
    std::env::var_os("HOME").unwrap_or_default().into()
}

/// The paths are the same directory.
fn same_dir(a: &str, b: &Path) -> bool {
    let a = Path::new(a);
    a == b
        || a.canonicalize()
            .ok()
            .is_some_and(|a| b.canonicalize().ok() == Some(a))
}

fn modified(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

/// The first line with text, in at most 80 characters.
fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty());
    line.unwrap_or("").chars().take(80).collect()
}

/// An RFC 3339 time in UTC (`2026-11-12T01:52:11.108Z`), in milliseconds since
/// the Unix epoch.
fn iso_ms(s: &str) -> Option<u64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let frac = s.get(19..).unwrap_or("");
    let ms = frac
        .strip_prefix('.')
        .map(|f| {
            let digits: String = f.chars().take_while(char::is_ascii_digit).take(3).collect();
            format!("{digits:0<3}").parse::<i64>().unwrap_or(0)
        })
        .unwrap_or(0);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let total = ((days * 24 + hh) * 60 + mm) * 60 + ss;
    u64::try_from(total * 1000 + ms).ok()
}

// ---- Claude Code

/// Where Claude Code keeps the transcripts of `cwd`: its path with every
/// character but letters and digits made a `-`, cut and hashed when long.
fn claude_dir(cwd: &Path) -> Result<PathBuf, String> {
    let root = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"))
        .join("projects");
    let name: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = root.join(&name);
    if dir.is_dir() || name.len() <= 200 {
        return Ok(dir);
    }
    // A long path is cut to 200 characters and given a hash we do not know.
    let prefix: String = name.chars().take(200).collect();
    std::fs::read_dir(&root)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
        })
        .ok_or("no conversations yet".into())
}

fn claude_past(dir: &Path, skip: &HashSet<String>, max: usize) -> Result<Vec<Past>, String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Ok(vec![]);
    };
    let mut files: Vec<(PathBuf, u64)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .map(|p| {
            let t = modified(&p);
            (p, t)
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    let mut out = vec![];
    for (file, updated) in files.into_iter().take(SCAN_MAX) {
        let Some(id) = file.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        if skip.contains(&id) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if let Some(title) = claude_title(&text) {
            out.push(Past { id, title, updated });
            if out.len() == max {
                break;
            }
        }
    }
    Ok(out)
}

/// The title Claude Code gave the conversation, or its first prompt; `None`
/// when nothing was said in it.
fn claude_title(text: &str) -> Option<String> {
    // The one the user gave it, the one Claude made up, and the summary of
    // older versions; the last of each counts.
    let (mut custom, mut ai, mut summary, mut first) = (None, None, None, None);
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let title = |k| str_of(&v, &[k]).map(first_line).filter(|t| !t.is_empty());
        match str_of(&v, &["type"]) {
            Some("custom-title") => custom = title("customTitle").or(custom),
            Some("ai-title") => ai = title("aiTitle").or(ai),
            Some("summary") => summary = title("summary").or(summary),
            Some("user") if first.is_none() => first = claude_prompt(&v).map(|t| first_line(&t)),
            _ => {}
        }
    }
    first.as_ref()?;
    custom.or(ai).or(summary).or(first)
}

/// What the user wrote in a `user` line, if it is something they wrote.
fn claude_prompt(v: &Value) -> Option<String> {
    let flag = |k: &str| v[k].as_bool() == Some(true);
    if flag("isMeta") || flag("isSidechain") || flag("isCompactSummary") {
        return None;
    }
    let text = match &v["message"]["content"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter(|b| str_of(b, &["type"]) == Some("text"))
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => return None,
    };
    // Slash commands and their output.
    let generated = ["<command-", "<local-command-"];
    let text = text.trim();
    (!text.is_empty() && !generated.iter().any(|g| text.starts_with(g))).then(|| text.into())
}

fn claude_history(text: &str) -> Vec<Logged> {
    let mut out = vec![];
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        let ts = str_of(&v, &["timestamp"]).and_then(iso_ms).unwrap_or(0);
        let mut push = |entry| out.push(Logged { ts, entry });
        match str_of(&v, &["type"]) {
            Some("user") => {
                if let Some(t) = claude_prompt(&v) {
                    push(Entry::User(t));
                }
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if str_of(block, &["type"]) == Some("tool_result") {
                        push(Entry::ToolResult {
                            id: str_of(block, &["tool_use_id"]).unwrap_or("").into(),
                            output: content_text(&block["content"]),
                            error: block["is_error"].as_bool() == Some(true),
                        });
                    }
                }
            }
            Some("assistant") => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    match str_of(block, &["type"]) {
                        Some("text") => {
                            if let Some(t) =
                                str_of(block, &["text"]).filter(|t| !t.trim().is_empty())
                            {
                                push(Entry::Text(t.into()));
                            }
                        }
                        Some("tool_use") => push(tool(
                            str_of(block, &["id"]).unwrap_or(""),
                            str_of(block, &["name"]).unwrap_or("tool"),
                            block["input"].clone(),
                        )),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn tool(id: &str, name: &str, input: Value) -> Entry {
    Entry::Tool {
        id: id.into(),
        name: name.into(),
        summary: tool_line(name, &input),
        input,
    }
}

// ---- Codex

fn codex_dir() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".codex"))
        .join("sessions")
}

/// The rollouts under `dir` (in `YYYY/MM/DD`), newest first, with when each
/// last changed.
fn codex_files(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut files = vec![];
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                let t = modified(&p);
                files.push((p, t));
            }
        }
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    files
}

/// A rollout's thread id and directory, from its first line.
fn codex_meta(file: &Path) -> Option<(String, String)> {
    use std::io::BufRead;
    let mut line = String::new();
    std::io::BufReader::new(std::fs::File::open(file).ok()?)
        .read_line(&mut line)
        .ok()?;
    let v: Value = serde_json::from_str(&line).ok()?;
    if str_of(&v, &["type"]) != Some("session_meta") {
        return None;
    }
    Some((
        str_of(&v, &["payload", "id"])?.into(),
        str_of(&v, &["payload", "cwd"])?.into(),
    ))
}

fn codex_past(
    dir: &Path,
    cwd: &Path,
    skip: &HashSet<String>,
    max: usize,
) -> Result<Vec<Past>, String> {
    let mut out = vec![];
    let mut looked = 0;
    for (file, updated) in codex_files(dir) {
        let Some((id, dir)) = codex_meta(&file) else {
            continue;
        };
        if skip.contains(&id) || !same_dir(&dir, cwd) {
            continue;
        }
        looked += 1;
        if looked > SCAN_MAX {
            break;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let title = codex_history(&text)
            .into_iter()
            .find_map(|l| match l.entry {
                Entry::User(t) => Some(first_line(&t)),
                _ => None,
            });
        if let Some(title) = title {
            out.push(Past { id, title, updated });
            if out.len() == max {
                break;
            }
        }
    }
    Ok(out)
}

fn codex_history(text: &str) -> Vec<Logged> {
    let mut out = vec![];
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let ts = str_of(&v, &["timestamp"]).and_then(iso_ms).unwrap_or(0);
        let p = &v["payload"];
        let entry = match (str_of(&v, &["type"]), str_of(p, &["type"])) {
            // What the user and the agent said; the `message` response items
            // say it again, with the instructions Codex adds.
            (Some("event_msg"), Some("user_message")) => {
                str_of(p, &["message"]).map(|t| Entry::User(t.trim().into()))
            }
            (Some("event_msg"), Some("agent_message")) => {
                str_of(p, &["message"]).map(|t| Entry::Text(t.into()))
            }
            (Some("response_item"), Some("function_call")) => {
                let args = str_of(p, &["arguments"]).unwrap_or("{}");
                let mut input = serde_json::from_str(args).unwrap_or(Value::String(args.into()));
                // `["bash", "-lc", "ls"]` reads better as `ls`.
                if let Some(cmd) = input["command"].as_array().and_then(|a| a.last()).cloned() {
                    input["command"] = cmd;
                }
                Some(tool(
                    str_of(p, &["call_id"]).unwrap_or(""),
                    str_of(p, &["name"]).unwrap_or("tool"),
                    input,
                ))
            }
            (Some("response_item"), Some("custom_tool_call")) => Some(tool(
                str_of(p, &["call_id"]).unwrap_or(""),
                str_of(p, &["name"]).unwrap_or("tool"),
                serde_json::json!({ "input": p["input"] }),
            )),
            (Some("response_item"), Some("function_call_output" | "custom_tool_call_output")) => {
                let output = match &p["output"] {
                    Value::Object(o) => content_text(o.get("content").unwrap_or(&Value::Null)),
                    o => content_text(o),
                };
                Some(Entry::ToolResult {
                    id: str_of(p, &["call_id"]).unwrap_or("").into(),
                    error: p["output"]["success"].as_bool() == Some(false),
                    output,
                })
            }
            _ => None,
        };
        if let Some(entry) =
            entry.filter(|e| !matches!(e, Entry::User(t) | Entry::Text(t) if t.is_empty()))
        {
            out.push(Logged { ts, entry });
        }
    }
    out
}

// ---- opencode

/// The JSON in what opencode printed, which may have other lines before it.
fn json_in(out: &str) -> Option<Value> {
    let mut at = 0;
    for line in out.split_inclusive('\n') {
        if line.starts_with(['[', '{']) {
            if let Ok(v) = serde_json::from_str(&out[at..]) {
                return Some(v);
            }
        }
        at += line.len();
    }
    None
}

/// The sessions in `opencode session list --format json`, which lists those
/// of the whole repository: only the ones in `cwd`.
fn opencode_past(out: &str, cwd: &Path, skip: &HashSet<String>, max: usize) -> Vec<Past> {
    let Some(Value::Array(items)) = json_in(out) else {
        return vec![];
    };
    let mut past: Vec<Past> = items
        .iter()
        .filter(|s| str_of(s, &["directory"]).is_some_and(|d| same_dir(d, cwd)))
        .filter_map(|s| {
            Some(Past {
                id: str_of(s, &["id"])?.into(),
                title: first_line(str_of(s, &["title"]).unwrap_or("")),
                updated: s["updated"].as_u64().unwrap_or(0),
            })
        })
        .filter(|p| !skip.contains(&p.id))
        .collect();
    past.sort_by_key(|p| std::cmp::Reverse(p.updated));
    past.truncate(max);
    past
}

/// The messages in `opencode session export`: opencode 2's (`type` and
/// `content` on each message) or 1's (`info` and `parts`).
fn opencode_history(out: &str, cwd: &Path) -> Result<Vec<Logged>, String> {
    let v = json_in(out).ok_or("opencode exported nothing")?;
    let dir = str_of(&v, &["info", "location", "directory"]).or(str_of(&v, &["info", "directory"]));
    if dir.is_some_and(|d| !same_dir(d, cwd)) {
        return Err("that conversation is in another directory".into());
    }
    let mut out = vec![];
    for m in v["messages"].as_array().into_iter().flatten() {
        let (role, ts, parts) = match m.get("info") {
            Some(info) => (
                str_of(info, &["role"]),
                &info["time"]["created"],
                &m["parts"],
            ),
            None => (str_of(m, &["type"]), &m["time"]["created"], &m["content"]),
        };
        let ts = ts.as_u64().unwrap_or(0);
        let mut push = |entry| out.push(Logged { ts, entry });
        if role == Some("user") {
            // opencode 2 has the text on the message.
            let text = str_of(m, &["text"]).map(String::from).unwrap_or_else(|| {
                parts
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|p| {
                        str_of(p, &["type"]) == Some("text")
                            && p["synthetic"].as_bool() != Some(true)
                    })
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            });
            if !text.trim().is_empty() {
                push(Entry::User(text.trim().into()));
            }
            continue;
        }
        if role != Some("assistant") {
            continue;
        }
        for p in parts.as_array().into_iter().flatten() {
            match str_of(p, &["type"]) {
                Some("text") => {
                    if let Some(t) = str_of(p, &["text"]).filter(|t| !t.trim().is_empty()) {
                        push(Entry::Text(t.into()));
                    }
                }
                Some("tool") => {
                    let id = str_of(p, &["id"]).or(str_of(p, &["callID"])).unwrap_or("");
                    let name = str_of(p, &["name"])
                        .or(str_of(p, &["tool"]))
                        .unwrap_or("tool");
                    let state = &p["state"];
                    push(tool(id, name, state["input"].clone()));
                    let status = str_of(state, &["status"]).unwrap_or("");
                    if matches!(status, "completed" | "error") {
                        let output = match state.get("output") {
                            Some(o) => content_text(o),
                            None => content_text(&state["content"]),
                        };
                        let output = match str_of(state, &["error"]) {
                            Some(e) if output.is_empty() => e.into(),
                            _ => output,
                        };
                        push(Entry::ToolResult {
                            id: id.into(),
                            output,
                            error: status == "error",
                        });
                    }
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entries(l: Vec<Logged>) -> Vec<Entry> {
        l.into_iter().map(|l| l.entry).collect()
    }

    #[test]
    fn times() {
        assert_eq!(iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_ms("2026-11-12T01:52:11.108Z"), Some(1_794_448_331_108));
        assert_eq!(iso_ms("2000-02-29T12:00:00.5Z"), Some(951_825_600_500));
        assert_eq!(iso_ms("nope"), None);
    }

    #[test]
    fn claude_transcripts() {
        let text = [
            json!({"type": "permission-mode", "permissionMode": "auto"}),
            json!({"type": "user", "isMeta": true, "message": {"role": "user", "content": "Caveat: ..."}}),
            json!({"type": "user", "timestamp": "1970-01-01T00:00:01Z", "message": {"role": "user", "content": "fix the build\nplease"}}),
            json!({"type": "assistant", "message": {"content": [{"type": "thinking", "thinking": "hm"}]}}),
            json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "On it."}]}}),
            json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cargo build"}}]}}),
            json!({"type": "user", "isSidechain": true, "message": {"role": "user", "content": "a subagent's prompt"}}),
            json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok", "is_error": false}]}}),
            json!({"type": "user", "message": {"role": "user", "content": "<command-name>/clear</command-name>"}}),
            json!({"type": "ai-title", "aiTitle": "Fix the build"}),
        ]
        .map(|v| v.to_string())
        .join("\n");
        let h = claude_history(&text);
        assert_eq!(h[0].ts, 1000);
        assert_eq!(
            entries(h),
            vec![
                Entry::User("fix the build\nplease".into()),
                Entry::Text("On it.".into()),
                Entry::Tool {
                    id: "t1".into(),
                    name: "Bash".into(),
                    summary: "Bash: cargo build".into(),
                    input: json!({"command": "cargo build"}),
                },
                Entry::ToolResult {
                    id: "t1".into(),
                    output: "ok".into(),
                    error: false
                },
            ]
        );
        assert_eq!(claude_title(&text).as_deref(), Some("Fix the build"));
        // Without a title, its first prompt; without a prompt, nothing.
        let untitled = text
            .lines()
            .filter(|l| !l.contains("ai-title"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(claude_title(&untitled).as_deref(), Some("fix the build"));
        assert_eq!(claude_title(r#"{"type":"ai-title","aiTitle":"x"}"#), None);
    }

    #[test]
    fn claude_lists_the_newest_with_something_in_them() {
        let dir =
            std::env::temp_dir().join(format!("grassline-claude-past-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let prompt = |t: &str| {
            json!({"type": "user", "message": {"role": "user", "content": t}}).to_string()
        };
        for (id, text) in [
            ("a", prompt("first")),
            ("b", String::new()),
            ("c", prompt("third")),
            ("d", prompt("fourth")),
        ] {
            std::fs::write(dir.join(format!("{id}.jsonl")), text).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        let skip = HashSet::from(["d".to_string()]);
        let past = claude_past(&dir, &skip, 5).unwrap();
        let got: Vec<_> = past
            .iter()
            .map(|p| (p.id.as_str(), p.title.as_str()))
            .collect();
        assert_eq!(got, [("c", "third"), ("a", "first")]);
        assert_eq!(claude_past(&dir, &skip, 1).unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codex_rollouts() {
        let text = [
            json!({"timestamp": "1970-01-01T00:00:02Z", "type": "session_meta", "payload": {"id": "th1", "cwd": "/p"}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>"}]}}),
            json!({"timestamp": "1970-01-01T00:00:03Z", "type": "event_msg", "payload": {"type": "user_message", "message": "list the files"}}),
            json!({"type": "response_item", "payload": {"type": "function_call", "name": "shell", "call_id": "c1", "arguments": "{\"command\":[\"bash\",\"-lc\",\"ls\"]}"}}),
            json!({"type": "response_item", "payload": {"type": "function_call_output", "call_id": "c1", "output": "a\nb"}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "a and b"}]}}),
            json!({"type": "event_msg", "payload": {"type": "agent_message", "message": "a and b"}}),
        ]
        .map(|v| v.to_string())
        .join("\n");
        let h = codex_history(&text);
        assert_eq!(h[0].ts, 3000);
        assert_eq!(
            entries(h),
            vec![
                Entry::User("list the files".into()),
                Entry::Tool {
                    id: "c1".into(),
                    name: "shell".into(),
                    summary: "shell: ls".into(),
                    input: json!({"command": "ls"})
                },
                Entry::ToolResult {
                    id: "c1".into(),
                    output: "a\nb".into(),
                    error: false
                },
                Entry::Text("a and b".into()),
            ]
        );

        let dir = std::env::temp_dir().join(format!("grassline-codex-past-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("2026/11/12")).unwrap();
        std::fs::write(dir.join("2026/11/12/rollout-1.jsonl"), &text).unwrap();
        let elsewhere = text.replace("\"/p\"", "\"/q\"").replace("th1", "th2");
        std::fs::write(dir.join("2026/11/12/rollout-2.jsonl"), elsewhere).unwrap();
        let past = codex_past(&dir, Path::new("/p"), &HashSet::new(), 5).unwrap();
        assert_eq!(past.len(), 1);
        assert_eq!(
            (past[0].id.as_str(), past[0].title.as_str()),
            ("th1", "list the files")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn opencode_lists_only_its_directory() {
        let out = json!([
            {"id": "s1", "title": "Old", "updated": 1, "directory": "/p"},
            {"id": "s2", "title": "Elsewhere", "updated": 3, "directory": "/p/sub"},
            {"id": "s3", "title": "New", "updated": 2, "directory": "/p"},
        ])
        .to_string();
        let past = opencode_past(&out, Path::new("/p"), &HashSet::new(), 5);
        let ids: Vec<_> = past.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["s3", "s1"]);
        let skip = HashSet::from(["s3".to_string()]);
        assert_eq!(opencode_past(&out, Path::new("/p"), &skip, 5).len(), 1);
    }

    #[test]
    fn opencode_exports() {
        // opencode 2.
        let v2 = json!({
            "info": {"id": "s1", "location": {"directory": "/p"}},
            "messages": [
                {"type": "user", "time": {"created": 5}, "text": "commit"},
                {"type": "assistant", "time": {"created": 6}, "content": [
                    {"type": "reasoning", "text": "hm"},
                    {"type": "tool", "id": "c1", "name": "shell", "state": {"status": "completed",
                        "input": {"command": "git commit"}, "content": [{"type": "text", "text": "done"}]}},
                    {"type": "text", "text": "Committed."},
                ]},
                {"type": "idle", "time": {"created": 7}},
            ],
        })
        .to_string();
        let h = opencode_history(&v2, Path::new("/p")).unwrap();
        assert_eq!(h[0].ts, 5);
        assert_eq!(
            entries(h),
            vec![
                Entry::User("commit".into()),
                Entry::Tool {
                    id: "c1".into(),
                    name: "shell".into(),
                    summary: "shell: git commit".into(),
                    input: json!({"command": "git commit"})
                },
                Entry::ToolResult {
                    id: "c1".into(),
                    output: "done".into(),
                    error: false
                },
                Entry::Text("Committed.".into()),
            ]
        );
        assert!(opencode_history(&v2, Path::new("/q")).is_err());

        // opencode 1.
        let v1 = json!({
            "info": {"id": "s1", "directory": "/p"},
            "messages": [
                {"info": {"role": "user", "time": {"created": 1}}, "parts": [{"type": "text", "text": "hi"}, {"type": "text", "text": "x", "synthetic": true}]},
                {"info": {"role": "assistant", "time": {"created": 2}}, "parts": [
                    {"type": "tool", "callID": "c1", "tool": "read", "state": {"status": "error", "input": {"filePath": "a"}, "error": "missing"}},
                    {"type": "text", "text": "Hello"},
                ]},
            ],
        })
        .to_string();
        assert_eq!(
            entries(opencode_history(&format!("some log line\n{v1}"), Path::new("/p")).unwrap()),
            vec![
                Entry::User("hi".into()),
                Entry::Tool {
                    id: "c1".into(),
                    name: "read".into(),
                    summary: "read: a".into(),
                    input: json!({"filePath": "a"})
                },
                Entry::ToolResult {
                    id: "c1".into(),
                    output: "missing".into(),
                    error: true
                },
                Entry::Text("Hello".into()),
            ]
        );
    }
}
