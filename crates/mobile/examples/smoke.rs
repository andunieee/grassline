//! A command-line controller, for trying a daemon without a phone:
//! `smoke <state dir> <ticket|-> <project> <prompt>`.

use grassline_mobile::Controller;
use serde_json::Value;
use std::time::{Duration, Instant};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let c = Controller::new(a[0].as_ref()).unwrap();
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
            assert!(t.elapsed() < Duration::from_secs(120), "timeout waiting for {what}: {s}");
            v = c.wait(v, Duration::from_secs(1));
        }
    };
    let s = until(&c, "connection", &|s| s["state"] == "connected" || s["state"] == "refused");
    println!("state {} {}", s["state"], s["reason"]);
    println!("projects {}", s["projects"]);
    c.create(&a[2]);
    let s = until(&c, "session", &|s| !s["sessions"].as_array().unwrap().is_empty());
    let id = s["sessions"][0]["id"].as_str().unwrap().to_string();
    c.open(&id);
    c.send_text(&id, &a[3]);
    let s = until(&c, "answer", &|s| {
        let m = s["messages"][&id].as_array();
        let running = s["sessions"][0]["running"].as_bool().unwrap_or(true);
        m.is_some_and(|m| m.len() >= 2) && !running
    });
    println!("{}", serde_json::to_string_pretty(&s["messages"][&id]).unwrap());
}
