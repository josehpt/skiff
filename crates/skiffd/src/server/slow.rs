use std::{path::Path, sync::Arc};
use anyhow::{anyhow, bail, Result};
use skiff_core::{project, protocol::{Request, Response}};
use crate::session::SessionPool;
use super::error;

/// Requests that wait on the disk or a new process. They run in order on
/// their own task, so input and resizes behind them are not held up.
///
/// Config and git edits belong to the app. Only requests that need the
/// sessions' environment, the login shell's PATH and aliases, are here.
pub(super) async fn answer_slow(pool: Arc<SessionPool>, request: Request) -> Response {
    match request {
        Request::CreateSession { spec } => match blocking(move || pool.create(spec)).await {
            Ok(session) => Response::Session { session },
            Err(e) => error(e),
        },
        // Both start processes, which a reload must not have running.
        Request::ListAgents => match blocking(move || {
            let _change = pool.change()?;
            Ok(project::agents())
        })
        .await
        {
            Ok(agents) => Response::Agents { agents },
            Err(e) => error(e),
        },
        Request::ListAgentSessions { command, cwd } => {
            match blocking(move || {
                let _change = pool.change()?;
                agent_sessions(&command, &cwd)
            })
            .await
            {
                Ok(stdout) => Response::Output { stdout },
                Err(e) => error(e),
            }
        }
        Request::Coordinator { spec, start, resume, expect } => {
            let ask = crate::session::Ask { spec, start, resume, expect };
            match blocking(move || pool.coordinator(ask)).await {
                Ok(Some(c)) => Response::Coordinator { session: c.session, created: c.created, typed: c.typed },
                Ok(None) => Response::Sessions { sessions: Vec::new() },
                Err(e) => error(e),
            }
        }
        _ => unreachable!("not a slow request"),
    }
}

/// Runs a list command with the login shell's PATH and aliases. A slow agent
/// CLI is stopped after `LIST_TIMEOUT`, so the menu never hangs on it.
fn agent_sessions(command: &[String], cwd: &Path) -> Result<String> {
    const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let (first, rest) = command.split_first().ok_or_else(|| anyhow!("empty command"))?;
    let mut words: Vec<String> = skiff_core::alias::expand(first).map(<[String]>::to_vec).unwrap_or_else(|| vec![first.clone()]);
    words.extend(rest.iter().cloned());
    let mut child = std::process::Command::new(&words[0])
        .args(&words[1..])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("{}: {e}", words[0]))?;
    let (status, out) = match crate::command::output(&mut child, LIST_TIMEOUT) {
        Ok(output) => output,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            if e.kind() == std::io::ErrorKind::TimedOut {
                bail!("{} took longer than {}s", words[0], LIST_TIMEOUT.as_secs());
            }
            return Err(e.into());
        }
    };
    if !status.success() {
        bail!("{} exited with {status}", words[0]);
    }
    Ok(String::from_utf8(out).unwrap_or_default())
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(f).await?
}
