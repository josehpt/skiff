//! M4 data plane: the selected Host's `skiff-host`, reached directly beside
//! its skiffd (M1f topology). It is a second endpoint of the same Host: its
//! Hello must name the `host_id` the session plane already accepted, or
//! nothing is sent to it. Its `instance_id` is its own, so its continuity is
//! tracked apart from skiffd's: a data-plane restart is NewInstance here and
//! changes nothing on the session plane.
//!
//! This Mac reaches the service's socket beside skiffd's. A remote Host is
//! reached through a forward of its own, to the socket beside that Host's
//! skiffd. This module never starts a service anywhere; the desktop policy
//! starts one only for a local endpoint.
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use skiff_host_api::{
    Error, Hello, HostChange, HostDiff, HostFile, HostFolder, HostId, HostImage, HostListing, HostProject, HostWorktree,
    ProjectEdit, ProjectFleet, ReadPurpose, Request, Response,
};
use tokio::net::UnixStream;

use super::{
    local::HostSelection,
    ssh::{SshForward, SshOptions, SshTarget},
    HostConnection,
};

/// How long the data-plane service may take to answer Hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);
/// The longest wait between two tries to open a remote data forward.
const RETRY_MAX: Duration = Duration::from_secs(30);

pub(super) enum DataRoute {
    Local(PathBuf),
    Ssh {
        target: SshTarget,
        options: SshOptions,
        dir: PathBuf,
    },
}

/// Kept per selection: the last accepted data Hello, for continuity, and
/// the wait after failed remote tries.
#[derive(Default)]
pub(super) struct DataState {
    previous: Option<Hello>,
    failures: u32,
    next: Option<Instant>,
}

/// One accepted data-plane connection, and for SSH the forward it runs over.
/// Dropping it closes both; it never signals the service.
pub struct DataPlane {
    connection: Arc<HostConnection>,
    _forward: Option<SshForward>,
}
impl DataPlane {
    pub fn connection(&self) -> Arc<HostConnection> {
        self.connection.clone()
    }
}

/// Why the data plane could not be reached, in words that name no local path
/// and repeat no ssh output.
#[derive(Debug)]
pub enum DataError {
    /// Nothing listens at the data socket (local), or the remote channel was
    /// refused: no `skiff-host` runs for this Host.
    NotRunning,
    /// A remote try failed recently; the next waits this long.
    Waiting { seconds: u64 },
}
impl std::fmt::Display for DataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning => write!(f, "skiff-host does not run for this Host"),
            Self::Waiting { seconds } => write!(f, "skiff-host was not reachable; trying again in {seconds}s"),
        }
    }
}
impl std::error::Error for DataError {}

impl HostSelection {
    /// Connects to this Host's data-plane service. `host` is the id the
    /// session plane accepted: a service that reports another is refused
    /// (`identity_changed`) before any request. Tries are serialized per
    /// selection; nothing is retried here.
    pub async fn connect_data(&self, host: &HostId) -> anyhow::Result<DataPlane> {
        let mut state = self.data_state().lock().await;
        if let Some(at) = state.next.filter(|at| *at > Instant::now()) {
            return Err(DataError::Waiting { seconds: (at - Instant::now()).as_secs() + 1 }.into());
        }
        let result = self.try_data(host, state.previous.as_ref()).await;
        match &result {
            Ok(plane) => {
                state.previous = Some(plane.connection.hello().clone());
                state.failures = 0;
                state.next = None;
            }
            Err(_) if !self.is_local() => {
                state.failures += 1;
                let wait = Duration::from_secs(1 << (state.failures - 1).min(5)).min(RETRY_MAX);
                state.next = Some(Instant::now() + wait);
            }
            Err(_) => {}
        }
        result
    }

    async fn try_data(&self, host: &HostId, previous: Option<&Hello>) -> anyhow::Result<DataPlane> {
        let (stream, forward) = match self.data_route() {
            DataRoute::Local(path) => {
                (UnixStream::connect(&path).await.map_err(|_| DataError::NotRunning)?, None)
            }
            DataRoute::Ssh { target, options, dir } => {
                let forward = SshForward::open(&target, &options, &dir).await?;
                let stream = UnixStream::connect(forward.socket()).await.map_err(|_| DataError::NotRunning)?;
                (stream, Some(forward))
            }
        };
        let connection = match HostConnection::connect_with_timeout(stream, previous, HELLO_TIMEOUT).await {
            Ok(c) => c,
            // A forward whose remote socket is missing closes the channel at once.
            Err(Error::TransportLost) => return Err(DataError::NotRunning.into()),
            Err(e) => return Err(e.into()),
        };
        let received = &connection.hello().host.host_id;
        if received != host {
            return Err(Error::IdentityChanged { expected: host.clone(), received: received.clone() }.into());
        }
        Ok(DataPlane { connection: Arc::new(connection), _forward: forward })
    }
}

/// Every project of the Host, open and closed, in its config order.
pub async fn project_list(c: &HostConnection) -> Result<Vec<HostProject>, Error> {
    Ok(project_rows(c).await?.0)
}

/// [`project_list`], and the revision a `project_config` change names.
/// `None` from a Host that predates it.
pub async fn project_rows(c: &HostConnection) -> Result<(Vec<HostProject>, Option<String>), Error> {
    match c.request(Request::ProjectList).await? {
        Response::Projects { projects, revision } => Ok((projects, revision)),
        _ => unreachable!("checked by Request::accepts"),
    }
}

/// One project read again.
pub async fn project_get(c: &HostConnection, name: &str) -> Result<HostProject, Error> {
    match c.request(Request::ProjectGet { name: name.into() }).await? {
        Response::Project { project } => Ok(*project),
        _ => unreachable!("checked by Request::accepts"),
    }
}

/// The changed files of `worktree`, a path on the Host.
pub async fn git_changes(c: &HostConnection, worktree: &str) -> Result<Vec<HostChange>, Error> {
    match c.request(Request::GitChanges { worktree: worktree.into() }).await? {
        Response::Changes { changes } => Ok(changes),
        _ => unreachable!("checked by Request::accepts"),
    }
}

/// One changed file's diff text in `worktree`, or the whole worktree's.
/// Both are paths on the Host.
pub async fn git_diff(c: &HostConnection, worktree: &str, file: Option<&str>) -> Result<HostDiff, Error> {
    match c.request(Request::GitDiff { worktree: worktree.into(), file: file.map(Into::into) }).await? {
        Response::Diff { diff } => Ok(diff),
        _ => unreachable!("checked by Request::accepts"),
    }
}

/// The folder `dir` (relative; empty for the worktree itself) of `worktree`.
pub async fn file_list(c: &HostConnection, worktree: &str, dir: &str) -> Result<HostListing, Error> {
    match c.request(Request::FileList { worktree: worktree.into(), dir: dir.into() }).await? {
        Response::Files { listing } => Ok(listing),
        _ => unreachable!("checked by Request::accepts"),
    }
}

/// `file` (relative) of `worktree`, read on the Host for `purpose`.
pub async fn file_read(c: &HostConnection, worktree: &str, file: &str, purpose: ReadPurpose) -> Result<HostFile, Error> {
    match c.request(Request::FileRead { worktree: worktree.into(), file: file.into(), purpose }).await? {
        Response::File { file } => Ok(*file),
        _ => unreachable!("checked by Request::accepts"),
    }
}

/// The project's background image, as bytes the Host checked.
pub async fn project_background(c: &HostConnection, project: &str) -> Result<HostImage, Error> {
    match c.request(Request::ProjectBackground { project: project.into() }).await? {
        Response::Image { image } => Ok(image),
        _ => unreachable!("checked by Request::accepts"),
    }
}

// Changes. Each is sent once: a lost or late reply is reported, never
// retried, since the Host may have made the change. A project change names
// the revision it was made from, so a stale or repeated one is refused.

/// What the Host would add for `path`, a folder on the Host.
pub async fn project_inspect(c: &HostConnection, path: &str) -> Result<HostFolder, Error> {
    match c.request(Request::ProjectInspect { path: path.into() }).await? {
        Response::Folder { folder } => Ok(folder),
        _ => unreachable!("checked by Request::accepts"),
    }
}

/// Adds the repository at `path` on the Host. Answers the row and the new revision.
pub async fn project_add(
    c: &HostConnection,
    revision: &str,
    path: &str,
    name: Option<String>,
    short: Option<String>,
    color: Option<String>,
) -> Result<(HostProject, String), Error> {
    let request = Request::ProjectAdd { path: path.into(), revision: revision.into(), name, short, color };
    match c.request(request).await? {
        Response::ProjectAdded { project, revision } => Ok((*project, revision)),
        _ => unreachable!("checked by Request::accepts"),
    }
}

/// One key of one project. Answers the new revision.
pub async fn project_edit(c: &HostConnection, revision: &str, project: &str, edit: ProjectEdit) -> Result<String, Error> {
    revised(c.request(Request::ProjectEdit { project: project.into(), revision: revision.into(), edit }).await?)
}

/// The order and the fleets given, in one write. Answers the new revision.
pub async fn project_arrange(
    c: &HostConnection,
    revision: &str,
    order: Vec<String>,
    fleets: Vec<ProjectFleet>,
) -> Result<String, Error> {
    revised(c.request(Request::ProjectArrange { revision: revision.into(), order, fleets }).await?)
}

/// Drops one project's table. Answers the new revision.
pub async fn project_remove(c: &HostConnection, revision: &str, project: &str) -> Result<String, Error> {
    revised(c.request(Request::ProjectRemove { project: project.into(), revision: revision.into() }).await?)
}

fn revised(reply: Response) -> Result<String, Error> {
    match reply {
        Response::Revision { revision } => Ok(revision),
        _ => unreachable!("checked by Request::accepts"),
    }
}

/// A worktree on the new `branch` of a Host project.
pub async fn worktree_add(c: &HostConnection, project: &str, branch: &str, base: Option<&str>) -> Result<HostWorktree, Error> {
    let request = Request::WorktreeAdd { project: project.into(), branch: branch.into(), base: base.map(Into::into) };
    match c.request(request).await? {
        Response::Worktree { worktree } => Ok(worktree),
        _ => unreachable!("checked by Request::accepts"),
    }
}

/// Removes a linked worktree, `path` on the Host, as its Host allows.
pub async fn worktree_remove(c: &HostConnection, project: &str, path: &str, force: bool) -> Result<(), Error> {
    c.request(Request::WorktreeRemove { project: project.into(), path: path.into(), force }).await.map(drop)
}
