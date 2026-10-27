//! The wire protocol between the daemon and a remote controller.
//!
//! The controller connects over iroh with the ALPN [`ALPN`] and opens one
//! bidirectional stream. Both directions carry JSON lines: [`ClientMsg`] from
//! the controller and [`ServerMsg`] from the daemon. The controller is
//! identified by its iroh endpoint key; a key the daemon does not know must
//! open with [`ClientMsg::Pair`] and a token from the admin interface.

use serde::{Deserialize, Serialize};

pub const ALPN: &[u8] = b"grassline/0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    /// A tool call the agent made, summarized in one line.
    Tool,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub text: String,
    /// Milliseconds since the Unix epoch.
    pub ts: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub path: String,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    Pair { token: String, name: String },
    /// Asks for the projects and the sessions again.
    Refresh,
    Create { project: String, provider: Option<String> },
    /// Starts receiving the messages of a session.
    Open { id: String },
    Send { id: String, text: String },
    Stop { id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// The answer to the first message: whether the key is let in.
    Hello { ok: bool, error: Option<String> },
    Projects { items: Vec<Project> },
    /// Every session, most recently updated first.
    Sessions { items: Vec<SessionInfo> },
    /// One session changed or was created.
    Session { info: SessionInfo },
    History { id: String, messages: Vec<Message> },
    /// The message at `index` of an opened session was added or grew.
    Msg { id: String, index: usize, message: Message },
    Error { message: String },
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
        let m = ClientMsg::Send { id: "a".into(), text: "hi".into() };
        let line = to_line(&m);
        assert_eq!(line, "{\"t\":\"send\",\"id\":\"a\",\"text\":\"hi\"}\n");
        assert_eq!(parse::<ClientMsg>(&line).unwrap(), m);
    }

    #[test]
    fn tickets() {
        assert_eq!(parse_ticket(&ticket("ab", "cd")), Some(("ab", "cd")));
        assert_eq!(parse_ticket("grassline:ab"), None);
        assert_eq!(parse_ticket("nope"), None);
    }
}
