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

use skiff_host_api::{Error, Hello, HostChange, HostId, HostProject, Request, Response};
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
    match c.request(Request::ProjectList).await? {
        Response::Projects { projects } => Ok(projects),
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
