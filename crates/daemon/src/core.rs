//! The daemon's state: settings, projects, and the agent sessions.

use crate::config::Config;
use grassline_agents::{Answer, Event, Provider, Turn};
use grassline_proto::{
    Agent, Ask, Choice, Decision, Message, Model, Permissions, Project, Question, Queued, Role,
    SessionInfo, ToolCall, ToolStatus,
};

/// The requests the agent waits for: the tool call's message index, and the
/// tool.
type Asks = HashMap<String, (usize, String)>;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, watch};

const TOKEN_TTL: Duration = Duration::from_secs(10 * 60);
/// How long an agent's list of models is kept.
const MODELS_TTL: Duration = Duration::from_secs(10 * 60);
/// How much of a tool's output is kept.
const OUTPUT_MAX: usize = 4000;
/// How much of the conversation so far an agent that takes it over gets.
const HANDOVER_MAX: usize = 40_000;
/// How much of a tool call's line it gets.
const HANDOVER_TOOL_MAX: usize = 300;

/// What changed in a session, for the connected controllers and the admin.
#[derive(Debug, Clone)]
pub enum Update {
    Session(SessionInfo),
    /// The session was deleted.
    Removed(String),
    Msg {
        id: String,
        index: usize,
        message: Message,
    },
}

#[derive(Serialize, Deserialize)]
struct Session {
    info: SessionInfo,
    /// The provider's own id for the conversation.
    provider_session: Option<String>,
    messages: Vec<Message>,
    /// Tools the user allowed for the whole session.
    #[serde(default)]
    allowed: Vec<String>,
}

/// What a controller asks of a running turn.
enum Control {
    Stop,
    Decide {
        request: String,
        decision: Decision,
    },
    /// Sends the queued message with this id now.
    SendNow(String),
}

pub struct Core {
    config_path: PathBuf,
    data_dir: PathBuf,
    config: RwLock<Config>,
    /// Bumped on every settings change.
    changes: watch::Sender<u64>,
    sessions: Mutex<HashMap<String, Session>>,
    updates: broadcast::Sender<Update>,
    turns: Mutex<HashMap<String, mpsc::UnboundedSender<Control>>>,
    tokens: Mutex<Vec<(String, Instant)>>,
    /// The models of each agent, by provider name, and when they were listed.
    models: Mutex<HashMap<String, (Instant, Vec<Model>)>>,
    /// The iroh endpoint id, once the endpoint is up.
    pub endpoint_id: OnceLock<String>,
    pub online: AtomicBool,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
                let Ok(s) = std::fs::read_to_string(e.path()) else {
                    continue;
                };
                let parsed = serde_json::from_str::<serde_json::Value>(&s).and_then(|v| {
                    let mut s: Session = serde_json::from_value(v.clone())?;
                    // From before sessions had their own.
                    if v["info"].get("permissions").is_none() {
                        s.info.permissions = config.permissions;
                    }
                    Ok(s)
                });
                match parsed {
                    Ok(mut s) => {
                        s.info.running = false;
                        s.info.waiting = false;
                        for q in &mut s.info.queue {
                            q.steering = false;
                        }
                        settle_tools(&mut s.messages);
                        if let Some(m) = s.messages.iter().rev().find(|m| m.role == Role::Assistant)
                        {
                            s.info.preview = first_line(&m.text);
                        }
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
            turns: Default::default(),
            tokens: Default::default(),
            models: Default::default(),
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
                c.paired.push(crate::config::Paired {
                    key: key.into(),
                    name: name.chars().take(60).collect(),
                });
            });
        }
        ok
    }

    /// Whether a token can still be used to pair.
    pub fn token_pending(&self, token: &str) -> bool {
        self.tokens
            .lock()
            .unwrap()
            .iter()
            .any(|(t, at)| t == token && at.elapsed() < TOKEN_TTL)
    }

    pub fn is_paired(&self, key: &str) -> bool {
        self.config
            .read()
            .unwrap()
            .paired
            .iter()
            .any(|p| p.key == key)
    }

    // ---- projects

    pub fn projects(&self) -> Vec<Project> {
        let c = self.config();
        let mut out: Vec<Project> = vec![];
        let mut add = |p: PathBuf| {
            let path = p.to_string_lossy().into_owned();
            if !out.iter().any(|o| o.path == path) {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(path.clone());
                out.push(Project { name, path });
            }
        };
        for root in &c.project_roots {
            let Ok(rd) = std::fs::read_dir(expand(root)) else {
                continue;
            };
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

    // ---- agents

    /// Every agent, and whether it is installed.
    pub fn agents(&self) -> Vec<Agent> {
        let cfg = self.config();
        grassline_agents::ALL
            .into_iter()
            .map(|p| Agent {
                name: p.name().into(),
                installed: p.detect(&cfg.options(p)).is_some(),
                steers: p.steers(),
            })
            .collect()
    }

    /// An installed agent by its name.
    fn installed(&self, name: &str) -> Result<Provider, String> {
        let provider = Provider::from_name(name).ok_or_else(|| format!("unknown agent {name}"))?;
        if provider.detect(&self.config().options(provider)).is_none() {
            return Err(format!("{} is not installed", provider.name()));
        }
        Ok(provider)
    }

    pub fn set_default_agent(&self, name: &str) -> Result<(), String> {
        let provider = self.installed(name)?;
        self.update_config(|c| c.default_provider = provider.name().into());
        Ok(())
    }

    /// Switches a session to another agent, which starts a new conversation
    /// that begins with what happened so far.
    pub fn set_agent(&self, id: &str, name: &str) -> Result<(), String> {
        let provider = self.installed(name)?;
        let model = self.default_model(provider.name());
        let permissions = self.config().permissions_for(provider.name(), &model);
        let info = {
            let mut map = self.sessions.lock().unwrap();
            let s = map.get_mut(id).ok_or("no such session")?;
            if s.info.running {
                return Err("the agent is still working".into());
            }
            if s.info.provider == provider.name() {
                return Ok(());
            }
            s.info.provider = provider.name().into();
            s.info.model = model;
            s.info.permissions = permissions;
            s.info.updated = now_ms();
            // The other agent's conversation id means nothing to this one,
            // nor do the tools allowed to it.
            s.provider_session = None;
            s.allowed.clear();
            s.info.clone()
        };
        self.save(id);
        let _ = self.updates.send(Update::Session(info));
        Ok(())
    }

    /// The model new sessions with this agent get.
    fn default_model(&self, provider: &str) -> String {
        self.config
            .read()
            .unwrap()
            .models
            .get(provider)
            .cloned()
            .unwrap_or_default()
    }

    /// The models an agent offers; asked of it at most every few minutes.
    pub async fn models(&self, name: &str) -> Result<Vec<Model>, String> {
        let provider = self.installed(name)?;
        if let Some((at, m)) = self.models.lock().unwrap().get(name) {
            if at.elapsed() < MODELS_TTL {
                return Ok(m.clone());
            }
        }
        let models: Vec<Model> = provider
            .models(&self.config().options(provider))
            .await
            .map_err(|e| format!("could not list the models of {name}: {e}"))?
            .into_iter()
            .map(|m| Model {
                id: m.id,
                name: m.name,
                description: m.description,
            })
            .collect();
        self.models
            .lock()
            .unwrap()
            .insert(name.into(), (Instant::now(), models.clone()));
        Ok(models)
    }

    /// Sets the model of a session, and of the next sessions with its agent.
    /// The session takes the mode last picked with the new model.
    pub fn set_model(&self, id: &str, model: &str) -> Result<(), String> {
        let model = model.trim().to_string();
        let cfg = self.config();
        let mut provider = String::new();
        self.edit_info(id, |i| {
            provider = i.provider.clone();
            i.model = model.clone();
            i.permissions = cfg.permissions_for(&i.provider, &model);
        })?;
        self.update_config(|c| c.models.insert(provider, model));
        Ok(())
    }

    /// Sets what a session's agent may do without asking, and remembers it
    /// for that session's model.
    pub fn set_permissions(&self, id: &str, permissions: Permissions) -> Result<(), String> {
        let mut provider = String::new();
        let mut model = String::new();
        self.edit_info(id, |i| {
            provider = i.provider.clone();
            model = i.model.clone();
            i.permissions = permissions;
        })?;
        self.update_config(|c| c.remember_permissions(&provider, &model, permissions));
        Ok(())
    }

    pub fn set_archived(&self, id: &str, archived: bool) -> Result<(), String> {
        self.edit_info(id, |i| i.archived = archived)
    }

    /// Changes what is known of a session, saves it and tells the watchers.
    fn edit_info<R>(&self, id: &str, f: impl FnOnce(&mut SessionInfo) -> R) -> Result<R, String> {
        let (r, info) = {
            let mut map = self.sessions.lock().unwrap();
            let s = map.get_mut(id).ok_or("no such session")?;
            (f(&mut s.info), s.info.clone())
        };
        self.save(id);
        let _ = self.updates.send(Update::Session(info));
        Ok(r)
    }

    // ---- sessions

    /// Every session, most recently updated first.
    pub fn sessions(&self) -> Vec<SessionInfo> {
        let mut v: Vec<_> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(|s| s.info.clone())
            .collect();
        v.sort_by(|a, b| b.updated.cmp(&a.updated).then(a.id.cmp(&b.id)));
        v
    }

    pub fn messages(&self, id: &str) -> Option<Vec<Message>> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.messages.clone())
    }

    pub fn create_session(
        &self,
        project: &str,
        provider: Option<&str>,
    ) -> Result<SessionInfo, String> {
        let project = self
            .projects()
            .into_iter()
            .find(|p| p.path == project || p.name == project)
            .ok_or("that is not one of the projects")?;
        let cfg = self.config();
        let provider = self.installed(
            provider
                .filter(|p| !p.is_empty())
                .unwrap_or(&cfg.default_provider),
        )?;
        let now = now_ms();
        let model = cfg.models.get(provider.name()).cloned().unwrap_or_default();
        let permissions = cfg.permissions_for(provider.name(), &model);
        let info = SessionInfo {
            id: random_hex(8),
            project: project.name,
            path: project.path,
            provider: provider.name().into(),
            title: String::new(),
            preview: String::new(),
            created: now,
            updated: now,
            running: false,
            waiting: false,
            model,
            permissions,
            queue: vec![],
            archived: false,
        };
        self.sessions.lock().unwrap().insert(
            info.id.clone(),
            Session {
                info: info.clone(),
                provider_session: None,
                messages: vec![],
                allowed: vec![],
            },
        );
        self.save(&info.id);
        let _ = self.updates.send(Update::Session(info.clone()));
        Ok(info)
    }

    pub fn delete_session(&self, id: &str) {
        self.stop(id);
        self.sessions.lock().unwrap().remove(id);
        let _ = std::fs::remove_file(self.data_dir.join("sessions").join(format!("{id}.json")));
        let _ = self.updates.send(Update::Removed(id.into()));
    }

    /// Deletes session `id` when nothing was ever sent to it.
    pub fn delete_if_unused(&self, id: &str) {
        let unused = self.sessions.lock().unwrap().get(id).is_some_and(|s| {
            s.messages.is_empty() && s.info.queue.is_empty() && !s.info.running
        });
        if unused {
            self.delete_session(id);
        }
    }

    pub fn stop(&self, id: &str) {
        self.control(id, Control::Stop);
    }

    /// Answers what a tool call of the running turn asks.
    pub fn decide(&self, id: &str, request: &str, decision: Decision) {
        self.control(
            id,
            Control::Decide {
                request: request.into(),
                decision,
            },
        );
    }

    fn control(&self, id: &str, c: Control) {
        if let Some(tx) = self.turns.lock().unwrap().get(id) {
            let _ = tx.send(c);
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
    /// and tells the watchers. Returns its index.
    fn push(&self, id: &str, role: Role, text: String, append: bool) -> Option<usize> {
        self.change(id, |s, now| {
            match s.messages.last_mut() {
                Some(m) if append && m.role == role => {
                    m.text.push_str("\n\n");
                    m.text.push_str(&text);
                    m.ts = now;
                }
                _ => s.messages.push(Message {
                    role,
                    text,
                    ts: now,
                    tool: None,
                }),
            }
            let index = s.messages.len() - 1;
            if s.info.title.is_empty() && role == Role::User {
                s.info.title = first_line(&s.messages[index].text);
            }
            Some(index)
        })
    }

    /// Adds streamed text to the last assistant message.
    fn extend(&self, id: &str, text: &str) {
        self.change(id, |s, now| {
            let m = s
                .messages
                .last_mut()
                .filter(|m| m.role == Role::Assistant)?;
            m.text.push_str(text);
            m.ts = now;
            Some(s.messages.len() - 1)
        });
    }

    fn push_tool(&self, id: &str, summary: String, call: ToolCall) -> Option<usize> {
        self.change(id, |s, now| {
            s.messages.push(Message {
                role: Role::Tool,
                text: summary,
                ts: now,
                tool: Some(call),
            });
            Some(s.messages.len() - 1)
        })
    }

    /// Changes the tool call at `index`, and tells the watchers.
    fn edit_tool(&self, id: &str, index: usize, f: impl FnOnce(&mut ToolCall)) {
        self.change(id, |s, now| {
            let m = s.messages.get_mut(index)?;
            f(m.tool.as_mut()?);
            m.ts = now;
            Some(index)
        });
    }

    /// Runs `f` on the session; when it returns the index of a message it
    /// changed, tells the watchers.
    fn change(
        &self,
        id: &str,
        f: impl FnOnce(&mut Session, u64) -> Option<usize>,
    ) -> Option<usize> {
        let (info, index, message) = {
            let mut map = self.sessions.lock().unwrap();
            let s = map.get_mut(id)?;
            let now = now_ms();
            let index = f(s, now)?;
            s.info.updated = now;
            if s.messages[index].role == Role::Assistant {
                s.info.preview = first_line(&s.messages[index].text);
            }
            (s.info.clone(), index, s.messages[index].clone())
        };
        let _ = self.updates.send(Update::Msg {
            id: id.into(),
            index,
            message,
        });
        let _ = self.updates.send(Update::Session(info));
        Some(index)
    }

    fn set_running(&self, id: &str, running: bool) {
        let info = {
            let mut map = self.sessions.lock().unwrap();
            let Some(s) = map.get_mut(id) else { return };
            s.info.running = running;
            s.info.waiting &= running;
            s.info.updated = now_ms();
            s.info.clone()
        };
        let _ = self.updates.send(Update::Session(info));
    }

    /// Sends a message to the session's agent: it starts a turn, or waits in
    /// the queue while one runs. Returns once the turn has started; its
    /// progress arrives as updates.
    pub fn send(self: &Arc<Self>, id: &str, text: &str) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty message".into());
        }
        let running = self.edit_info(id, |i| {
            i.archived = false;
            i.queue.push(Queued {
                id: random_hex(4),
                text: text.into(),
                steering: false,
            });
            i.running
        })?;
        if running {
            return Ok(());
        }
        self.flush(id, None)
    }

    /// Sends a queued message now: into the running turn when its agent
    /// steers, otherwise in the next turn, ahead of the rest of the queue.
    pub fn send_now(self: &Arc<Self>, id: &str, queued: &str) -> Result<(), String> {
        let running = {
            let map = self.sessions.lock().unwrap();
            let s = map.get(id).ok_or("no such session")?;
            if !s.info.queue.iter().any(|q| q.id == queued) {
                return Ok(());
            }
            s.info.running
        };
        if running {
            self.control(id, Control::SendNow(queued.into()));
            return Ok(());
        }
        self.flush(id, Some(queued))
    }

    /// Takes a message out of the queue, unless the agent has it already.
    pub fn unqueue(&self, id: &str, queued: &str) -> Result<(), String> {
        self.edit_info(id, |i| {
            if i.queue.iter().any(|q| q.id == queued && q.steering) {
                return Err("the agent has it already".to_string());
            }
            i.queue.retain(|q| q.id != queued);
            Ok(())
        })?
    }

    /// Runs one turn of the session's agent with a queued message: `first`,
    /// or the oldest. Returns once the turn has started.
    fn flush(self: &Arc<Self>, id: &str, first: Option<&str>) -> Result<(), String> {
        let (provider, path, resume, model, permissions, queued, prompt) = {
            let mut map = self.sessions.lock().unwrap();
            let s = map.get_mut(id).ok_or("no such session")?;
            if s.info.running {
                return Err("the agent is still working".into());
            }
            if s.info.queue.is_empty() {
                return Ok(());
            }
            let at = s
                .info
                .queue
                .iter()
                .position(|q| Some(q.id.as_str()) == first)
                .unwrap_or(0);
            s.info.running = true;
            let queued = s.info.queue.remove(at);
            // No conversation of its own yet, as after switching agents: it
            // gets what happened so far.
            let prompt = match s.provider_session {
                None => handover(&s.messages, &queued.text),
                Some(_) => queued.text.clone(),
            };
            (
                s.info.provider.clone(),
                s.info.path.clone(),
                s.provider_session.clone(),
                s.info.model.clone(),
                s.info.permissions,
                queued,
                prompt,
            )
        };
        let Some(provider) = Provider::from_name(&provider) else {
            self.requeue(id, queued);
            return Err("unknown agent".into());
        };
        let opts = grassline_agents::Options {
            model,
            permissions: match permissions {
                Permissions::Ask => grassline_agents::Permissions::Ask,
                Permissions::Auto => grassline_agents::Permissions::Auto,
                Permissions::All => grassline_agents::Permissions::All,
            },
            ..self.config().options(provider)
        };
        let turn = match Turn::start(
            provider,
            &opts,
            Path::new(&path),
            &prompt,
            resume.as_deref(),
        ) {
            Ok(t) => t,
            Err(e) => {
                self.requeue(id, queued);
                return Err(format!("could not start {}: {e}", provider.name()));
            }
        };
        self.push(id, Role::User, queued.text, false);
        let (tx, rx) = mpsc::unbounded_channel();
        self.turns.lock().unwrap().insert(id.into(), tx);
        tokio::spawn(self.clone().run(id.to_string(), provider, turn, rx));
        Ok(())
    }

    /// Puts back the message of a turn that could not start.
    fn requeue(&self, id: &str, queued: Queued) {
        let _ = self.edit_info(id, |i| {
            i.queue.insert(0, queued);
            i.running = false;
        });
    }

    async fn run(
        self: Arc<Self>,
        id: String,
        provider: Provider,
        mut turn: Turn,
        mut rx: mpsc::UnboundedReceiver<Control>,
    ) {
        // Text after a tool call starts a new message.
        let mut appending = false;
        // The user stopped it: the queue waits.
        let mut stopped = false;
        // The queued message to send first when the turn is over.
        let mut first = None;
        // The message index of each tool call, by its id.
        let mut tools = HashMap::<String, usize>::new();
        let mut asks = Asks::new();
        loop {
            self.set_waiting(&id, !asks.is_empty());
            let events = tokio::select! {
                e = turn.next() => e,
                c = rx.recv() => {
                    match c {
                        Some(Control::Decide { request, decision }) => {
                            self.decided(&id, &mut turn, &mut asks, &request, decision)
                        }
                        Some(Control::SendNow(q)) if provider.steers() => self.steer(&id, &mut turn, &q),
                        // It cannot take it while it works: the turn makes way.
                        Some(Control::SendNow(q)) => {
                            turn.kill().await;
                            self.push(&id, Role::Error, "interrupted".into(), false);
                            first = Some(q);
                            break;
                        }
                        Some(Control::Stop) | None => {
                            turn.kill().await;
                            self.push(&id, Role::Error, "stopped".into(), false);
                            stopped = true;
                            break;
                        }
                    }
                    continue;
                }
            };
            let Some(events) = events else { break };
            for ev in events {
                match ev {
                    Event::Session(sid) => {
                        if let Some(s) = self.sessions.lock().unwrap().get_mut(&id) {
                            s.provider_session = Some(sid);
                        }
                    }
                    Event::Text(t) => {
                        self.push(&id, Role::Assistant, t, appending);
                        appending = true;
                    }
                    Event::Delta(t) => {
                        if appending {
                            self.extend(&id, &t);
                        } else {
                            self.push(&id, Role::Assistant, t, false);
                            appending = true;
                        }
                    }
                    Event::Tool {
                        id: tool_id,
                        name,
                        summary,
                        input,
                    } => {
                        appending = false;
                        if let Some(&index) = tools.get(&tool_id).filter(|_| !tool_id.is_empty()) {
                            // Its input became known.
                            let input = pretty(&input);
                            self.change(&id, |s, _| {
                                let m = s.messages.get_mut(index)?;
                                m.text = summary;
                                m.tool.as_mut()?.input = input;
                                Some(index)
                            });
                        } else {
                            let call = ToolCall {
                                id: tool_id.clone(),
                                name,
                                input: pretty(&input),
                                status: ToolStatus::Running,
                                output: None,
                                ask: None,
                            };
                            if let Some(index) = self.push_tool(&id, summary, call) {
                                tools.insert(tool_id, index);
                            }
                        }
                    }
                    Event::ToolResult {
                        id: tool_id,
                        output,
                        error,
                    } => {
                        if let Some(&index) = tools.get(&tool_id) {
                            self.edit_tool(&id, index, |t| {
                                if t.status != ToolStatus::Denied {
                                    t.status = if error {
                                        ToolStatus::Failed
                                    } else {
                                        ToolStatus::Done
                                    };
                                }
                                t.output = Some(clip(&output));
                                t.ask = None;
                            });
                        }
                    }
                    Event::Ask {
                        request,
                        tool_id,
                        name,
                        input,
                        questions,
                    } => {
                        if questions.is_empty() && self.is_allowed(&id, &name) {
                            turn.answer(&request, Answer::allow());
                            continue;
                        }
                        let questions = questions.into_iter().map(question).collect();
                        let known = tool_id.as_ref().and_then(|t| tools.get(t)).copied();
                        let index = known.or_else(|| {
                            let call = ToolCall {
                                id: tool_id.clone().unwrap_or_default(),
                                name: name.clone(),
                                input: pretty(&input),
                                status: ToolStatus::Running,
                                output: None,
                                ask: None,
                            };
                            let index = self.push_tool(
                                &id,
                                grassline_agents::tool_line(&name, &input),
                                call,
                            )?;
                            tools.insert(tool_id.unwrap_or_default(), index);
                            Some(index)
                        });
                        let Some(index) = index else { continue };
                        appending = false;
                        let ask = Ask {
                            request: request.clone(),
                            questions,
                        };
                        self.edit_tool(&id, index, |t| {
                            t.status = ToolStatus::Waiting;
                            t.ask = Some(ask);
                        });
                        asks.insert(request, (index, name));
                    }
                    Event::Steered(q) => {
                        let text = self
                            .edit_info(&id, |i| {
                                let at = i.queue.iter().position(|x| x.id == q)?;
                                Some(i.queue.remove(at).text)
                            })
                            .ok()
                            .flatten();
                        if let Some(text) = text {
                            self.push(&id, Role::User, text, false);
                        }
                        appending = false;
                    }
                    Event::AskCancelled(request) => {
                        if let Some((index, ..)) = asks.remove(&request) {
                            self.edit_tool(&id, index, |t| {
                                t.status = ToolStatus::Running;
                                t.ask = None;
                            });
                        }
                    }
                    Event::Error(e) => {
                        self.push(&id, Role::Error, e, false);
                        appending = false;
                    }
                    Event::Done => {}
                }
            }
        }
        self.turns.lock().unwrap().remove(&id);
        let settled = match self.sessions.lock().unwrap().get_mut(&id) {
            Some(s) => settle_tools(&mut s.messages),
            None => vec![],
        };
        for index in settled {
            self.edit_tool(&id, index, |_| {});
        }
        // What the turn did not take in waits again.
        let queued = self
            .edit_info(&id, |i| {
                for q in &mut i.queue {
                    q.steering = false;
                }
                !i.queue.is_empty()
            })
            .unwrap_or(false);
        self.set_running(&id, false);
        self.save(&id);
        if queued && !stopped {
            if let Err(e) = self.flush(&id, first.as_deref()) {
                self.push(&id, Role::Error, e, false);
            }
        }
    }

    /// Gives a queued message to the running turn.
    fn steer(&self, id: &str, turn: &mut Turn, queued: &str) {
        let text = self.edit_info(id, |i| {
            let q = i.queue.iter_mut().find(|q| q.id == queued && !q.steering)?;
            q.steering = true;
            Some(q.text.clone())
        });
        let Ok(Some(text)) = text else { return };
        if !turn.steer(queued, &text) {
            // It is ending: the message goes in the next turn.
            let _ = self.edit_info(id, |i| {
                for q in i.queue.iter_mut().filter(|q| q.id == queued) {
                    q.steering = false;
                }
            });
        }
    }

    /// Answers the agent with what a controller decided.
    fn decided(
        &self,
        id: &str,
        turn: &mut Turn,
        asks: &mut Asks,
        request: &str,
        decision: Decision,
    ) {
        // Another controller may have answered first.
        let Some((index, name)) = asks.remove(request) else {
            return;
        };
        if decision.allow && decision.always {
            self.allow_always(id, &name);
            // The same tool may be waiting in another call.
            let same: Vec<String> = asks
                .iter()
                .filter(|(_, a)| a.1 == name)
                .map(|(r, _)| r.clone())
                .collect();
            for r in same {
                let (index, _) = asks.remove(&r).unwrap();
                let allow = Decision {
                    allow: true,
                    ..Default::default()
                };
                self.answer(id, turn, &r, index, allow);
            }
        }
        self.answer(id, turn, request, index, decision);
    }

    fn answer(&self, id: &str, turn: &mut Turn, request: &str, index: usize, decision: Decision) {
        let allow = decision.allow;
        let answer = if allow {
            Answer::Allow {
                answers: decision.answers,
            }
        } else {
            Answer::Deny(
                decision
                    .message
                    .filter(|m| !m.trim().is_empty())
                    .map(|m| format!("The user denied this: {m}"))
                    .unwrap_or_else(|| "The user denied this.".into()),
            )
        };
        turn.answer(request, answer);
        self.edit_tool(id, index, |t| {
            t.status = if allow {
                ToolStatus::Running
            } else {
                ToolStatus::Denied
            };
            t.ask = None;
        });
    }

    fn set_waiting(&self, id: &str, waiting: bool) {
        let info = {
            let mut map = self.sessions.lock().unwrap();
            let Some(s) = map.get_mut(id) else { return };
            if s.info.waiting == waiting {
                return;
            }
            s.info.waiting = waiting;
            s.info.clone()
        };
        let _ = self.updates.send(Update::Session(info));
    }

    fn is_allowed(&self, id: &str, tool: &str) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|s| s.allowed.iter().any(|t| t == tool))
    }

    fn allow_always(&self, id: &str, tool: &str) {
        if let Some(s) = self.sessions.lock().unwrap().get_mut(id) {
            if !s.allowed.iter().any(|t| t == tool) {
                s.allowed.push(tool.into());
            }
        }
        self.save(id);
    }
}

/// Marks the tool calls that will not finish any more (the turn is over);
/// returns their indices.
fn settle_tools(messages: &mut [Message]) -> Vec<usize> {
    let mut out = vec![];
    for (i, m) in messages.iter_mut().enumerate() {
        if let Some(t) = m.tool.as_mut() {
            if matches!(t.status, ToolStatus::Running | ToolStatus::Waiting) {
                t.status = ToolStatus::Failed;
                t.ask = None;
                out.push(i);
            }
        }
    }
    out
}

fn question(q: grassline_agents::Question) -> Question {
    Question {
        question: q.question,
        header: q.header,
        multi: q.multi,
        options: q
            .options
            .into_iter()
            .map(|o| Choice {
                label: o.label,
                description: o.description,
            })
            .collect(),
    }
}

fn pretty(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// The first line with text, in at most 80 characters.
fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty());
    line.unwrap_or("").chars().take(80).collect()
}

fn clip(s: &str) -> String {
    if s.len() <= OUTPUT_MAX {
        return s.into();
    }
    let mut end = OUTPUT_MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…", &s[..end])
}

/// `text` for an agent that takes over the conversation, after what happened
/// so far: the messages, and each tool call in one line without its output.
/// Past [`HANDOVER_MAX`] the oldest are left out.
fn handover(messages: &[Message], text: &str) -> String {
    let mut lines = vec![];
    let mut size = 0;
    for m in messages.iter().rev() {
        let line = match m.role {
            Role::User => format!("[user] {}", m.text),
            Role::Assistant => format!("[agent] {}", m.text),
            Role::Error => format!("[error] {}", m.text),
            Role::Tool => {
                let mut line: String = m.text.lines().next().unwrap_or("").into();
                if line.chars().count() > HANDOVER_TOOL_MAX {
                    line = line.chars().take(HANDOVER_TOOL_MAX).collect::<String>() + "…";
                }
                let status = match m.tool.as_ref().map(|t| t.status) {
                    Some(ToolStatus::Failed) => " (failed)",
                    Some(ToolStatus::Denied) => " (denied by the user)",
                    _ => "",
                };
                format!("[tool] {line}{status}")
            }
        };
        size += line.len();
        if size > HANDOVER_MAX {
            lines.push("[the earlier part is left out]".to_string());
            break;
        }
        lines.push(line);
    }
    if lines.is_empty() {
        return text.into();
    }
    lines.reverse();
    format!(
        "This conversation started before you joined it, likely with another agent, \
         so it is not in your history. What happened so far:\n\n{}\n\n---\n\n{text}",
        lines.join("\n\n")
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for Claude Code's stream-json: each prompt takes a second,
    /// in which the messages written to it are taken in; it answers with
    /// what it got.
    const FAKE_CLAUDE: &str = r#"#!/usr/bin/env python3
import json, os, select, sys, time
def out(v): print(json.dumps(v), flush=True)
buf = b""
def lines(timeout):
    global buf
    while b"\n" not in buf:
        r, _, _ = select.select([0], [], [], timeout)
        if not r: return None
        b = os.read(0, 65536)
        if not b: sys.exit(0)
        buf += b
    l, buf = buf.split(b"\n", 1)
    return json.loads(l)
while True:
    v = lines(None)
    if v.get("type") != "user": continue
    got = [v["message"]["content"]]
    out({"type": "system", "subtype": "init", "session_id": "s1"})
    end = time.time() + 1
    while time.time() < end:
        w = lines(0.05)
        if w:
            out({"type": "user", "isReplay": True, "uuid": w["uuid"], "message": w["message"]})
            got.append(w["message"]["content"])
    out({"type": "assistant", "message": {"content": [{"type": "text", "text": " + ".join(got)}]}})
    out({"type": "result", "is_error": False})
"#;

    fn core(name: &str) -> (Arc<Core>, String, PathBuf) {
        let dir = std::env::temp_dir().join(format!("grassline-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project")).unwrap();
        let fake = dir.join("claude");
        std::fs::write(&fake, FAKE_CLAUDE).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let core = Core::open(dir.join("config.json"), dir.join("data"));
        core.update_config(|c| {
            c.project_dirs = vec![dir.join("project").to_string_lossy().into()];
            c.binaries
                .insert("claude".into(), fake.to_string_lossy().into());
        });
        let id = core
            .create_session(&dir.join("project").to_string_lossy(), Some("claude"))
            .unwrap()
            .id;
        (core, id, dir)
    }

    fn info(core: &Core, id: &str) -> SessionInfo {
        core.sessions().into_iter().find(|s| s.id == id).unwrap()
    }

    async fn until(what: &str, f: impl Fn() -> bool) {
        for _ in 0..200 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timeout waiting for {what}");
    }

    fn texts(core: &Core, id: &str) -> Vec<(Role, String)> {
        core.messages(id)
            .unwrap()
            .into_iter()
            .map(|m| (m.role, m.text))
            .collect()
    }

    #[test]
    fn remembers_the_mode_picked_for_each_model() {
        let (core, id, dir) = core("mode");
        let project = dir.join("project").to_string_lossy().to_string();
        core.set_model(&id, "one").unwrap();
        core.set_permissions(&id, Permissions::All).unwrap();

        // The next session with that model starts in its mode.
        let next = core.create_session(&project, Some("claude")).unwrap();
        assert_eq!(next.model, "one");
        assert_eq!(next.permissions, Permissions::All);

        // Another model starts from the configured default, and remembers
        // what is picked with it.
        core.set_model(&next.id, "two").unwrap();
        assert_eq!(info(&core, &next.id).permissions, Permissions::Auto);
        core.set_permissions(&next.id, Permissions::Ask).unwrap();
        let third = core.create_session(&project, Some("claude")).unwrap();
        assert_eq!(third.model, "two");
        assert_eq!(third.permissions, Permissions::Ask);

        // Going back to the first model brings its mode back.
        core.set_model(&third.id, "one").unwrap();
        assert_eq!(info(&core, &third.id).permissions, Permissions::All);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn queued_messages_steer_or_wait_for_the_turn() {
        let (core, id, dir) = core("queue");
        for text in ["one", "two", "three", "four"] {
            core.send(&id, text).unwrap();
        }
        let queue = info(&core, &id).queue;
        assert_eq!(queue.len(), 3);
        core.send_now(&id, &queue[1].id).unwrap();
        until("the last turn", || {
            texts(&core, &id).last().map(|m| m.1.as_str()) == Some("four")
                && !info(&core, &id).running
        })
        .await;
        use Role::*;
        // The steered one goes in the turn; the rest one turn each.
        assert_eq!(
            texts(&core, &id),
            vec![
                (User, "one".into()),
                (User, "three".into()),
                (Assistant, "one + three".into()),
                (User, "two".into()),
                (Assistant, "two".into()),
                (User, "four".into()),
                (Assistant, "four".into()),
            ]
        );
        assert!(info(&core, &id).queue.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn only_unused_sessions_are_deleted_on_close() {
        let (core, unused, dir) = core("unused");
        let used = core
            .create_session(&dir.join("project").to_string_lossy(), Some("claude"))
            .unwrap()
            .id;
        core.send(&used, "one").unwrap();
        until("the answer", || !info(&core, &used).running).await;
        core.delete_if_unused(&unused);
        core.delete_if_unused(&used);
        let ids: Vec<_> = core.sessions().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, [used]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn sending_takes_a_session_out_of_the_archive() {
        let (core, id, dir) = core("archive");
        core.set_archived(&id, true).unwrap();
        assert!(info(&core, &id).archived);
        core.send(&id, "one").unwrap();
        assert!(!info(&core, &id).archived);
        until("the answer", || !info(&core, &id).running).await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn stopping_keeps_the_queue() {
        let (core, id, dir) = core("stop");
        core.send(&id, "one").unwrap();
        core.send(&id, "two").unwrap();
        core.stop(&id);
        until("the stop", || !info(&core, &id).running).await;
        let queue = info(&core, &id).queue;
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].text, "two");
        // Sent now, it starts the next turn; stopped before the agent had a
        // conversation, the next one gets the stopped message too.
        core.send_now(&id, &queue[0].id).unwrap();
        until("the answer", || {
            texts(&core, &id).last().is_some_and(|m| {
                m.0 == Role::Assistant && m.1.ends_with("[error] stopped\n\n---\n\ntwo")
            }) && !info(&core, &id).running
        })
        .await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_new_agent_gets_the_conversation_so_far() {
        let (core, id, dir) = core("handover");
        core.send(&id, "one").unwrap();
        until("the answer", || !info(&core, &id).running).await;
        // As after switching agents.
        core.sessions
            .lock()
            .unwrap()
            .get_mut(&id)
            .unwrap()
            .provider_session = None;
        core.send(&id, "two").unwrap();
        until("the second answer", || {
            !info(&core, &id).running && texts(&core, &id).len() == 4
        })
        .await;
        let m = texts(&core, &id);
        // The user sees what they wrote; the agent got the rest before it.
        assert_eq!(m[2], (Role::User, "two".into()));
        assert!(
            m[3].1.contains("[user] one\n\n[agent] one\n\n---\n\ntwo"),
            "{}",
            m[3].1
        );
        // Once it has a conversation of its own, it only gets the message.
        core.send(&id, "three").unwrap();
        until("the third answer", || {
            !info(&core, &id).running && texts(&core, &id).len() == 6
        })
        .await;
        assert_eq!(texts(&core, &id)[5].1, "three");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn handover_leaves_out_the_oldest() {
        let msg = |role, text: &str| Message {
            role,
            text: text.into(),
            ts: 0,
            tool: None,
        };
        assert_eq!(handover(&[], "hi"), "hi");
        let old = "x".repeat(HANDOVER_MAX);
        let mut tool = msg(Role::Tool, "bash: rm -rf build\nmore");
        tool.tool = Some(ToolCall {
            id: "t".into(),
            name: "bash".into(),
            input: "{}".into(),
            status: ToolStatus::Denied,
            output: Some("secret output".into()),
            ask: None,
        });
        let h = handover(
            &[msg(Role::User, &old), msg(Role::User, "recent"), tool],
            "now",
        );
        assert!(!h.contains(&old));
        assert!(!h.contains("secret output"));
        assert!(h.contains(
            "[the earlier part is left out]\n\n[user] recent\n\n\
             [tool] bash: rm -rf build (denied by the user)\n\n---\n\nnow"
        ));
    }
}
