//! The admin interface itself.

use crate::config::{hostname, Config, SPEECH_PROVIDERS};
#[cfg(test)]
use crate::core::random_hex;
use crate::core::{now_ms, Core};
use grassline_agents::ALL;
use hypereffect::vtree::{
    self, badge, button, card, checkbox, column, image, input, list, notice, row, select, tab,
    tabs, text, Node,
};
use hypereffect::{cel, json, Action, App, Handler, Metadata, Outbox, Value};
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
    /// What the sessions list showed last, to redraw it only when it changes.
    shown: String,
    /// A one-time pairing ticket on display, and its token.
    ticket: Option<(String, String)>,
    /// The last thing that went wrong or was done.
    note: Option<(String, &'static str)>,
    /// When the note goes away by itself (ms since the epoch).
    note_until: Option<u64>,
    /// The tree the renderer has, once it has been sent.
    view: Option<Node>,
}

fn sig(a: &Action, path: &str) -> String {
    a.signal(path)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
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

fn heading(id: &str, label: &str) -> Node {
    text(id).variant("h3").text(label)
}

fn caption(id: &str, label: impl Into<Value>) -> Node {
    text(id).variant("caption").text(label)
}

/// A form row: an input and the button that sends it.
fn add_row(id: &str, placeholder: &str, label: &str, action: &str) -> Node {
    row(format!("{id}-row")).children([
        input(id)
            .placeholder(placeholder)
            .bind(id)
            .on_submit(Handler::action(action)),
        button(format!("{id}-go"))
            .variant("primary")
            .label(label)
            .on_press(Handler::action(action)),
    ])
}

/// A control for a setting the server has to confirm, kept in the signal
/// `{key: {value, loading}}`. A change marks it loading, which disables it,
/// until the server answers with what it actually applied (see
/// [`Admin::settings`]).
fn setting(n: Node, key: &str, action: &str) -> Node {
    n.bind(format!("{key}.value"))
        .disabled(cel(format!("s.{key}.loading")))
        .on_change(Handler::action(action).with_patch(json!({ key: { "loading": true } })))
}

/// A line with a remove button.
fn item(id: &str, label: String, action: &str, payload: &str, button_label: &str) -> Node {
    row(id).justify("between").children([
        text(format!("{id}-t")).text(label),
        button(format!("{id}-x"))
            .variant("plain")
            .label(button_label)
            .on_press(Handler::action(action).payload(payload)),
    ])
}

impl Admin {
    /// What the sessions list shows, to redraw it only when that changes. The
    /// minute is in it because the relative times change with it.
    fn sessions_snapshot() -> String {
        let sessions = core().sessions();
        let rows: Vec<_> = sessions
            .iter()
            .map(|s| (&s.id, ago(s.updated), s.running, &s.title, &s.provider))
            .collect();
        serde_json_string(&rows)
    }

    fn sessions() -> Node {
        let sessions = core().sessions();
        let none = sessions.is_empty().then(|| {
            caption(
                "sessions-none",
                "No sessions yet. Sessions are started from the remote controller.",
            )
        });
        column("sessions-body")
            .children(none)
            .children(sessions.iter().map(|s| {
                let id = format!("s-{}", s.id);
                let line = if s.title.is_empty() {
                    "(empty)".to_string()
                } else {
                    s.title.clone()
                };
                let stop = s.running.then(|| {
                    button(format!("{id}-stop"))
                        .label("Stop")
                        .on_press(Handler::action("stop").payload(s.id.as_str()))
                });
                card(id.as_str()).children([
                    row(format!("{id}-head")).justify("between").children([
                        text(format!("{id}-name"))
                            .variant("h3")
                            .text(s.project.as_str()),
                        badge(format!("{id}-state"))
                            .variant(if s.running { "success" } else { "neutral" })
                            .text(if s.running {
                                format!("{} working", s.provider)
                            } else {
                                s.provider.clone()
                            }),
                    ]),
                    text(format!("{id}-title")).text(line),
                    caption(
                        &format!("{id}-when"),
                        format!("{} · {} · {}", s.id, s.path, ago(s.updated)),
                    ),
                    row(format!("{id}-btns"))
                        .justify("end")
                        .children(stop)
                        .child(
                            button(format!("{id}-del"))
                                .variant("failure")
                                .label("Delete")
                                .on_press(Handler::action("delete").payload(s.id.as_str())),
                        ),
                ])
            }))
    }

    /// Sends what changed since the last time.
    fn render(&mut self, out: &mut Outbox) {
        let tree = self.build();
        match &self.view {
            Some(old) => out.extend(vtree::diff(old, &tree)),
            None => out.extend(vtree::render(&tree)),
        }
        self.view = Some(tree);
        self.shown = Self::sessions_snapshot();
    }

    /// The whole interface.
    fn build(&self) -> Node {
        let cfg = core().config();
        let note = self
            .note
            .as_ref()
            .map(|(msg, variant)| notice("note").variant(*variant).text(msg.as_str()));
        column("root").children([
            text("title").variant("h1").text("Grassline"),
            column("notes").children(note),
            tabs("tabs").children([
                self.access(&cfg),
                Self::projects(&cfg),
                Self::admin(&cfg),
                Self::agents(&cfg),
                Self::voice(&cfg),
            ]),
        ])
    }

    /// The remote controllers and their sessions.
    fn access(&self, cfg: &Config) -> Node {
        let key = match core().endpoint_id.get() {
            Some(id) => format!("Daemon key: {id}"),
            None => "The iroh endpoint is not up yet.".into(),
        };
        let ticket = self.ticket.as_ref().map(|(t, _)| {
            let qr = super::qr::data_uri(t).map(|src| {
                image("ticket-qr")
                    .src(src)
                    .alt("QR code of the pairing code")
            });
            card("ticket")
                .title("Scan this with the app, or paste the code (valid for 10 minutes, once)")
                .children(qr)
                .child(text("ticket-t").variant("code").text(t.as_str()))
        });
        let devices = cfg.paired.iter().enumerate().map(|(i, d)| {
            let short: String = d.key.chars().take(12).collect();
            item(
                &format!("dev-{i}"),
                format!("{} ({short}…)", d.name),
                "unpair",
                &d.key,
                "Revoke",
            )
        });
        tab("tab-access").label("Access").children([
            column("mobile-body")
                .child(caption("m-id", key))
                .child(
                    button("pair")
                        .variant("primary")
                        .label("New pairing code")
                        .on_press(Handler::action("pair")),
                )
                .children(ticket)
                .child(heading("m-dev-h", "Paired devices"))
                .child(list("devices").empty("No devices.").children(devices)),
            column("sessions")
                .child(heading("sessions-h", "Sessions"))
                .child(Self::sessions()),
        ])
    }

    fn projects(cfg: &Config) -> Node {
        let roots = cfg
            .project_roots
            .iter()
            .enumerate()
            .map(|(i, r)| item(&format!("root-{i}"), r.clone(), "remove_root", r, "Remove"));
        let dirs = cfg
            .project_dirs
            .iter()
            .enumerate()
            .map(|(i, r)| item(&format!("dir-{i}"), r.clone(), "remove_dir", r, "Remove"));
        let found =
            core().projects().into_iter().enumerate().map(|(i, p)| {
                text(format!("found-{i}")).text(format!("{}  ·  {}", p.name, p.path))
            });
        tab("tab-projects").label("Projects").child(
            column("projects-body")
                .child(caption(
                    "p-help",
                    "Every directory inside a root is a project. A single directory can also be a project of its own.",
                ))
                .child(heading("p-roots-h", "Roots"))
                .children(roots)
                .child(add_row("new_root", "/home/me/code", "Add root", "add_root"))
                .child(heading("p-dirs-h", "Single projects"))
                .children(dirs)
                .child(add_row(
                    "new_dir",
                    "/home/me/some/project",
                    "Add project",
                    "add_dir",
                ))
                .child(heading("p-found-h", "Available projects"))
                .child(list("found").empty("None yet.").children(found)),
        )
    }

    fn admin(cfg: &Config) -> Node {
        let hostname = hostname();
        let status = super::HTTP_STATUS.lock().unwrap().clone();
        let exposed = !cfg.admin_http.addr.starts_with("127.")
            && !cfg.admin_http.addr.starts_with("localhost")
            && !cfg.admin_http.addr.starts_with("[::1]");
        let warning = exposed.then(|| {
            notice("a-warn").variant("warning").text(
                "Not bound to localhost: other machines must prove a key from the list below to get in.",
            )
        });
        let iroh = core()
            .endpoint_id
            .get()
            .filter(|_| cfg.admin_iroh)
            .map(|id| format!("hyperb://{id}/"));
        let iroh_qr = iroh
            .as_deref()
            .and_then(super::qr::data_uri)
            .map(|src| image("a-qr").src(src).alt("QR code of the iroh address"));
        let iroh_addr = iroh.map(|url| text("a-addr").variant("code").text(url));
        let keys = cfg
            .admin_keys
            .iter()
            .enumerate()
            .map(|(i, k)| item(&format!("key-{i}"), k.clone(), "remove_key", k, "Remove"));
        tab("tab-admin").label("Admin").child(
            column("admin-body")
                .child(heading("a-name-h", "Machine name"))
                .child(caption(
                    "a-name-help",
                    format!(
                        "Shown as grassline@name in hyperb and to paired phones. Empty uses {hostname}."
                    ),
                ))
                .child(add_row("machine_name", &hostname, "Rename", "set_name"))
                .child(heading("a-http-h", "HTTP"))
                .child(setting(
                    checkbox("http_on")
                        .variant("switch")
                        .label("Serve the admin interface over HTTP"),
                    "http_on",
                    "set_http",
                ))
                .child(caption("a-status", status))
                .child(add_row("http_addr", "127.0.0.1:7878", "Rebind", "set_addr"))
                .children(warning)
                .child(heading("a-iroh-h", "Iroh"))
                .child(setting(
                    checkbox("iroh_on")
                        .variant("switch")
                        .label("Serve the admin interface over iroh"),
                    "iroh_on",
                    "set_iroh",
                ))
                .children(iroh_qr)
                .children(iroh_addr)
                .child(heading("a-keys-h", "Allowed admin keys"))
                .children(keys)
                .child(add_row(
                    "new_key",
                    "hyperb identity (64 hex)",
                    "Allow key",
                    "add_key",
                )),
        )
    }

    fn agents(cfg: &Config) -> Node {
        let providers = ALL.into_iter().map(|p| {
            let found = p.detect(&cfg.options(p));
            let id = format!("prov-{}", p.name());
            card(id.as_str()).title(p.name()).children([
                badge(format!("{id}-b"))
                    .variant(if found.is_some() {
                        "success"
                    } else {
                        "failure"
                    })
                    .text(match &found {
                        Some(path) => path.to_string_lossy().into_owned(),
                        None => "not found".into(),
                    }),
                row(format!("{id}-row")).children([
                    input(format!("{id}-bin"))
                        .placeholder("binary path (optional)")
                        .bind(format!("bin.{}", p.name()))
                        .on_submit(Handler::action("set_bin").payload(p.name())),
                    button(format!("{id}-save"))
                        .label("Save")
                        .on_press(Handler::action("set_bin").payload(p.name())),
                ]),
            ])
        });
        tab("tab-agents").label("Agents").child(
            column("agents-body")
                .child(setting(
                    select("provider")
                        .label("Default agent")
                        .variant("segmented")
                        .options(Value::Array(
                            ALL.iter()
                                .map(|p| json!({ "value": p.name(), "label": p.name() }))
                                .collect(),
                        )),
                    "provider",
                    "set_provider",
                ))
                .child(setting(
                    select("permissions")
                        .label("New sessions run tools (the app changes it for each session)")
                        .variant("segmented")
                        .options(json!([
                            { "value": "ask", "label": "Asking the app" },
                            { "value": "auto", "label": "Asking for risky ones" },
                            { "value": "all", "label": "Without asking" },
                        ])),
                    "permissions",
                    "set_permissions",
                ))
                .children(providers),
        )
    }

    fn voice(cfg: &Config) -> Node {
        let mut options = vec![json!({ "value": "", "label": "Off" })];
        options.extend(
            SPEECH_PROVIDERS
                .iter()
                .map(|p| json!({ "value": p.name, "label": p.label })),
        );
        options.push(json!({ "value": "custom", "label": "Custom" }));
        let body = column("voice-body")
            .child(caption(
                "v-help",
                "Voice messages from the app are turned into text by this service. The app calls it directly with this key: the audio does not go through the daemon.",
            ))
            .child(setting(
                select("stt")
                    .label("Speech to text")
                    .variant("segmented")
                    .options(Value::Array(options)),
                "stt",
                "set_stt",
            ));
        let t = &cfg.transcription;
        if t.provider.is_empty() {
            return tab("tab-voice").label("Voice").child(body);
        }
        let preset = SPEECH_PROVIDERS.iter().find(|p| p.name == t.provider);
        let url = preset.is_none().then(|| {
            input("stt-url")
                .label("Endpoint (OpenAI-style audio/transcriptions)")
                .placeholder("http://localhost:8000/v1/audio/transcriptions")
                .bind("stt_url")
                .on_submit(Handler::action("set_stt_details"))
        });
        let body = body
            .child(
                input("stt-key")
                    .label("API key")
                    .bind("stt_key")
                    .on_submit(Handler::action("set_stt_details")),
            )
            .child(
                input("stt-model")
                    .label("Model")
                    .placeholder(preset.map_or("whisper-1", |p| p.model))
                    .bind("stt_model")
                    .on_submit(Handler::action("set_stt_details")),
            )
            .children(url)
            .child(
                row("stt-row").justify("between").children([
                    badge("stt-state")
                        .variant(if t.resolve().is_some() {
                            "success"
                        } else {
                            "warning"
                        })
                        .text(match t.resolve() {
                            Some(r) => format!("the app uses {}", r.model),
                            None if preset.is_none() => {
                                "needs an endpoint, a model and a key".into()
                            }
                            None => "needs a key".into(),
                        }),
                    button("stt-save")
                        .variant("primary")
                        .label("Save")
                        .on_press(Handler::action("set_stt_details")),
                ]),
            );
        tab("tab-voice").label("Voice").child(body)
    }

    /// The settings as they are, which every action answers with: it puts
    /// back a control the server did not follow, and ends its loading.
    fn settings() -> Value {
        let c = core().config();
        let s = |v: Value| json!({ "value": v, "loading": false });
        json!({
            "http_on": s(c.admin_http.enabled.into()),
            "iroh_on": s(c.admin_iroh.into()),
            "provider": s(c.default_provider.into()),
            "permissions": s(json!(c.permissions)),
            "stt": s(c.transcription.provider.into()),
        })
    }

    fn signals(&self) -> Value {
        let c = core().config();
        let bins: serde_json::Map<String, Value> = ALL
            .iter()
            .map(|p| {
                (
                    p.name().to_string(),
                    c.binaries.get(p.name()).cloned().unwrap_or_default().into(),
                )
            })
            .collect();
        let mut s = json!({
            "new_root": "", "new_dir": "", "new_key": "",
            "machine_name": c.name.clone(),
            "http_addr": c.admin_http.addr,
            "bin": bins,
            "stt_key": c.transcription.key,
            "stt_model": c.transcription.model,
            "stt_url": c.transcription.url,
        });
        hypereffect::signals::merge_patch(&mut s, &Self::settings());
        s
    }

    fn ok(&mut self, msg: impl Into<String>) {
        self.note = Some((msg.into(), "success"));
    }

    /// A note that goes away after a few seconds.
    fn flash(&mut self, msg: impl Into<String>) {
        self.ok(msg);
        self.note_until = Some(now_ms() + 4000);
    }

    fn fail(&mut self, msg: impl Into<String>) {
        self.note = Some((msg.into(), "failure"));
    }
}

/// assets/logo.svg as a 48px, 8-color PNG: hyperb shows only `data:` icons,
/// and not every renderer reads SVG.
const ICON: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAADAAAAAwBAMAAAClLOS0AAAAGFBMVEUufTLT6tRam14AAACFtoinz6guezMtfDG4CgaaAAAACHRSTlP+//8A//8rqOUUUYoAAAEQSURBVHjatZRBSsQwFIa/eekYZqWDCiIIwaWIBGbhTgQRt3MB7yAIHsJreA5FXDvQhbjVEzizrXRaF63jNHkBFf1Xffn6/uRPmvZGMKErW4xA4CkYp+AEBFMQ6doj7KHoBjyqjFgdXJDQCj+V5Z8kiWdcCviU1cZ3bJcL00uAMtXhFCDJTGPAlTGo+g7WANjtdjx+hpODrmV/E4ZD4DSYoy6bNWWTAFR4Adg5isAD4PB5GPC4cbqPkxuAefaKGtnFezUru0dlFm8M3rYHL+922tbtEciXR9XMkjXV6nQB6tR+tx5tx/qS1d0ff62/uQaH+vCl1DqYma2zXLu0tfCsNZwjzJWVXeUI7Ie/AcstfABPHC2+8Ov20wAAAABJRU5ErkJggg==";

fn serde_json_string<T: serde::Serialize>(v: &T) -> String {
    hypereffect::serde_json::to_string(v).unwrap_or_default()
}

/// What the renderer calls this interface, so that several daemons can be
/// told apart.
fn metadata() -> Metadata {
    Metadata::new()
        .name(core().config().daemon_name())
        .description("Runs coding agents for the Grassline app")
        .icon(ICON)
}

impl App for Admin {
    fn hello(&mut self, out: &mut Outbox, _: &BTreeMap<String, String>) {
        out.metadata(metadata());
        out.signal(self.signals());
        self.render(out);
    }

    fn action(&mut self, out: &mut Outbox, a: &Action) {
        let payload = a.payload_as::<String>().unwrap_or_default();
        self.note = None;
        self.note_until = None;
        let core = core();
        match a.name.as_str() {
            "add_root" | "add_dir" => {
                let path = sig(
                    a,
                    if a.name == "add_root" {
                        "new_root"
                    } else {
                        "new_dir"
                    },
                );
                let expanded = crate::core::expand(&path);
                if path.is_empty() {
                    return;
                } else if !expanded.is_dir() {
                    self.fail(format!("{path} is not a directory"));
                } else {
                    let root = a.name == "add_root";
                    core.update_config(|c| {
                        let list = if root {
                            &mut c.project_roots
                        } else {
                            &mut c.project_dirs
                        };
                        if !list.contains(&path) {
                            list.push(path.clone());
                        }
                    });
                    out.signal(json!({ "new_root": "", "new_dir": "" }));
                }
            }
            "remove_root" => core.update_config(|c| c.project_roots.retain(|r| *r != payload)),
            "remove_dir" => core.update_config(|c| c.project_dirs.retain(|r| *r != payload)),
            "pair" => {
                let token = core.new_pairing_token();
                match core.endpoint_id.get() {
                    Some(id) => self.ticket = Some((grassline_proto::ticket(id, &token), token)),
                    None => self.fail("The iroh endpoint is not up yet; try again in a moment."),
                }
            }
            "unpair" => core.update_config(|c| c.paired.retain(|p| p.key != payload)),
            "set_http" => {
                let on = a
                    .signal("http_on.value")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
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
            "set_name" => {
                let name = sig(a, "machine_name");
                core.update_config(|c| c.name = name.clone());
                out.signal(json!({ "machine_name": name }));
                out.metadata(metadata());
                self.flash("Saved");
            }
            "set_iroh" => {
                let on = a
                    .signal("iroh_on.value")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                core.update_config(|c| c.admin_iroh = on);
                if on {
                    if let Some(id) = core.endpoint_id.get() {
                        println!("admin interface on hyperb://{id}/ (over iroh)");
                    }
                }
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
                let p = sig(a, "provider.value");
                core.update_config(|c| c.default_provider = p);
            }
            "set_permissions" => {
                let p = a.signal("permissions.value").cloned().unwrap_or_default();
                if let Ok(p) = serde_json::from_value(p) {
                    core.update_config(|c| c.permissions = p);
                }
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
            "set_stt" => {
                let p = sig(a, "stt.value");
                core.update_config(|c| c.transcription.provider = p);
            }
            "set_stt_details" => {
                let (key, model, url) = (sig(a, "stt_key"), sig(a, "stt_model"), sig(a, "stt_url"));
                if !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
                    self.fail(format!("{url} is not an http(s) URL"));
                } else {
                    core.update_config(|c| {
                        c.transcription.key = key;
                        c.transcription.model = model;
                        c.transcription.url = url;
                    });
                    self.flash("Saved");
                }
            }
            "stop" => core.stop(&payload),
            "delete" => core.delete_session(&payload),
            _ => return,
        }
        out.signal(Self::settings());
        self.render(out);
    }

    fn tick(&mut self, out: &mut Outbox, _now_ms: f64) {
        if self.note_until.is_some_and(|t| now_ms() >= t) {
            self.note = None;
            self.note_until = None;
            self.render(out);
        }
        // A code that was used or expired goes away; a new device shows up.
        if let Some((_, token)) = &self.ticket {
            if !core().token_pending(token) {
                self.ticket = None;
                self.shown.clear();
            }
        }
        if Self::sessions_snapshot() != self.shown {
            self.render(out);
        }
    }

    fn tick_interval(&self) -> Option<u32> {
        // Sooner while a note is about to go away.
        Some(if self.note_until.is_some() { 250 } else { 2000 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypereffect::server::Loopback;

    #[test]
    fn pairing_code_has_a_qr_code() {
        let dir = std::env::temp_dir().join(format!("grassline-test-{}", random_hex(4)));
        let core = Core::open(dir.join("config.json"), dir.join("data"));
        let _ = core.endpoint_id.set("e".repeat(52));
        install(core);
        let mut l = Loopback::<Admin>::new();
        let expected = format!("grassline@{}", super::core().config().machine_name());
        assert_eq!(l.client.metadata().name.as_deref(), Some(expected.as_str()));
        assert_eq!(l.client.metadata().icon.as_deref(), Some(ICON));
        assert!(l.trigger("pair", "onPress"));
        let src = l.client.attr("ticket-qr", "src").expect("a QR code");
        assert!(src.as_str().unwrap().starts_with("data:image/png;base64,"));
        assert!(l.client.take_diagnostics().is_empty());

        // A setting waits for the server, which answers with what it
        // applied, and sends no elements when nothing else changed.
        l.write("permissions.value", json!("auto"));
        let line = l
            .client
            .trigger("permissions", "onChange")
            .expect("an action");
        assert_eq!(l.client.attr("permissions", "disabled"), Some(json!(true)));
        l.server.receive(line.as_bytes());
        let out = l.server.output().take();
        assert!(!out.contains("element "), "{out}");
        l.client.receive(out.as_bytes());
        assert_eq!(l.client.attr("permissions", "disabled"), Some(json!(false)));
        assert_eq!(
            super::core().config().permissions,
            grassline_proto::Permissions::Auto
        );

        // Speech to text asks for a key once a provider is picked.
        assert_eq!(l.client.attr("stt-key", "bind"), None);
        l.write("stt.value", json!("groq"));
        assert!(l.trigger("stt", "onChange"));
        assert!(l.client.attr("stt-key", "bind").is_some());
        l.write("stt_key", json!("k"));
        assert!(l.trigger("stt-save", "onPress"));
        let t = super::core().config().transcription.resolve().unwrap();
        assert_eq!(t.model, "whisper-large-v3-turbo");

        // The machine can be renamed, and an empty name goes back to the
        // hostname.
        l.write("machine_name", json!("Studio"));
        assert!(l.trigger("machine_name-go", "onPress"));
        assert_eq!(
            l.client.metadata().name.as_deref(),
            Some("grassline@Studio")
        );
        assert_eq!(super::core().config().machine_name(), "Studio");
        l.write("machine_name", json!(""));
        assert!(l.trigger("machine_name-go", "onPress"));
        assert_eq!(l.client.metadata().name.as_deref(), Some(expected.as_str()));

        assert!(l.client.take_diagnostics().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
