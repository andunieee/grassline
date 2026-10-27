//! The daemon's state: settings, projects, and the agent sessions.

use crate::config::Config;
use grassline_agents::{Event, Provider, Turn};
use grassline_proto::{Message, Project, Role, SessionInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, watch};

const TOKEN_TTL: Duration = Duration::from_secs(10 * 60);

/// What changed in a session, for the connected controllers and the admin.
#[derive(Debug, Clone)]
pub enum Update {
    Session(SessionInfo),
    Msg { id: String, index: usize, message: Message },
}

#[derive(Serialize, Deserialize)]
struct Session {
    info: SessionInfo,
    /// The provider's own id for the conversation.
    provider_session: Option<String>,
    messages: Vec<Message>,
}

pub struct Core {
    config_path: PathBuf,
    data_dir: PathBuf,
    config: RwLock<Config>,
    /// Bumped on every settings change.
    changes: watch::Sender<u64>,
    sessions: Mutex<HashMap<String, Session>>,
    updates: broadcast::Sender<Update>,
    stops: Mutex<HashMap<String, Arc<tokio::sync::Notify>>>,
    tokens: Mutex<Vec<(String, Instant)>>,
    /// The iroh endpoint id, once the endpoint is up.
    pub endpoint_id: OnceLock<String>,
    pub online: AtomicBool,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

pub fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::getrandom(&mut b).expect("system random number generator");
    hex::encode(b)
}

impl Core {
    pub fn open(config_path: PathBuf, data_dir: PathBuf) -> Arc<Core> {
        let config = Config::load(&config_path);
        let mut sessions = HashMap::new();
        if let Ok(rd) = std::fs::read_dir(data_dir.join("sessions")) {
            for e in rd.flatten() {
                let Ok(s) = std::fs::read_to_string(e.path()) else { continue };
                match serde_json::from_str::<Session>(&s) {
                    Ok(mut s) => {
                        s.info.running = false;
                        sessions.insert(s.info.id.clone(), s);
                    }
                    Err(e2) => eprintln!("{}: {e2}", e.path().display()),
                }
            }
        }
        Arc::new(Core {
            config_path,
            data_dir,
            config: RwLock::new(config),
            changes: watch::channel(0).0,
            sessions: Mutex::new(sessions),
            updates: broadcast::channel(1024).0,
            stops: Default::default(),
            tokens: Default::default(),
            endpoint_id: OnceLock::new(),
            online: AtomicBool::new(false),
        })
    }

    // ---- settings

    pub fn config(&self) -> Config {
        self.config.read().unwrap().clone()
    }

    /// Edits the settings, saves them and tells the watchers.
    pub fn update_config<R>(&self, f: impl FnOnce(&mut Config) -> R) -> R {
        let mut c = self.config.write().unwrap();
        let r = f(&mut c);
        if let Err(e) = c.save(&self.config_path) {
            eprintln!("could not save {}: {e}", self.config_path.display());
        }
        self.changes.send_modify(|v| *v += 1);
        r
    }

    pub fn watch_config(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Update> {
        self.updates.subscribe()
    }

    // ---- pairing

    /// A one-time token that lets a controller pair for the next ten minutes.
    pub fn new_pairing_token(&self) -> String {
        let token = random_hex(12);
        let mut t = self.tokens.lock().unwrap();
        t.retain(|(_, at)| at.elapsed() < TOKEN_TTL);
        t.push((token.clone(), Instant::now()));
        token
    }

    /// Consumes a token, and remembers the controller's key if it was valid.
    pub fn pair(&self, key: &str, name: &str, token: &str) -> bool {
        let ok = {
            let mut t = self.tokens.lock().unwrap();
            t.retain(|(_, at)| at.elapsed() < TOKEN_TTL);
            let before = t.len();
            t.retain(|(tok, _)| tok != token);
            t.len() != before
        };
        if ok {
            self.update_config(|c| {
                c.paired.retain(|p| p.key != key);
                c.paired.push(crate::config::Paired { key: key.into(), name: name.chars().take(60).collect() });
            });
        }
        ok
    }

    pub fn is_paired(&self, key: &str) -> bool {
        self.config.read().unwrap().paired.iter().any(|p| p.key == key)
    }

    // ---- projects

    pub fn projects(&self) -> Vec<Project> {
        let c = self.config();
        let mut out: Vec<Project> = vec![];
        let mut add = |p: PathBuf| {
            let path = p.to_string_lossy().into_owned();
            if !out.iter().any(|o| o.path == path) {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(path.clone());
                out.push(Project { name, path });
            }
        };
        for root in &c.project_roots {
            let Ok(rd) = std::fs::read_dir(expand(root)) else { continue };
            for e in rd.flatten() {
                let hidden = e.file_name().to_string_lossy().starts_with('.');
                if !hidden && e.path().is_dir() {
                    add(e.path());
                }
            }
        }
        for d in &c.project_dirs {
            let d = expand(d);
            if d.is_dir() {
                add(d);
            }
        }
        out.sort_by_key(|p| p.name.to_lowercase());
        out
    }

    // ---- sessions

    /// Every session, most recently updated first.
    pub fn sessions(&self) -> Vec<SessionInfo> {
        let mut v: Vec<_> = self.sessions.lock().unwrap().values().map(|s| s.info.clone()).collect();
        v.sort_by(|a, b| b.updated.cmp(&a.updated).then(a.id.cmp(&b.id)));
        v
    }


    pub fn messages(&self, id: &str) -> Option<Vec<Message>> {
        self.sessions.lock().unwrap().get(id).map(|s| s.messages.clone())
    }

    pub fn create_session(&self, project: &str, provider: Option<&str>) -> Result<SessionInfo, String> {
        let project = self
            .projects()
            .into_iter()
            .find(|p| p.path == project || p.name == project)
            .ok_or("that is not one of the projects")?;
        let cfg = self.config();
        let provider = provider.filter(|p| !p.is_empty()).unwrap_or(&cfg.default_provider);
        let provider = Provider::from_name(provider).ok_or_else(|| format!("unknown agent {provider}"))?;
        if provider.detect(&cfg.options(provider)).is_none() {
            return Err(format!("{} is not installed", provider.name()));
        }
        let now = now_ms();
        let info = SessionInfo {
            id: random_hex(8),
            project: project.name,
            path: project.path,
            provider: provider.name().into(),
            title: String::new(),
            created: now,
            updated: now,
            running: false,
        };
        self.sessions.lock().unwrap().insert(
            info.id.clone(),
            Session { info: info.clone(), provider_session: None, messages: vec![] },
        );
        self.save(&info.id);
        let _ = self.updates.send(Update::Session(info.clone()));
        Ok(info)
    }

    pub fn delete_session(&self, id: &str) {
        self.stop(id);
        self.sessions.lock().unwrap().remove(id);
        let _ = std::fs::remove_file(self.data_dir.join("sessions").join(format!("{id}.json")));
    }

    pub fn stop(&self, id: &str) {
        if let Some(n) = self.stops.lock().unwrap().get(id) {
            n.notify_one();
        }
    }

    fn save(&self, id: &str) {
        let json = match self.sessions.lock().unwrap().get(id) {
            Some(s) => serde_json::to_string(s).expect("serializable"),
            None => return,
        };
        let dir = self.data_dir.join("sessions");
        let _ = std::fs::create_dir_all(&dir);
        if let Err(e) = std::fs::write(dir.join(format!("{id}.json")), json) {
            eprintln!("could not save session {id}: {e}");
        }
    }

    /// Adds a message (or text to the last assistant message when `append`),
    /// and tells the watchers.
    fn push(&self, id: &str, role: Role, text: String, append: bool) {
        let update = {
            let mut map = self.sessions.lock().unwrap();
            let Some(s) = map.get_mut(id) else { return };
            let now = now_ms();
            s.info.updated = now;
            match s.messages.last_mut() {
                Some(m) if append && m.role == role => {
                    m.text.push_str("\n\n");
                    m.text.push_str(&text);
                    m.ts = now;
                }
                _ => s.messages.push(Message { role, text, ts: now }),
            }
            let index = s.messages.len() - 1;
            if s.info.title.is_empty() && role == Role::User {
                s.info.title = s.messages[index].text.lines().next().unwrap_or("").chars().take(80).collect();
            }
            (s.info.clone(), index, s.messages[index].clone())
        };
        let _ = self.updates.send(Update::Msg { id: id.into(), index: update.1, message: update.2 });
        let _ = self.updates.send(Update::Session(update.0));
    }

    fn set_running(&self, id: &str, running: bool) {
        let info = {
            let mut map = self.sessions.lock().unwrap();
            let Some(s) = map.get_mut(id) else { return };
            s.info.running = running;
            s.info.updated = now_ms();
            s.info.clone()
        };
        let _ = self.updates.send(Update::Session(info));
    }

    /// Runs one turn of the session's agent with `text`. Returns once the
    /// turn has started; its progress arrives as updates.
    pub fn send(self: &Arc<Self>, id: &str, text: &str) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty message".into());
        }
        let (provider, path, resume) = {
            let mut map = self.sessions.lock().unwrap();
            let s = map.get_mut(id).ok_or("no such session")?;
            if s.info.running {
                return Err("the agent is still working".into());
            }
            s.info.running = true;
            (s.info.provider.clone(), s.info.path.clone(), s.provider_session.clone())
        };
        let provider = Provider::from_name(&provider).ok_or("unknown agent")?;
        let opts = self.config().options(provider);
        let turn = Turn::start(provider, &opts, Path::new(&path), text, resume.as_deref());
        let mut turn = match turn {
            Ok(t) => t,
            Err(e) => {
                self.set_running(id, false);
                return Err(format!("could not start {}: {e}", provider.name()));
            }
        };
        self.push(id, Role::User, text.into(), false);
        let stop = Arc::new(tokio::sync::Notify::new());
        self.stops.lock().unwrap().insert(id.into(), stop.clone());
        let (core, id) = (self.clone(), id.to_string());
        tokio::spawn(async move {
            // Text after a tool call starts a new message.
            let mut appending = false;
            loop {
                let events = tokio::select! {
                    e = turn.next() => e,
                    _ = stop.notified() => {
                        turn.kill().await;
                        core.push(&id, Role::Error, "stopped".into(), false);
                        break;
                    }
                };
                let Some(events) = events else { break };
                for ev in events {
                    match ev {
                        Event::Session(sid) => {
                            if let Some(s) = core.sessions.lock().unwrap().get_mut(&id) {
                                s.provider_session = Some(sid);
                            }
                        }
                        Event::Text(t) => {
                            core.push(&id, Role::Assistant, t, appending);
                            appending = true;
                        }
                        Event::Tool(t) => {
                            core.push(&id, Role::Tool, t, false);
                            appending = false;
                        }
                        Event::Error(e) => {
                            core.push(&id, Role::Error, e, false);
                            appending = false;
                        }
                        Event::Done => {}
                    }
                }
            }
            core.stops.lock().unwrap().remove(&id);
            core.set_running(&id, false);
            core.save(&id);
        });
        Ok(())
    }
}

/// Expands a leading `~/`.
pub fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(p),
    }
}

pub fn set_online(core: &Core, v: bool) {
    core.online.store(v, Ordering::Relaxed);
}
