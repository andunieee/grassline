//! The admin interface itself.

use crate::core::{now_ms, Core};
use grassline_agents::ALL;
use hypereffect::ui::*;
use hypereffect::{json, Action, App, Element, Handler, Outbox, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

static CORE: OnceLock<Arc<Core>> = OnceLock::new();

pub fn install(core: Arc<Core>) {
    let _ = CORE.set(core);
}

fn core() -> &'static Arc<Core> {
    CORE.get().expect("installed at startup")
}

#[derive(Default)]
pub struct Admin {
    /// What the sessions tab showed last, to redraw it only when it changes.
    shown: String,
    /// A one-time pairing ticket on display.
    ticket: Option<String>,
    /// The last thing that went wrong or was done.
    note: Option<(String, &'static str)>,
}

fn sig(a: &Action, path: &str) -> String {
    a.signal(path).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

fn ago(ms: u64) -> String {
    let s = now_ms().saturating_sub(ms) / 1000;
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

/// A heading and a form row: an input and the button that sends it.
fn add_row(out: &mut Outbox, parent: &str, id: &str, placeholder: &str, label: &str, action: &str) {
    let row_id = format!("{id}-row");
    out.element(row(row_id.as_str()).parent(parent));
    out.element(input(id).parent(row_id.as_str()).placeholder(placeholder).bind(id).on_submit(Handler::action(action)));
    out.element(button(format!("{id}-go")).parent(row_id.as_str()).variant("primary").label(label).on_press(Handler::action(action)));
}

/// A line with a remove button.
fn item(out: &mut Outbox, parent: &str, id: &str, label: String, action: &str, payload: &str, button_label: &str) {
    out.element(row(id).parent(parent).justify("between"));
    out.element(text(format!("{id}-t")).parent(id).text(label));
    out.element(
        button(format!("{id}-x")).parent(id).variant("plain").label(button_label).on_press(Handler::action(action).payload(payload)),
    );
}

impl Admin {
    /// What the sessions tab shows, to redraw it only when that changes. The
    /// minute is in it because the relative times change with it.
    fn sessions_snapshot() -> String {
        let sessions = core().sessions();
        let rows: Vec<_> = sessions.iter().map(|s| (&s.id, ago(s.updated), s.running, &s.title)).collect();
        serde_json_string(&rows)
    }

    fn render_sessions(&mut self, out: &mut Outbox) {
        out.remove("sessions-body");
        out.element(column("sessions-body").parent("tab-sessions"));
        let sessions = core().sessions();
        if sessions.is_empty() {
            out.element(text("sessions-none").parent("sessions-body").variant("caption").text(
                "No sessions yet. Sessions are started from the remote controller.",
            ));
        }
        for s in &sessions {
            let id = format!("s-{}", s.id);
            out.element(card(id.as_str()).parent("sessions-body"));
            out.element(row(format!("{id}-head")).parent(id.as_str()).justify("between"));
            out.element(text(format!("{id}-name")).parent(format!("{id}-head")).variant("h3").text(s.project.as_str()));
            out.element(
                badge(format!("{id}-state"))
                    .parent(format!("{id}-head"))
                    .variant(if s.running { "success" } else { "neutral" })
                    .text(if s.running { format!("{} working", s.provider) } else { s.provider.clone() }),
            );
            let line = if s.title.is_empty() { "(empty)".to_string() } else { s.title.clone() };
            out.element(text(format!("{id}-title")).parent(id.as_str()).text(line));
            out.element(text(format!("{id}-when")).parent(id.as_str()).variant("caption").text(format!("{} · {}", s.path, ago(s.updated))));
            out.element(row(format!("{id}-btns")).parent(id.as_str()).justify("end"));
            if s.running {
                out.element(
                    button(format!("{id}-stop")).parent(format!("{id}-btns")).label("Stop").on_press(Handler::action("stop").payload(s.id.as_str())),
                );
            }
            out.element(
                button(format!("{id}-del"))
                    .parent(format!("{id}-btns"))
                    .variant("failure")
                    .label("Delete")
                    .on_press(Handler::action("delete").payload(s.id.as_str())),
            );
        }
        self.shown = Self::sessions_snapshot();
    }

    fn render(&mut self, out: &mut Outbox) {
        let cfg = core().config();
        out.remove("root");
        out.element(column("root"));
        out.element(text("title").parent("root").variant("h1").text("Grassline"));
        if let Some((msg, variant)) = &self.note {
            out.element(notice("note").parent("root").variant(*variant).text(msg.as_str()));
        }
        out.element(tabs("tabs").parent("root"));

        // ---- sessions
        out.element(tab("tab-sessions").parent("tabs").label("Sessions"));
        self.render_sessions(out);

        // ---- projects
        out.element(tab("tab-projects").parent("tabs").label("Projects"));
        out.element(text("p-help").parent("tab-projects").variant("caption").text(
            "Every directory inside a root is a project. A single directory can also be a project of its own.",
        ));
        out.element(text("p-roots-h").parent("tab-projects").variant("h3").text("Roots"));
        for (i, r) in cfg.project_roots.iter().enumerate() {
            item(out, "tab-projects", &format!("root-{i}"), r.clone(), "remove_root", r, "Remove");
        }
        add_row(out, "tab-projects", "new_root", "/home/me/code", "Add root", "add_root");
        out.element(text("p-dirs-h").parent("tab-projects").variant("h3").text("Single projects"));
        for (i, r) in cfg.project_dirs.iter().enumerate() {
            item(out, "tab-projects", &format!("dir-{i}"), r.clone(), "remove_dir", r, "Remove");
        }
        add_row(out, "tab-projects", "new_dir", "/home/me/some/project", "Add project", "add_dir");
        out.element(text("p-found-h").parent("tab-projects").variant("h3").text("Available projects"));
        out.element(list("found").parent("tab-projects").empty("None yet."));
        for (i, p) in core().projects().into_iter().enumerate() {
            out.element(text(format!("found-{i}")).parent("found").text(format!("{}  ·  {}", p.name, p.path)));
        }

        // ---- mobile
        out.element(tab("tab-mobile").parent("tabs").label("Remote control"));
        out.element(checkbox("mobile").parent("tab-mobile").variant("switch").label("Accept remote controllers").bind("mobile").on_change(Handler::action("set_mobile")));
        match core().endpoint_id.get() {
            Some(id) => out.element(text("m-id").parent("tab-mobile").variant("caption").text(format!("Daemon key: {id}"))),
            None => out.element(text("m-id").parent("tab-mobile").variant("caption").text("The iroh endpoint is not up yet.")),
        }
        out.element(button("pair").parent("tab-mobile").variant("primary").label("New pairing code").on_press(Handler::action("pair")));
        if let Some(t) = &self.ticket {
            out.element(card("ticket").parent("tab-mobile").title("Paste this in the app (valid for 10 minutes, once)"));
            out.element(text("ticket-t").parent("ticket").variant("code").text(t.as_str()));
        }
        out.element(text("m-dev-h").parent("tab-mobile").variant("h3").text("Paired devices"));
        out.element(list("devices").parent("tab-mobile").empty("No devices."));
        for (i, d) in cfg.paired.iter().enumerate() {
            let short: String = d.key.chars().take(12).collect();
            item(out, "devices", &format!("dev-{i}"), format!("{} ({short}…)", d.name), "unpair", &d.key, "Revoke");
        }

        // ---- admin access
        out.element(tab("tab-admin").parent("tabs").label("Admin access"));
        let status = super::HTTP_STATUS.lock().unwrap().clone();
        out.element(text("a-http-h").parent("tab-admin").variant("h3").text("HTTP"));
        out.element(checkbox("http_on").parent("tab-admin").variant("switch").label("Serve the admin interface over HTTP").bind("http_on").on_change(Handler::action("set_http")));
        out.element(text("a-status").parent("tab-admin").variant("caption").text(status));
        add_row(out, "tab-admin", "http_addr", "127.0.0.1:7878", "Rebind", "set_addr");
        if !cfg.admin_http.addr.starts_with("127.") && !cfg.admin_http.addr.starts_with("localhost") && !cfg.admin_http.addr.starts_with("[::1]") {
            out.element(notice("a-warn").parent("tab-admin").variant("warning").text(
                "Not bound to localhost: other machines must prove a key from the list below to get in.",
            ));
        }
        out.element(text("a-iroh-h").parent("tab-admin").variant("h3").text("Iroh"));
        out.element(checkbox("iroh_on").parent("tab-admin").variant("switch").label("Serve the admin interface over iroh").bind("iroh_on").on_change(Handler::action("set_iroh")));
        if let Some(id) = core().endpoint_id.get() {
            out.element(text("a-addr").parent("tab-admin").variant("code").text(format!("hyperb://{id}/")));
        }
        out.element(text("a-keys-h").parent("tab-admin").variant("h3").text("Allowed admin keys"));
        for (i, k) in cfg.admin_keys.iter().enumerate() {
            item(out, "tab-admin", &format!("key-{i}"), k.clone(), "remove_key", k, "Remove");
        }
        add_row(out, "tab-admin", "new_key", "hyperb identity (64 hex)", "Allow key", "add_key");

        // ---- agents
        out.element(tab("tab-agents").parent("tabs").label("Agents"));
        out.element(select("provider").parent("tab-agents").label("Default agent").bind("provider").variant("segmented").options(
            Value::Array(ALL.iter().map(|p| json!({ "value": p.name(), "label": p.name() })).collect()),
        ).on_change(Handler::action("set_provider")));
        out.element(checkbox("auto").parent("tab-agents").variant("switch").label("Let agents run tools without asking (nobody is there to approve)").bind("auto").on_change(Handler::action("set_auto")));
        for p in ALL {
            let opts = cfg.options(p);
            let found = p.detect(&opts);
            let id = format!("prov-{}", p.name());
            out.element(card(id.as_str()).parent("tab-agents").title(p.name()));
            out.element(
                badge(format!("{id}-b"))
                    .parent(id.as_str())
                    .variant(if found.is_some() { "success" } else { "failure" })
                    .text(match &found {
                        Some(path) => path.to_string_lossy().into_owned(),
                        None => "not found".into(),
                    }),
            );
            out.element(row(format!("{id}-row")).parent(id.as_str()));
            out.element(
                input(format!("{id}-bin"))
                    .parent(format!("{id}-row"))
                    .placeholder("binary path (optional)")
                    .bind(format!("bin.{}", p.name()))
                    .on_submit(Handler::action("set_bin").payload(p.name())),
            );
            out.element(
                button(format!("{id}-save")).parent(format!("{id}-row")).label("Save").on_press(Handler::action("set_bin").payload(p.name())),
            );
        }
    }

    fn signals(&self) -> Value {
        let c = core().config();
        let bins: serde_json::Map<String, Value> =
            ALL.iter().map(|p| (p.name().to_string(), c.binaries.get(p.name()).cloned().unwrap_or_default().into())).collect();
        json!({
            "new_root": "", "new_dir": "", "new_key": "",
            "http_addr": c.admin_http.addr,
            "http_on": c.admin_http.enabled,
            "iroh_on": c.admin_iroh,
            "mobile": c.mobile,
            "provider": c.default_provider,
            "auto": c.auto_approve,
            "bin": bins,
        })
    }

    fn ok(&mut self, msg: impl Into<String>) {
        self.note = Some((msg.into(), "success"));
    }

    fn fail(&mut self, msg: impl Into<String>) {
        self.note = Some((msg.into(), "failure"));
    }
}

fn serde_json_string<T: serde::Serialize>(v: &T) -> String {
    hypereffect::serde_json::to_string(v).unwrap_or_default()
}

impl App for Admin {
    fn hello(&mut self, out: &mut Outbox, _: &BTreeMap<String, String>) {
        out.signal(self.signals());
        self.render(out);
    }

    fn action(&mut self, out: &mut Outbox, a: &Action) {
        let payload = a.payload_as::<String>().unwrap_or_default();
        self.note = None;
        let core = core();
        match a.name.as_str() {
            "add_root" | "add_dir" => {
                let path = sig(a, if a.name == "add_root" { "new_root" } else { "new_dir" });
                let expanded = crate::core::expand(&path);
                if path.is_empty() {
                    return;
                } else if !expanded.is_dir() {
                    self.fail(format!("{path} is not a directory"));
                } else {
                    let root = a.name == "add_root";
                    core.update_config(|c| {
                        let list = if root { &mut c.project_roots } else { &mut c.project_dirs };
                        if !list.contains(&path) {
                            list.push(path.clone());
                        }
                    });
                    out.signal(json!({ "new_root": "", "new_dir": "" }));
                }
            }
            "remove_root" => core.update_config(|c| c.project_roots.retain(|r| *r != payload)),
            "remove_dir" => core.update_config(|c| c.project_dirs.retain(|r| *r != payload)),
            "set_mobile" => {
                let on = a.signal("mobile").and_then(Value::as_bool).unwrap_or(true);
                core.update_config(|c| c.mobile = on);
            }
            "pair" => {
                let token = core.new_pairing_token();
                match core.endpoint_id.get() {
                    Some(id) => self.ticket = Some(grassline_proto::ticket(id, &token)),
                    None => self.fail("The iroh endpoint is not up yet; try again in a moment."),
                }
            }
            "unpair" => core.update_config(|c| c.paired.retain(|p| p.key != payload)),
            "set_http" => {
                let on = a.signal("http_on").and_then(Value::as_bool).unwrap_or(true);
                core.update_config(|c| c.admin_http.enabled = on);
                if !on {
                    self.ok("HTTP is off: this session stays open, but new ones can only come over iroh.");
                }
            }
            "set_addr" => {
                let addr = sig(a, "http_addr");
                if addr.parse::<std::net::SocketAddr>().is_err() {
                    self.fail(format!("{addr} is not an address like 127.0.0.1:7878"));
                } else {
                    core.update_config(|c| c.admin_http.addr = addr);
                    // The listener moves a moment later.
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            "set_iroh" => {
                let on = a.signal("iroh_on").and_then(Value::as_bool).unwrap_or(false);
                core.update_config(|c| c.admin_iroh = on);
            }
            "add_key" => {
                let key = sig(a, "new_key").to_lowercase();
                if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                    self.fail("A key is 64 hexadecimal characters.");
                } else {
                    core.update_config(|c| {
                        if !c.admin_keys.contains(&key) {
                            c.admin_keys.push(key);
                        }
                    });
                    out.signal(json!({ "new_key": "" }));
                }
            }
            "remove_key" => core.update_config(|c| c.admin_keys.retain(|k| *k != payload)),
            "set_provider" => {
                let p = sig(a, "provider");
                core.update_config(|c| c.default_provider = p);
            }
            "set_auto" => {
                let on = a.signal("auto").and_then(Value::as_bool).unwrap_or(false);
                core.update_config(|c| c.auto_approve = on);
            }
            "set_bin" => {
                let bin = sig(a, &format!("bin.{payload}"));
                core.update_config(|c| {
                    if bin.is_empty() {
                        c.binaries.remove(&payload);
                    } else {
                        c.binaries.insert(payload.clone(), bin);
                    }
                });
            }
            "stop" => core.stop(&payload),
            "delete" => core.delete_session(&payload),
            _ => return,
        }
        self.render(out);
    }

    fn tick(&mut self, out: &mut Outbox, _now_ms: f64) {
        if Self::sessions_snapshot() != self.shown {
            self.render_sessions(out);
        }
    }

    fn tick_interval(&self) -> Option<u32> {
        Some(2000)
    }
}

#[allow(dead_code)]
fn _assert_element(_: Element) {}
