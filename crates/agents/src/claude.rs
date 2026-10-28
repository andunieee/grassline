//! Claude Code with `--input-format stream-json --output-format stream-json`:
//! the prompt and the answers go to stdin, and `--permission-prompt-tool
//! stdio` makes it ask there (`can_use_tool`) before a tool that needs
//! permission, and for `AskUserQuestion`.

use crate::{content_text, str_of, tool, Answer, Choice, Driver, Event, Model, Question, Start};
use serde_json::{json, Value};
use std::collections::HashMap;

pub(crate) struct Claude {
    prompt: String,
    /// The input of each tool call that waits, by request.
    pending: HashMap<String, Value>,
}

impl Claude {
    pub(crate) fn new(s: Start) -> Claude {
        Claude {
            prompt: s.prompt,
            pending: HashMap::new(),
        }
    }
}

impl Driver for Claude {
    fn start(&mut self, w: &mut Vec<Value>) {
        w.push(json!({
            "type": "control_request",
            "request_id": "init",
            "request": { "subtype": "initialize", "hooks": null },
        }));
        w.push(json!({
            "type": "user",
            "message": { "role": "user", "content": self.prompt },
            "parent_tool_use_id": null,
            "session_id": "default",
        }));
    }

    fn line(&mut self, v: Value, _w: &mut Vec<Value>) -> Vec<Event> {
        let mut out = vec![];
        if let Some(id) = str_of(&v, &["session_id"]) {
            out.push(Event::Session(id.into()));
        }
        match str_of(&v, &["type"]) {
            Some("assistant") => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    match str_of(block, &["type"]) {
                        Some("text") => {
                            out.extend(str_of(block, &["text"]).map(|t| Event::Text(t.into())))
                        }
                        Some("tool_use") => out.push(tool(
                            str_of(block, &["id"]).unwrap_or(""),
                            str_of(block, &["name"]).unwrap_or("tool"),
                            block["input"].clone(),
                        )),
                        _ => {}
                    }
                }
            }
            Some("user") => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if str_of(block, &["type"]) == Some("tool_result") {
                        out.push(Event::ToolResult {
                            id: str_of(block, &["tool_use_id"]).unwrap_or("").into(),
                            output: content_text(&block["content"]),
                            error: block["is_error"].as_bool() == Some(true),
                        });
                    }
                }
            }
            Some("control_request")
                if str_of(&v, &["request", "subtype"]) == Some("can_use_tool") =>
            {
                let r = &v["request"];
                let request = str_of(&v, &["request_id"]).unwrap_or("").to_string();
                let name = str_of(r, &["tool_name"]).unwrap_or("tool").to_string();
                let input = r["input"].clone();
                self.pending.insert(request.clone(), input.clone());
                out.push(Event::Ask {
                    request,
                    tool_id: str_of(r, &["tool_use_id"]).map(Into::into),
                    questions: if name == "AskUserQuestion" {
                        questions(&input)
                    } else {
                        vec![]
                    },
                    name,
                    input,
                });
            }
            Some("control_cancel_request") => {
                if let Some(r) = str_of(&v, &["request_id"]) {
                    self.pending.remove(r);
                    out.push(Event::AskCancelled(r.into()));
                }
            }
            Some("result") => {
                if v["is_error"].as_bool() == Some(true) {
                    let why = str_of(&v, &["result"])
                        .or(str_of(&v, &["subtype"]))
                        .unwrap_or("the agent failed");
                    out.push(Event::Error(why.into()));
                }
                out.push(Event::Done);
            }
            _ => {}
        }
        out
    }

    fn answer(&mut self, request: &str, answer: Answer, w: &mut Vec<Value>) {
        let Some(mut input) = self.pending.remove(request) else {
            return;
        };
        let response = match answer {
            Answer::Allow { answers } => {
                // AskUserQuestion takes the answers in its input.
                if !answers.is_empty() {
                    input["answers"] = json!(answers);
                }
                json!({ "behavior": "allow", "updatedInput": input })
            }
            Answer::Deny(message) => json!({ "behavior": "deny", "message": message }),
        };
        w.push(json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request, "response": response },
        }));
    }
}

fn questions(input: &Value) -> Vec<Question> {
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or("").to_string();
    input["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|q| Question {
            question: s(q, "question"),
            header: s(q, "header"),
            multi: q["multiSelect"].as_bool().unwrap_or(false),
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
        .collect()
}

/// The models in the answer to `initialize`; its "default" is the empty id.
pub(crate) fn models(v: &Value) -> Option<Vec<Model>> {
    if str_of(v, &["response", "request_id"]) != Some("init") {
        return None;
    }
    let list = v["response"]["response"]["models"].as_array()?;
    Some(
        list.iter()
            .filter_map(|m| {
                let id = str_of(m, &["value"])?;
                Some(Model {
                    id: if id == "default" { "" } else { id }.into(),
                    name: str_of(m, &["displayName"]).unwrap_or(id).into(),
                    description: str_of(m, &["description"]).unwrap_or("").into(),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{feed, test_start};

    #[test]
    fn lines() {
        let mut d = Claude::new(test_start(None));
        let (events, _) = feed(
            &mut d,
            &[
                r#"{"type":"system","subtype":"init","session_id":"s1"}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hi"},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}}]}}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"t1","type":"tool_result","content":[{"type":"text","text":"ok"}],"is_error":true}]}}"#,
                r#"{"type":"result","is_error":true,"result":"boom"}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                Event::Session("s1".into()),
                Event::Text("Hi".into()),
                Event::Tool {
                    id: "t1".into(),
                    name: "Read".into(),
                    summary: "Read: a.rs".into(),
                    input: json!({ "file_path": "a.rs" }),
                },
                Event::ToolResult {
                    id: "t1".into(),
                    output: "ok".into(),
                    error: true
                },
                Event::Error("boom".into()),
                Event::Done,
            ]
        );
    }

    #[test]
    fn listing_models() {
        let v = json!({ "type": "control_response", "response": {
            "subtype": "success", "request_id": "init", "response": { "models": [
                { "value": "default", "displayName": "Default (recommended)", "description": "Opus" },
                { "value": "sonnet", "displayName": "Sonnet", "description": "" },
            ]},
        }});
        let m = models(&v).unwrap();
        assert_eq!(m[0].id, "");
        assert_eq!(m[1].id, "sonnet");
        assert_eq!(models(&json!({ "type": "system" })), None);
    }

    #[test]
    fn questions_are_answered_in_the_input() {
        let mut d = Claude::new(test_start(None));
        let (events, _) = feed(
            &mut d,
            &[
                r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[{"question":"Tea?","header":"Drink","multiSelect":false,"options":[{"label":"Yes","description":""}]}]},"tool_use_id":"t1"}}"#,
            ],
        );
        let Event::Ask { questions, .. } = &events[0] else {
            panic!("{events:?}")
        };
        assert_eq!(questions[0].question, "Tea?");
        let mut w = vec![];
        d.answer(
            "r1",
            Answer::Allow {
                answers: [("Tea?".to_string(), "Yes".to_string())].into(),
            },
            &mut w,
        );
        assert_eq!(
            w[0]["response"]["response"]["updatedInput"]["answers"],
            json!({ "Tea?": "Yes" })
        );
        assert_eq!(w[0]["response"]["request_id"], "r1");
    }
}
