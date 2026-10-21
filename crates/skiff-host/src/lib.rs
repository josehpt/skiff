//! `skiff-host`: the Host's data-plane service (M4, first slice). It speaks
//! the Host envelope on its own socket beside skiffd's and answers read-only
//! projects and Git status from the Host's own config and git, through the
//! same `skiff-core` code the app ran before. It holds no PTY and no session:
//! restarting it never touches skiffd or a terminal (#62, M1f topology).
//!
//! Identity: the same durable `host_id` store skiffd reads, beside the
//! selected `projects.toml`, so both endpoints of one Host agree. Its
//! `instance_id` is its own and changes on every start.
use anyhow::{bail, Context};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use skiff_core::{config::ProjectConfig, git, project};
use skiff_host_api::{
    Capability, ChangeStatus, Envelope, Error, Hello, Host, HostChange, HostId, HostProject,
    HostWorktree, InstanceId, Limits, Message, Request, Response, API_MAJOR,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};

/// The largest frame either side sends, newline included.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
/// Requests one connection may have admitted at once, until each reply is
/// written. A client that sends more waits: the service reads no further.
pub const MAX_IN_FLIGHT: usize = 16;

/// What this service offers. Nothing here writes.
pub fn capabilities() -> Vec<Capability> {
    vec![Capability::projects(), Capability::git_status()]
}

/// This run's Hello: the Host's durable id from the store beside the
/// selected `projects.toml`, and a new instance id. Without an id the
/// service does not start: a data plane that cannot say which Host it is
/// must never be paired with one.
pub fn hello() -> anyhow::Result<Hello> {
    let host_id = skiff_core::host_identity::load(&skiff_core::host_identity::path())?;
    Ok(Hello {
        host: Host {
            host_id: HostId::try_from(host_id).map_err(anyhow::Error::msg)?,
            os: std::env::consts::OS.into(),
            account: skiff_core::host_identity::account(),
        },
        instance_id: InstanceId::try_from(uuid::Uuid::new_v4().to_string()).map_err(anyhow::Error::msg)?,
        service_versions: BTreeMap::from([("skiff-host".into(), skiff_core::VERSION.into())]),
        api_major: API_MAJOR,
        capabilities: capabilities(),
        limits: Limits { max_frame_bytes: MAX_FRAME, max_in_flight: MAX_IN_FLIGHT },
    })
}

/// The socket file this service bound. Only it is ever removed:
/// [`BoundSocket::release`] checks that the path still names it.
pub struct BoundSocket {
    path: std::path::PathBuf,
    id: (u64, u64),
}

/// Binds the service socket at `path`. Only a folder it creates gets locked
/// down (`0700`); the socket is `0600`. An entry already at `path` is:
/// - a live socket: another service, so this one does not start
/// - a stale socket of this account: a crash's leftover, removed
/// - anything else (a file, a link, a folder, another account's socket):
///   refused and left as it is
pub async fn bind(path: &Path) -> anyhow::Result<(UnixListener, BoundSocket)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    if let Some(dir) = path.parent().filter(|d| !d.exists()) {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(found) => {
            if !found.file_type().is_socket() {
                bail!("{} exists and is not a socket; skiff-host leaves it as it is", path.display());
            }
            // SAFETY: geteuid takes no arguments and cannot fail.
            if found.uid() != unsafe { libc::geteuid() } {
                bail!("{} belongs to another account; skiff-host leaves it as it is", path.display());
            }
            if UnixStream::connect(path).await.is_ok() {
                bail!("skiff-host is already running at {}", path.display());
            }
            // Still the same stale socket: nothing replaced it meanwhile.
            let again = std::fs::symlink_metadata(path)?;
            if (again.dev(), again.ino()) != (found.dev(), found.ino()) {
                bail!("{} changed while skiff-host checked it", path.display());
            }
            std::fs::remove_file(path)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("check {}", path.display())),
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let meta = std::fs::symlink_metadata(path)?;
    Ok((listener, BoundSocket { path: path.to_path_buf(), id: (meta.dev(), meta.ino()) }))
}

impl BoundSocket {
    /// Removes the socket file only if the path still names the socket this
    /// service bound: never a newer service's socket, nor anything else.
    pub fn release(self) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if let Ok(m) = std::fs::symlink_metadata(&self.path) {
            if m.file_type().is_socket() && (m.dev(), m.ino()) == self.id {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// Accepts connections until the listener fails. Each one is served on its
/// own; dropping this future closes them all, as a stopped service does.
pub async fn serve(listener: UnixListener, hello: Arc<Hello>) -> std::io::Result<()> {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                connections.spawn(handle(stream, hello.clone()));
            }
            Some(_) = connections.join_next() => {}
        }
    }
}

/// Requests admitted and not yet written back or abandoned, over every
/// connection of this process. For diagnostics and tests.
pub fn admitted() -> usize {
    ADMITTED.load(Ordering::Relaxed)
}
static ADMITTED: AtomicUsize = AtomicUsize::new(0);

/// One admitted request, from its frame until its reply is written. Its
/// permit bounds what a connection holds: work, encoded replies and the
/// reply being written, together.
struct Admission(#[allow(dead_code)] OwnedSemaphorePermit);
impl Admission {
    fn new(permit: OwnedSemaphorePermit) -> Self {
        ADMITTED.fetch_add(1, Ordering::Relaxed);
        Self(permit)
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        ADMITTED.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One client: newline-delimited envelopes in, replies out, in any order.
///
/// At most [`MAX_IN_FLIGHT`] requests are admitted at once, and one stays
/// admitted until its reply is written. A client that stops reading stalls
/// at that bound: no more frames are read, so nothing more is computed or
/// queued for it. The work belongs to the connection: when the client is
/// gone, or a frame is unreadable or oversize, what is left is dropped.
/// Nothing is retried.
pub async fn handle(stream: UnixStream, hello: Arc<Hello>) {
    let (rd, mut wr) = stream.into_split();
    let slots = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    // Never full: every queued reply holds one of the permits.
    let (tx, mut rx) = mpsc::channel::<(Vec<u8>, Admission)>(MAX_IN_FLIGHT);
    let writer = async move {
        while let Some((line, admission)) = rx.recv().await {
            if wr.write_all(&line).await.is_err() {
                return;
            }
            drop(admission);
        }
    };
    let reader = async move {
        let mut work = JoinSet::new();
        let mut reader = BufReader::new(rd);
        loop {
            // Finished requests leave the set; a read is never interrupted.
            while work.try_join_next().is_some() {}
            let mut line = Vec::new();
            let read = (&mut reader).take(MAX_FRAME as u64 + 1).read_until(b'\n', &mut line).await;
            match read {
                // The client is done sending: finish what it asked for.
                Ok(0) => break,
                Err(_) => return,
                Ok(_) if line.len() > MAX_FRAME || line.last() != Some(&b'\n') => return,
                Ok(_) => {}
            }
            let Ok(Envelope { id, request }) = serde_json::from_slice::<Envelope>(&line) else {
                return;
            };
            let Ok(permit) = slots.clone().acquire_owned().await else { return };
            let admission = Admission::new(permit);
            let (tx, hello) = (tx.clone(), hello.clone());
            work.spawn(async move {
                let result = answer(request, &hello).await;
                let _ = tx.send((encode(id, result), admission)).await;
            });
        }
        while work.join_next().await.is_some() {}
    };
    tokio::pin!(writer);
    tokio::select! {
        // Every request answered and queued: the writer drains the queue.
        _ = reader => writer.await,
        // The client is gone: the reader and its work are dropped here.
        _ = &mut writer => {}
    }
}

fn encode(id: u64, result: Result<Response, Error>) -> Vec<u8> {
    let mut line = serde_json::to_vec(&Message::Reply { id, result }).unwrap_or_default();
    if line.len() >= MAX_FRAME {
        line = serde_json::to_vec(&Message::Reply {
            id,
            result: Err(Error::OperationFailed { message: "the reply is larger than a frame".into() }),
        })
        .unwrap_or_default();
    }
    line.push(b'\n');
    line
}

/// One request. A family this service does not offer is refused before it
/// runs, as the Hello says.
pub async fn answer(request: Request, hello: &Hello) -> Result<Response, Error> {
    let Some(capability) = request.capability() else {
        return Ok(Response::Hello { hello: hello.clone() });
    };
    hello.require(&capability)?;
    blocking(move || match request {
        Request::ProjectList => Ok(Response::Projects {
            projects: project::list().map_err(failed)?.into_iter().map(host_project).collect(),
        }),
        Request::ProjectGet { name } => {
            let config = config()?;
            let p = config.into_iter().find(|p| p.name == name).ok_or_else(|| Error::NotFound {
                message: format!("no such project: {name}"),
            })?;
            Ok(Response::Project { project: Box::new(host_project(project::Project::from_config(&p))) })
        }
        Request::GitChanges { worktree } => {
            // Held open before the check, so a folder swapped in after it
            // is refused rather than read.
            let root = git::Root::open(Path::new(&worktree));
            let worktree = known_worktree(&worktree)?;
            Ok(Response::Changes {
                changes: git::changes_in(&root.map_err(failed)?, &worktree)
                    .map_err(failed)?
                    .into_iter()
                    .map(host_change)
                    .collect(),
            })
        }
        _ => Err(Error::UnsupportedCapability { capability }),
    })
    .await
}

async fn blocking(f: impl FnOnce() -> Result<Response, Error> + Send + 'static) -> Result<Response, Error> {
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| Err(failed(e)))
}

fn failed(e: impl std::fmt::Display) -> Error {
    Error::OperationFailed { message: format!("{e:#}") }
}

fn config() -> Result<Vec<ProjectConfig>, Error> {
    Ok(skiff_core::config::load().map_err(failed)?.projects)
}

/// `path` as a worktree of one of the Host's open projects, or `not_found`.
/// The service runs git in no other folder, whatever a client names.
fn known_worktree(path: &str) -> Result<std::path::PathBuf, Error> {
    let wanted = Path::new(path);
    for p in config()?.into_iter().filter(|p| !p.closed && p.path.is_dir()) {
        let Ok(worktrees) = git::list_worktrees(&p.path) else { continue };
        if let Some(w) = worktrees.into_iter().find(|w| project::same_path(&w.path, wanted)) {
            return Ok(w.path);
        }
    }
    Err(Error::NotFound { message: format!("{path} is not a worktree of a project on this Host") })
}

/// The app's project row, field for field. Paths stay the Host's own.
pub fn host_project(p: project::Project) -> HostProject {
    HostProject {
        name: p.name,
        short: p.short,
        path: p.path.to_string_lossy().into_owned(),
        color: p.color,
        agents: p.agents,
        icon: p.icon,
        background: p.background,
        background_opacity: p.background_opacity,
        worktrees: p
            .worktrees
            .into_iter()
            .map(|w| HostWorktree {
                path: w.path.to_string_lossy().into_owned(),
                branch: w.branch,
                head: w.head,
                is_main: w.is_main,
                locked: w.locked,
                prunable: w.prunable,
            })
            .collect(),
        default_branch: p.default_branch,
        error: p.error,
        closed: p.closed,
        fleet: p.fleet,
    }
}

pub fn host_change(c: git::Change) -> HostChange {
    HostChange {
        path: c.path,
        status: match c.status {
            'A' => ChangeStatus::A,
            'D' => ChangeStatus::D,
            'U' => ChangeStatus::U,
            _ => ChangeStatus::M,
        },
        added: c.added,
        removed: c.removed,
    }
}
