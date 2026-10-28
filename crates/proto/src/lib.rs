//! The wire protocol between the daemon and a remote controller.
//!
//! The controller connects over iroh with the ALPN [`ALPN`] and opens one
//! bidirectional stream. Both directions carry JSON lines: [`ClientMsg`] from
//! the controller and [`ServerMsg`] from the daemon. The controller is
//! identified by its iroh endpoint key; a key the daemon does not know must
//! open with [`ClientMsg::Pair`] and a token from the admin interface.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ALPN: &[u8] = b"grassline/0";

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
}

/// Where the controller sends voice messages to be turned into text: an
/// OpenAI-style `audio/transcriptions` endpoint, called from the controller
/// itself, so the audio does not go through the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcription {
    /// The endpoint's full URL.
    pub url: String,
    pub model: String,
    /// Sent as a bearer token.
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    /// The project's name (its directory's name).
    pub project: String,
    pub path: String,
    pub provider: String,
    pub title: String,
    pub created: u64,
    pub updated: u64,
    pub running: bool,
    /// The agent waits for a decision.
    #[serde(default)]
    pub waiting: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
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
    /// Starts receiving the messages of a session.
    Open {
        id: String,
    },
    Send {
        id: String,
        text: String,
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
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// The answer to the first message: whether the key is let in.
    Hello {
        ok: bool,
        error: Option<String>,
    },
    Projects {
        items: Vec<Project>,
    },
    /// The agents, and the one new sessions use by default.
    Agents {
        items: Vec<Agent>,
        default: String,
    },
    /// How to turn voice messages into text; `None` when it is not set up.
    Transcription {
        config: Option<Transcription>,
    },
    /// Every session, most recently updated first.
    Sessions {
        items: Vec<SessionInfo>,
    },
    /// One session changed or was created.
    Session {
        info: SessionInfo,
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

pub fn to_line<T: Serialize>(m: &T) -> String {
    let mut s = serde_json::to_string(m).expect("serializable");
    s.push('\n');
    s
}

pub fn parse<'a, T: Deserialize<'a>>(line: &'a str) -> Result<T, serde_json::Error> {
    serde_json::from_str(line.trim())
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
    fn lines_round_trip() {
        let m = ClientMsg::Send {
            id: "a".into(),
            text: "hi".into(),
        };
        let line = to_line(&m);
        assert_eq!(line, "{\"t\":\"send\",\"id\":\"a\",\"text\":\"hi\"}\n");
        assert_eq!(parse::<ClientMsg>(&line).unwrap(), m);
    }

    #[test]
    fn decisions_default() {
        let m: ClientMsg =
            parse(r#"{"t":"decide","id":"a","request":"r","decision":{"allow":true}}"#).unwrap();
        assert_eq!(
            m,
            ClientMsg::Decide {
                id: "a".into(),
                request: "r".into(),
                decision: Decision {
                    allow: true,
                    ..Default::default()
                }
            }
        );
    }

    #[test]
    fn tickets() {
        assert_eq!(parse_ticket(&ticket("ab", "cd")), Some(("ab", "cd")));
        assert_eq!(parse_ticket("grassline:ab"), None);
        assert_eq!(parse_ticket("nope"), None);
    }
}
