//! The Agent Client Protocol (JSON-RPC 2.0) of `opencode acp`: `initialize`,
//! `session/new` or `session/resume`, then `session/prompt`, whose answer ends
//! the turn. Meanwhile `session/update` notifications stream the answer and
//! the tool calls, and `session/request_permission` asks before a tool.
//! Its question tool asks with `elicitation/create`: a form, as a JSON schema.

use crate::{content_text, str_of, tool, Answer, Choice, Driver, Event, Question, Start};
use serde_json::{json, Value};
use std::collections::HashMap;

const INIT: u64 = 1;
const SESSION: u64 = 2;
const PROMPT: u64 = 3;

pub(crate) struct Acp {
    start: Start,
    /// Each tool call's name, by its id.
    names: HashMap<String, String>,
    /// The requests waiting for an answer, by their JSON-RPC id.
    pending: HashMap<String, Pending>,
    /// The message the last text belonged to.
    message: Option<String>,
}

enum Pending {
    /// The JSON-RPC id and the options offered.
    Permission(Value, Vec<Value>),
    /// The JSON-RPC id and the form's fields.
    Form(Value, Vec<Field>),
}

/// A property of a form's schema, asked as a question.
struct Field {
    key: String,
    question: String,
    /// The JSON type of its value.
    kind: String,
    /// The value of each option, by its label.
    options: Vec<(String, Value)>,
    /// The key of the free text answer that goes with it, if any.
    custom: Option<String>,
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
            json!({
                "protocolVersion": 1,
                "clientCapabilities": { "elicitation": { "form": {} } },
            }),
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
                self.pending
                    .insert(request.clone(), Pending::Permission(id, options));
                vec![Event::Ask {
                    request,
                    tool_id,
                    name,
                    input: call["rawInput"].clone(),
                    questions: vec![],
                }]
            }
            (Some(id), Some("elicitation/create"))
                if str_of(&v, &["params", "mode"]) == Some("form") =>
            {
                let p = &v["params"];
                let (fields, questions) = form(&p["requestedSchema"]).into_iter().unzip();
                let request = id.to_string();
                self.pending
                    .insert(request.clone(), Pending::Form(id, fields));
                vec![Event::Ask {
                    request,
                    tool_id: str_of(p, &["toolCallId"]).map(str::to_string),
                    name: "question".into(),
                    input: p["requestedSchema"].clone(),
                    questions,
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
        let (id, options) = match self.pending.remove(request) {
            Some(Pending::Permission(id, options)) => (id, options),
            Some(Pending::Form(id, fields)) => {
                let result = match answer {
                    Answer::Allow { answers } => {
                        let mut content = serde_json::Map::new();
                        for f in &fields {
                            if let Some(a) = answers.get(&f.question) {
                                f.fill(a, &mut content);
                            }
                        }
                        json!({ "action": "accept", "content": content })
                    }
                    Answer::Deny(_) => json!({ "action": "decline" }),
                };
                w.push(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                return;
            }
            None => return,
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

/// The questions of a form's schema: each property with the question it asks.
/// opencode adds `<key>_custom` for a free text answer to a property with
/// options; it is the text field under that question, not one of its own.
fn form(schema: &Value) -> Vec<(Field, Question)> {
    let Some(props) = schema["properties"].as_object() else {
        return vec![];
    };
    let mut keys: Vec<&String> = props
        .keys()
        .filter(|k| {
            k.strip_suffix("_custom")
                .is_none_or(|base| !props.contains_key(base))
        })
        .collect();
    // q0, q1, …, q10 in the order asked.
    keys.sort_by_key(|k| (k.len(), k.as_str()));
    keys.into_iter()
        .map(|key| {
            let p = &props[key];
            let multi = p["type"] == "array";
            let options = if multi { &p["items"]["anyOf"] } else { &p["oneOf"] };
            let options: Vec<(Choice, Value)> = options
                .as_array()
                .into_iter()
                .flatten()
                .map(|o| {
                    let value = o["const"].clone();
                    let label = str_of(o, &["title"])
                        .map(str::to_string)
                        .unwrap_or_else(|| value.as_str().map_or(value.to_string(), Into::into));
                    let description = str_of(o, &["description"]).unwrap_or("").to_string();
                    (Choice { label, description }, value)
                })
                .collect();
            let title = str_of(p, &["title"]).unwrap_or("");
            let question = str_of(p, &["description"])
                .filter(|d| !d.is_empty())
                .unwrap_or(if title.is_empty() { key } else { title })
                .to_string();
            let custom = format!("{key}_custom");
            let field = Field {
                key: key.clone(),
                question: question.clone(),
                kind: str_of(p, &["type"]).unwrap_or("string").to_string(),
                options: options.iter().map(|(c, v)| (c.label.clone(), v.clone())).collect(),
                custom: props.contains_key(&custom).then_some(custom),
            };
            let question = Question {
                question,
                header: if title == field.question { "" } else { title }.to_string(),
                options: options.into_iter().map(|(c, _)| c).collect(),
                multi,
            };
            (field, question)
        })
        .collect()
}

impl Field {
    /// Puts an answer (the picked labels joined with ", ", or free text) in
    /// the form's content.
    fn fill(&self, answer: &str, content: &mut serde_json::Map<String, Value>) {
        let value = |label: &str| {
            self.options
                .iter()
                .find(|(l, _)| l == label)
                .map(|(_, v)| v.clone())
        };
        let mut text = None;
        if self.options.is_empty() {
            let v = match self.kind.as_str() {
                "number" => answer.trim().parse::<f64>().ok().map(Value::from),
                "integer" => answer.trim().parse::<i64>().ok().map(Value::from),
                "boolean" => Some(json!(matches!(
                    answer.trim().to_lowercase().as_str(),
                    "true" | "yes" | "y"
                ))),
                _ => Some(json!(answer)),
            };
            if let Some(v) = v {
                content.insert(self.key.clone(), v);
            }
        } else if self.kind == "array" {
            let (picked, rest): (Vec<_>, Vec<_>) =
                answer.split(", ").partition(|l| value(l).is_some());
            if !picked.is_empty() {
                let picked = picked.into_iter().filter_map(value).collect();
                content.insert(self.key.clone(), Value::Array(picked));
            }
            if !rest.is_empty() {
                text = Some(rest.join(", "));
            }
        } else if let Some(v) = value(answer) {
            content.insert(self.key.clone(), v);
        } else {
            text = Some(answer.to_string());
        }
        if let (Some(t), Some(k)) = (text, &self.custom) {
            content.insert(k.clone(), json!(t));
        }
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

    #[test]
    fn questions_are_a_form() {
        let mut d = Acp::new(test_start(None));
        let mut w = vec![];
        d.start(&mut w);
        assert_eq!(w[0]["params"]["clientCapabilities"]["elicitation"]["form"], json!({}));
        // What opencode's question tool asks.
        let (events, _) = feed(
            &mut d,
            &[r#"{"jsonrpc":"2.0","id":7,"method":"elicitation/create","params":{"mode":"form","sessionId":"s","toolCallId":"c1","message":"Questions","requestedSchema":{"type":"object","properties":{
                "q0":{"type":"string","title":"Drink","description":"Tea?","oneOf":[{"const":"Yes","title":"Yes","description":"Hot"},{"const":"No","title":"No","description":""}]},
                "q0_custom":{"type":"string","title":"Drink (other)","description":"Type your own answer"},
                "q1":{"type":"array","title":"Food","description":"Which?","items":{"anyOf":[{"const":"Cake","title":"Cake"},{"const":"Pie","title":"Pie"}]}},
                "q1_custom":{"type":"string","title":"Food (other)","description":"Add your own answer"}
            },"required":[]}}}"#],
        );
        let Event::Ask {
            request,
            tool_id,
            questions,
            ..
        } = &events[0]
        else {
            panic!("{events:?}");
        };
        assert_eq!(tool_id.as_deref(), Some("c1"));
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].question, "Tea?");
        assert_eq!(questions[0].header, "Drink");
        assert_eq!(questions[0].options[0].description, "Hot");
        assert!(!questions[0].multi);
        assert!(questions[1].multi);

        let answers = [("Tea?", "Coffee"), ("Which?", "Cake, Pie, Toast")]
            .map(|(q, a)| (q.to_string(), a.to_string()))
            .into();
        let mut w = vec![];
        d.answer(request, Answer::Allow { answers }, &mut w);
        assert_eq!(
            w[0],
            json!({ "jsonrpc": "2.0", "id": 7, "result": { "action": "accept", "content": {
                "q0_custom": "Coffee", "q1": ["Cake", "Pie"], "q1_custom": "Toast",
            } } })
        );
    }
}
