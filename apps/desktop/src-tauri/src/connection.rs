use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::{atomic::{AtomicBool, Ordering}, Arc}, time::Duration};
use serde::Serialize;
use skiff_client::Client;
use skiff_core::{socket::socket_path, session::SessionState};
use tauri::{async_runtime::JoinHandle, State};
use tokio::sync::Mutex;
use crate::{input::Input, reload::{self, NoPlan, Plan}, streaming::Flow};

#[derive(Default)]
pub(crate) struct App {
    pub(crate) client: Mutex<Option<Arc<Client>>>,
    /// The same connection, for keystrokes, which must not wait on a lock.
    pub(crate) live: std::sync::Mutex<Option<Arc<Client>>>,
    pub(crate) subs: Mutex<HashMap<String, JoinHandle<()>>>,
    /// The task that streams events to the page.
    pub(crate) events: Mutex<Option<JoinHandle<()>>>,
    /// Terminal input on its way to the daemon.
    pub(crate) input: Input,
    /// Per session: how far the page's xterm is behind its stream.
    pub(crate) flows: std::sync::Mutex<HashMap<String, Arc<Flow>>>,
    /// Set when the daemon differs from this app and was not replaced.
    pub(crate) warning: Mutex<Option<Warning>>,
    /// The version of a daemon this app replaced at launch.
    pub(crate) replaced: Mutex<Option<String>>,
    /// An older daemon that can reload onto the bundled one. The page starts
    /// the reload with `reload_daemon`.
    pub(crate) reload: Mutex<Option<Plan>>,
    /// A reload ran. The app does not start another one on its own.
    pub(crate) reload_tried: AtomicBool,
    /// Why the daemon is gone after a reload. No daemon starts on its own
    /// while it is set: only an explicit restart starts one.
    pub(crate) failed: std::sync::Mutex<Option<String>>,
}

#[derive(Serialize, Clone)]
pub(crate) struct Warning {
    /// `outdated`, `protocol`, `newer`, `hung`, or `failed` for a reload that
    /// did not come back.
    pub(crate) kind: &'static str,
    pub(crate) message: String,
    /// Live sessions a restart would stop. `None` when the daemon did not say.
    pub(crate) sessions: Option<usize>,
}

#[derive(Serialize, Clone)]
pub(crate) struct DaemonStatus {
    connected: bool,
    version: Option<String>,
    /// The daemon's protocol. `None` when the app could not ask.
    protocol: Option<u32>,
    socket: String,
    /// The daemon's pid, from the socket's peer credentials.
    pid: Option<i32>,
    spawned: bool,
    warning: Option<Warning>,
    replaced: Option<String>,
    /// The daemon is older and can reload onto this app's skiffd. The page
    /// calls `reload_daemon` next.
    reload: bool,
}

pub(crate) fn daemon_binary() -> PathBuf {
    if let Ok(p) = std::env::var("SKIFF_DAEMON") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        let beside = exe.with_file_name("skiffd");
        if beside.exists() {
            return beside;
        }
    }
    PathBuf::from("skiffd")
}

fn spawn_daemon() -> anyhow::Result<()> {
    let log = socket_path().with_extension("log");
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Keep the last daemon's log: it saw what the new one restores.
    let _ = std::fs::rename(&log, log.with_extension("log.1"));
    let log = std::fs::File::create(&log)?;
    let mut cmd = std::process::Command::new(daemon_binary());
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    // The daemon passes its environment to every session it starts.
    if crate::SET_WEBKIT_DMABUF.load(std::sync::atomic::Ordering::Relaxed) {
        cmd.env_remove(crate::WEBKIT_DMABUF_VAR);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()?;
    Ok(())
}

/// A daemon that takes longer than this to answer `hello` is hung.
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_secs(2);

/// Reuses a live connection, else connects, else starts the daemon and connects.
/// A live connection costs no round trip: every keystroke comes through here.
pub(crate) async fn ensure_client(app: &App) -> Result<(Arc<Client>, bool), String> {
    let mut guard = app.client.lock().await;
    if let Some(c) = guard.as_ref() {
        if !c.is_closed() {
            return Ok((c.clone(), false));
        }
    }
    *guard = None;
    let path = socket_path();
    let mut spawned = false;
    let mut replaced = false;
    let mut last = String::new();
    for _ in 0..40 {
        let c = match Client::connect(&path).await {
            Ok(c) => c,
            Err(e) => {
                last = format!("{e:#}");
                // A reload failed: starting a new daemon is the user's call.
                if let Some(why) = app.failed.lock().unwrap().clone() {
                    return Err(why);
                }
                if !spawned {
                    spawn_daemon().map_err(|e| format!("start skiffd: {e:#}"))?;
                    spawned = true;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        match check_daemon(app, &c, !replaced).await {
            Check::Use => {
                *app.failed.lock().unwrap() = None;
                let c = Arc::new(c);
                *guard = Some(c.clone());
                *app.live.lock().unwrap() = Some(c.clone());
                return Ok((c, spawned));
            }
            Check::Replaced => {
                replaced = true;
                spawned = false;
            }
            Check::Hung => return Err("skiffd is not responding".into()),
        }
    }
    Err(format!("cannot reach skiffd at {}: {last}", path.display()))
}

pub(crate) enum Check {
    Use,
    /// The old daemon was stopped: connect again, to a fresh one.
    Replaced,
    Hung,
}

/// Compares the daemon with this app. An older daemon that can reload is
/// planned for a reload onto the bundled skiffd, which keeps its sessions.
/// Else an older or incompatible daemon with no live session is replaced
/// without a word. With live sessions, the restart is the user's call: this
/// records a warning and keeps the daemon.
pub(crate) async fn check_daemon(app: &App, c: &Client, may_replace: bool) -> Check {
    let Ok(Ok(info)) = tokio::time::timeout(HELLO_TIMEOUT, c.daemon_info()).await else {
        *app.warning.lock().await = Some(Warning {
            kind: "hung",
            message: format!(
                "skiffd accepts connections but did not answer within {}s. \
                 This app cannot reach its sessions. Restarting stops it and starts a new one.",
                HELLO_TIMEOUT.as_secs()
            ),
            sessions: None,
        });
        return Check::Hung;
    };
    let version = info.version.clone();
    // The skiffd this app ships with, not the app version.
    let order = compare_versions(&version, skiff_core::VERSION);
    let same_protocol = info.protocol == skiff_core::PROTOCOL;
    if same_protocol && order.is_eq() {
        *app.warning.lock().await = None;
        return Check::Use;
    }
    // Never a downgrade, and one try per app run.
    let mut old_build = None;
    if order.is_lt() && !app.reload_tried.load(Ordering::Relaxed) {
        match reload::plan(&info).await {
            Ok(plan) => {
                *app.reload.lock().await = Some(plan);
                *app.warning.lock().await = None;
                return Check::Use;
            }
            Err(NoPlan::Stale(bundled)) => old_build = Some(bundled),
            Err(NoPlan::Other(why)) => eprintln!("skiffd {version} does not reload: {why}"),
        }
    }
    let live = tokio::time::timeout(HELLO_TIMEOUT, c.list_sessions())
        .await
        .ok()
        .and_then(|r| r.ok())
        .map(|v| v.iter().filter(|s| s.state != SessionState::Done).count());
    // The skiffd this app starts is no newer than the one that runs. A
    // restart starts the same build again and asks again: no prompt, and no
    // silent replace. Only a dev build gets here.
    if let Some(bundled) = old_build {
        let why = format!(
            "the skiffd this app starts ({}) is {bundled}, older than the app's {}. Rebuild it: cargo build -p skiffd",
            daemon_binary().display(),
            skiff_core::VERSION
        );
        eprintln!("skiffd {version} does not reload: {why}");
        *app.warning.lock().await = match same_protocol {
            true => None,
            false => Some(restart_warning(&version, info.protocol, live, Some(&why))),
        };
        return Check::Use;
    }
    let stale = !same_protocol || order.is_lt();
    if let (true, true, Some(0), Some(pid)) = (may_replace, stale, live, c.daemon_pid) {
        if stop_daemon(pid).await {
            *app.replaced.lock().await = Some(version);
            return Check::Replaced;
        }
    }
    *app.warning.lock().await = Some(restart_warning(&version, info.protocol, live, None));
    Check::Use
}

/// The warning for a daemon that differs from this app and keeps running.
/// `why`: the reload that failed, and the daemon carries on as before.
pub(crate) fn restart_warning(version: &str, protocol: u32, live: Option<usize>, why: Option<&str>) -> Warning {
    let ours = skiff_core::VERSION;
    let n = live.unwrap_or(0);
    let running = match n {
        1 => "1 session is running".to_string(),
        n => format!("{n} sessions are running"),
    };
    let ends = "Restarting stops them. Panes come back as shells in their folders.";
    let failed = why.map(|w| format!("It did not reload: {w}. ")).unwrap_or_default();
    let order = compare_versions(version, ours);
    if protocol != skiff_core::PROTOCOL {
        Warning {
            kind: "protocol",
            message: format!(
                "skiffd {version} speaks another protocol than this app ({ours}). {failed}\
                 Parts of the app fail until it restarts. {running}. {ends}"
            ),
            sessions: live,
        }
    } else if order.is_lt() {
        Warning {
            kind: "outdated",
            message: format!(
                "skiffd {version} still runs. {failed}Restart it to finish the update to {ours}. \
                 {running}. {ends}"
            ),
            sessions: live,
        }
    } else {
        Warning {
            kind: "newer",
            message: format!(
                "skiffd {version} is newer than this app ({ours}). \
                 They speak the same protocol, so the app keeps using it."
            ),
            sessions: live,
        }
    }
}

/// Orders dotted versions by number. A part that is not a number counts as 0.
pub(crate) fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |v: &str| -> Vec<u64> {
        v.split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect()
    };
    parts(a).cmp(&parts(b))
}

/// SIGTERM, up to 3 s to exit, then SIGKILL and up to 2 s more. A hung
/// daemon may ignore SIGTERM. True once the process is gone: a new daemon
/// refuses to start while the old one still holds the socket.
async fn stop_daemon(pid: i32) -> bool {
    // SAFETY: kill and waitpid take plain integers and touch no memory of ours.
    let gone = || unsafe {
        // The app may have started it: reap the zombie, which kill(0) still sees.
        libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) == pid
            || (libc::kill(pid, 0) != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH))
    };
    for (signal, ticks) in [(libc::SIGTERM, 30), (libc::SIGKILL, 20)] {
        if unsafe { libc::kill(pid, signal) } != 0 {
            return gone();
        }
        for _ in 0..ticks {
            if gone() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    false
}

/// Stops the running daemon, which ends its sessions, and starts a new one.
#[tauri::command]
pub(crate) async fn restart_daemon(app: State<'_, App>) -> Result<DaemonStatus, String> {
    {
        let mut guard = app.client.lock().await;
        let pid = match guard.as_ref() {
            Some(c) => c.daemon_pid,
            None => Client::connect(&socket_path()).await.ok().and_then(|c| c.daemon_pid),
        };
        *guard = None;
        *app.live.lock().unwrap() = None;
        if let Some(pid) = pid {
            if !stop_daemon(pid).await {
                return Err(format!("skiffd (pid {pid}) did not stop"));
            }
        }
        *app.warning.lock().await = None;
        *app.failed.lock().unwrap() = None;
        *app.reload.lock().await = None;
    }
    for (_, task) in app.subs.lock().await.drain() {
        task.abort();
    }
    daemon_status(app).await
}

pub(crate) fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[tauri::command]
pub(crate) async fn daemon_status(app: State<'_, App>) -> Result<DaemonStatus, String> {
    Ok(status_now(&app).await)
}

pub(crate) async fn status_now(app: &App) -> DaemonStatus {
    let socket = socket_path().display().to_string();
    match ensure_client(app).await {
        Ok((c, spawned)) => {
            let hello = tokio::time::timeout(HELLO_TIMEOUT, c.hello()).await.ok().and_then(|r| r.ok());
            DaemonStatus {
                connected: true,
                version: hello.as_ref().map(|(v, _)| v.clone()),
                protocol: hello.map(|(_, p)| p),
                socket,
                pid: c.daemon_pid,
                spawned,
                warning: app.warning.lock().await.clone(),
                replaced: app.replaced.lock().await.clone(),
                reload: app.reload.lock().await.is_some(),
            }
        }
        Err(_) => DaemonStatus {
            connected: false,
            version: None,
            protocol: None,
            socket,
            pid: None,
            spawned: false,
            warning: app.warning.lock().await.clone(),
            replaced: None,
            reload: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{compare_versions, stop_daemon};
    use std::cmp::Ordering::*;

    #[test]
    fn orders_versions_by_number() {
        assert_eq!(compare_versions("0.3.2", "0.5.0"), Less);
        assert_eq!(compare_versions("0.10.0", "0.9.9"), Greater);
        assert_eq!(compare_versions("0.5.0", "0.5.0"), Equal);
    }

    #[test]
    fn kills_a_process_that_ignores_sigterm() {
        let child = std::process::Command::new("sh")
            .args(["-c", "trap '' TERM; sleep 30"])
            .spawn()
            .unwrap();
        // Let the shell set its trap first.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        assert!(rt.block_on(stop_daemon(child.id() as i32)));
    }
}
