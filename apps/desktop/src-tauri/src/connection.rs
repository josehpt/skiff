use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::{atomic::{AtomicBool, Ordering}, Arc}, time::Duration};
use serde::Serialize;
use skiff_client::{Client, host::local::{LocalSelection, SelectedLocalHost, connection_status}};
use skiff_core::{socket::socket_path, session::SessionState};
use tauri::{async_runtime::JoinHandle, State};
use tokio::sync::Mutex;
use crate::{input::Input, reload::{self, NoPlan, Plan}, streaming::Flow};

#[derive(Default)]
pub(crate) struct App {
    pub(crate) host: Mutex<Option<Arc<SelectedLocalHost>>>,
    /// One local selection for this runtime; no ambient endpoint re-selection
    /// on reconnect. Legacy paths only borrow its accepted Client.
    local: std::sync::OnceLock<LocalSelection>,
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
    /// Watcher pids skiffd registered, from its peer credentials, kept from
    /// before a reload, which drops the registrations. They identify a
    /// watcher from before the lock record.
    pub(crate) watch_pids: std::sync::Mutex<Vec<u32>>,
    /// The app tried to replace the watcher this run. Set before the
    /// signal, so a watcher that did not stop is not signalled again.
    pub(crate) watch_tried: Arc<AtomicBool>,
    /// Held across each watcher check, so a start and a restore at once do
    /// not both act on the same lock.
    pub(crate) watch_gate: Mutex<()>,
}

impl App {
    pub(crate) fn local(&self) -> &LocalSelection {
        self.local.get_or_init(|| LocalSelection::new(
            socket_path(),
            skiff_core::config::config_path().with_file_name("local-host-id"),
        ))
    }
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
    let (host, spawned) = ensure_host(app).await?;
    Ok((host.client(), spawned))
}

/// The one M1b read path: same owner and legacy result, through HostConnection.
pub(crate) async fn screen_now(app: &App, session: &str, rows: u16) -> Result<Vec<String>, String> {
    let (host, _) = ensure_host(app).await?;
    skiff_client::host::local::legacy_screen(&host.connection(), session, rows).await.map_err(err)
}

/// The one M1c read path; lifecycle compatibility checks still use Client.
pub(crate) async fn inventory_now(app: &App) -> Result<Vec<skiff_core::session::SessionInfo>, String> {
    let (host, _) = ensure_host(app).await?;
    skiff_client::host::local::legacy_inventory(&host.connection()).await.map_err(err)
}

/// The one M1d event path: page events from the same selected owner. The
/// receiver starts now, before the caller's next inventory read, as before.
pub(crate) async fn updates_now(app: &App) -> Result<skiff_client::host::local::LegacyUpdates, String> {
    let (host, _) = ensure_host(app).await?;
    skiff_client::host::local::legacy_updates(&host.connection()).map_err(err)
}

async fn ensure_host(app: &App) -> Result<(Arc<SelectedLocalHost>, bool), String> {
    let mut guard = app.host.lock().await;
    if let Some(host) = guard.as_ref() {
        if !host.client().is_closed() {
            return Ok((host.clone(), false));
        }
    }
    *guard = None;
    let path = app.local().socket();
    let mut spawned = false;
    let mut replaced = false;
    let mut last = String::new();
    for _ in 0..40 {
        let c = match app.local().connect().await {
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
        match check_daemon(app, c.client(), !replaced).await {
            Check::Use => {
                *app.failed.lock().unwrap() = None;
                let host = Arc::new(c.activate().await.map_err(err)?);
                *app.live.lock().unwrap() = Some(host.client());
                *guard = Some(host.clone());
                return Ok((host, spawned));
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
/// So is a checkout's daemon of the same version under a release app: the
/// same version string is not the same build. Else an older daemon, or an
/// incompatible one that is not newer, with no live session is stopped with
/// SIGTERM and replaced without a word. A newer daemon is never replaced.
/// One that does not stop, or has live sessions, keeps running: this records
/// a warning that says the update is not finished.
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
    let other_build = order.is_eq() && !cfg!(debug_assertions) && info.debug == Some(true);
    if same_protocol && order.is_eq() && !other_build {
        *app.warning.lock().await = None;
        return Check::Use;
    }
    // Never a downgrade, and one try per app run.
    let mut old_build = None;
    if (order.is_lt() || other_build) && !app.reload_tried.load(Ordering::Relaxed) {
        match reload::plan(&info, other_build).await {
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
    // Never a downgrade: a newer daemon keeps running, whatever its protocol.
    let stale = !order.is_gt() && (!same_protocol || order.is_lt() || other_build);
    // Never SIGKILL here: a daemon that does not stop keeps running as it is.
    if let (true, true, Some(0), Some(pid)) = (may_replace, stale, live, c.daemon_pid) {
        if stop_daemon(pid, false).await {
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
    let ends = "They keep running. A restart stops them, and panes come back as shells in their folders.";
    let failed = why.map(|w| format!("It did not reload: {w}. ")).unwrap_or_default();
    let order = compare_versions(version, ours);
    if protocol != skiff_core::PROTOCOL {
        Warning {
            kind: "protocol",
            message: format!(
                "skiffd {version} speaks another protocol than this app ({ours}). {failed}\
                 Parts of the app fail while it runs. {running}. {ends}"
            ),
            sessions: live,
        }
    } else if !order.is_gt() {
        Warning {
            kind: "outdated",
            message: format!(
                "skiffd {version} still runs, so the update to {ours} is not finished. {failed}\
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

pub(crate) use skiff_core::compare_versions;

/// SIGTERM, up to 3 s to exit. With `escalate`, which only a restart the
/// user asked for passes, then SIGKILL and up to 2 s more: a hung daemon may
/// ignore SIGTERM. True once the process is gone: a new daemon refuses to
/// start while the old one still holds the socket.
async fn stop_daemon(pid: i32, escalate: bool) -> bool {
    // SAFETY: kill and waitpid take plain integers and touch no memory of ours.
    let gone = || unsafe {
        // The app may have started it: reap the zombie, which kill(0) still sees.
        libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) == pid
            || (libc::kill(pid, 0) != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH))
    };
    let steps: &[(i32, u32)] = if escalate { &[(libc::SIGTERM, 30), (libc::SIGKILL, 20)] } else { &[(libc::SIGTERM, 30)] };
    for &(signal, ticks) in steps {
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
        let mut guard = app.host.lock().await;
        let pid = match guard.as_ref() {
            Some(host) => host.client().daemon_pid,
            None => app.local().connect().await.ok().and_then(|c| c.client().daemon_pid),
        };
        *guard = None;
        *app.live.lock().unwrap() = None;
        if let Some(pid) = pid {
            if !stop_daemon(pid, true).await {
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
    let socket = app.local().socket().display().to_string();
    match ensure_host(app).await {
        Ok((host, spawned)) => {
            let c = host.client();
            let hello = tokio::time::timeout(HELLO_TIMEOUT, connection_status(&host.connection())).await.ok().and_then(|r| r.ok());
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

    #[tokio::test]
    async fn status_and_legacy_commands_share_the_selected_local_connection() {
        use super::*;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let dir = std::env::temp_dir().join(format!("skiff-m1a-desktop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let socket = dir.join("selected.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        // Existing data remains byte-for-byte unchanged by the new bootstrap.
        let config = dir.join("projects.toml");
        let ledger = dir.join("ledger.db");
        std::fs::write(&config, "# existing config\n").unwrap();
        std::fs::write(&ledger, b"existing ledger fixture").unwrap();
        let app = App::default();
        assert!(app.local.set(LocalSelection::new(socket.clone(), dir.join("local-host-id"))).is_ok());
        let inventory_fixture: serde_json::Value = serde_json::from_str(include_str!("../../../../crates/skiff-host-api/tests/fixtures/m1c.json")).unwrap();
        let inventory = inventory_fixture["reply"]["sessions"].clone();
        let expected_inventory: Vec<skiff_core::session::SessionInfo> = serde_json::from_value(inventory.clone()).unwrap();
        let updates_fixture: serde_json::Value = serde_json::from_str(include_str!("../../../../crates/skiff-host-api/tests/fixtures/m1d.json")).unwrap();
        let page_events = updates_fixture["legacy"].as_array().unwrap().clone();
        let wire_events = page_events.clone();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            for id in 1..=3 {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), serde_json::json!({"id":id,"cmd":"ping"}));
                let reply = serde_json::json!({"id":id,"type":"pong","version":skiff_core::VERSION,"protocol":skiff_core::PROTOCOL});
                let mut bytes = serde_json::to_vec(&reply).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            for (id, response) in [
                (4, serde_json::json!({"type":"screen","lines":["", "λ  "]})),
                (5, serde_json::json!({"type":"error","message":"unknown variant `screen`"})),
            ] {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), serde_json::json!({"id":id,"cmd":"screen","session":"snapshot","rows":2}));
                let mut response = response; response["id"] = id.into();
                let mut bytes = serde_json::to_vec(&response).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            for (id, response) in [
                (6, serde_json::json!({"type":"sessions","sessions":inventory})),
                (7, serde_json::json!({"type":"error","message":"inventory unavailable"})),
            ] {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), serde_json::json!({"id":id,"cmd":"list_sessions"}));
                let mut response = response; response["id"] = id.into();
                let mut bytes = serde_json::to_vec(&response).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            // Pushed events, with no subscribe request from the page stream.
            for event in wire_events {
                let mut bytes = serde_json::to_vec(&event).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            // Keep the one connection alive through the reuse assertions.
            let mut line = String::new();
            stream.read_line(&mut line).await.ok();
        });
        let first = status_now(&app).await;
        assert!(first.connected && !first.spawned);
        assert_eq!(first.version.as_deref(), Some(skiff_core::VERSION));
        assert_eq!(first.protocol, Some(skiff_core::PROTOCOL));
        assert_eq!(first.socket, socket.display().to_string());
        assert!(first.warning.is_none() && !first.reload);
        let owner = app.host.lock().await.as_ref().unwrap().clone();
        let mut updates = updates_now(&app).await.unwrap();
        let (client, spawned) = ensure_client(&app).await.unwrap();
        assert!(!spawned && Arc::ptr_eq(&owner.client(), &client));
        assert!(Arc::ptr_eq(&app.live.lock().unwrap().clone().unwrap(), &client));
        let second = status_now(&app).await;
        assert_eq!(second.version, first.version);
        assert_eq!(screen_now(&app, "snapshot", 2).await.unwrap(), vec!["", "λ  "]);
        assert_eq!(screen_now(&app, "snapshot", 2).await.unwrap_err(), "unknown variant `screen`");
        let listed = inventory_now(&app).await.unwrap();
        assert_eq!(listed, expected_inventory);
        assert_eq!(serde_json::to_value(&listed).unwrap(), serde_json::to_value(&expected_inventory).unwrap());
        assert_eq!(inventory_now(&app).await.unwrap_err(), "inventory unavailable");
        assert!(Arc::ptr_eq(&owner, app.host.lock().await.as_ref().unwrap()));
        for want in &page_events {
            assert_eq!(serde_json::to_value(updates.recv().await.unwrap()).unwrap(), *want);
        }
        // Dropping the selected owner, as reconnect/restart/reload do, ends it.
        drop(owner);
        drop(client);
        *app.host.lock().await = None;
        assert!(updates.recv().await.is_none());
        assert_eq!(std::fs::read_to_string(config).unwrap(), "# existing config\n");
        assert_eq!(std::fs::read(ledger).unwrap(), b"existing ledger fixture");
        peer.abort();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn orders_versions_by_number() {
        assert_eq!(compare_versions("0.3.2", "0.5.0"), Less);
        assert_eq!(compare_versions("0.10.0", "0.9.9"), Greater);
        assert_eq!(compare_versions("0.5.0", "0.5.0"), Equal);
    }

    #[test]
    #[allow(clippy::zombie_processes, reason = "stop_daemon reaps the pid with waitpid")]
    fn kills_a_process_that_ignores_sigterm() {
        let child = std::process::Command::new("sh")
            // exec keeps the ignored TERM disposition and leaves no child
            // sleep behind when the SIGKILL fallback stops this process.
            .args(["-c", "trap '' TERM; exec sleep 30"])
            .spawn()
            .unwrap();
        // Let the shell set its trap first.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        assert!(rt.block_on(stop_daemon(child.id() as i32, true)));
    }

    /// The automatic stop sends SIGTERM only. A process that ignores it
    /// keeps running, and the stop says so.
    #[test]
    fn the_automatic_stop_never_kills() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "trap '' TERM; exec sleep 30"])
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let stopped = rt.block_on(stop_daemon(child.id() as i32, false));
        let running = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(!stopped);
        assert!(running, "the process was stopped");
    }

    /// The warning for an update that did not finish states the outcome. It
    /// names no command to run.
    #[test]
    fn an_unfinished_update_reads_as_an_outcome() {
        let w = super::restart_warning("0.0.1", skiff_core::PROTOCOL, Some(2), Some("it predates reload"));
        assert_eq!(w.kind, "outdated");
        assert!(w.message.contains("not finished") && w.message.contains("keep running"), "{}", w.message);
        for word in ["Restart it", "skiff ", "cargo", "SKIFF_SOCKET", "Run "] {
            assert!(!w.message.contains(word), "{word}: {}", w.message);
        }
    }

    /// A stand-in daemon on a private socket: it answers `ping` as
    /// `version` on `protocol` and lists no session. The connection names
    /// `pid` as the daemon, a process the test owns, so a stop shows on it.
    async fn fake_daemon(dir: &std::path::Path, version: &'static str, protocol: u32, pid: u32) -> skiff_client::Client {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let socket = dir.join(format!("{version}.sock"));
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(rd).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                let id = req["id"].clone();
                let reply = match req["cmd"].as_str() {
                    Some("ping") => serde_json::json!({"id": id, "type": "pong", "version": version, "protocol": protocol}),
                    Some("list_sessions") => serde_json::json!({"id": id, "type": "sessions", "sessions": []}),
                    other => serde_json::json!({"id": id, "type": "error", "message": format!("{other:?}")}),
                };
                let mut out = serde_json::to_vec(&reply).unwrap();
                out.push(b'\n');
                wr.write_all(&out).await.unwrap();
            }
        });
        let mut c = skiff_client::Client::connect(&socket).await.unwrap();
        c.daemon_pid = Some(pid as i32);
        c
    }

    /// Never a downgrade: a newer daemon on another protocol with no live
    /// session keeps running, and the app does not replace it. An older one
    /// in the same place is stopped and replaced, which shows the stop works.
    #[test]
    fn a_newer_daemon_is_never_replaced() {
        let dir = std::env::temp_dir().join(format!("skiff-app-newer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let other = skiff_core::PROTOCOL + 1;
        for (version, replaced) in [("99.0.0", false), ("0.0.1", true)] {
            let mut stand_in = std::process::Command::new("sleep").arg("30").spawn().unwrap();
            let app = super::App::default();
            let check = rt.block_on(async {
                let c = fake_daemon(&dir, version, other, stand_in.id()).await;
                super::check_daemon(&app, &c, true).await
            });
            // The stop reaps what it ends, so a gone child reads as ECHILD.
            let running = stand_in.try_wait().is_ok_and(|s| s.is_none());
            let _ = stand_in.kill();
            let _ = stand_in.wait();
            assert_eq!(matches!(check, super::Check::Replaced), replaced, "skiffd {version}");
            assert_eq!(running, !replaced, "skiffd {version}: signalled");
            assert_eq!(rt.block_on(app.replaced.lock()).is_some(), replaced);
            if !replaced {
                assert_eq!(rt.block_on(app.warning.lock()).as_ref().map(|w| w.kind), Some("protocol"));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
