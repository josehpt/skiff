//! M3 agent list: the agent CLIs the Host account can start, as its own
//! environment and settings say (its login PATH, its `projects.toml`). Never
//! the client's. Read-only: editing agent settings is not a Host API yet.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostAgent {
    pub id: String,
    /// The command line a new session of this agent runs on the Host.
    pub command: String,
    /// Empty for an agent the Host account added itself.
    #[serde(default)]
    pub default_command: String,
    #[serde(default)]
    pub custom: bool,
    /// Found on the Host account's PATH.
    pub installed: bool,
    /// Offered to start. Only an installed agent is enabled.
    pub enabled: bool,
}
