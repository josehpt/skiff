//! `skiff-host [--session-socket <skiffd socket>]`
//!
//! Serves the Host's data plane at `<skiffd socket stem>-host.sock`, beside
//! the skiffd it pairs with, and uses that socket's state: a private test or
//! debug socket gets its own `projects.toml` and Host id, exactly as skiffd
//! does. It starts nothing and stops nothing else; SIGTERM or SIGINT end
//! only this process.
use std::{path::PathBuf, sync::Arc};

use anyhow::{bail, Context, Result};
use tokio::{
    net::{UnixListener, UnixStream},
    signal::unix::{signal, SignalKind},
};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut session = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--session-socket" => session = Some(PathBuf::from(args.next().context("--session-socket needs a path")?)),
            "--version" => {
                println!("skiff-host {}", skiff_core::VERSION);
                return Ok(());
            }
            other => bail!("unknown argument {other}; usage: skiff-host [--session-socket <skiffd socket>]"),
        }
    }
    let session = session.unwrap_or_else(skiff_core::socket::socket_path);
    // Before anything reads config or identity: the state follows skiffd's socket.
    skiff_core::state::set_socket(session.clone());
    let hello = Arc::new(skiff_host::hello().context("read the Host identity")?);
    let path = skiff_core::socket::data_socket(&session);
    tokio::runtime::Runtime::new()?.block_on(async move {
        let listener = bind(&path).await?;
        eprintln!(
            "skiff-host {} listening on {} for Host {} (instance {})",
            skiff_core::VERSION,
            path.display(),
            hello.host.host_id.as_str(),
            hello.instance_id.as_str()
        );
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        let result = tokio::select! {
            r = skiff_host::serve(listener, hello) => r.context("accept"),
            _ = term.recv() => Ok(()),
            _ = int.recv() => Ok(()),
        };
        let _ = std::fs::remove_file(&path);
        result
    })
}

/// As skiffd binds: only a directory it creates is locked down, a live
/// socket means another service, and a dead one is a crash's leftover.
async fn bind(path: &std::path::Path) -> Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(dir) = path.parent().filter(|d| !d.exists()) {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            bail!("skiff-host is already running at {}", path.display());
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}
