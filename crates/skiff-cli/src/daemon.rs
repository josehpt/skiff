//! daemon reload and stop.

use std::{collections::HashMap, path::{Path, PathBuf}, time::{Duration, Instant}};

use anyhow::{bail, Context, Result};
use skiff_client::Client;
use skiff_core::session::SessionState;

/// How long the new image may take to adopt the sessions and answer.
const RECONNECT: Duration = Duration::from_secs(15);
/// How long a stopping daemon may take to save and let go of its socket.
const STOP: Duration = Duration::from_secs(10);

/// Reloads the daemon onto `binary`, then proves the sessions survived:
/// the same daemon pid, and each live session with the same id and pid.
pub async fn reload(socket: &Path, binary: Option<PathBuf>) -> Result<()> {
    let binary = match binary {
        Some(b) => Some(std::path::absolute(&b).with_context(|| format!("resolve {}", b.display()))?),
        None => None,
    };
    let c = crate::connect(socket).await?;
    let before = c.daemon_info().await?;
    let (Some(_), Some(pid)) = (before.reload_state, before.pid) else {
        bail!("skiffd {} cannot reload; restarting it ends every session", before.version);
    };
    let live: HashMap<String, Option<u32>> = c
        .list_sessions()
        .await?
        .into_iter()
        .filter(|s| s.state != SessionState::Done)
        .map(|s| (s.id, s.pid))
        .collect();
    c.reload(binary).await?;
    drop(c);

    let start = Instant::now();
    let (c, after) = loop {
        if let Ok(c) = Client::connect(socket).await {
            if let Ok(info) = c.daemon_info().await {
                break (c, info);
            }
        }
        if start.elapsed() > RECONNECT {
            bail!("skiffd did not answer after the reload; check its log");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    if after.pid != Some(pid) {
        bail!(
            "skiffd restarted instead of reloading (pid {pid}, now {}); its sessions did not survive",
            after.pid.map_or("unknown".into(), |p| p.to_string())
        );
    }
    let now: HashMap<String, Option<u32>> = c.list_sessions().await?.into_iter().map(|s| (s.id, s.pid)).collect();
    let lost: Vec<&String> = live.iter().filter(|(id, p)| now.get(*id) != Some(p)).map(|(id, _)| id).collect();
    println!("skiffd {} -> {}, pid {pid}, {} of {} live sessions kept", before.version, after.version, live.len() - lost.len(), live.len());
    if !lost.is_empty() {
        let ids: Vec<&str> = lost.iter().map(|s| s.as_str()).collect();
        bail!("lost in the reload: {}", ids.join(", "));
    }
    Ok(())
}

/// Asks the daemon to stop, then waits until its socket no longer answers.
pub async fn stop(socket: &Path) -> Result<()> {
    let c = crate::connect(socket).await?;
    let info = c.daemon_info().await?;
    c.shutdown().await.with_context(|| format!("skiffd {} did not take the request", info.version))?;
    drop(c);
    let start = Instant::now();
    while Client::connect(socket).await.is_ok() {
        if start.elapsed() > STOP {
            bail!("skiffd still answers at {}", socket.display());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let pid = info.pid.map_or(String::new(), |p| format!(", pid {p}"));
    println!("skiffd {} stopped{pid}", info.version);
    Ok(())
}
