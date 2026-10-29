//! The Codex app server (`codex app-server`, JSON-RPC without the `jsonrpc`
//! field): `initialize`, `thread/start` or `thread/resume`, then `turn/start`;
//! the `turn/completed` notification ends the turn. Meanwhile `item/*`
//! notifications stream the answer and the tool calls, and it asks before
//! commands, file changes and extra permissions, and for answers to its
//! questions (`item/tool/requestUserInput`). `turn/steer` adds messages to
//! the running turn.

use crate::{str_of, tool, Answer, Choice, Driver, Event, Model, Permissions, Question, Start};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

const INIT: u64 = 1;
const THREAD: u64 = 2;
const TURN: u64 = 3;
const MODELS: u64 = 4;
/// The first id of the `turn/steer` requests.
const STEER: u64 = 100;

/// What a waiting request is, to answer it in its shape.
enum Kind {
    /// A command or a file change.
    Approval,
    /// The permissions asked for.
    Permissions(Value),
    /// The id of each question, by its text.
    Questions(Vec<(String, String)>),
}

pub(crate) struct Codex {
    start: Start,
    pending: HashMap<String, (Value, Kind)>,
    /// The messages that arrived in pieces.
    streamed: HashSet<String>,
    /// The message the last text belonged to.
    message: Option<String>,
    thread: Option<String>,
    turn: Option<String>,
    /// The key of each message given with `turn/steer`, by its request id.
    steering: HashMap<u64, String>,
    /// How many were given.
    steered: u64,
    /// The messages to steer with once the turn has an id: key and text.
    held: Vec<(String, String)>,
}

impl Codex {
    pub(crate) fn new(start: Start) -> Codex {
        Codex {
            start,
            pending: HashMap::new(),
            streamed: HashSet::new(),
            message: None,
            thread: None,
            turn: None,
            steering: HashMap::new(),
            steered: 0,
            held: vec![],
        }
    }

    fn steer_now(&mut self, key: &str, text: &str, w: &mut Vec<Value>) {
        let (Some(thread), Some(turn)) = (&self.thread, &self.turn) else {
            return;
        };
        let id = STEER + self.steered;
        self.steered += 1;
        w.push(request(
            id,
            "turn/steer",
            json!({ "threadId": thread, "expectedTurnId": turn, "input": input(text) }),
        ));
        self.steering.insert(id, key.into());
    }

    /// The approval policy, sandbox and model, as thread parameters.
    fn policy(&self) -> Map<String, Value> {
        let (approval, sandbox) = match self.start.permissions {
            Permissions::Ask => ("untrusted", "workspace-write"),
            // It runs what it can in the sandbox, and asks to leave it.
            Permissions::Auto => ("on-request", "workspace-write"),
            Permissions::All => ("never", "danger-full-access"),
        };
        let mut m = Map::new();
        m.insert("cwd".into(), json!(self.start.cwd));
        m.insert("approvalPolicy".into(), json!(approval));
        m.insert("sandbox".into(), json!(sandbox));
        if !self.start.model.is_empty() {
            m.insert("model".into(), json!(self.start.model));
        }
        m
    }
}

/// A message as the input of a turn.
fn input(text: &str) -> Value {
    json!([{ "type": "text", "text": text, "text_elements": [] }])
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "id": id, "method": method, "params": params })
}

pub(crate) fn initialize() -> Value {
    request(
        INIT,
        "initialize",
        json!({
            "clientInfo": { "name": "grassline", "title": null, "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "experimentalApi": true, "requestAttestation": false },
        }),
    )
}

/// Where listing the models is, after a line of the app server.
pub(crate) enum Listing {
    /// Write these.
    Next(Vec<Value>),
    /// A page of models, and the request for the next one if there is one.
    Page(Vec<Model>, Option<Value>),
    None,
}

/// Lists the models (`model/list`) after [`initialize`].
pub(crate) fn models(v: &Value) -> std::io::Result<Listing> {
    if v.get("method").is_some() {
        return Ok(Listing::None);
    }
    if let Some(e) = v.get("error") {
        let why = str_of(e, &["message"]).unwrap_or("the agent failed");
        return Err(std::io::Error::other(why.to_string()));
    }
    Ok(match v["id"].as_u64() {
        Some(INIT) => Listing::Next(vec![
            json!({ "method": "initialized" }),
            request(MODELS, "model/list", json!({})),
        ]),
        Some(MODELS) => {
            let r = &v["result"];
            let page = r["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| {
                    let id = str_of(m, &["model"]).or(str_of(m, &["id"]))?;
                    Some(Model {
                        id: id.into(),
                        name: str_of(m, &["displayName"]).unwrap_or(id).into(),
                        description: s(m, "description"),
                    })
                })
                .collect();
            let next = str_of(r, &["nextCursor"])
                .map(|c| request(MODELS, "model/list", json!({ "cursor": c })));
            Listing::Page(page, next)
        }
        _ => Listing::None,
    })
}

impl Driver for Codex {
    fn start(&mut self, w: &mut Vec<Value>) {
        w.push(initialize());
    }

    fn line(&mut self, v: Value, w: &mut Vec<Value>) -> Vec<Event> {
        let method = str_of(&v, &["method"]).map(str::to_string);
        let p = &v["params"];
        match (v.get("id").cloned(), method.as_deref()) {
            // An answer to us.
            (Some(id), None) => {
                // A steered message is taken in when its request is answered;
                // one that is not is told about when the turn ends.
                if let Some(key) = id.as_u64().and_then(|i| self.steering.remove(&i)) {
                    if v.get("error").is_none() {
                        return vec![Event::Steered(key)];
                    }
                    return vec![];
                }
                if let Some(e) = v.get("error") {
                    let why = str_of(e, &["message"]).unwrap_or("the agent failed");
                    return vec![Event::Error(why.into()), Event::Done];
                }
                match id.as_u64() {
                    Some(INIT) => {
                        w.push(json!({ "method": "initialized" }));
                        let mut params = self.policy();
                        let method = match &self.start.resume {
                            Some(tid) => {
                                params.insert("threadId".into(), json!(tid));
                                "thread/resume"
                            }
                            None => "thread/start",
                        };
                        w.push(request(THREAD, method, Value::Object(params)));
                        vec![]
                    }
                    Some(THREAD) => {
                        let Some(tid) = str_of(&v, &["result", "thread", "id"]) else {
                            return vec![Event::Error("no thread".into()), Event::Done];
                        };
                        w.push(request(
                            TURN,
                            "turn/start",
                            json!({ "threadId": tid, "input": input(&self.start.prompt) }),
                        ));
                        self.thread = Some(tid.into());
                        vec![Event::Session(tid.into())]
                    }
                    Some(TURN) => {
                        self.turn = str_of(&v, &["result", "turn", "id"]).map(Into::into);
                        for (key, text) in std::mem::take(&mut self.held) {
                            self.steer_now(&key, &text, w);
                        }
                        vec![]
                    }
                    _ => vec![],
                }
            }
            // A request to us.
            (Some(id), Some(m)) => {
                let item = str_of(p, &["itemId"]).map(str::to_string);
                let reason = &p["reason"];
                let (name, input, kind, questions) = match m {
                    "item/commandExecution/requestApproval" => (
                        "shell",
                        json!({ "command": p["command"], "cwd": p["cwd"], "reason": reason }),
                        Kind::Approval,
                        vec![],
                    ),
                    "item/fileChange/requestApproval" => (
                        "edit",
                        json!({ "reason": reason, "grantRoot": p["grantRoot"] }),
                        Kind::Approval,
                        vec![],
                    ),
                    "item/permissions/requestApproval" => (
                        "permissions",
                        json!({ "reason": reason, "permissions": p["permissions"] }),
                        Kind::Permissions(p["permissions"].clone()),
                        vec![],
                    ),
                    "item/tool/requestUserInput" => {
                        let qs: Vec<&Value> =
                            p["questions"].as_array().into_iter().flatten().collect();
                        let ids = qs.iter().map(|q| (s(q, "question"), s(q, "id"))).collect();
                        let questions = qs
                            .iter()
                            .map(|q| Question {
                                question: s(q, "question"),
                                header: s(q, "header"),
                                multi: false,
                                options: q["options"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .map(|o| Choice {
                                        label: s(o, "label"),
                                        description: s(o, "description"),
                                    })
                                    .collect(),
                            })
                            .collect();
                        ("question", p.clone(), Kind::Questions(ids), questions)
                    }
                    "mcpServer/elicitation/request" => {
                        w.push(json!({ "id": id, "result": { "action": "decline", "content": null, "_meta": null } }));
                        return vec![];
                    }
                    _ => {
                        w.push(json!({ "id": id, "error": { "code": -32601, "message": "not supported" } }));
                        return vec![];
                    }
                };
                let request = id.to_string();
                self.pending.insert(request.clone(), (id, kind));
                vec![Event::Ask {
                    request,
                    tool_id: item,
                    name: name.into(),
                    input,
                    questions,
                }]
            }
            (None, Some("item/agentMessage/delta")) => {
                let (Some(item), Some(delta)) = (str_of(p, &["itemId"]), str_of(p, &["delta"]))
                else {
                    return vec![];
                };
                self.streamed.insert(item.into());
                let same = self.message.as_deref() == Some(item);
                self.message = Some(item.into());
                vec![if same {
                    Event::Delta(delta.into())
                } else {
                    Event::Text(delta.into())
                }]
            }
            (None, Some(m @ ("item/started" | "item/completed"))) => {
                let item = &p["item"];
                let id = str_of(item, &["id"]).unwrap_or("");
                if str_of(item, &["type"]) == Some("agentMessage") {
                    if m == "item/completed" && !self.streamed.contains(id) {
                        self.message = Some(id.into());
                        return str_of(item, &["text"])
                            .map(|t| Event::Text(t.into()))
                            .into_iter()
                            .collect();
                    }
                    return vec![];
                }
                let Some((name, input)) = call(item) else {
                    return vec![];
                };
                self.message = None;
                let mut out = vec![tool(id, &name, input)];
                if m == "item/completed" {
                    let (output, error) = result(item);
                    out.push(Event::ToolResult {
                        id: id.into(),
                        output,
                        error,
                    });
                }
                out
            }
            (None, Some("turn/completed")) => {
                let turn = &p["turn"];
                let mut out = vec![];
                if str_of(turn, &["status"]) == Some("failed") {
                    let why = str_of(turn, &["error", "message"]).unwrap_or("the agent failed");
                    out.push(Event::Error(why.into()));
                }
                out.push(Event::Done);
                out
            }
            _ => vec![],
        }
    }

    fn answer(&mut self, request: &str, answer: Answer, w: &mut Vec<Value>) {
        let Some((id, kind)) = self.pending.remove(request) else {
            return;
        };
        let allow = matches!(answer, Answer::Allow { .. });
        let result = match kind {
            // The protocol has no room for why.
            Kind::Approval => json!({ "decision": if allow { "accept" } else { "decline" } }),
            Kind::Permissions(asked) => {
                let granted: Map<String, Value> = match (allow, asked) {
                    (true, Value::Object(o)) => {
                        o.into_iter().filter(|(_, v)| !v.is_null()).collect()
                    }
                    _ => Map::new(),
                };
                json!({ "permissions": granted, "scope": "turn" })
            }
            Kind::Questions(ids) => {
                let mut answers = Map::new();
                if let Answer::Allow { answers: given } = answer {
                    for (text, qid) in ids {
                        if let Some(a) = given.get(&text) {
                            answers.insert(qid, json!({ "answers": [a] }));
                        }
                    }
                }
                json!({ "answers": answers })
            }
        };
        w.push(json!({ "id": id, "result": result }));
    }

    fn steer(&mut self, key: &str, text: &str, w: &mut Vec<Value>) -> bool {
        if self.turn.is_some() {
            self.steer_now(key, text, w);
        } else {
            self.held.push((key.into(), text.into()));
        }
        true
    }
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

/// The tool name and input of an item that is a tool call.
fn call(item: &Value) -> Option<(String, Value)> {
    Some(match str_of(item, &["type"])? {
        "commandExecution" => (
            "shell".into(),
            json!({ "command": item["command"], "cwd": item["cwd"] }),
        ),
        "fileChange" => {
            let changes = item["changes"].as_array().cloned().unwrap_or_default();
            let path = changes
                .first()
                .map(|c| c["path"].clone())
                .unwrap_or(Value::Null);
            ("edit".into(), json!({ "path": path, "changes": changes }))
        }
        "mcpToolCall" | "dynamicToolCall" => (
            str_of(item, &["tool"]).unwrap_or("tool").into(),
            item["arguments"].clone(),
        ),
        "webSearch" => {
            let mut input = item.clone();
            if let Some(o) = input.as_object_mut() {
                o.remove("type");
                o.remove("id");
            }
            ("web_search".into(), input)
        }
        _ => return None,
    })
}

/// What a finished tool call returned, and whether it failed.
fn result(item: &Value) -> (String, bool) {
    let status = str_of(item, &["status"]);
    let mut error = matches!(status, Some("failed" | "declined"));
    let output = match str_of(item, &["type"]) {
        Some("commandExecution") => {
            error |= item["exitCode"].as_i64().is_some_and(|c| c != 0);
            s(item, "aggregatedOutput")
        }
        Some("fileChange") => item["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| format!("{} {}", kind_of(&c["kind"]), s(c, "path")))
            .collect::<Vec<_>>()
            .join("\n"),
        Some("mcpToolCall") => {
            if !item["error"].is_null() {
                error = true;
                str_of(item, &["error", "message"])
                    .unwrap_or("failed")
                    .into()
            } else {
                crate::content_text(&item["result"]["content"])
            }
        }
        Some("dynamicToolCall") => {
            error |= item["success"].as_bool() == Some(false);
            crate::content_text(&item["contentItems"])
        }
        _ => String::new(),
    };
    (output, error)
}

/// `add`, `delete`, `update`: the kind of a file change.
fn kind_of(k: &Value) -> String {
    match k {
        Value::String(s) => s.clone(),
        Value::Object(o) => o
            .get("type")
            .and_then(Value::as_str)
            .map(Into::into)
            .or_else(|| o.keys().next().cloned())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{feed, test_start};

    #[test]
    fn a_turn() {
        let mut d = Codex::new(test_start(None));
        let mut w = vec![];
        d.start(&mut w);
        assert_eq!(w[0]["method"], "initialize");
        let (_, w) = feed(&mut d, &[r#"{"id":1,"result":{}}"#]);
        assert_eq!(w[0], json!({ "method": "initialized" }));
        assert_eq!(w[1]["method"], "thread/start");
        assert_eq!(w[1]["params"]["approvalPolicy"], "untrusted");
        let (events, w) = feed(&mut d, &[r#"{"id":2,"result":{"thread":{"id":"t1"}}}"#]);
        assert_eq!(events, vec![Event::Session("t1".into())]);
        assert_eq!(w[0]["method"], "turn/start");
        assert_eq!(w[0]["params"]["input"][0]["text"], "hi");

        let (events, _) = feed(
            &mut d,
            &[
                r#"{"id":3,"result":{"turn":{"id":"u1","status":"inProgress"}}}"#,
                r#"{"method":"item/started","params":{"item":{"type":"commandExecution","id":"i1","command":"ls","cwd":"/p","status":"inProgress"}}}"#,
                r#"{"id":7,"method":"item/commandExecution/requestApproval","params":{"itemId":"i1","command":"ls","cwd":"/p"}}"#,
                r#"{"method":"item/completed","params":{"item":{"type":"commandExecution","id":"i1","command":"ls","cwd":"/p","status":"completed","aggregatedOutput":"a.rs\n","exitCode":0}}}"#,
                r#"{"method":"item/agentMessage/delta","params":{"itemId":"m1","delta":"do"}}"#,
                r#"{"method":"item/agentMessage/delta","params":{"itemId":"m1","delta":"ne"}}"#,
                r#"{"method":"item/completed","params":{"item":{"type":"agentMessage","id":"m1","text":"done"}}}"#,
                r#"{"method":"turn/completed","params":{"turn":{"id":"u1","status":"completed","error":null}}}"#,
            ],
        );
        let shell = tool("i1", "shell", json!({ "command": "ls", "cwd": "/p" }));
        assert_eq!(
            events,
            vec![
                shell.clone(),
                Event::Ask {
                    request: "7".into(),
                    tool_id: Some("i1".into()),
                    name: "shell".into(),
                    input: json!({ "command": "ls", "cwd": "/p", "reason": null }),
                    questions: vec![]
                },
                shell,
                Event::ToolResult {
                    id: "i1".into(),
                    output: "a.rs\n".into(),
                    error: false
                },
                Event::Text("do".into()),
                Event::Delta("ne".into()),
                Event::Done,
            ]
        );
        let mut w = vec![];
        d.answer("7", Answer::allow(), &mut w);
        assert_eq!(w[0], json!({ "id": 7, "result": { "decision": "accept" } }));
    }

    #[test]
    fn steering() {
        let mut d = Codex::new(test_start(None));
        let mut w = vec![];
        d.start(&mut w);
        feed(&mut d, &[r#"{"id":1,"result":{}}"#]);
        feed(&mut d, &[r#"{"id":2,"result":{"thread":{"id":"t1"}}}"#]);
        // Before the turn has an id, it waits.
        let mut w = vec![];
        assert!(d.steer("q1", "also this", &mut w));
        assert!(w.is_empty());
        let (_, w) = feed(
            &mut d,
            &[r#"{"id":3,"result":{"turn":{"id":"u1","status":"inProgress"}}}"#],
        );
        assert_eq!(w[0]["method"], "turn/steer");
        assert_eq!(w[0]["params"]["expectedTurnId"], "u1");
        assert_eq!(w[0]["params"]["input"][0]["text"], "also this");
        let mut w2 = vec![];
        d.steer("q2", "and this", &mut w2);
        let (events, _) = feed(
            &mut d,
            &[
                &json!({ "id": w[0]["id"], "result": { "turnId": "u1" } }).to_string(),
                &json!({ "id": w2[0]["id"], "error": { "message": "no active turn" } }).to_string(),
            ],
        );
        // A refused one is not an error of the turn.
        assert_eq!(events, vec![Event::Steered("q1".into())]);
    }

    #[test]
    fn listing_models() {
        let Listing::Next(w) = models(&json!({ "id": 1, "result": {} })).unwrap() else {
            panic!()
        };
        assert_eq!(w[1]["method"], "model/list");
        let page = json!({ "id": 4, "result": {
            "data": [{ "id": "a", "model": "gpt-a", "displayName": "GPT A", "description": "fast" }],
            "nextCursor": "c2",
        }});
        let Listing::Page(page, Some(next)) = models(&page).unwrap() else {
            panic!()
        };
        assert_eq!(page[0].id, "gpt-a");
        assert_eq!(page[0].name, "GPT A");
        assert_eq!(next["params"]["cursor"], "c2");
    }

    #[test]
    fn questions() {
        let mut d = Codex::new(test_start(Some("t1")));
        let (_, w) = feed(&mut d, &[r#"{"id":1,"result":{}}"#]);
        assert_eq!(w[1]["method"], "thread/resume");
        assert_eq!(w[1]["params"]["threadId"], "t1");
        let (events, _) = feed(
            &mut d,
            &[
                r#"{"id":"q","method":"item/tool/requestUserInput","params":{"itemId":"i2","isBlocking":true,"questions":[{"id":"drink","header":"Drink","question":"Tea?","isOther":true,"isSecret":false,"options":[{"label":"Yes","description":""}]}]}}"#,
            ],
        );
        let Event::Ask {
            request, questions, ..
        } = &events[0]
        else {
            panic!("{events:?}")
        };
        assert_eq!(questions[0].options[0].label, "Yes");
        let mut w = vec![];
        d.answer(
            request,
            Answer::Allow {
                answers: [("Tea?".to_string(), "Yes".to_string())].into(),
            },
            &mut w,
        );
        assert_eq!(
            w[0],
            json!({ "id": "q", "result": { "answers": { "drink": { "answers": ["Yes"] } } } })
        );
    }
}
