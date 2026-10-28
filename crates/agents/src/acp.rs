//! The Agent Client Protocol (JSON-RPC 2.0) of `opencode acp`: `initialize`,
//! `session/new` or `session/resume`, then `session/prompt`, whose answer ends
//! the turn. Meanwhile `session/update` notifications stream the answer and
//! the tool calls, and `session/request_permission` asks before a tool.

use crate::{content_text, str_of, tool, Answer, Driver, Event, Start};
use serde_json::{json, Value};
use std::collections::HashMap;

const INIT: u64 = 1;
const SESSION: u64 = 2;
const PROMPT: u64 = 3;

pub(crate) struct Acp {
    start: Start,
    /// Each tool call's name, by its id.
    names: HashMap<String, String>,
    /// What each waiting request offers: its JSON-RPC id and options.
    pending: HashMap<String, (Value, Vec<Value>)>,
    /// The message the last text belonged to.
    message: Option<String>,
}

impl Acp {
    pub(crate) fn new(start: Start) -> Acp {
        Acp {
            start,
            names: HashMap::new(),
            pending: HashMap::new(),
            message: None,
        }
    }
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn error_of(v: &Value) -> Option<String> {
    let e = v.get("error")?;
    Some(
        str_of(e, &["message"])
            .unwrap_or("the agent failed")
            .to_string(),
    )
}

impl Driver for Acp {
    fn start(&mut self, w: &mut Vec<Value>) {
        w.push(request(
            INIT,
            "initialize",
            json!({ "protocolVersion": 1, "clientCapabilities": {} }),
        ));
    }

    fn line(&mut self, v: Value, w: &mut Vec<Value>) -> Vec<Event> {
        let method = str_of(&v, &["method"]).map(str::to_string);
        match (v.get("id").cloned(), method.as_deref()) {
            // An answer to us.
            (Some(id), None) => {
                if let Some(e) = error_of(&v) {
                    return vec![Event::Error(e), Event::Done];
                }
                match id.as_u64() {
                    Some(INIT) => {
                        let cwd = &self.start.cwd;
                        w.push(match &self.start.resume {
                            Some(sid) => request(
                                SESSION,
                                "session/resume",
                                json!({ "sessionId": sid, "cwd": cwd, "mcpServers": [] }),
                            ),
                            None => request(
                                SESSION,
                                "session/new",
                                json!({ "cwd": cwd, "mcpServers": [] }),
                            ),
                        });
                        vec![]
                    }
                    Some(SESSION) => {
                        let Some(sid) = str_of(&v, &["result", "sessionId"])
                            .map(str::to_string)
                            .or(self.start.resume.clone())
                        else {
                            return vec![Event::Error("no session".into()), Event::Done];
                        };
                        w.push(request(
                            PROMPT,
                            "session/prompt",
                            json!({
                                "sessionId": sid,
                                "prompt": [{ "type": "text", "text": self.start.prompt }],
                            }),
                        ));
                        vec![Event::Session(sid)]
                    }
                    Some(PROMPT) => {
                        let mut out = vec![];
                        if str_of(&v, &["result", "stopReason"]) == Some("refusal") {
                            out.push(Event::Error("the agent refused".into()));
                        }
                        out.push(Event::Done);
                        out
                    }
                    _ => vec![],
                }
            }
            // A request to us.
            (Some(id), Some("session/request_permission")) => {
                let p = &v["params"];
                let call = &p["toolCall"];
                let tool_id = str_of(call, &["toolCallId"]).map(str::to_string);
                let name = tool_id
                    .as_ref()
                    .and_then(|t| self.names.get(t).cloned())
                    .or(str_of(call, &["kind"]).map(Into::into))
                    .unwrap_or("tool".into());
                let options = p["options"].as_array().cloned().unwrap_or_default();
                let request = id.to_string();
                self.pending.insert(request.clone(), (id, options));
                vec![Event::Ask {
                    request,
                    tool_id,
                    name,
                    input: call["rawInput"].clone(),
                    questions: vec![],
                }]
            }
            (Some(id), Some(_)) => {
                // We said we can do nothing else.
                w.push(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": "not supported" },
                }));
                vec![]
            }
            (None, Some("session/update")) => self.update(&v["params"]["update"]),
            _ => vec![],
        }
    }

    fn answer(&mut self, request: &str, answer: Answer, w: &mut Vec<Value>) {
        let Some((id, options)) = self.pending.remove(request) else {
            return;
        };
        let kinds: &[&str] = match answer {
            Answer::Allow { .. } => &["allow_once", "allow_always"],
            // The protocol has no room for why.
            Answer::Deny(_) => &["reject_once", "reject_always"],
        };
        let option = kinds.iter().find_map(|k| {
            options
                .iter()
                .find(|o| str_of(o, &["kind"]) == Some(k))
                .and_then(|o| str_of(o, &["optionId"]))
        });
        let outcome = match option {
            Some(o) => json!({ "outcome": "selected", "optionId": o }),
            None => json!({ "outcome": "cancelled" }),
        };
        w.push(json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": outcome } }));
    }
}

impl Acp {
    fn update(&mut self, u: &Value) -> Vec<Event> {
        match str_of(u, &["sessionUpdate"]) {
            Some("agent_message_chunk") => {
                let Some(text) = str_of(u, &["content", "text"]) else {
                    return vec![];
                };
                let message = str_of(u, &["messageId"]).map(str::to_string);
                let same = message.is_some() && message == self.message;
                self.message = message;
                vec![if same {
                    Event::Delta(text.into())
                } else {
                    Event::Text(text.into())
                }]
            }
            Some(kind @ ("tool_call" | "tool_call_update")) => {
                self.message = None;
                let Some(id) = str_of(u, &["toolCallId"]) else {
                    return vec![];
                };
                let mut out = vec![];
                if kind == "tool_call" {
                    let name = str_of(u, &["title"]).unwrap_or("tool");
                    self.names.insert(id.into(), name.into());
                }
                let name = self.names.get(id).cloned().unwrap_or("tool".into());
                let input = &u["rawInput"];
                // The first input is often only the directory; it grows.
                if kind == "tool_call" || input.as_object().is_some_and(|o| !o.is_empty()) {
                    out.push(tool(id, &name, input.clone()));
                }
                if let Some(s @ ("completed" | "failed")) = str_of(u, &["status"]) {
                    let mut output = content_text(
                        &u["content"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|c| c["content"].clone())
                            .collect::<Value>(),
                    );
                    if output.is_empty() {
                        output = str_of(u, &["rawOutput", "output"])
                            .unwrap_or("")
                            .to_string();
                    }
                    out.push(Event::ToolResult {
                        id: id.into(),
                        output,
                        error: s == "failed",
                    });
                }
                out
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{feed, test_start};

    #[test]
    fn a_turn() {
        let mut d = Acp::new(test_start(Some("ses_1")));
        let mut w = vec![];
        d.start(&mut w);
        assert_eq!(w[0]["method"], "initialize");
        let (events, w) = feed(
            &mut d,
            &[r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}"#],
        );
        assert!(events.is_empty());
        assert_eq!(w[0]["method"], "session/resume");
        assert_eq!(w[0]["params"]["sessionId"], "ses_1");
        let (events, w) = feed(&mut d, &[r#"{"jsonrpc":"2.0","id":2,"result":{}}"#]);
        assert_eq!(events, vec![Event::Session("ses_1".into())]);
        assert_eq!(w[0]["method"], "session/prompt");
        assert_eq!(w[0]["params"]["prompt"][0]["text"], "hi");

        let (events, _) = feed(
            &mut d,
            &[
                r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"c1","title":"bash","status":"pending","rawInput":{}}}}"#,
                r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"in_progress","title":"ls","rawInput":{"command":"ls"}}}}"#,
                r#"{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"toolCall":{"toolCallId":"c1","rawInput":{"command":"ls"}},"options":[{"optionId":"once","kind":"allow_once"},{"optionId":"reject","kind":"reject_once"}]}}"#,
                r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"a.rs"}}]}}}"#,
                r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"do"}}}}"#,
                r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"ne"}}}}"#,
                r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                Event::Tool {
                    id: "c1".into(),
                    name: "bash".into(),
                    summary: "bash".into(),
                    input: json!({})
                },
                Event::Tool {
                    id: "c1".into(),
                    name: "bash".into(),
                    summary: "bash: ls".into(),
                    input: json!({ "command": "ls" })
                },
                Event::Ask {
                    request: "0".into(),
                    tool_id: Some("c1".into()),
                    name: "bash".into(),
                    input: json!({ "command": "ls" }),
                    questions: vec![]
                },
                Event::ToolResult {
                    id: "c1".into(),
                    output: "a.rs".into(),
                    error: false
                },
                Event::Text("do".into()),
                Event::Delta("ne".into()),
                Event::Done,
            ]
        );
        let mut w = vec![];
        d.answer("0", Answer::Deny("no".into()), &mut w);
        assert_eq!(
            w[0],
            json!({ "jsonrpc": "2.0", "id": 0, "result": { "outcome": { "outcome": "selected", "optionId": "reject" } } })
        );
    }
}
