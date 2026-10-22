//! M0 Host contract. No I/O, service implementation, application state or core types.
//! This contract is not sent by the existing desktop/CLI connection.
use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};

pub mod agents;
pub mod files;
pub mod groups;
pub mod inventory;
pub mod coordinator;
pub mod lifecycle;
pub mod projects;
pub mod terminal;
pub mod updates;
pub use agents::HostAgent;
pub use files::{EntryKind, FileContent, HostEntry, HostFile, HostImage, HostListing, ReadPurpose};
pub use groups::Group;
pub use inventory::InventorySession;
pub use coordinator::{HostLedgerRow, HostSkipper, HostTaskType};
pub use lifecycle::SessionSpec;
pub use projects::{ChangeStatus, HostChange, HostDiff, HostFolder, HostProject, HostWorktree, ProjectEdit, ProjectFleet};
pub use terminal::{EchoTimes, TerminalFrame};
pub use updates::SessionUpdate;

pub const API_MAJOR: u32 = 1;
pub const SESSIONS: &str = "sessions";
pub const CONNECTION_STATUS: &str = "connection_status";
pub const SESSION_SCREEN: &str = "session_screen";
pub const SESSION_INVENTORY: &str = "session_inventory";
pub const SESSION_UPDATES: &str = "session_updates";
pub const SESSION_TERMINAL: &str = "session_terminal";
pub const SESSION_LIFECYCLE: &str = "session_lifecycle";
pub const SESSION_GROUPS: &str = "session_groups";
pub const SESSION_AGENTS: &str = "session_agents";
pub const PROJECTS: &str = "projects";
pub const GIT_STATUS: &str = "git_status";
pub const GIT_DIFF: &str = "git_diff";
pub const PROJECT_CONFIG: &str = "project_config";
pub const WORKTREES: &str = "worktrees";
pub const FILES: &str = "files";
pub const PROJECT_ASSETS: &str = "project_assets";
pub const LEDGER: &str = "ledger";
pub const SKIPPER: &str = "skipper";

macro_rules! identity {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl TryFrom<String> for $name {
            type Error = String;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                if value.trim().is_empty() {
                    return Err(concat!(stringify!($name), " must not be empty").into());
                }
                Ok(Self(value))
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}
identity!(HostId);
identity!(InstanceId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub host_id: HostId,
    pub os: String,
    pub account: String,
}

/// A locator, never identity or an instruction to launch a process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Endpoint {
    Local {
        socket: String,
    },
    /// Descriptor only; M0 has no SSH implementation.
    Ssh {
        hostname: String,
        account: String,
        port: u16,
        socket: String,
    },
}
impl Endpoint {
    /// Only a local endpoint may let the app start, signal, reload or link
    /// processes on its machine. Every other endpoint is reached, never managed.
    pub fn is_local(&self) -> bool {
        match self {
            Self::Local { .. } => true,
            Self::Ssh { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    /// Feature major; unrelated to API_MAJOR and skiffd's PROTOCOL.
    pub version: u32,
}
impl Capability {
    /// Create, end (kill), rename, theme and mark seen one session. Grants
    /// no terminal I/O, inventory or group operation.
    pub fn session_lifecycle() -> Self {
        Self {
            name: SESSION_LIFECYCLE.into(),
            version: 1,
        }
    }
    /// List, save and delete the Host's session groups.
    pub fn session_groups() -> Self {
        Self {
            name: SESSION_GROUPS.into(),
            version: 1,
        }
    }
    /// List the agent CLIs the Host account can start, from its own
    /// environment. Read-only; grants no create and no settings change.
    pub fn session_agents() -> Self {
        Self {
            name: SESSION_AGENTS.into(),
            version: 1,
        }
    }
    /// Read the Host's projects, fleets and worktrees. Data plane
    /// (`skiff-host`); grants no project, config or worktree change.
    pub fn projects() -> Self {
        Self {
            name: PROJECTS.into(),
            version: 1,
        }
    }
    /// Read the changed files of one of the Host's project worktrees. Data
    /// plane; grants no diff text, file read or Git write.
    pub fn git_status() -> Self {
        Self {
            name: GIT_STATUS.into(),
            version: 1,
        }
    }
    /// Read one changed file's diff text, or a whole worktree's, in one of
    /// the Host's project worktrees. Data plane; bounded plain text only:
    /// grants no file read beyond the diff, no external tool and no write.
    pub fn git_diff() -> Self {
        Self {
            name: GIT_DIFF.into(),
            version: 1,
        }
    }
    /// Change the Host's `[[project]]` tables: add, settings, order and
    /// fleets, close, open and remove. Each change names the revision it was
    /// made from, and a changed revision refuses it (`conflict`). Data plane;
    /// grants no image, file, worktree or Git write.
    pub fn project_config() -> Self {
        Self {
            name: PROJECT_CONFIG.into(),
            version: 1,
        }
    }
    /// Add a worktree on a new branch to one of the Host's open projects, or
    /// remove a linked one that no live session uses. Data plane; grants no
    /// other Git write.
    pub fn worktrees() -> Self {
        Self {
            name: WORKTREES.into(),
            version: 1,
        }
    }
    /// List the folders and read the files of one of the Host's open project
    /// worktrees, bounded, without following links or opening anything but
    /// a regular file. Data plane; grants no write, no Git and no path the
    /// client may open.
    pub fn files() -> Self {
        Self {
            name: FILES.into(),
            version: 1,
        }
    }
    /// Read a project's configured background image as validated bytes.
    /// Data plane; grants no write and no other file.
    pub fn project_assets() -> Self {
        Self {
            name: PROJECT_ASSETS.into(),
            version: 1,
        }
    }
    /// Read the Host's ledger as the Ledger panel shows it, a stamp that
    /// changes with it, and a row's result as bounded text or bytes. Data
    /// plane; grants no ledger write, no path the client may open and no
    /// file outside the ledger's folder.
    pub fn ledger_read() -> Self {
        Self {
            name: LEDGER.into(),
            version: 1,
        }
    }
    /// Read the Host's `[skiff_agent]` settings and task types. Data plane;
    /// grants no settings change, no Skipper launch and no watcher control.
    pub fn skipper_settings() -> Self {
        Self {
            name: SKIPPER.into(),
            version: 1,
        }
    }
    /// Attach/output, input, resize and detach of existing sessions; grants
    /// no create, kill, rename or inventory operation.
    pub fn session_terminal() -> Self {
        Self {
            name: SESSION_TERMINAL.into(),
            version: 1,
        }
    }
    /// Read-only non-output session events; grants no request or output.
    pub fn session_updates() -> Self {
        Self {
            name: SESSION_UPDATES.into(),
            version: 1,
        }
    }
    pub fn session_inventory() -> Self {
        Self {
            name: SESSION_INVENTORY.into(),
            version: 1,
        }
    }

    /// Read-only text snapshot; does not grant session lifecycle or events.
    pub fn session_screen() -> Self {
        Self {
            name: SESSION_SCREEN.into(),
            version: 1,
        }
    }
    pub fn connection_status() -> Self {
        Self {
            name: CONNECTION_STATUS.into(),
            version: 1,
        }
    }
    pub fn sessions() -> Self {
        Self {
            name: SESSIONS.into(),
            version: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub max_frame_bytes: usize,
    pub max_in_flight: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    #[serde(flatten)]
    pub host: Host,
    pub instance_id: InstanceId,
    pub service_versions: BTreeMap<String, String>,
    pub api_major: u32,
    pub capabilities: Vec<Capability>,
    pub limits: Limits,
}
impl Hello {
    pub fn validate(&self) -> Result<(), Error> {
        if self.api_major != API_MAJOR {
            return Err(Error::IncompatibleVersion {
                supported: API_MAJOR,
                received: self.api_major,
            });
        }
        if self.host.os.is_empty()
            || self.host.account.is_empty()
            || self.limits.max_frame_bytes == 0
            || self.limits.max_in_flight == 0
            || self
                .capabilities
                .iter()
                .any(|c| c.name.is_empty() || c.version == 0)
            || self
                .capabilities
                .iter()
                .enumerate()
                .any(|(i, c)| self.capabilities[..i].contains(c))
        {
            return Err(Error::MalformedContract {
                message: "invalid Hello metadata or limits".into(),
            });
        }
        Ok(())
    }

    pub fn require(&self, capability: &Capability) -> Result<(), Error> {
        if self.capabilities.contains(capability) {
            Ok(())
        } else {
            Err(Error::UnsupportedCapability {
                capability: capability.clone(),
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Error {
    UnsupportedCapability {
        capability: Capability,
    },
    IncompatibleVersion {
        supported: u32,
        received: u32,
    },
    MalformedContract {
        message: String,
    },
    /// Legacy service rejection without a structured cause; never guess a
    /// not-found/permission code from message text.
    OperationFailed {
        message: String,
    },
    NotFound {
        message: String,
    },
    /// A value the request carries is not one the Host takes.
    Invalid {
        message: String,
    },
    Conflict {
        message: String,
    },
    Permission {
        message: String,
    },
    HostBusy,
    TransportLost,
    Cancelled,
    TimedOut,
    IdentityChanged {
        expected: HostId,
        received: HostId,
    },
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// M0 session projection only. No config, filesystem, ledger or core re-exports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub cwd: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Hello,
    ConnectionStatus,
    SessionInventory,
    SessionScreen {
        session: String,
        rows: u16,
    },
    ListSessions,
    Write {
        session: String,
        #[serde(with = "bytes")]
        data: Vec<u8>,
        #[serde(default)]
        paste: bool,
    },
    Resize {
        session: String,
        cols: u16,
        rows: u16,
    },
    Subscribe {
        session: String,
    },
    Unsubscribe {
        session: String,
    },
    /// (Re)subscribe: a `terminal_snapshot` precedes later output and can
    /// arrive before this reply. Take the output receiver first.
    TerminalAttach {
        session: String,
    },
    TerminalDetach {
        session: String,
    },
    /// A session has one PTY size for every attached client. The Host
    /// applies resizes in the order it receives them: the last one applied
    /// is the size, and every client hears it as a session update. A client
    /// does not answer another client's size with its own; it sends again
    /// only when its own view changes size. Answers `ok`.
    TerminalResize {
        session: String,
        cols: u16,
        rows: u16,
    },
    /// Ordered keystrokes or a paste. Sent without waiting for its reply;
    /// never replayed after loss.
    TerminalInput {
        session: String,
        #[serde(with = "bytes")]
        data: Vec<u8>,
        #[serde(default)]
        paste: bool,
    },
    /// Answers `session` with the new row. Not retried after a timeout:
    /// the session may have started.
    SessionCreate {
        spec: SessionSpec,
    },
    /// Ends the session's process. Answers `ok`. The caller asks first.
    SessionKill {
        session: String,
    },
    /// Answers `session` with the renamed row.
    SessionRename {
        session: String,
        name: String,
    },
    /// `None` clears the session's own theme. Answers `session`.
    SessionSetTheme {
        session: String,
        theme: Option<String>,
    },
    /// Clears unread output and the agent exit mark. Answers `ok`.
    SessionSeen {
        session: String,
    },
    GroupList,
    /// Creates (empty id) or replaces a group. Answers `group` as stored.
    GroupSave {
        group: Group,
    },
    GroupDelete {
        group: String,
    },
    /// Answers `agents` with the Host account's agent list.
    AgentList,
    /// Answers `projects` with every project in the Host's config, open and
    /// closed, in config order.
    ProjectList,
    /// Answers `project` with one project read again: git runs in it only.
    ProjectGet {
        name: String,
    },
    /// Answers `changes`, sorted by path. `worktree` is a Host path, and
    /// must be a worktree of one of the Host's open projects: the Host runs
    /// git nowhere else (`not_found`).
    GitChanges {
        worktree: String,
    },
    /// Answers `diff`: `file`'s diff against HEAD, or the whole worktree's
    /// without untracked files. `worktree` as for `git_changes`; `file` is
    /// relative to it, else `not_found`.
    GitDiff {
        worktree: String,
        #[serde(default)]
        file: Option<String>,
    },
    /// Answers `folder`: what `project_add` would do for `path`, a folder on
    /// the Host. Writes nothing.
    ProjectInspect {
        path: String,
    },
    /// Adds the repository that holds `path` (a Host folder) as a project, or
    /// opens it again when it is a closed one. Answers `project_added`.
    /// `revision` as for `project_edit`.
    ProjectAdd {
        path: String,
        revision: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        short: Option<String>,
        #[serde(default)]
        color: Option<String>,
    },
    /// Changes one key of one project. Answers `revision`, the new one.
    /// Refused (`conflict`) when the Host's `[[project]]` tables are no
    /// longer at `revision`, from `projects` or an earlier write: then the
    /// client reads again. A change is never retried by the client.
    ProjectEdit {
        project: String,
        revision: String,
        edit: ProjectEdit,
    },
    /// Puts the projects in `order` (the rest after them) and sets the
    /// fleets given, in one write. Every name must be a project. Answers
    /// `revision`.
    ProjectArrange {
        revision: String,
        order: Vec<String>,
        #[serde(default)]
        fleets: Vec<ProjectFleet>,
    },
    /// Drops the project's table and settings; its folder stays. Answers `revision`.
    ProjectRemove {
        project: String,
        revision: String,
    },
    /// Adds a worktree on the new `branch`, beside the project's folder.
    /// Answers `worktree`. Not retried: git may have made it.
    WorktreeAdd {
        project: String,
        branch: String,
        #[serde(default)]
        base: Option<String>,
    },
    /// Removes a linked worktree of the project. Refused (`conflict`) while
    /// a session of the Host's skiffd runs in it, or with uncommitted changes
    /// unless `force`, which deletes them; the main worktree is `invalid`.
    /// Answers `ok`.
    WorktreeRemove {
        project: String,
        path: String,
        #[serde(default)]
        force: bool,
    },
    /// Answers `files`: the folder `dir` (relative; empty for the worktree
    /// itself) of `worktree`, checked as for `git_changes`. A path that
    /// leaves the worktree, or passes through a link, is `not_found`.
    FileList {
        worktree: String,
        #[serde(default)]
        dir: String,
    },
    /// Answers `file`: `file` (relative) of `worktree`, read for `purpose`.
    FileRead {
        worktree: String,
        file: String,
        #[serde(default)]
        purpose: ReadPurpose,
    },
    /// Answers `image`: the project's configured background, read on the
    /// Host. No background is `not_found`; a file that is not an image, or
    /// is over 5 MiB, is `invalid`.
    ProjectBackground {
        project: String,
    },
    /// Answers `ledger`: the panel's rows, oldest first. No `ledger.db` on
    /// the Host is no rows; one older than the readers take is `invalid`.
    LedgerRows,
    /// Answers `ledger_stamp`: a value that changes when the ledger does.
    LedgerStamp,
    /// Answers `file`: the result of the row `uid`, read as a preview. A
    /// row with no result, or one outside the ledger's folder, through a
    /// link or not a regular file, is `not_found`.
    LedgerResult {
        uid: String,
    },
    /// Answers `skipper`.
    SkipperSettings,
}
impl Request {
    pub fn capability(&self) -> Option<Capability> {
        match self {
            Self::Hello => None,
            Self::ConnectionStatus => Some(Capability::connection_status()),
            Self::SessionInventory => Some(Capability::session_inventory()),
            Self::SessionScreen { .. } => Some(Capability::session_screen()),
            Self::TerminalAttach { .. }
            | Self::TerminalDetach { .. }
            | Self::TerminalResize { .. }
            | Self::TerminalInput { .. } => Some(Capability::session_terminal()),
            Self::SessionCreate { .. }
            | Self::SessionKill { .. }
            | Self::SessionRename { .. }
            | Self::SessionSetTheme { .. }
            | Self::SessionSeen { .. } => Some(Capability::session_lifecycle()),
            Self::GroupList | Self::GroupSave { .. } | Self::GroupDelete { .. } => {
                Some(Capability::session_groups())
            }
            Self::AgentList => Some(Capability::session_agents()),
            Self::ProjectList | Self::ProjectGet { .. } => Some(Capability::projects()),
            Self::GitChanges { .. } => Some(Capability::git_status()),
            Self::GitDiff { .. } => Some(Capability::git_diff()),
            Self::FileList { .. } | Self::FileRead { .. } => Some(Capability::files()),
            Self::ProjectBackground { .. } => Some(Capability::project_assets()),
            Self::LedgerRows | Self::LedgerStamp | Self::LedgerResult { .. } => Some(Capability::ledger_read()),
            Self::SkipperSettings => Some(Capability::skipper_settings()),
            Self::ProjectInspect { .. }
            | Self::ProjectAdd { .. }
            | Self::ProjectEdit { .. }
            | Self::ProjectArrange { .. }
            | Self::ProjectRemove { .. } => Some(Capability::project_config()),
            Self::WorktreeAdd { .. } | Self::WorktreeRemove { .. } => {
                Some(Capability::worktrees())
            }
            Self::ListSessions
            | Self::Write { .. }
            | Self::Resize { .. }
            | Self::Subscribe { .. }
            | Self::Unsubscribe { .. } => Some(Capability::sessions()),
        }
    }
    pub fn accepts(&self, reply: &Response) -> bool {
        matches!(
            (self, reply),
            (Self::Hello, Response::Hello { .. })
                | (Self::ListSessions, Response::Sessions { .. })
                | (Self::ConnectionStatus, Response::ConnectionStatus { .. })
                | (Self::SessionScreen { .. }, Response::SessionScreen { .. })
                | (Self::SessionInventory, Response::SessionInventory { .. })
                | (
                    Self::SessionCreate { .. }
                        | Self::SessionRename { .. }
                        | Self::SessionSetTheme { .. },
                    Response::Session { .. }
                )
                | (Self::GroupList, Response::Groups { .. })
                | (Self::GroupSave { .. }, Response::Group { .. })
                | (Self::AgentList, Response::Agents { .. })
                | (Self::ProjectList, Response::Projects { .. })
                | (Self::ProjectGet { .. }, Response::Project { .. })
                | (Self::GitChanges { .. }, Response::Changes { .. })
                | (Self::GitDiff { .. }, Response::Diff { .. })
                | (Self::ProjectInspect { .. }, Response::Folder { .. })
                | (Self::ProjectAdd { .. }, Response::ProjectAdded { .. })
                | (
                    Self::ProjectEdit { .. }
                        | Self::ProjectArrange { .. }
                        | Self::ProjectRemove { .. },
                    Response::Revision { .. }
                )
                | (Self::WorktreeAdd { .. }, Response::Worktree { .. })
                | (Self::FileList { .. }, Response::Files { .. })
                | (Self::FileRead { .. }, Response::File { .. })
                | (Self::ProjectBackground { .. }, Response::Image { .. })
                | (Self::LedgerRows, Response::Ledger { .. })
                | (Self::LedgerStamp, Response::LedgerStamp { .. })
                | (Self::LedgerResult { .. }, Response::File { .. })
                | (Self::SkipperSettings, Response::Skipper { .. })
        ) || (!matches!(
            self,
            Self::Hello
                | Self::ListSessions
                | Self::ConnectionStatus
                | Self::SessionScreen { .. }
                | Self::SessionInventory
                | Self::SessionCreate { .. }
                | Self::SessionRename { .. }
                | Self::SessionSetTheme { .. }
                | Self::GroupList
                | Self::GroupSave { .. }
                | Self::AgentList
                | Self::ProjectList
                | Self::ProjectGet { .. }
                | Self::GitChanges { .. }
                | Self::GitDiff { .. }
                | Self::ProjectInspect { .. }
                | Self::ProjectAdd { .. }
                | Self::ProjectEdit { .. }
                | Self::ProjectArrange { .. }
                | Self::ProjectRemove { .. }
                | Self::WorktreeAdd { .. }
                | Self::FileList { .. }
                | Self::FileRead { .. }
                | Self::ProjectBackground { .. }
                | Self::LedgerRows
                | Self::LedgerStamp
                | Self::LedgerResult { .. }
                | Self::SkipperSettings
        ) && matches!(reply, Response::Ok))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: u64,
    pub request: Request,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// `Session` is boxed: a row would otherwise size every reply. Same JSON.
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Session { session: Box<InventorySession> },
    Groups { groups: Vec<Group> },
    Group { group: Group },
    Agents { agents: Vec<HostAgent> },
    Projects {
        projects: Vec<HostProject>,
        /// The `[[project]]` tables these rows were read from, for a
        /// `project_config` change. Absent from a Host without it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<String>,
    },
    /// Boxed, as `Session`. Same JSON.
    Project { project: Box<HostProject> },
    Changes { changes: Vec<HostChange> },
    Diff { diff: HostDiff },
    Folder { folder: HostFolder },
    /// The `[[project]]` tables' revision after a change.
    Revision { revision: String },
    /// Boxed, as `Project`. Same JSON.
    ProjectAdded { project: Box<HostProject>, revision: String },
    Worktree { worktree: HostWorktree },
    Files { listing: HostListing },
    /// Boxed, as `Project`. Same JSON.
    File { file: Box<HostFile> },
    Image { image: HostImage },
    Ledger { rows: Vec<HostLedgerRow> },
    LedgerStamp { stamp: String },
    /// Boxed, as `Project`. Same JSON.
    Skipper {
        #[serde(rename = "skipper")]
        settings: Box<HostSkipper>,
    },
    SessionInventory { sessions: Vec<InventorySession> },
    SessionScreen { lines: Vec<String> },
    ConnectionStatus { version: String, protocol: u32 },
    Hello { hello: Hello },
    Sessions { sessions: Vec<Session> },
    Ok,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Snapshot {
        session: String,
        #[serde(with = "bytes")]
        data: Vec<u8>,
        cols: u16,
        rows: u16,
    },
    Output {
        session: String,
        #[serde(with = "bytes")]
        data: Vec<u8>,
    },
    SessionChanged {
        session: Session,
    },
    /// Boxed: an inventory row would otherwise size every event. Same JSON.
    SessionUpdate {
        update: Box<SessionUpdate>,
    },
    TerminalSnapshot {
        session: String,
        #[serde(with = "bytes")]
        data: Vec<u8>,
        cols: u16,
        rows: u16,
    },
    TerminalOutput {
        session: String,
        #[serde(with = "bytes")]
        data: Vec<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        echo: Option<EchoTimes>,
    },
}
impl Event {
    /// A terminal event's session and frame; None for any other family.
    pub fn into_terminal(self) -> Option<(String, TerminalFrame)> {
        match self {
            Self::TerminalSnapshot {
                session,
                data,
                cols,
                rows,
            } => Some((session, TerminalFrame::Snapshot { data, cols, rows })),
            Self::TerminalOutput {
                session,
                data,
                echo,
            } => Some((session, TerminalFrame::Output { data, echo })),
            Self::Snapshot { .. }
            | Self::Output { .. }
            | Self::SessionChanged { .. }
            | Self::SessionUpdate { .. } => None,
        }
    }
    /// The capability whose negotiation makes this event family meaningful.
    pub fn capability(&self) -> Capability {
        match self {
            Self::SessionUpdate { .. } => Capability::session_updates(),
            Self::TerminalSnapshot { .. } | Self::TerminalOutput { .. } => {
                Capability::session_terminal()
            }
            Self::Snapshot { .. } | Self::Output { .. } | Self::SessionChanged { .. } => {
                Capability::sessions()
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Reply {
        id: u64,
        result: Result<Response, Error>,
    },
    Event {
        event: Event,
    },
}

mod opt_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(bytes: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(b) => serializer.serialize_some(&STANDARD.encode(b)),
            None => serializer.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|s| STANDARD.decode(s).map_err(serde::de::Error::custom))
            .transpose()
    }
}

mod bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        STANDARD
            .decode(String::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}
