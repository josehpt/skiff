//! M6 data plane: Skipper's ledger as the Ledger panel reads it (`ledger@1`)
//! and Skipper's settings and task types (`skipper@1`). Served by
//! `skiff-host` from the Host's own `ledger.db` and config, read-only: `skiff
//! ledger` on the Host stays the only writer. A result is read by its row's
//! uid, never by a path the client names, and comes back as bounded bytes or
//! text. Paths here are the Host's, for display only.
use serde::{Deserialize, Serialize};

/// One ledger row as the panel takes it: a task, or a plan with the state
/// its tasks give it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLedgerRow {
    pub id: i64,
    /// 8 hex characters; names the row in `ledger_result`.
    pub uid: String,
    /// `task`, or `plan`: a row with no pane that groups tasks.
    pub kind: String,
    /// The plan's uid.
    #[serde(default)]
    pub parent: Option<String>,
    /// The full session id on the Host's skiffd. A plan has none.
    #[serde(default)]
    pub pane_id: Option<String>,
    /// The pane's generation at the send.
    #[serde(default)]
    pub pane_gen: Option<i64>,
    /// Empty: no project.
    pub project: String,
    pub task: String,
    pub sent_at: String,
    #[serde(default)]
    pub title: Option<String>,
    /// `sent`, `pending`, `blocked` or `done`. A plan's comes from its tasks.
    pub state: String,
    pub updated_at: String,
    /// The task type from `[skiff_agent.tasks]`.
    #[serde(default, rename = "type")]
    pub task_type: Option<String>,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub reported_at: Option<String>,
    /// When the person closed the work. Only a work head has it.
    #[serde(default)]
    pub closed_at: Option<String>,
    /// The pane is on another skiffd than the Host's: it was not checked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub elsewhere: bool,
    /// The Host can serve the row's result (`ledger_result`): a regular
    /// file inside the ledger's folder, reached without a link.
    pub result_exists: bool,
}

/// A task type: Skipper starts a task of this type with this agent and model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostTaskType {
    #[serde(rename = "type")]
    pub name: String,
    pub agent: String,
    #[serde(default)]
    pub model: Option<String>,
    /// The command line the Host's skiffd builds. `None` when the agent is gone.
    #[serde(default)]
    pub command: Option<String>,
    /// Why the row does not start what it says.
    #[serde(default)]
    pub error: Option<String>,
}

/// `[skiff_agent]` as the Host reads it, with the folder's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSkipper {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    pub enabled: bool,
    /// The Host's Skipper folder. Absent when the override is refused.
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub folder_error: Option<String>,
    /// The folder the Host's config names, refused or not.
    pub configured: String,
    /// `ready`, `new`, `other` or `refused`.
    pub folder_status: String,
    pub continue_on_restore: bool,
    /// `[skiff_agent.tasks]` in file order.
    pub tasks: Vec<HostTaskType>,
}
