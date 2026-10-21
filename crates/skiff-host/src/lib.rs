//! `skiff-host`: the Host's data-plane service (M4, first slice). It speaks
//! the Host envelope on its own socket beside skiffd's and answers read-only
//! projects and Git status from the Host's own config and git, through the
//! same `skiff-core` code the app ran before. It holds no PTY and no session:
//! restarting it never touches skiffd or a terminal (#62, M1f topology).
//!
//! Identity: the same durable `host_id` store skiffd reads, beside the
//! selected `projects.toml`, so both endpoints of one Host agree. Its
//! `instance_id` is its own and changes on every start.
use std::{collections::BTreeMap, path::Path, sync::Arc};

use skiff_core::{config::ProjectConfig, git, project};
use skiff_host_api::{
    Capability, ChangeStatus, Envelope, Error, Hello, Host, HostChange, HostId, HostProject,
    HostWorktree, InstanceId, Limits, Message, Request, Response, API_MAJOR,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, Semaphore},
};

/// The largest frame either side sends, newline included.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
/// Requests one connection may have running at once. More get `host_busy`.
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

/// One client: newline-delimited envelopes in, replies out, in any order.
/// An unreadable or oversize frame ends the connection; nothing is retried.
pub async fn handle(stream: UnixStream, hello: Arc<Hello>) {
    let (rd, mut wr) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(MAX_IN_FLIGHT * 2);
    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if wr.write_all(&line).await.is_err() {
                break;
            }
        }
    });
    let slots = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let mut reader = BufReader::new(rd);
    loop {
        let mut line = Vec::new();
        match (&mut reader).take(MAX_FRAME as u64 + 1).read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) if line.len() > MAX_FRAME || line.last() != Some(&b'\n') => break,
            Ok(_) => {}
        }
        let Ok(Envelope { id, request }) = serde_json::from_slice::<Envelope>(&line) else {
            break;
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            send(&tx, id, Err(Error::HostBusy)).await;
            continue;
        };
        let (tx, hello) = (tx.clone(), hello.clone());
        tokio::spawn(async move {
            let result = answer(request, &hello).await;
            drop(slot);
            send(&tx, id, result).await;
        });
    }
    drop(tx);
    let _ = writer.await;
}

async fn send(tx: &mpsc::Sender<Vec<u8>>, id: u64, result: Result<Response, Error>) {
    let mut line = serde_json::to_vec(&Message::Reply { id, result }).unwrap_or_default();
    if line.len() >= MAX_FRAME {
        line = serde_json::to_vec(&Message::Reply {
            id,
            result: Err(Error::OperationFailed { message: "the reply is larger than a frame".into() }),
        })
        .unwrap_or_default();
    }
    line.push(b'\n');
    let _ = tx.send(line).await;
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
            let worktree = known_worktree(&worktree)?;
            Ok(Response::Changes {
                changes: git::changes(&worktree).map_err(failed)?.into_iter().map(host_change).collect(),
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
