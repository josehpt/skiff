//! `skiff skipper`: attach to the Skiff agent, or start it first. From any
//! terminal, an SSH login included.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use skiff_client::{skipper, Client};

use crate::attach;

/// How long a daemon this command started may take to answer.
const START: Duration = Duration::from_secs(10);

pub async fn run(socket: &Path, no_attach: bool) -> Result<()> {
    let c = connect_or_start(socket).await?;
    let info = c.daemon_info().await?;
    if info.protocol < skiff_core::PROTOCOL {
        bail!("skiffd {} is too old for this skiff. Run `skiff daemon reload`", info.version);
    }
    let Some(found) = skipper::open(&c, skipper::Open::Start).await? else {
        bail!("skiffd did not start the Skiff agent");
    };
    let s = found.session;
    let name = s.name.clone().unwrap_or_else(|| s.label.clone());
    let exe = std::env::current_exe().context("find this skiff binary")?;
    if let Err(e) = skipper::start_watch(socket, &exe, &s.id) {
        eprintln!("skiff: {name} runs, but its watcher did not start: {e:#}");
    }
    if no_attach {
        println!("{}", s.id);
        return Ok(());
    }
    if found.created {
        eprintln!("skiff: started {name}");
    } else if found.typed {
        eprintln!("skiff: started the agent in {name}'s shell");
    }
    attach::attach(&c, &s.id).await
}

/// Connects. With no daemon at the socket, starts one, as the app does.
async fn connect_or_start(socket: &Path) -> Result<Client> {
    if let Ok(c) = Client::connect(socket).await {
        return Ok(c);
    }
    // One start at a time: a second `skiff skipper` waits here, then finds
    // the daemon the first one started.
    let _lock = start_lock(socket).await?;
    if let Ok(c) = Client::connect(socket).await {
        return Ok(c);
    }
    start_daemon(socket)?;
    let since = Instant::now();
    loop {
        if let Ok(c) = Client::connect(socket).await {
            if c.daemon_info().await.is_ok() {
                return Ok(c);
            }
        }
        if since.elapsed() > START {
            bail!("skiffd did not start at {}; see {}", socket.display(), socket.with_extension("log").display());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The lock beside the socket that a daemon start holds. It frees itself
/// when this process ends.
async fn start_lock(socket: &Path) -> Result<std::fs::File> {
    let path = socket.with_extension("start.lock");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let f = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let since = Instant::now();
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(f),
            Err(std::fs::TryLockError::WouldBlock) if since.elapsed() < START => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(std::fs::TryLockError::WouldBlock) => bail!("another skiff is starting skiffd at {}", socket.display()),
            Err(std::fs::TryLockError::Error(e)) => return Err(e).with_context(|| format!("lock {}", path.display())),
        }
    }
}

/// `SKIFF_DAEMON`, else the `skiffd` beside this binary, else the one on `PATH`.
fn daemon_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("SKIFF_DAEMON") {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .map(|e| e.with_file_name("skiffd"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("skiffd"))
}

/// Starts skiffd on `socket` in a session of its own, so it outlives this
/// terminal and an SSH logout. Logs beside the socket, as the app does.
fn start_daemon(socket: &Path) -> Result<()> {
    let log = socket.with_extension("log");
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    // Keep the last daemon's log: it saw what the new one restores.
    let _ = std::fs::rename(&log, log.with_extension("log.1"));
    let file = std::fs::File::create(&log).with_context(|| format!("create {}", log.display()))?;
    let bin = daemon_binary();
    let mut cmd = std::process::Command::new(&bin);
    cmd.env("SKIFF_SOCKET", socket).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(file));
    skipper::detach(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("start {}", bin.display()))?;
    // It forks nothing: reap it if it ends while this command runs.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
