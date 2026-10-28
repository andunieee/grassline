//! A command-line controller, for trying a daemon without a phone:
//! `smoke <state dir> <ticket|-> <project|=session id> <prompt> [deny]`. It allows every
//! tool call the agent asks about (or denies them), and picks the first option
//! of every question.

use grassline_mobile::Controller;
use serde_json::Value;
use std::time::{Duration, Instant};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let c = Controller::new(a[0].as_ref()).unwrap();
    println!("key {}", c.key());
    if a[1] != "-" {
        c.pair(&a[1], "smoke").unwrap();
    }
    let snap = |c: &Controller| serde_json::from_str::<Value>(&c.snapshot()).unwrap();
    let until = |c: &Controller, what: &str, f: &dyn Fn(&Value) -> bool| {
        let t = Instant::now();
        let mut v = 0;
        loop {
            let s = snap(c);
            if f(&s) {
                return s;
            }
            assert!(
                t.elapsed() < Duration::from_secs(120),
                "timeout waiting for {what}: {s}"
            );
            v = c.wait(v, Duration::from_secs(1));
        }
    };
    let s = until(&c, "connection", &|s| {
        s["state"] == "connected" || s["state"] == "refused"
    });
    println!("state {} {}", s["state"], s["reason"]);
    println!("projects {}", s["projects"]);
    let id = match a[2].strip_prefix('=') {
        Some(id) => id.to_string(),
        None => {
            let since = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            c.create(&a[2]);
            let new = |s: &Value| {
                s["sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|i| i["created"].as_u64() >= Some(since))
                    .map(|i| i["id"].as_str().unwrap().to_string())
            };
            let s = until(&c, "session", &|s| new(s).is_some());
            new(&s).unwrap()
        }
    };
    println!("session {id}");
    c.open(&id);
    let s = until(&c, "history", &|s| s["messages"][&id].is_array());
    let before = s["messages"][&id].as_array().unwrap().len();
    c.send_text(&id, &a[3]);
    let deny = a.get(4).is_some_and(|d| d == "deny");
    let answered = std::cell::RefCell::new(std::collections::HashSet::new());
    let s = until(&c, "answer", &|s| {
        let m = s["messages"][&id].as_array();
        for t in m.into_iter().flatten().filter_map(|m| m.get("tool")) {
            let Some(request) = t["ask"]["request"].as_str() else {
                continue;
            };
            if !answered.borrow_mut().insert(request.to_string()) {
                continue;
            }
            let answers: serde_json::Map<String, Value> = t["ask"]["questions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|q| {
                    (
                        q["question"].as_str().unwrap().into(),
                        q["options"][0]["label"].clone(),
                    )
                })
                .collect();
            let allow = !deny || !answers.is_empty();
            println!(
                "{} {}: {}",
                if allow { "allow" } else { "deny" },
                t["name"],
                t["input"]
            );
            let d = serde_json::json!({ "allow": allow, "answers": answers });
            c.decide(&id, request, &d.to_string()).unwrap();
        }
        let running = s["sessions"][0]["running"].as_bool().unwrap_or(true);
        m.is_some_and(|m| m.len() >= before + 2) && !running
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&s["messages"][&id]).unwrap()
    );
}
