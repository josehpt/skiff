//! M4 (first slice) data plane, read-only: the Host's projects, fleets and
//! worktrees (`projects@1`) and a worktree's changed files (`git_status@1`).
//! Served by `skiff-host`, the Host's restartable data-plane service, never
//! by skiffd. Every path is a path on the Host: opaque to the client, never
//! opened, resolved or checked on the client's machine.
use serde::{Deserialize, Serialize};

/// One git worktree of a project, as the Host's git reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostWorktree {
    pub path: String,
    /// Short branch name. `None` when detached.
    pub branch: Option<String>,
    /// Commit sha.
    pub head: String,
    pub is_main: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// One project of the Host's `projects.toml`, joined with its worktrees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProject {
    pub name: String,
    pub short: String,
    pub path: String,
    pub color: Option<String>,
    pub agents: Vec<String>,
    /// The rail image as a data URL: bytes, never a path.
    #[serde(default)]
    pub icon: Option<String>,
    /// The background image's path on the Host. Only that Host can read it;
    /// its bytes are a files capability, not this one.
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub background_opacity: Option<u8>,
    pub worktrees: Vec<HostWorktree>,
    #[serde(default)]
    pub default_branch: Option<String>,
    /// Why `worktrees` is empty, e.g. the path is missing on the Host.
    pub error: Option<String>,
    /// Out of the rail. Its worktrees are not read.
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub fleet: Option<String>,
}

/// One changed file in a worktree against HEAD, staged and unstaged together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostChange {
    /// Relative to the worktree root.
    pub path: String,
    /// `M` modified, `A` added or untracked, `D` deleted, `U` unmerged.
    pub status: ChangeStatus,
    /// Lines added and removed. `None` for a binary file.
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeStatus {
    M,
    A,
    D,
    U,
}
