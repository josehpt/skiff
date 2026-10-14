//! Newline-delimited JSON over a Unix socket.
//!
//! Client → daemon: one [`Envelope`] per line.
//! Daemon → client: one [`ServerMessage`] per line, either a [`Reply`] that
//! carries the request `id`, or an [`Event`] with no `id`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::group::Group;
use crate::project::AgentInfo;
use crate::session::{Away, SessionId, SessionInfo, SessionSpec, SessionState};

/// Base64 for byte payloads so the wire stays valid UTF-8 JSON.
pub mod b64 {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        STANDARD.encode(v).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    ListSessions,
    CreateSession {
        spec: SessionSpec,
    },
    Write {
        session: SessionId,
        #[serde(with = "b64")]
        data: Vec<u8>,
        /// The data is a paste. The daemon wraps it in bracketed paste when
        /// the program turned that on, so the program does not take its
        /// newlines, or a fast Enter after it, as part of the text.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        paste: bool,
    },
    /// Types `data` and, with `enter`, presses Enter after
    /// [`ENTER_DELAY`], as one step: only if the session took no input since
    /// its `input_gen` was `expect`. For text a program sends on its own,
    /// so it never mixes with what the person types. Answers
    /// [`Response::Ok`] when sent, [`Response::Refused`] when not.
    ///
    /// [`ENTER_DELAY`]: crate::session::ENTER_DELAY
    Submit {
        session: SessionId,
        #[serde(with = "b64")]
        data: Vec<u8>,
        /// As in [`Request::Write`].
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        paste: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        enter: bool,
        expect: u64,
    },
    Resize {
        session: SessionId,
        cols: u16,
        rows: u16,
    },
    /// Start receiving output for this session on this connection. The daemon
    /// sends one [`Event::Snapshot`] first, then [`Event::Output`] from there on.
    /// The reply can arrive before or after the snapshot.
    Subscribe {
        session: SessionId,
    },
    Unsubscribe {
        session: SessionId,
    },
    /// Sets the user's name for a session. Empty clears it.
    RenameSession {
        session: SessionId,
        name: String,
    },
    /// Sets one terminal's theme. `null` returns it to the project's.
    SetSessionTheme {
        session: SessionId,
        #[serde(default)]
        theme: Option<String>,
    },
    /// The session's pane has the keys: the user has seen it. Clears
    /// `unread` and `agent_exit`.
    Seen {
        session: SessionId,
    },
    /// The last `rows` rows of the session's text above the agent's input
    /// box, for mission control. Answers [`Response::Screen`].
    Screen {
        session: SessionId,
        rows: u16,
    },
    /// Terminate the process and drop the session.
    Kill {
        session: SessionId,
    },
    /// The known agent CLIs, installed and enabled.
    ListAgents,
    /// Runs an agent's own session list command in a folder, for the
    /// resume list. Answers with what it printed.
    ListAgentSessions {
        command: Vec<String>,
        cwd: PathBuf,
    },
    ListGroups,
    /// Empty `group.id` creates a group. Otherwise replaces that group.
    SaveGroup {
        group: Group,
    },
    /// `group` is the group id.
    DeleteGroup {
        group: String,
    },
    /// Moves skiffd onto another binary in place. The pid, sessions, PTYs and
    /// terminals stay. `binary` is an absolute path; `None` is the daemon's
    /// own executable as it is on disk now. A failure answers with an error
    /// from the old daemon, which carries on. Success ends the connection
    /// with no reply; only a new connection can confirm it (same `pid` in
    /// [`Response::Pong`], same session ids and pids).
    ///
    /// Keep this request's JSON form, `{"cmd":"reload","binary":"/path"}`,
    /// across protocol changes: it is how an app moves an older daemon on.
    Reload {
        #[serde(default)]
        binary: Option<PathBuf>,
    },
    /// Stops skiffd as SIGTERM from outside does: it saves the layout and
    /// ends every session. Answers `ok` first. A pane uses this for a
    /// deliberate stop: skiffd ignores signals from its own sessions.
    Shutdown,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Envelope {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Pong {
        version: String,
        /// 0 from daemons that predate the field.
        #[serde(default)]
        protocol: u32,
        /// The handover version the daemon writes on reload. `None`: it
        /// cannot reload.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reload_state: Option<u32>,
        /// The daemon's process id, which a reload keeps.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
    Sessions { sessions: Vec<SessionInfo> },
    Session { session: SessionInfo },
    Agents { agents: Vec<AgentInfo> },
    Output { stdout: String },
    Screen { lines: Vec<String> },
    Groups { groups: Vec<Group> },
    Group { group: Group },
    Error { message: String },
    /// A [`Request::Submit`] typed nothing: input came after its check.
    Refused,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Reply {
    pub id: u64,
    #[serde(flatten)]
    pub response: Response,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Output {
        session: SessionId,
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    /// Bytes that redraw the current screen and scrollback. Starts with a
    /// full reset. Sized for the session's current `cols` x `rows`.
    Snapshot {
        session: SessionId,
        #[serde(with = "b64")]
        data: Vec<u8>,
        cols: u16,
        rows: u16,
    },
    State {
        session: SessionId,
        state: SessionState,
    },
    /// The worktrees other than its own where the session works changed.
    Away {
        session: SessionId,
        away: Vec<Away>,
    },
    /// The program set or cleared its terminal title.
    Title {
        session: SessionId,
        title: Option<String>,
    },
    Exit {
        session: SessionId,
        code: Option<i32>,
    },
    SessionCreated {
        session: SessionInfo,
    },
    /// An agent finished a turn, or a program ended and left the shell in
    /// front: there is a result to read. Sent for each one, though `unread`
    /// is set already.
    TurnEnded {
        session: SessionId,
    },
    /// A session's info changed outside the state and title events.
    SessionUpdated {
        session: SessionInfo,
    },
    SessionRemoved {
        session: SessionId,
    },
    /// A group was saved, deleted, or lost a killed session. Clients reload groups.
    GroupsChanged {},
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum ServerMessage {
    Reply(Reply),
    Event(Event),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_and_event_round_trip() {
        let reply = ServerMessage::Reply(Reply {
            id: 7,
            response: Response::Pong {
                version: "0.1.0".into(),
                protocol: 2,
                reload_state: None,
                pid: None,
            },
        });
        let json = serde_json::to_string(&reply).unwrap();
        assert!(json.contains("\"id\":7") && json.contains("\"type\":\"pong\""));
        match serde_json::from_str::<ServerMessage>(&json).unwrap() {
            ServerMessage::Reply(r) => assert_eq!(r.id, 7),
            other => panic!("expected reply, got {other:?}"),
        }

        let event = ServerMessage::Event(Event::Output {
            session: "abc".into(),
            data: b"hi\x07".to_vec(),
        });
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"output\""));
        match serde_json::from_str::<ServerMessage>(&json).unwrap() {
            ServerMessage::Event(Event::Output { session, data }) => {
                assert_eq!(session, "abc");
                assert_eq!(data, b"hi\x07");
            }
            other => panic!("expected output event, got {other:?}"),
        }
    }

    #[test]
    fn request_envelope_parses() {
        let line = r#"{"id":1,"cmd":"write","session":"s1","data":"aGk="}"#;
        let env: Envelope = serde_json::from_str(line).unwrap();
        match env.request {
            Request::Write { session, data, .. } => {
                assert_eq!(session, "s1");
                assert_eq!(data, b"hi");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
