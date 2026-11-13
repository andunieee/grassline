//! The wire protocol between the daemon and a remote controller.
//!
//! The controller connects over iroh with the ALPN of [`VERSION`] and opens one
//! bidirectional stream. Both directions carry [frames](encode): [`ClientMsg`]
//! from the controller and [`ServerMsg`] from the daemon. The controller is
//! identified by its iroh endpoint key; a key the daemon does not know must
//! open with [`ClientMsg::Pair`] and a token from the admin interface.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The version of the protocol, in its ALPN. Bump it whenever a message
/// changes: the daemon then tells controllers of older versions to update.
pub const VERSION: u32 = 11;

/// The code the daemon closes a connection of an older version with; the
/// reason is its own [`VERSION`].
pub const OUTDATED: u32 = 1;

/// The ALPN of a version: `grassline/<version>`.
pub fn alpn(version: u32) -> Vec<u8> {
    format!("grassline/{version}").into_bytes()
}

/// The version of an ALPN of [`alpn`].
pub fn alpn_version(alpn: &[u8]) -> Option<u32> {
    std::str::from_utf8(alpn)
        .ok()?
        .strip_prefix("grassline/")?
        .parse()
        .ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    /// A tool call the agent made: `text` summarizes it in one line, `tool`
    /// follows its progress.
    Tool,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub text: String,
    /// Milliseconds since the Unix epoch.
    pub ts: u64,
    #[serde(default)]
    pub tool: Option<ToolCall>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider's id for the call.
    pub id: String,
    pub name: String,
    /// The arguments, as pretty JSON.
    pub input: String,
    pub status: ToolStatus,
    /// What it returned, cut to a few kilobytes.
    #[serde(default)]
    pub output: Option<String>,
    /// While `status` is `Waiting`: what the agent asks.
    #[serde(default)]
    pub ask: Option<Ask>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Running,
    /// Waiting for a [`ClientMsg::Decide`].
    Waiting,
    Done,
    Failed,
    Denied,
}

/// A decision the agent waits for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    /// The id to answer with.
    pub request: String,
    /// Questions to answer by picking options; empty when the agent only asks
    /// for permission to run the tool.
    #[serde(default)]
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub question: String,
    /// A short label for it.
    pub header: String,
    pub options: Vec<Choice>,
    /// Several options may be picked.
    pub multi: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub label: String,
    pub description: String,
}

/// The answer to an [`Ask`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Decision {
    pub allow: bool,
    /// Also allow this tool, without asking, for the rest of the session.
    pub always: bool,
    /// By question: the picked labels, joined with ", ", or free text.
    pub answers: BTreeMap<String, String>,
    /// Why it was denied, for the agent.
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub path: String,
}

/// An agent the daemon can run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    /// The provider's name, as in [`SessionInfo::provider`].
    pub name: String,
    /// Its binary was found on the daemon's machine.
    pub installed: bool,
    /// It takes queued messages while it works ([`ClientMsg::SendNow`]).
    pub steers: bool,
}

/// A model an agent can use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    /// What [`ClientMsg::SetModel`] takes; empty for the agent's default.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// What the agent of a session may do without asking. It can still ask
/// questions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permissions {
    /// It asks before every tool that needs permission.
    Ask,
    /// It decides by itself what is safe and asks for the rest (how depends
    /// on the agent).
    #[default]
    Auto,
    /// It never asks.
    All,
}

/// Where the controller sends voice messages to be turned into text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum Transcription {
    /// An OpenAI-style `audio/transcriptions` endpoint, called from the
    /// controller itself, so the audio does not go through the daemon.
    Remote {
        /// The endpoint's full URL.
        url: String,
        model: String,
        /// Sent as a bearer token.
        key: String,
    },
    /// A model on the daemon's machine. The controller sends the audio to the
    /// daemon with [`ClientMsg::Transcribe`].
    Local { model: String },
}

/// A [`Transcription`] as it goes over the wire.
///
/// The protocol is postcard, which cannot deserialize the internally tagged
/// enum above: it needs a flat struct. The controller shows this as the app's
/// transcription setting, whose JSON shape is the same as the enum's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeechConfig {
    /// "remote" or "local".
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub key: String,
}

impl From<Transcription> for SpeechConfig {
    fn from(t: Transcription) -> Self {
        match t {
            Transcription::Remote { url, model, key } => SpeechConfig {
                provider: "remote".into(),
                url,
                model,
                key,
            },
            Transcription::Local { model } => SpeechConfig {
                provider: "local".into(),
                url: String::new(),
                model,
                key: String::new(),
            },
        }
    }
}

/// A conversation the agent of a session had before in its directory, which
/// the session can take up with [`ClientMsg::Resume`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resumable {
    /// The agent's id for it.
    pub id: String,
    pub title: String,
    /// Milliseconds since the Unix epoch.
    pub updated: u64,
}

/// How many [`Resumable`] conversations the daemon offers, the newest.
pub const RESUMABLE_MAX: usize = 5;

/// A message waiting for the agent to finish its turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Queued {
    pub id: String,
    pub text: String,
    /// It was given to the running turn, which has not taken it in yet.
    #[serde(default)]
    pub steering: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    /// The project's name (its directory's name).
    pub project: String,
    pub path: String,
    pub provider: String,
    pub title: String,
    /// The start of the last message, the user's or the agent's.
    #[serde(default)]
    pub preview: String,
    pub created: u64,
    pub updated: u64,
    pub running: bool,
    /// The agent waits for a decision.
    #[serde(default)]
    pub waiting: bool,
    /// A [`Model::id`]; empty for the agent's default.
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub permissions: Permissions,
    /// The messages sent while the agent works, in order; each goes to it
    /// once it has answered the one before.
    #[serde(default)]
    pub queue: Vec<Queued>,
    /// It was put away: it is left out of [`ServerMsg::Sessions`] and listed
    /// in [`ServerMsg::Archived`] instead.
    #[serde(default)]
    pub archived: bool,
    /// The index of the agent's first message no controller has seen: it
    /// arrived with the session closed everywhere. Opening the session
    /// clears it.
    #[serde(default)]
    pub unread: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientMsg {
    Pair {
        token: String,
        name: String,
    },
    /// Asks for the projects and the sessions again.
    Refresh,
    /// Creates a session; without `provider`, with the default agent.
    Create {
        project: String,
        provider: Option<String>,
    },
    /// Makes directory `name` in `root`, one of [`ServerMsg::Projects`]'
    /// roots, and creates a session in it like [`ClientMsg::Create`].
    CreateProject {
        root: String,
        name: String,
        provider: Option<String>,
    },
    /// Sets the agent new sessions use by default.
    SetDefaultAgent {
        provider: String,
    },
    /// Switches session `id` to another agent. It starts a new conversation
    /// with it: the agent does not see the earlier messages.
    SetAgent {
        id: String,
        provider: String,
    },
    /// Sets the model of session `id` from its next message on; new
    /// sessions with the same agent get it too.
    SetModel {
        id: String,
        model: String,
    },
    /// Sets what the agent of session `id` may do without asking, from its
    /// next message on.
    SetPermissions {
        id: String,
        permissions: Permissions,
    },
    /// Asks for the models of an agent; answered with [`ServerMsg::Models`].
    Models {
        provider: String,
    },
    /// One piece of a voice message, turned into text with the daemon's local
    /// speech model. The pieces of a 16 kHz mono WAV arrive in order, each at
    /// most [`VOICE_CHUNK`] bytes, the last with `last` set; together they are
    /// answered once with [`ServerMsg::Transcribed`].
    Transcribe {
        request: String,
        audio: Vec<u8>,
        /// This is the last piece of `request`.
        last: bool,
    },
    /// Puts session `id` in the archive, or takes it out. Sending it a
    /// message takes it out too.
    Archive {
        id: String,
        archived: bool,
    },
    /// Asks for the archived sessions; answered with [`ServerMsg::Archived`].
    Archived,
    /// Deletes session `id` and its stored messages from the daemon.
    Delete {
        id: String,
    },
    /// Asks for the conversations session `id`'s agent had before in its
    /// directory, which no session has; answered with
    /// [`ServerMsg::Resumable`].
    Resumable {
        id: String,
    },
    /// Takes up one of those conversations in session `id`, which must have
    /// no messages yet: they become the agent's, and the next message goes
    /// on with it.
    Resume {
        id: String,
        conversation: String,
    },
    /// Starts receiving the messages of a session; it is read from then on.
    Open {
        id: String,
    },
    /// Stops receiving the messages of a session; it is deleted when
    /// nothing was sent to it.
    Close {
        id: String,
    },
    /// Sends a message; while the agent works, it is queued.
    Send {
        id: String,
        text: String,
    },
    /// Sends a queued message now: into the running turn when the agent
    /// [steers](Agent::steers), otherwise by stopping the turn and starting
    /// the next with it.
    SendNow {
        id: String,
        queued: String,
    },
    /// Takes a message out of the queue.
    Unqueue {
        id: String,
        queued: String,
    },
    Stop {
        id: String,
    },
    /// Answers the [`Ask`] of a tool call in session `id`.
    Decide {
        id: String,
        request: String,
        decision: Decision,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerMsg {
    /// The answer to the first message: whether the key is let in.
    Hello {
        ok: bool,
        error: Option<String>,
        /// What the daemon calls itself, for the app to tell daemons apart.
        name: String,
    },
    Projects {
        items: Vec<Project>,
        /// The directories whose subdirectories are projects, as
        /// configured; new projects go in one of them.
        #[serde(default)]
        roots: Vec<String>,
    },
    /// The agents, and the one new sessions use by default.
    Agents {
        items: Vec<Agent>,
        default: String,
    },
    /// The models of an agent, or why they could not be listed.
    Models {
        provider: String,
        items: Vec<Model>,
        error: Option<String>,
    },
    /// How to turn voice messages into text; `None` when it is not set up.
    Transcription {
        config: Option<SpeechConfig>,
    },
    /// The answer to a [`ClientMsg::Transcribe`]. Exactly one of `text` and
    /// `error` is set.
    Transcribed {
        request: String,
        text: Option<String>,
        error: Option<String>,
    },
    /// Every session but the archived ones, most recently updated first.
    Sessions {
        items: Vec<SessionInfo>,
    },
    /// The archived sessions, most recently updated first.
    Archived {
        items: Vec<SessionInfo>,
    },
    /// The answer to a [`ClientMsg::Resumable`]: at most [`RESUMABLE_MAX`],
    /// newest first, or why they could not be listed.
    Resumable {
        id: String,
        items: Vec<Resumable>,
        error: Option<String>,
    },
    /// One session changed or was created; it may have gone in or out of the
    /// archive.
    Session {
        info: SessionInfo,
    },
    /// Session `id` was deleted, with its stored messages.
    Removed {
        id: String,
    },
    History {
        id: String,
        messages: Vec<Message>,
    },
    /// The message at `index` of an opened session was added or grew.
    Msg {
        id: String,
        index: usize,
        message: Message,
    },
    Error {
        message: String,
    },
}

/// How long a voice message may be, in seconds. Keep this in sync with
/// `MAX_VOICE_MINUTES` in the Android app.
pub const MAX_VOICE_SECONDS: usize = 15 * 60;

/// The most audio a single voice message may carry, in bytes: [`MAX_VOICE_SECONDS`]
/// of 16 kHz mono 16-bit PCM, with room for the WAV header.
pub const MAX_VOICE_AUDIO: usize = MAX_VOICE_SECONDS * 32_000 + 4 * 1024;

/// The most audio one [`ClientMsg::Transcribe`] piece carries.
pub const VOICE_CHUNK: usize = 1 << 20;

/// The most a stream may hold back before a frame is whole. A voice message
/// goes in [`VOICE_CHUNK`] pieces, so this leaves room for the other messages,
/// like a long session's history.
const MAX_PENDING: usize = 16 << 20;

/// `m` as a frame: its length as a little-endian `u32`, then `m` in postcard.
/// Postcard does not say which fields are there, so both ends must have the
/// same types: bump [`VERSION`] when they change.
pub fn encode<T: Serialize>(m: &T) -> Vec<u8> {
    let body = postcard::to_allocvec(m).expect("serializable");
    let mut frame = (body.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&body);
    frame
}

/// The frames in the bytes of a stream.
#[derive(Default)]
pub struct Frames(Vec<u8>);

impl Frames {
    /// Adds bytes read from the stream; fails when too many wait for the rest
    /// of their frame.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.0.len() + bytes.len() > MAX_PENDING {
            return Err("frame too long".into());
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }

    /// The next whole frame, decoded; it is taken out even when it does not
    /// decode.
    pub fn next<T: DeserializeOwned>(&mut self) -> Option<Result<T, String>> {
        let len = u32::from_le_bytes(self.0.get(..4)?.try_into().unwrap()) as usize;
        let body = self.0.get(4..4 + len)?;
        let m = postcard::from_bytes(body).map_err(|e| e.to_string());
        self.0.drain(..4 + len);
        Some(m)
    }
}

/// The text a controller scans or pastes to pair: `grassline:<endpoint id>:<token>`.
pub fn ticket(endpoint: &str, token: &str) -> String {
    format!("grassline:{endpoint}:{token}")
}

/// Splits a [`ticket`] into the endpoint id and the token.
pub fn parse_ticket(s: &str) -> Option<(&str, &str)> {
    let rest = s.trim().strip_prefix("grassline:")?;
    let (id, token) = rest.split_once(':')?;
    (!id.is_empty() && !token.is_empty()).then_some((id, token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let a = ClientMsg::Send {
            id: "a".into(),
            text: "hi".into(),
        };
        let b = ClientMsg::Refresh;
        let bytes = [encode(&a), encode(&b)].concat();
        let mut frames = Frames::default();
        // A byte at a time, as a stream may cut them.
        let mut got = vec![];
        for byte in &bytes {
            frames.push(&[*byte]).unwrap();
            while let Some(m) = frames.next::<ClientMsg>() {
                got.push(m.unwrap());
            }
        }
        assert_eq!(got, [a, b]);
    }

    #[test]
    fn voice_pieces_fit_a_frame() {
        let piece = ClientMsg::Transcribe {
            request: "voice-0123456789abcdef".into(),
            audio: vec![0; VOICE_CHUNK],
            last: false,
        };
        assert!(encode(&piece).len() <= VOICE_CHUNK + 1024);
    }

    #[test]
    fn bad_frames_are_skipped() {
        let mut frames = Frames::default();
        frames.push(&[1, 0, 0, 0, 255]).unwrap();
        frames.push(&encode(&ClientMsg::Refresh)).unwrap();
        assert!(frames.next::<ClientMsg>().unwrap().is_err());
        assert_eq!(frames.next::<ClientMsg>().unwrap(), Ok(ClientMsg::Refresh));
        assert!(frames.next::<ClientMsg>().is_none());
    }

    #[test]
    fn transcriptions_say_where_they_run() {
        let local = Transcription::Local {
            model: "parakeet-tdt-0.6b-v2-int8".into(),
        };
        assert_eq!(
            serde_json::to_value(&local).unwrap(),
            serde_json::json!({
                "provider": "local",
                "model": "parakeet-tdt-0.6b-v2-int8",
            })
        );
        let remote = Transcription::Remote {
            url: "https://example.com/v1/audio/transcriptions".into(),
            model: "whisper-1".into(),
            key: "k".into(),
        };
        assert_eq!(
            serde_json::to_value(&remote).unwrap(),
            serde_json::json!({
                "provider": "remote",
                "url": "https://example.com/v1/audio/transcriptions",
                "model": "whisper-1",
                "key": "k",
            })
        );
        // The app's snapshot has the same flat shape, so the Kotlin side
        // reads both the same way.
        assert_eq!(
            serde_json::to_value(SpeechConfig::from(remote)).unwrap(),
            serde_json::json!({
                "provider": "remote",
                "url": "https://example.com/v1/audio/transcriptions",
                "model": "whisper-1",
                "key": "k",
            })
        );
        assert_eq!(
            serde_json::to_value(SpeechConfig::from(local)).unwrap(),
            serde_json::json!({
                "provider": "local",
                "url": "",
                "model": "parakeet-tdt-0.6b-v2-int8",
                "key": "",
            })
        );
    }

    #[test]
    fn transcriptions_survive_postcard() {
        // The internally tagged enum is fine for JSON, but postcard needs the
        // flat `SpeechConfig` to deserialize it.
        for t in [
            Transcription::Local {
                model: "parakeet-tdt-0.6b-v2-int8".into(),
            },
            Transcription::Remote {
                url: "https://example.com/v1/audio/transcriptions".into(),
                model: "whisper-1".into(),
                key: "k".into(),
            },
        ] {
            let msg = ServerMsg::Transcription {
                config: Some(t.into()),
            };
            let mut frames = Frames::default();
            frames.push(&encode(&msg)).unwrap();
            assert_eq!(frames.next::<ServerMsg>().unwrap(), Ok(msg));
        }
    }

    #[test]
    fn decisions_default() {
        let d: Decision = serde_json::from_str(r#"{"allow":true}"#).unwrap();
        assert_eq!(
            d,
            Decision {
                allow: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn alpns() {
        assert_eq!(alpn_version(&alpn(VERSION)), Some(VERSION));
        assert_eq!(alpn_version(b"grassline/x"), None);
        assert_eq!(alpn_version(b"hyperb/0"), None);
    }

    #[test]
    fn tickets() {
        assert_eq!(parse_ticket(&ticket("ab", "cd")), Some(("ab", "cd")));
        assert_eq!(parse_ticket("grassline:ab"), None);
        assert_eq!(parse_ticket("nope"), None);
    }
}
