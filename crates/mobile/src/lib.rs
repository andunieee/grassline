//! The remote controller's core: pairing with a daemon, the iroh connection to
//! it, and the state the UI shows. The Kotlin app drives it through the JNI
//! functions in [`jni_bridge`]; it reads the whole state with
//! [`Controller::snapshot`] whenever [`Controller::wait`] says it changed.

mod jni_bridge;

use grassline_proto::{
    encode, parse_ticket, Agent, ClientMsg, Frames, Message, Model, Permissions, Project,
    ServerMsg, SessionInfo, Transcription, OUTDATED, VERSION,
};
use iroh::endpoint::{presets, Connection, ConnectionError};
use iroh::{Endpoint, EndpointId, SecretKey};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum Status {
    /// No daemon is paired.
    Unpaired,
    Connecting,
    Connected,
    /// The connection is down; it is retried.
    Offline(String),
    /// Pairing was refused.
    Refused(String),
    /// The daemon is on this newer version of the protocol: the app must be
    /// updated.
    Outdated(u32),
}

#[derive(Default)]
struct State {
    status: Option<Status>,
    projects: Vec<Project>,
    agents: Vec<Agent>,
    /// The agent new sessions use by default.
    default_agent: String,
    sessions: Vec<SessionInfo>,
    /// The archived sessions, once asked for.
    archived: Option<Vec<SessionInfo>>,
    /// Where voice messages are turned into text, if the daemon set it up.
    transcription: Option<Transcription>,
    /// The models of the agents they were asked for, by provider name.
    models: HashMap<String, Models>,
    /// The messages of the sessions that were opened.
    messages: HashMap<String, Vec<Message>>,
    /// The last error the daemon reported.
    error: Option<String>,
    version: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct Models {
    items: Vec<Model>,
    error: Option<String>,
    /// They were asked for and have not arrived yet.
    loading: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct Saved {
    endpoint: String,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    out: Mutex<Option<mpsc::UnboundedSender<ClientMsg>>>,
    opened: Mutex<HashSet<String>>,
}

impl Shared {
    fn edit(&self, f: impl FnOnce(&mut State)) {
        let mut s = self.state.lock().unwrap();
        f(&mut s);
        s.version += 1;
        self.changed.notify_all();
    }

    fn status(&self, st: Status) {
        self.edit(|s| s.status = Some(st));
    }
}

pub struct Controller {
    dir: PathBuf,
    secret: SecretKey,
    shared: Arc<Shared>,
    rt: tokio::runtime::Runtime,
    conn_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Controller {
    /// Opens the controller whose key and pairing are kept in `dir`, and
    /// connects to the daemon it is paired with, if any.
    pub fn new(dir: &Path) -> std::io::Result<Controller> {
        std::fs::create_dir_all(dir)?;
        let secret = load_secret(&dir.join("identity"))?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let c = Controller {
            dir: dir.into(),
            secret,
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                changed: Condvar::new(),
                out: Mutex::new(None),
                opened: Mutex::new(HashSet::new()),
            }),
            rt,
            conn_task: Mutex::new(None),
        };
        match c.saved() {
            Some(saved) => c.connect(saved.endpoint, None),
            None => c.shared.status(Status::Unpaired),
        }
        Ok(c)
    }

    /// This device's key.
    pub fn key(&self) -> String {
        self.secret.public().to_string()
    }

    fn saved(&self) -> Option<Saved> {
        serde_json::from_str(&std::fs::read_to_string(self.dir.join("daemon.json")).ok()?).ok()
    }

    /// Pairs with the daemon a ticket names (see `grassline_proto::ticket`).
    pub fn pair(&self, ticket: &str, device_name: &str) -> Result<(), String> {
        let (endpoint, token) = parse_ticket(ticket).ok_or("that is not a pairing code")?;
        endpoint
            .parse::<EndpointId>()
            .map_err(|_| "that is not a pairing code")?;
        self.connect(endpoint.into(), Some((token.into(), device_name.into())));
        Ok(())
    }

    /// Forgets the daemon.
    pub fn unpair(&self) {
        if let Some(t) = self.conn_task.lock().unwrap().take() {
            t.abort();
        }
        let _ = std::fs::remove_file(self.dir.join("daemon.json"));
        self.forget();
        self.shared.status(Status::Unpaired);
    }

    /// Drops what the last daemon told us.
    fn forget(&self) {
        *self.shared.out.lock().unwrap() = None;
        self.shared.opened.lock().unwrap().clear();
        self.shared.edit(|s| {
            *s = State {
                version: s.version,
                ..Default::default()
            }
        });
    }

    fn connect(&self, endpoint: String, pairing: Option<(String, String)>) {
        if let Some(t) = self.conn_task.lock().unwrap().take() {
            t.abort();
        }
        // Pairing may switch to another daemon: its sessions are not ours.
        if pairing.is_some() {
            self.forget();
        }
        self.shared.status(Status::Connecting);
        let (shared, secret, dir) = (self.shared.clone(), self.secret.clone(), self.dir.clone());
        let task = self
            .rt
            .spawn(connection_loop(shared, secret, dir, endpoint, pairing));
        *self.conn_task.lock().unwrap() = Some(task);
    }

    fn send(&self, m: ClientMsg) {
        if let Some(tx) = &*self.shared.out.lock().unwrap() {
            let _ = tx.send(m);
        }
    }

    pub fn refresh(&self) {
        self.send(ClientMsg::Refresh);
    }

    /// Creates a session with `provider`, or the default agent when it is
    /// empty.
    pub fn create(&self, project: &str, provider: &str) {
        self.send(ClientMsg::Create {
            project: project.into(),
            provider: (!provider.is_empty()).then(|| provider.into()),
        });
    }

    pub fn set_default_agent(&self, provider: &str) {
        self.send(ClientMsg::SetDefaultAgent {
            provider: provider.into(),
        });
    }

    /// Switches a session to another agent; it starts a new conversation.
    pub fn set_agent(&self, id: &str, provider: &str) {
        self.send(ClientMsg::SetAgent {
            id: id.into(),
            provider: provider.into(),
        });
    }

    /// Sets a session's model from its next message on.
    pub fn set_model(&self, id: &str, model: &str) {
        self.send(ClientMsg::SetModel {
            id: id.into(),
            model: model.into(),
        });
    }

    /// Sets what a session's agent may do without asking: `ask`, `auto` or
    /// `all`.
    pub fn set_permissions(&self, id: &str, permissions: &str) -> Result<(), String> {
        let permissions: Permissions =
            serde_json::from_value(json!(permissions)).map_err(|e| e.to_string())?;
        self.send(ClientMsg::SetPermissions {
            id: id.into(),
            permissions,
        });
        Ok(())
    }

    /// Asks the daemon for an agent's models; they show up in the snapshot.
    pub fn models(&self, provider: &str) {
        self.shared.edit(|s| {
            let m = s.models.entry(provider.into()).or_default();
            m.loading = true;
            m.error = None;
        });
        self.send(ClientMsg::Models {
            provider: provider.into(),
        });
    }

    /// Puts a session in the archive, or takes it out.
    pub fn archive(&self, id: &str, archived: bool) {
        self.send(ClientMsg::Archive {
            id: id.into(),
            archived,
        });
    }

    /// Asks the daemon for the archived sessions; they show up in the
    /// snapshot.
    pub fn archived(&self) {
        self.send(ClientMsg::Archived);
    }

    pub fn open(&self, id: &str) {
        self.shared.opened.lock().unwrap().insert(id.into());
        self.send(ClientMsg::Open { id: id.into() });
    }

    /// Leaves a session; the daemon deletes it when nothing was sent to it.
    pub fn close(&self, id: &str) {
        self.shared.opened.lock().unwrap().remove(id);
        self.send(ClientMsg::Close { id: id.into() });
    }

    pub fn send_text(&self, id: &str, text: &str) {
        self.send(ClientMsg::Send {
            id: id.into(),
            text: text.into(),
        });
    }

    /// Sends a queued message now (see `ClientMsg::SendNow`).
    pub fn send_now(&self, id: &str, queued: &str) {
        self.send(ClientMsg::SendNow {
            id: id.into(),
            queued: queued.into(),
        });
    }

    pub fn unqueue(&self, id: &str, queued: &str) {
        self.send(ClientMsg::Unqueue {
            id: id.into(),
            queued: queued.into(),
        });
    }

    pub fn stop(&self, id: &str) {
        self.send(ClientMsg::Stop { id: id.into() });
    }

    /// Answers what a tool call asks (its `tool.ask.request`). `decision` is a
    /// `grassline_proto::Decision` as JSON.
    pub fn decide(&self, id: &str, request: &str, decision: &str) -> Result<(), String> {
        let decision = serde_json::from_str(decision).map_err(|e| e.to_string())?;
        self.send(ClientMsg::Decide {
            id: id.into(),
            request: request.into(),
            decision,
        });
        Ok(())
    }

    pub fn dismiss_error(&self) {
        self.shared.edit(|s| s.error = None);
    }

    /// The state as JSON: `status`, `reason`, `projects`, `agents` (with
    /// whether each is installed and `steers`), `default_agent`, `models` (by
    /// agent: `items`, `error` and `loading`), `sessions` (most recent first,
    /// each with its `model`, `permissions` and `queue`), `archived` (the
    /// same, or null until asked for), `transcription` (`url`, `model` and `key` of the
    /// speech-to-text service, or null), `messages` (by session id; a tool call's `tool` has its
    /// progress and what it asks), `error` and `key`.
    pub fn snapshot(&self) -> String {
        let s = self.shared.state.lock().unwrap();
        let mut v = serde_json::to_value(s.status.clone().unwrap_or(Status::Connecting)).unwrap();
        let o = v.as_object_mut().unwrap();
        o.insert("projects".into(), json!(s.projects));
        o.insert("agents".into(), json!(s.agents));
        o.insert("default_agent".into(), json!(s.default_agent));
        o.insert("models".into(), json!(s.models));
        o.insert("sessions".into(), json!(s.sessions));
        o.insert("archived".into(), json!(s.archived));
        o.insert("transcription".into(), json!(s.transcription));
        o.insert("messages".into(), json!(s.messages));
        o.insert("error".into(), json!(s.error));
        o.insert("key".into(), json!(self.key()));
        v.to_string()
    }

    /// The version of the state, bumped on every change.
    pub fn version(&self) -> u64 {
        self.shared.state.lock().unwrap().version
    }

    /// Blocks until the state is newer than `seen` (or the timeout); returns
    /// the current version.
    pub fn wait(&self, seen: u64, timeout: Duration) -> u64 {
        let s = self.shared.state.lock().unwrap();
        let (s, _) = self
            .shared
            .changed
            .wait_timeout_while(s, timeout, |s| s.version <= seen)
            .unwrap();
        s.version
    }
}

fn load_secret(path: &Path) -> std::io::Result<SecretKey> {
    if let Ok(s) = std::fs::read_to_string(path) {
        let mut b = [0u8; 32];
        if hex::decode_to_slice(s.trim(), &mut b).is_ok() {
            return Ok(SecretKey::from_bytes(&b));
        }
    }
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(path, hex::encode(b))?;
    Ok(SecretKey::from_bytes(&b))
}

/// Keeps a connection to the daemon, reconnecting while paired.
async fn connection_loop(
    shared: Arc<Shared>,
    secret: SecretKey,
    dir: PathBuf,
    endpoint_id: String,
    mut pairing: Option<(String, String)>,
) {
    let Ok(id) = endpoint_id.parse::<EndpointId>() else {
        return;
    };
    let endpoint = match Endpoint::builder(presets::N0)
        .secret_key(secret)
        .bind()
        .await
    {
        Ok(e) => e,
        Err(e) => return shared.status(Status::Offline(e.to_string())),
    };
    loop {
        // The pairing is retried until the daemon answers it.
        let result = session(&shared, &endpoint, id, &endpoint_id, &dir, &mut pairing).await;
        *shared.out.lock().unwrap() = None;
        match result {
            Err(Fail::Refused(why)) => return shared.status(Status::Refused(why)),
            Err(Fail::Outdated(version)) => return shared.status(Status::Outdated(version)),
            Err(Fail::Other(why)) => shared.status(Status::Offline(why)),
            Ok(()) => shared.status(Status::Offline("the daemon closed the connection".into())),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        shared.status(Status::Connecting);
    }
}

enum Fail {
    Refused(String),
    Outdated(u32),
    Other(String),
}

impl<E: ToString> From<E> for Fail {
    fn from(e: E) -> Self {
        Fail::Other(e.to_string())
    }
}

async fn session(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: EndpointId,
    endpoint_id: &str,
    dir: &Path,
    pairing: &mut Option<(String, String)>,
) -> Result<(), Fail> {
    let conn = endpoint
        .connect(id, &grassline_proto::alpn(VERSION))
        .await?;
    let result = talk(shared, &conn, endpoint_id, dir, pairing).await;
    // A daemon on a newer version closes right away and says which.
    if let Some(ConnectionError::ApplicationClosed(c)) = conn.close_reason() {
        if c.error_code == OUTDATED.into() {
            let version = String::from_utf8_lossy(&c.reason).parse().unwrap_or(0);
            return Err(Fail::Outdated(version));
        }
    }
    result
}

async fn talk(
    shared: &Arc<Shared>,
    conn: &Connection,
    endpoint_id: &str,
    dir: &Path,
    pairing: &mut Option<(String, String)>,
) -> Result<(), Fail> {
    let (mut send, mut recv) = conn.open_bi().await?;
    let paired_now = pairing.is_some();
    let first = match pairing.clone() {
        Some((token, name)) => ClientMsg::Pair { token, name },
        None => ClientMsg::Refresh,
    };
    // The stream reaches the daemon with its first bytes.
    send.write_all(&encode(&first)).await?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    // Re-open what was open, and say we are here.
    for id in shared.opened.lock().unwrap().iter() {
        let _ = tx.send(ClientMsg::Open { id: id.clone() });
    }
    *shared.out.lock().unwrap() = Some(tx);

    let mut frames = Frames::default();
    let mut buf = vec![0u8; 16 * 1024];
    let mut hello = false;
    loop {
        tokio::select! {
            n = recv.read(&mut buf) => {
                let n = n?.ok_or("closed")?;
                frames.push(&buf[..n])?;
                while let Some(msg) = frames.next::<ServerMsg>() {
                    let Ok(msg) = msg else { continue };
                    if let ServerMsg::Hello { ok, error } = &msg {
                        if !ok {
                            return Err(Fail::Refused(error.clone().unwrap_or_else(|| "refused".into())));
                        }
                        hello = true;
                        if paired_now {
                            let _ = std::fs::write(dir.join("daemon.json"), json!({ "endpoint": endpoint_id }).to_string());
                            *pairing = None;
                        }
                        shared.status(Status::Connected);
                        continue;
                    }
                    if hello {
                        apply(shared, msg);
                    }
                }
            }
            m = rx.recv() => {
                let Some(m) = m else { return Ok(()) };
                send.write_all(&encode(&m)).await?;
            }
        }
    }
}

fn apply(shared: &Shared, msg: ServerMsg) {
    shared.edit(|s| match msg {
        ServerMsg::Hello { .. } => {}
        ServerMsg::Projects { items } => s.projects = items,
        ServerMsg::Agents { items, default } => {
            s.agents = items;
            s.default_agent = default;
        }
        ServerMsg::Models {
            provider,
            items,
            error,
        } => {
            s.models.insert(
                provider,
                Models {
                    items,
                    error,
                    loading: false,
                },
            );
        }
        ServerMsg::Transcription { config } => s.transcription = config,
        ServerMsg::Sessions { items } => s.sessions = items,
        ServerMsg::Archived { items } => s.archived = Some(items),
        ServerMsg::Session { info } => {
            s.sessions.retain(|i| i.id != info.id);
            if let Some(a) = &mut s.archived {
                a.retain(|i| i.id != info.id);
            }
            // It goes in the list it now belongs to, unless that is the
            // archive and it was not asked for.
            let list = if info.archived {
                s.archived.as_mut()
            } else {
                Some(&mut s.sessions)
            };
            if let Some(list) = list {
                list.push(info);
                list.sort_by(|a, b| b.updated.cmp(&a.updated).then(a.id.cmp(&b.id)));
            }
        }
        ServerMsg::History { id, messages } => {
            s.messages.insert(id, messages);
        }
        ServerMsg::Msg { id, index, message } => {
            let list = s.messages.entry(id).or_default();
            if index < list.len() {
                list[index] = message;
            } else if index == list.len() {
                list.push(message);
            }
            // A gap means we missed some; the daemon resends the history on a reconnect.
        }
        ServerMsg::Error { message } => s.error = Some(message),
    });
}
