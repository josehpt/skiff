//! Groups and sessions saved to disk, so a daemon restart or a reboot brings
//! back the layout and the names. Processes do not survive: each session comes
//! back as a shell in its folder, under its old id.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use skiff_core::{
    group::Group,
    session::{Role, SessionId, SessionInfo, Was},
};

#[derive(Serialize, Deserialize, Default)]
pub struct Workspace {
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub sessions: Vec<SavedSession>,
}

/// What a session needs to come back. `command` and `args` are what ran, kept
/// for resuming the agent later; a restore starts a shell for now.
#[derive(Serialize, Deserialize, Clone)]
pub struct SavedSession {
    pub id: SessionId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub role: Role,
    pub cwd: PathBuf,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub theme: Option<String>,
    /// The last agent seen in the pane, shown after a restore.
    #[serde(default)]
    pub was: Option<Was>,
    /// The exit code of the last agent, when no client has seen it yet.
    #[serde(default)]
    pub agent_exit: Option<i32>,
    pub cols: u16,
    pub rows: u16,
    /// The Skiff agent's session.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub coordinator: bool,
}

impl From<&SessionInfo> for SavedSession {
    fn from(info: &SessionInfo) -> Self {
        Self {
            id: info.id.clone(),
            name: info.name.clone(),
            label: info.label.clone(),
            role: info.role,
            cwd: info.cwd.clone(),
            command: info.command.clone(),
            args: info.args.clone(),
            theme: info.theme.clone(),
            was: info.was.clone(),
            agent_exit: info.agent_exit,
            cols: info.cols,
            rows: info.rows,
            coordinator: info.coordinator,
        }
    }
}

/// `SKIFF_STATE` overrides. Otherwise `<state dir>/skiff/workspace.json`,
/// or `workspace-dev.json` for a debug build, which runs its own daemon.
pub fn path() -> PathBuf {
    path_for(cfg!(debug_assertions))
}

/// The path a debug (`dev`) or release build computes. `SKIFF_STATE` still
/// overrides.
pub fn path_for(dev: bool) -> PathBuf {
    if let Ok(p) = std::env::var("SKIFF_STATE") {
        return PathBuf::from(p);
    }
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("skiff")
        .join(if dev { "workspace-dev.json" } else { "workspace.json" })
}

/// A missing file is an empty workspace, not an error.
pub fn load(file: &Path) -> Result<Workspace> {
    if !file.exists() {
        return Ok(Workspace::default());
    }
    let text = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse {}", file.display()))
}

pub fn save(file: &Path, ws: &Workspace) -> Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    skiff_core::config::write_atomic(file, &serde_json::to_string_pretty(ws)?)
}
