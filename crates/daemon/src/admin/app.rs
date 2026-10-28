//! The admin interface itself.

use super::view::{Tree, View};
use crate::config::SPEECH_PROVIDERS;
#[cfg(test)]
use crate::core::random_hex;
use crate::core::{now_ms, Core};
use grassline_agents::ALL;
use hypereffect::ui::*;
use hypereffect::{cel, json, Action, App, Element, Handler, Metadata, Outbox, Value};
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
    /// What the renderer has.
    view: View,
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

/// A heading and a form row: an input and the button that sends it.
fn add_row(out: &mut Tree, parent: &str, id: &str, placeholder: &str, label: &str, action: &str) {
    let row_id = format!("{id}-row");
    out.element(row(row_id.as_str()).parent(parent));
    out.element(
        input(id)
            .parent(row_id.as_str())
            .placeholder(placeholder)
            .bind(id)
            .on_submit(Handler::action(action)),
    );
    out.element(
        button(format!("{id}-go"))
            .parent(row_id.as_str())
            .variant("primary")
            .label(label)
            .on_press(Handler::action(action)),
    );
}

/// A control for a setting the server has to confirm, kept in the signal
/// `{key: {value, loading}}`. A change marks it loading, which disables it,
/// until the server answers with what it actually applied (see
/// [`Admin::settings`]).
fn setting(e: Element, key: &str, action: &str) -> Element {
    e.bind(format!("{key}.value"))
        .disabled(cel(format!("s.{key}.loading")))
        .on_change(Handler::action(action).with_patch(json!({ key: { "loading": true } })))
}

/// A line with a remove button.
fn item(
    out: &mut Tree,
    parent: &str,
    id: &str,
    label: String,
    action: &str,
    payload: &str,
    button_label: &str,
) {
    out.element(row(id).parent(parent).justify("between"));
    out.element(text(format!("{id}-t")).parent(id).text(label));
    out.element(
        button(format!("{id}-x"))
            .parent(id)
            .variant("plain")
            .label(button_label)
            .on_press(Handler::action(action).payload(payload)),
    );
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

    fn sessions(out: &mut Tree) {
        out.element(column("sessions-body").parent("sessions"));
        let sessions = core().sessions();
        if sessions.is_empty() {
            out.element(
                text("sessions-none")
                    .parent("sessions-body")
                    .variant("caption")
                    .text("No sessions yet. Sessions are started from the remote controller."),
            );
        }
        for s in &sessions {
            let id = format!("s-{}", s.id);
            out.element(card(id.as_str()).parent("sessions-body"));
            out.element(
                row(format!("{id}-head"))
                    .parent(id.as_str())
                    .justify("between"),
            );
            out.element(
                text(format!("{id}-name"))
                    .parent(format!("{id}-head"))
                    .variant("h3")
                    .text(s.project.as_str()),
            );
            out.element(
                badge(format!("{id}-state"))
                    .parent(format!("{id}-head"))
                    .variant(if s.running { "success" } else { "neutral" })
                    .text(if s.running {
                        format!("{} working", s.provider)
                    } else {
                        s.provider.clone()
                    }),
            );
            let line = if s.title.is_empty() {
                "(empty)".to_string()
            } else {
                s.title.clone()
            };
            out.element(text(format!("{id}-title")).parent(id.as_str()).text(line));
            out.element(
                text(format!("{id}-when"))
                    .parent(id.as_str())
                    .variant("caption")
                    .text(format!("{} · {} · {}", s.id, s.path, ago(s.updated))),
            );
            out.element(row(format!("{id}-btns")).parent(id.as_str()).justify("end"));
            if s.running {
                out.element(
                    button(format!("{id}-stop"))
                        .parent(format!("{id}-btns"))
                        .label("Stop")
                        .on_press(Handler::action("stop").payload(s.id.as_str())),
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
    }

    /// Sends what changed since the last time.
    fn render(&mut self, out: &mut Outbox) {
        let mut tree = Tree::default();
        self.build(&mut tree);
        self.view.sync(out, tree);
        self.shown = Self::sessions_snapshot();
    }

    /// The whole interface.
    fn build(&self, out: &mut Tree) {
        let cfg = core().config();
        out.element(column("root"));
        out.element(text("title").parent("root").variant("h1").text("Grassline"));
        out.element(column("notes").parent("root"));
        if let Some((msg, variant)) = &self.note {
            out.element(
                notice("note")
                    .parent("notes")
                    .variant(*variant)
                    .text(msg.as_str()),
            );
        }
        out.element(tabs("tabs").parent("root"));

        // ---- access: the remote controllers and their sessions
        out.element(tab("tab-access").parent("tabs").label("Access"));
        out.element(column("mobile-body").parent("tab-access"));
        match core().endpoint_id.get() {
            Some(id) => out.element(
                text("m-id")
                    .parent("mobile-body")
                    .variant("caption")
                    .text(format!("Daemon key: {id}")),
            ),
            None => out.element(
                text("m-id")
                    .parent("mobile-body")
                    .variant("caption")
                    .text("The iroh endpoint is not up yet."),
            ),
        }
        out.element(
            button("pair")
                .parent("mobile-body")
                .variant("primary")
                .label("New pairing code")
                .on_press(Handler::action("pair")),
        );
        if let Some((t, _)) = &self.ticket {
            out.element(
                card("ticket").parent("mobile-body").title(
                    "Scan this with the app, or paste the code (valid for 10 minutes, once)",
                ),
            );
            if let Some(src) = super::qr::data_uri(t) {
                out.element(
                    image("ticket-qr")
                        .parent("ticket")
                        .src(src)
                        .alt("QR code of the pairing code"),
                );
            }
            out.element(
                text("ticket-t")
                    .parent("ticket")
                    .variant("code")
                    .text(t.as_str()),
            );
        }
        out.element(
            text("m-dev-h")
                .parent("mobile-body")
                .variant("h3")
                .text("Paired devices"),
        );
        out.element(list("devices").parent("mobile-body").empty("No devices."));
        for (i, d) in cfg.paired.iter().enumerate() {
            let short: String = d.key.chars().take(12).collect();
            item(
                out,
                "devices",
                &format!("dev-{i}"),
                format!("{} ({short}…)", d.name),
                "unpair",
                &d.key,
                "Revoke",
            );
        }
        out.element(column("sessions").parent("tab-access"));
        out.element(
            text("sessions-h")
                .parent("sessions")
                .variant("h3")
                .text("Sessions"),
        );
        Self::sessions(out);

        // ---- projects
        out.element(tab("tab-projects").parent("tabs").label("Projects"));
        out.element(column("projects-body").parent("tab-projects"));
        out.element(text("p-help").parent("projects-body").variant("caption").text(
            "Every directory inside a root is a project. A single directory can also be a project of its own.",
        ));
        out.element(
            text("p-roots-h")
                .parent("projects-body")
                .variant("h3")
                .text("Roots"),
        );
        for (i, r) in cfg.project_roots.iter().enumerate() {
            item(
                out,
                "projects-body",
                &format!("root-{i}"),
                r.clone(),
                "remove_root",
                r,
                "Remove",
            );
        }
        add_row(
            out,
            "projects-body",
            "new_root",
            "/home/me/code",
            "Add root",
            "add_root",
        );
        out.element(
            text("p-dirs-h")
                .parent("projects-body")
                .variant("h3")
                .text("Single projects"),
        );
        for (i, r) in cfg.project_dirs.iter().enumerate() {
            item(
                out,
                "projects-body",
                &format!("dir-{i}"),
                r.clone(),
                "remove_dir",
                r,
                "Remove",
            );
        }
        add_row(
            out,
            "projects-body",
            "new_dir",
            "/home/me/some/project",
            "Add project",
            "add_dir",
        );
        out.element(
            text("p-found-h")
                .parent("projects-body")
                .variant("h3")
                .text("Available projects"),
        );
        out.element(list("found").parent("projects-body").empty("None yet."));
        for (i, p) in core().projects().into_iter().enumerate() {
            out.element(
                text(format!("found-{i}"))
                    .parent("found")
                    .text(format!("{}  ·  {}", p.name, p.path)),
            );
        }

        // ---- admin
        out.element(tab("tab-admin").parent("tabs").label("Admin"));
        out.element(column("admin-body").parent("tab-admin"));
        let status = super::HTTP_STATUS.lock().unwrap().clone();
        out.element(
            text("a-http-h")
                .parent("admin-body")
                .variant("h3")
                .text("HTTP"),
        );
        out.element(setting(
            checkbox("http_on")
                .parent("admin-body")
                .variant("switch")
                .label("Serve the admin interface over HTTP"),
            "http_on",
            "set_http",
        ));
        out.element(
            text("a-status")
                .parent("admin-body")
                .variant("caption")
                .text(status),
        );
        add_row(
            out,
            "admin-body",
            "http_addr",
            "127.0.0.1:7878",
            "Rebind",
            "set_addr",
        );
        if !cfg.admin_http.addr.starts_with("127.")
            && !cfg.admin_http.addr.starts_with("localhost")
            && !cfg.admin_http.addr.starts_with("[::1]")
        {
            out.element(notice("a-warn").parent("admin-body").variant("warning").text(
                "Not bound to localhost: other machines must prove a key from the list below to get in.",
            ));
        }
        out.element(
            text("a-iroh-h")
                .parent("admin-body")
                .variant("h3")
                .text("Iroh"),
        );
        out.element(setting(
            checkbox("iroh_on")
                .parent("admin-body")
                .variant("switch")
                .label("Serve the admin interface over iroh"),
            "iroh_on",
            "set_iroh",
        ));
        if let Some(id) = core().endpoint_id.get().filter(|_| cfg.admin_iroh) {
            let url = format!("hyperb://{id}/");
            if let Some(src) = super::qr::data_uri(&url) {
                out.element(
                    image("a-qr")
                        .parent("admin-body")
                        .src(src)
                        .alt("QR code of the iroh address"),
                );
            }
            out.element(
                text("a-addr")
                    .parent("admin-body")
                    .variant("code")
                    .text(url),
            );
        }
        out.element(
            text("a-keys-h")
                .parent("admin-body")
                .variant("h3")
                .text("Allowed admin keys"),
        );
        for (i, k) in cfg.admin_keys.iter().enumerate() {
            item(
                out,
                "admin-body",
                &format!("key-{i}"),
                k.clone(),
                "remove_key",
                k,
                "Remove",
            );
        }
        add_row(
            out,
            "admin-body",
            "new_key",
            "hyperb identity (64 hex)",
            "Allow key",
            "add_key",
        );

        // ---- agents
        out.element(tab("tab-agents").parent("tabs").label("Agents"));
        out.element(column("agents-body").parent("tab-agents"));
        out.element(setting(
            select("provider")
                .parent("agents-body")
                .label("Default agent")
                .variant("segmented")
                .options(Value::Array(
                    ALL.iter()
                        .map(|p| json!({ "value": p.name(), "label": p.name() }))
                        .collect(),
                )),
            "provider",
            "set_provider",
        ));
        out.element(setting(
            checkbox("auto")
                .parent("agents-body")
                .variant("switch")
                .label("Let agents run tools without asking (otherwise they ask the app)"),
            "auto",
            "set_auto",
        ));
        for p in ALL {
            let opts = cfg.options(p);
            let found = p.detect(&opts);
            let id = format!("prov-{}", p.name());
            out.element(card(id.as_str()).parent("agents-body").title(p.name()));
            out.element(
                badge(format!("{id}-b"))
                    .parent(id.as_str())
                    .variant(if found.is_some() {
                        "success"
                    } else {
                        "failure"
                    })
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
                button(format!("{id}-save"))
                    .parent(format!("{id}-row"))
                    .label("Save")
                    .on_press(Handler::action("set_bin").payload(p.name())),
            );
        }

        // ---- voice
        out.element(tab("tab-voice").parent("tabs").label("Voice"));
        out.element(column("voice-body").parent("tab-voice"));
        out.element(text("v-help").parent("voice-body").variant("caption").text(
            "Voice messages from the app are turned into text by this service. The app calls it directly with this key: the audio does not go through the daemon.",
        ));
        let mut options = vec![json!({ "value": "", "label": "Off" })];
        options.extend(
            SPEECH_PROVIDERS
                .iter()
                .map(|p| json!({ "value": p.name, "label": p.label })),
        );
        options.push(json!({ "value": "custom", "label": "Custom" }));
        out.element(setting(
            select("stt")
                .parent("voice-body")
                .label("Speech to text")
                .variant("segmented")
                .options(Value::Array(options)),
            "stt",
            "set_stt",
        ));
        let t = &cfg.transcription;
        if !t.provider.is_empty() {
            let preset = SPEECH_PROVIDERS.iter().find(|p| p.name == t.provider);
            out.element(
                input("stt-key")
                    .parent("voice-body")
                    .label("API key")
                    .bind("stt_key")
                    .on_submit(Handler::action("set_stt_details")),
            );
            out.element(
                input("stt-model")
                    .parent("voice-body")
                    .label("Model")
                    .placeholder(preset.map_or("whisper-1", |p| p.model))
                    .bind("stt_model")
                    .on_submit(Handler::action("set_stt_details")),
            );
            if preset.is_none() {
                out.element(
                    input("stt-url")
                        .parent("voice-body")
                        .label("Endpoint (OpenAI-style audio/transcriptions)")
                        .placeholder("http://localhost:8000/v1/audio/transcriptions")
                        .bind("stt_url")
                        .on_submit(Handler::action("set_stt_details")),
                );
            }
            out.element(row("stt-row").parent("voice-body").justify("between"));
            out.element(
                badge("stt-state")
                    .parent("stt-row")
                    .variant(if t.resolve().is_some() {
                        "success"
                    } else {
                        "warning"
                    })
                    .text(match t.resolve() {
                        Some(r) => format!("the app uses {}", r.model),
                        None if preset.is_none() => "needs an endpoint, a model and a key".into(),
                        None => "needs a key".into(),
                    }),
            );
            out.element(
                button("stt-save")
                    .parent("stt-row")
                    .variant("primary")
                    .label("Save")
                    .on_press(Handler::action("set_stt_details")),
            );
        }
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
            "auto": s(c.auto_approve.into()),
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

impl App for Admin {
    fn hello(&mut self, out: &mut Outbox, _: &BTreeMap<String, String>) {
        out.metadata(
            Metadata::new()
                .name("Grassline")
                .description("Runs coding agents for the Grassline app")
                .icon(ICON),
        );
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
            "set_iroh" => {
                let on = a
                    .signal("iroh_on.value")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
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
                let p = sig(a, "provider.value");
                core.update_config(|c| c.default_provider = p);
            }
            "set_auto" => {
                let on = a
                    .signal("auto.value")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
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

#[allow(dead_code)]
fn _assert_element(_: Element) {}

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
        assert_eq!(l.client.metadata().name.as_deref(), Some("Grassline"));
        assert_eq!(l.client.metadata().icon.as_deref(), Some(ICON));
        assert!(l.trigger("pair", "onPress"));
        let src = l.client.attr("ticket-qr", "src").expect("a QR code");
        assert!(src.as_str().unwrap().starts_with("data:image/png;base64,"));
        assert!(l.client.take_diagnostics().is_empty());

        // A setting waits for the server, which answers with what it
        // applied, and sends no elements when nothing else changed.
        l.write("auto.value", json!(true));
        let line = l.client.trigger("auto", "onChange").expect("an action");
        assert_eq!(l.client.attr("auto", "disabled"), Some(json!(true)));
        l.server.receive(line.as_bytes());
        let out = l.server.output().take();
        assert!(!out.contains("element "), "{out}");
        l.client.receive(out.as_bytes());
        assert_eq!(l.client.attr("auto", "disabled"), Some(json!(false)));
        assert!(super::core().config().auto_approve);

        // Speech to text asks for a key once a provider is picked.
        assert_eq!(l.client.attr("stt-key", "bind"), None);
        l.write("stt.value", json!("groq"));
        assert!(l.trigger("stt", "onChange"));
        assert!(l.client.attr("stt-key", "bind").is_some());
        l.write("stt_key", json!("k"));
        assert!(l.trigger("stt-save", "onPress"));
        let t = super::core().config().transcription.resolve().unwrap();
        assert_eq!(t.model, "whisper-large-v3-turbo");
        assert!(l.client.take_diagnostics().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
