//! The remote controller's core: pairing with daemons, the iroh connection to
//! the one the app is on, and the state the UI shows. The Kotlin app drives it through the JNI
//! functions in [`jni_bridge`]; it reads the whole state with
//! [`Controller::snapshot`] whenever [`Controller::wait`] says it changed.

mod jni_bridge;

use grassline_proto::{
    encode, parse_ticket, Agent, ClientMsg, Frames, Message, Model, Permissions, Project,
    Resumable, ServerMsg, SessionInfo, SpeechConfig, MAX_VOICE_AUDIO, OUTDATED, VERSION,
    VOICE_CHUNK,
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
    /// Where new projects can go.
    roots: Vec<String>,
    agents: Vec<Agent>,
    /// The agent new sessions use by default.
    default_agent: String,
    sessions: Vec<SessionInfo>,
    /// The archived sessions, once asked for.
    archived: Option<Vec<SessionInfo>>,
    /// Where voice messages are turned into text, if the daemon set it up.
    transcription: Option<SpeechConfig>,
    /// The answer to a transcription request, while the app waits for it.
    transcription_result: Option<(String, Result<String, String>)>,
    /// The models of the agents they were asked for, by provider name.
    models: HashMap<String, Models>,
    /// The past conversations each session could take up, by session id, for
    /// the sessions they were asked for.
    resumable: HashMap<String, Resumables>,
    /// The messages of the sessions that are open.
    messages: HashMap<String, Vec<Message>>,
    /// The last error the daemon reported.
    error: Option<String>,
    /// The connection is a pairing the daemon has not let in yet.
    pairing: bool,
    version: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct Models {
    items: Vec<Model>,
    error: Option<String>,
    /// They were asked for and have not arrived yet.
    loading: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
struct Resumables {
    items: Vec<Resumable>,
    error: Option<String>,
    /// They were asked for and have not arrived yet.
    loading: bool,
}

/// A daemon this device is paired with.
#[derive(Serialize, Deserialize, Clone)]
struct Daemon {
    endpoint: String,
    /// What it calls itself; empty until it says.
    #[serde(default)]
    name: String,
}

/// The daemons this device is paired with, kept in `daemons.json`.
#[derive(Serialize, Deserialize, Default)]
struct Saved {
    daemons: Vec<Daemon>,
    /// The one the app is on.
    current: Option<String>,
}

impl Saved {
    fn load(dir: &Path) -> Saved {
        let read = |name| std::fs::read_to_string(dir.join(name)).ok();
        if let Some(s) = read("daemons.json").and_then(|s| serde_json::from_str(&s).ok()) {
            return s;
        }
        // From when there was only one.
        #[derive(Deserialize)]
        struct Old {
            endpoint: String,
        }
        let Some(old) = read("daemon.json").and_then(|s| serde_json::from_str::<Old>(&s).ok())
        else {
            return Saved::default();
        };
        let s = Saved {
            daemons: vec![Daemon {
                endpoint: old.endpoint.clone(),
                name: String::new(),
            }],
            current: Some(old.endpoint),
        };
        if s.write(dir).is_ok() {
            let _ = std::fs::remove_file(dir.join("daemon.json"));
        }
        s
    }

    fn write(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::write(
            dir.join("daemons.json"),
            serde_json::to_string(self).unwrap(),
        )
    }
}

struct Shared {
    dir: PathBuf,
    state: Mutex<State>,
    saved: Mutex<Saved>,
    changed: Condvar,
    out: Mutex<Option<mpsc::UnboundedSender<ClientMsg>>>,
    opened: Mutex<HashSet<String>>,
    /// The sessions that were left: what the daemon still sends for them on its
    /// way to seeing the close is dropped, so they stay out of memory.
    left: Mutex<HashSet<String>>,
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

    /// Changes the saved daemons and writes them down.
    fn save<T>(&self, f: impl FnOnce(&mut Saved) -> T) -> T {
        let r = {
            let mut saved = self.saved.lock().unwrap();
            let r = f(&mut saved);
            if let Err(e) = saved.write(&self.dir) {
                log::warn!("cannot save the daemons: {e}");
            }
            r
        };
        self.edit(|_| {});
        r
    }

    /// Stops keeping session `id` open and its messages: opening it asks for
    /// them again.
    fn close_session(&self, id: &str) {
        self.opened.lock().unwrap().remove(id);
        self.left.lock().unwrap().insert(id.into());
        if self.state.lock().unwrap().messages.contains_key(id) {
            self.edit(|s| {
                s.messages.remove(id);
            });
        }
        if self.state.lock().unwrap().resumable.contains_key(id) {
            self.edit(|s| {
                s.resumable.remove(id);
            });
        }
    }
}

pub struct Controller {
    secret: SecretKey,
    shared: Arc<Shared>,
    rt: tokio::runtime::Runtime,
    conn_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Controller {
    /// Opens the controller whose key and pairings are kept in `dir`, and
    /// connects to the daemon it was last on, if any.
    pub fn new(dir: &Path) -> std::io::Result<Controller> {
        std::fs::create_dir_all(dir)?;
        let secret = load_secret(&dir.join("identity"))?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let saved = Saved::load(dir);
        let current = saved.current.clone();
        let c = Controller {
            secret,
            shared: Arc::new(Shared {
                dir: dir.into(),
                state: Mutex::new(State::default()),
                saved: Mutex::new(saved),
                changed: Condvar::new(),
                out: Mutex::new(None),
                opened: Mutex::new(HashSet::new()),
                left: Mutex::new(HashSet::new()),
            }),
            rt,
            conn_task: Mutex::new(None),
        };
        match current {
            Some(endpoint) => c.connect(endpoint, None),
            None => c.shared.status(Status::Unpaired),
        }
        Ok(c)
    }

    /// This device's key.
    pub fn key(&self) -> String {
        self.secret.public().to_string()
    }

    /// Pairs with the daemon a ticket names (see `grassline_proto::ticket`)
    /// and goes to it once it lets us in; the others stay paired.
    pub fn pair(&self, ticket: &str, device_name: &str) -> Result<(), String> {
        let (endpoint, token) = parse_ticket(ticket).ok_or("that is not a pairing code")?;
        endpoint
            .parse::<EndpointId>()
            .map_err(|_| "that is not a pairing code")?;
        self.connect(endpoint.into(), Some((token.into(), device_name.into())));
        Ok(())
    }

    /// Goes to another daemon this device is paired with.
    pub fn switch_to(&self, endpoint: &str) {
        let known = self.shared.save(|s| {
            let known = s.daemons.iter().any(|d| d.endpoint == endpoint);
            if known {
                s.current = Some(endpoint.into());
            }
            known
        });
        if known {
            self.connect(endpoint.into(), None);
        }
    }

    /// Forgets the daemon the app is on, and goes to another one if there is.
    pub fn unpair(&self) {
        let next = self.shared.save(|s| {
            let gone = s.current.take();
            s.daemons.retain(|d| Some(&d.endpoint) != gone.as_ref());
            s.current = s.daemons.first().map(|d| d.endpoint.clone());
            s.current.clone()
        });
        match next {
            Some(endpoint) => self.connect(endpoint, None),
            None => {
                self.disconnect();
                self.forget();
                self.shared.status(Status::Unpaired);
            }
        }
    }

    fn disconnect(&self) {
        if let Some(t) = self.conn_task.lock().unwrap().take() {
            t.abort();
        }
    }

    /// Drops what the last daemon told us.
    fn forget(&self) {
        *self.shared.out.lock().unwrap() = None;
        self.shared.opened.lock().unwrap().clear();
        self.shared.left.lock().unwrap().clear();
        self.shared.edit(|s| {
            *s = State {
                version: s.version,
                ..Default::default()
            }
        });
    }

    fn connect(&self, endpoint: String, pairing: Option<(String, String)>) {
        self.disconnect();
        // The last daemon's sessions are not this one's.
        self.forget();
        self.shared.edit(|s| {
            s.status = Some(Status::Connecting);
            s.pairing = pairing.is_some();
        });
        let (shared, secret) = (self.shared.clone(), self.secret.clone());
        let task = self
            .rt
            .spawn(connection_loop(shared, secret, endpoint, pairing));
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

    /// Sends a 16 kHz mono WAV to the daemon's local speech model and waits
    /// for its text. Blocks, so call it off the UI thread.
    pub fn transcribe(&self, audio: Vec<u8>) -> Result<String, String> {
        if audio.is_empty() {
            return Err("nothing was recorded".into());
        }
        if audio.len() > MAX_VOICE_AUDIO {
            return Err("the voice message is too long".into());
        }
        let request = format!("voice-{}", random_hex(8));
        if self.shared.out.lock().unwrap().is_none() {
            return Err("not connected to the daemon".into());
        }
        self.shared.edit(|s| s.transcription_result = None);
        let pieces = audio.chunks(VOICE_CHUNK);
        let count = pieces.len();
        for (i, piece) in pieces.enumerate() {
            self.send(ClientMsg::Transcribe {
                request: request.clone(),
                audio: piece.to_vec(),
                last: i + 1 == count,
            });
        }
        drop(audio);
        let s = self.shared.state.lock().unwrap();
        let (s, wait) = self
            .shared
            .changed
            .wait_timeout_while(s, Duration::from_secs(300), |s| {
                !matches!(
                    &s.transcription_result,
                    Some((id, _)) if id == &request
                )
            })
            .unwrap();
        if wait.timed_out() {
            return Err("the daemon did not answer; is it still running?".into());
        }
        match s.transcription_result.as_ref() {
            Some((id, result)) if id == &request => result.clone(),
            _ => Err("the daemon did not answer".into()),
        }
    }

    /// Creates a session with `provider`, or the default agent when it is
    /// empty.
    pub fn create(&self, project: &str, provider: &str) {
        self.send(ClientMsg::Create {
            project: project.into(),
            provider: (!provider.is_empty()).then(|| provider.into()),
        });
    }

    /// Makes project `name` in `root` and creates a session in it with
    /// `provider`, or the default agent when it is empty.
    pub fn create_project(&self, root: &str, name: &str, provider: &str) {
        self.send(ClientMsg::CreateProject {
            root: root.into(),
            name: name.into(),
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

    /// Asks the daemon for the past conversations session `id` could take
    /// up; they show up in the snapshot.
    pub fn resumable(&self, id: &str) {
        self.shared.edit(|s| {
            let r = s.resumable.entry(id.into()).or_default();
            r.loading = true;
            r.error = None;
        });
        self.send(ClientMsg::Resumable { id: id.into() });
    }

    /// Takes up one of those conversations in session `id`, which has no
    /// messages yet; they arrive as its history.
    pub fn resume(&self, id: &str, conversation: &str) {
        self.shared.edit(|s| {
            s.resumable.remove(id);
        });
        self.send(ClientMsg::Resume {
            id: id.into(),
            conversation: conversation.into(),
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

    /// Deletes a session from the daemon, with its stored messages.
    pub fn delete(&self, id: &str) {
        self.shared.close_session(id);
        self.send(ClientMsg::Delete { id: id.into() });
    }

    pub fn open(&self, id: &str) {
        self.shared.left.lock().unwrap().remove(id);
        self.shared.opened.lock().unwrap().insert(id.into());
        self.send(ClientMsg::Open { id: id.into() });
    }

    /// Leaves a session; the daemon deletes it when nothing was sent to it.
    pub fn close(&self, id: &str) {
        self.shared.close_session(id);
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

    /// The state as JSON: `status`, `reason`, `projects`, `roots` (where new
    /// projects can go), `agents` (with
    /// whether each is installed and `steers`), `default_agent`, `models` (by
    /// agent: `items`, `error` and `loading`), `resumable` (by session id, the
    /// past conversations it could take up, each with its `id`, `title` and
    /// `updated`, as `items`, `error` and `loading`), `sessions` (most recent first,
    /// each with its `model`, `permissions` and `queue`), `archived` (the
    /// same, or null until asked for), `transcription` (`provider`, and `url`, `model` and `key`
    /// for a remote endpoint, or just `model` for a local one; null when off), `messages` (by session id; a tool call's `tool` has its
    /// progress and what it asks), `error`, `key` (this phone's pairing key),
    /// `version` and `wire_version` (what to show in About), `daemons` (each
    /// paired daemon's `endpoint` and `name`), `daemon` (the endpoint of the
    /// one the app is on, or null) and `pairing` (the connection is a pairing
    /// not let in yet: when it succeeds, the app goes to that daemon).
    pub fn snapshot(&self) -> String {
        let s = self.shared.state.lock().unwrap();
        let saved = self.shared.saved.lock().unwrap();
        let mut v = serde_json::to_value(s.status.clone().unwrap_or(Status::Connecting)).unwrap();
        let o = v.as_object_mut().unwrap();
        o.insert("projects".into(), json!(s.projects));
        o.insert("roots".into(), json!(s.roots));
        o.insert("agents".into(), json!(s.agents));
        o.insert("default_agent".into(), json!(s.default_agent));
        o.insert("models".into(), json!(s.models));
        o.insert("resumable".into(), json!(s.resumable));
        o.insert("sessions".into(), json!(s.sessions));
        o.insert("archived".into(), json!(s.archived));
        o.insert("transcription".into(), json!(s.transcription));
        o.insert("messages".into(), json!(s.messages));
        o.insert("error".into(), json!(s.error));
        o.insert("key".into(), json!(self.key()));
        o.insert("version".into(), json!(grassline_version::full()));
        o.insert("wire_version".into(), json!(VERSION));
        o.insert("daemons".into(), json!(saved.daemons));
        o.insert("daemon".into(), json!(saved.current));
        o.insert("pairing".into(), json!(s.pairing));
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

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    if getrandom::getrandom(&mut b).is_err() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        return format!("{n:x}");
    }
    hex::encode(b)
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
        let result = session(&shared, &endpoint, id, &endpoint_id, &mut pairing).await;
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
    pairing: &mut Option<(String, String)>,
) -> Result<(), Fail> {
    let conn = endpoint
        .connect(id, &grassline_proto::alpn(VERSION))
        .await?;
    let result = talk(shared, &conn, endpoint_id, pairing).await;
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
    pairing: &mut Option<(String, String)>,
) -> Result<(), Fail> {
    let (mut send, mut recv) = conn.open_bi().await?;
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
                    if let ServerMsg::Hello { ok, error, name } = msg {
                        if !ok {
                            return Err(Fail::Refused(error.unwrap_or_else(|| "refused".into())));
                        }
                        hello = true;
                        let paired_now = pairing.take().is_some();
                        shared.save(|s| {
                            match s.daemons.iter_mut().find(|d| d.endpoint == endpoint_id) {
                                Some(d) => d.name = name,
                                None if paired_now => s.daemons.push(Daemon { endpoint: endpoint_id.into(), name }),
                                // Unpaired meanwhile.
                                None => {}
                            }
                            if paired_now {
                                s.current = Some(endpoint_id.into());
                            }
                        });
                        shared.edit(|s| {
                            s.status = Some(Status::Connected);
                            s.pairing = false;
                        });
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
    if let ServerMsg::Removed { id } = &msg {
        shared.opened.lock().unwrap().remove(id);
        shared.left.lock().unwrap().remove(id);
    }
    // The chat was left before the daemon saw its close: keep none of it.
    let keep = match &msg {
        ServerMsg::History { id, .. } | ServerMsg::Msg { id, .. } => {
            !shared.left.lock().unwrap().contains(id)
        }
        _ => true,
    };
    shared.edit(|s| match msg {
        ServerMsg::Hello { .. } => {}
        ServerMsg::Projects { items, roots } => {
            s.projects = items;
            s.roots = roots;
        }
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
        ServerMsg::Resumable { id, items, error } => {
            // Unless the session was left meanwhile.
            if let Some(r) = s.resumable.get_mut(&id) {
                *r = Resumables {
                    items,
                    error,
                    loading: false,
                };
            }
        }
        ServerMsg::Transcription { config } => s.transcription = config,
        ServerMsg::Transcribed {
            request,
            text,
            error,
        } => {
            let result = match (text, error) {
                (Some(text), _) => Ok(text),
                (_, Some(error)) => Err(error),
                _ => Err("the daemon sent no transcript".into()),
            };
            s.transcription_result = Some((request, result));
        }
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
        ServerMsg::Removed { id } => {
            s.sessions.retain(|i| i.id != id);
            if let Some(a) = &mut s.archived {
                a.retain(|i| i.id != id);
            }
            s.messages.remove(&id);
        }
        ServerMsg::History { id, messages } => {
            if keep {
                s.messages.insert(id, messages);
            }
        }
        ServerMsg::Msg { id, index, message } => {
            if keep {
                let list = s.messages.entry(id).or_default();
                if index < list.len() {
                    list[index] = message;
                } else if index == list.len() {
                    list.push(message);
                }
                // A gap means we missed some; the daemon resends the history on a reconnect.
            }
        }
        ServerMsg::Error { message } => s.error = Some(message),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_daemon_paired_before_there_could_be_several() {
        let dir = std::env::temp_dir().join(format!("grassline-saved-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("daemon.json"), r#"{"endpoint":"abc"}"#).unwrap();
        for _ in 0..2 {
            let s = Saved::load(&dir);
            assert_eq!(s.current.as_deref(), Some("abc"));
            assert_eq!(s.daemons.len(), 1);
            assert_eq!(s.daemons[0].endpoint, "abc");
        }
        assert!(!dir.join("daemon.json").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn leaving_a_session_drops_its_messages() {
        let shared = Shared {
            dir: std::env::temp_dir(),
            state: Mutex::new(State::default()),
            saved: Mutex::new(Saved::default()),
            changed: Condvar::new(),
            out: Mutex::new(None),
            opened: Mutex::new(HashSet::new()),
            left: Mutex::new(HashSet::new()),
        };
        let message = |text: &str| Message {
            role: grassline_proto::Role::User,
            text: text.into(),
            ts: 0,
            tool: None,
        };

        // Opening a session brings its history in.
        shared.opened.lock().unwrap().insert("a".into());
        apply(
            &shared,
            ServerMsg::History {
                id: "a".into(),
                messages: vec![message("one")],
            },
        );
        assert_eq!(shared.state.lock().unwrap().messages["a"].len(), 1);

        // Leaving it drops them; what the daemon still sends meanwhile does
        // not bring them back.
        shared.close_session("a");
        assert!(!shared.state.lock().unwrap().messages.contains_key("a"));
        apply(
            &shared,
            ServerMsg::Msg {
                id: "a".into(),
                index: 0,
                message: message("two"),
            },
        );
        assert!(!shared.state.lock().unwrap().messages.contains_key("a"));

        // Opening it again gets the history again.
        shared.left.lock().unwrap().remove("a");
        shared.opened.lock().unwrap().insert("a".into());
        apply(
            &shared,
            ServerMsg::History {
                id: "a".into(),
                messages: vec![message("one"), message("two")],
            },
        );
        assert_eq!(shared.state.lock().unwrap().messages["a"].len(), 2);
    }
}
