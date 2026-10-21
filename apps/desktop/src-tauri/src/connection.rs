use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::{atomic::{AtomicBool, Ordering}, Arc}, time::Duration};
use serde::Serialize;
use skiff_client::{Client, host::{HostConnection, local::{HostSelection, ProcessControl, SelectedHost, connection_status}}};
use skiff_core::{socket::socket_path, session::SessionState};
use tauri::{async_runtime::JoinHandle, State};
use tokio::sync::Mutex;
use crate::{input::Input, reload::{self, NoPlan, Plan}, streaming::Flow};

#[derive(Default)]
pub(crate) struct App {
    pub(crate) host: Mutex<Option<Arc<SelectedHost>>>,
    /// The one endpoint this window reaches: this Mac, or the remote Host
    /// `hosts.toml` selects. Only `select_host` changes it, never a reconnect.
    /// Legacy paths only borrow its accepted Client.
    selection: std::sync::Mutex<Option<HostSelection>>,
    /// The same selected Host connection, for keystrokes, which must not
    /// wait on a lock.
    pub(crate) live: std::sync::Mutex<Option<Arc<HostConnection>>>,
    /// The shown remote Host reported another identity than its pin: the
    /// destination and the id it reported, for `accept_host_identity`.
    pub(crate) changed: std::sync::Mutex<Option<(String, skiff_host_api::HostId)>>,
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
    /// The selected endpoint, from `hosts.toml` on first use. A file that
    /// cannot be read selects this Mac.
    pub(crate) fn selection(&self) -> HostSelection {
        let mut selection = self.selection.lock().unwrap();
        let selection = selection.get_or_insert_with(|| {
            crate::hosts::load(&crate::hosts::path())
                .map(|hosts| hosts.selection(local_selection))
                .unwrap_or_else(|e| {
                    eprintln!("hosts.toml: {e:#}; showing this Mac");
                    local_selection()
                })
        });
        selection.clone()
    }

    /// The window shows a remote Host. Commands that read this Mac's data
    /// are refused then (see `lib.rs`).
    pub(crate) fn remote(&self) -> bool {
        !self.selection().is_local()
    }

    /// The Host id this window's live connection accepted, if it has one.
    pub(crate) fn live_identity(&self) -> Option<skiff_host_api::HostId> {
        self.live.lock().unwrap().as_ref().map(|c| c.hello().host.host_id.clone())
    }

    #[cfg(test)]
    pub(crate) fn set_selection(&self, selection: HostSelection) {
        *self.selection.lock().unwrap() = Some(selection);
    }
}

/// This Mac: the local skiffd socket, and the bootstrap id for an older one.
pub(crate) fn local_selection() -> HostSelection {
    HostSelection::new(socket_path(), skiff_core::config::config_path().with_file_name("local-host-id"))
}

/// Shows another endpoint. The old connection, its streams and any held
/// input go first: nothing typed for one Host reaches another.
pub(crate) async fn replace_selection(app: &App, selection: HostSelection) {
    let mut guard = app.host.lock().await;
    *guard = None;
    *app.live.lock().unwrap() = None;
    app.input.discard();
    for (_, task) in app.subs.lock().await.drain() {
        task.abort();
    }
    app.flows.lock().unwrap().clear();
    if let Some(task) = app.events.lock().await.take() {
        task.abort();
    }
    *app.warning.lock().await = None;
    *app.replaced.lock().await = None;
    *app.reload.lock().await = None;
    *app.failed.lock().unwrap() = None;
    *app.changed.lock().unwrap() = None;
    *app.selection.lock().unwrap() = Some(selection);
}

/// How long a remote skiffd may take to answer the first Ping.
const REMOTE_HELLO_TIMEOUT: Duration = Duration::from_secs(15);

/// A remote Host: connect through its forward, check the identity it reports
/// against its pin, and never start, reload, stop or replace anything. A
/// version mismatch is reported, not acted on. Every failure leaves its cause
/// in the warning, so the status and the picker say why, in words that never
/// repeat ssh's own output.
async fn connect_remote(app: &App, selection: &HostSelection, guard: &mut Option<Arc<SelectedHost>>) -> Result<(Arc<SelectedHost>, bool), String> {
    let destination = selection.ssh_target().map(|t| t.destination.clone()).unwrap_or_default();
    match try_remote(app, selection, &destination, crate::hosts::path()).await {
        Ok(host) => {
            *app.changed.lock().unwrap() = None;
            *app.live.lock().unwrap() = Some(host.connection());
            *guard = Some(host.clone());
            Ok((host, false))
        }
        Err(e) => {
            let (warning, message) = remote_failure(&e, &destination);
            if let Some(skiff_host_api::Error::IdentityChanged { received, .. }) = e.downcast_ref() {
                *app.changed.lock().unwrap() = Some((destination.clone(), received.clone()));
            }
            // The wait between tries is not a cause: the last cause stays.
            if let Some(w) = warning {
                *app.warning.lock().await = Some(w);
            }
            Err(message)
        }
    }
}

async fn try_remote(app: &App, selection: &HostSelection, destination: &str, file: PathBuf) -> anyhow::Result<Arc<SelectedHost>> {
    let mut c = selection.connect().await?;
    let info = match tokio::time::timeout(REMOTE_HELLO_TIMEOUT, c.daemon_info()).await {
        Ok(info) => info?,
        Err(_) => anyhow::bail!(RemoteSilent),
    };
    *app.warning.lock().await = (info.protocol != skiff_core::PROTOCOL).then(|| Warning {
        kind: "remote",
        message: format!(
            "skiffd {} on {destination} speaks protocol {}, and this app speaks {}. Parts of the app fail. \
             Update Skiff on {destination}: this app does not change it.",
            info.version, info.protocol, skiff_core::PROTOCOL
        ),
        sessions: None,
    });
    let first = selection.pinned().is_none();
    let host = Arc::new(c.activate().await?);
    if first {
        // The first identity becomes the Host's pin before anything uses it:
        // stored in hosts.toml first, then pinned here. Until the store
        // succeeds the selection stays unpinned, so every try stores again
        // and none connects on an identity only this window holds.
        let id = host.connection().hello().host.host_id.clone();
        let d = destination.to_string();
        let pinned = id.clone();
        let stored = tauri::async_runtime::spawn_blocking(move || crate::hosts::pin_first(&file, &d, &id)).await?;
        match stored {
            Ok(()) => selection.repin(pinned),
            Err(e) => {
                // Another window or run stored another identity first: that
                // one is the pin from now on, here too.
                if let Some(skiff_host_api::Error::IdentityChanged { expected, .. }) = e.downcast_ref() {
                    selection.repin(expected.clone());
                }
                return Err(e);
            }
        }
    }
    Ok(host)
}

/// The remote skiffd did not answer the first Ping in time.
#[derive(Debug)]
struct RemoteSilent;
impl std::fmt::Display for RemoteSilent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no answer within {}s", REMOTE_HELLO_TIMEOUT.as_secs())
    }
}
impl std::error::Error for RemoteSilent {}

/// The warning and the message for a failed remote connection. Only known
/// causes are named; anything else gets one plain line with what to check.
/// `None` warning: a wait between tries, which keeps the last cause.
pub(crate) fn remote_failure(e: &anyhow::Error, destination: &str) -> (Option<Warning>, String) {
    use skiff_client::host::ssh::SshError;
    let warn = |kind: &'static str, message: String| (Some(Warning { kind, message: message.clone(), sessions: None }), message);
    if let Some(e) = e.downcast_ref::<SshError>() {
        return match e {
            SshError::Waiting { .. } => (None, e.to_string()),
            _ => warn("remote", e.to_string()),
        };
    }
    match e.downcast_ref::<skiff_host_api::Error>() {
        Some(skiff_host_api::Error::IdentityChanged { .. }) => warn(
            "identity",
            format!(
                "{destination} answers as another Skiff Host than before. Skiff stopped and sends nothing to it. \
                 If you expect this, as after Skiff was reinstalled there, accept the new identity."
            ),
        ),
        Some(skiff_host_api::Error::MalformedContract { message }) => warn("remote", message.clone()),
        _ if e.downcast_ref::<RemoteSilent>().is_some() => warn(
            "remote",
            format!("skiffd on {destination} did not answer within {}s. Run `skiff info` on that Host to check it.", REMOTE_HELLO_TIMEOUT.as_secs()),
        ),
        _ => warn(
            "remote",
            format!("Cannot reach skiffd on {destination} over ssh. Run `skiff info` on that Host to check that skiffd runs and which socket it uses."),
        ),
    }
}

#[derive(Serialize, Clone)]
pub(crate) struct Warning {
    /// `outdated`, `protocol`, `newer`, `hung`, `failed` for a reload that
    /// did not come back, `remote` for a remote Host's problem, or
    /// `identity` for a remote Host that reported another identity.
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

/// Only for a local endpoint: `_local` proves it.
fn spawn_daemon(_local: &ProcessControl) -> anyhow::Result<()> {
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

/// The selected owner's Host connection: the M1e terminal plane and the M1g
/// session lifecycle and groups. The caller keeps no Client of its own.
pub(crate) async fn host_now(app: &App) -> Result<Arc<HostConnection>, String> {
    let (host, _) = ensure_host(app).await?;
    Ok(host.connection())
}

/// The selected Host's capabilities as `name@major`, for the page: it offers
/// a session or terminal surface only when the Host has its capability. The
/// Host connection checks them again on every request.
#[tauri::command]
pub(crate) async fn host_capabilities(app: State<'_, App>) -> Result<Vec<String>, String> {
    let c = host_now(&app).await?;
    Ok(capability_names(&c))
}

fn capability_names(c: &HostConnection) -> Vec<String> {
    c.hello().capabilities.iter().map(|c| format!("{}@{}", c.name, c.version)).collect()
}

/// The one M1d event path: page events from the same selected owner. The
/// receiver starts now, before the caller's next inventory read, as before.
pub(crate) async fn updates_now(app: &App) -> Result<skiff_client::host::local::LegacyUpdates, String> {
    let (host, _) = ensure_host(app).await?;
    skiff_client::host::local::legacy_updates(&host.connection()).map_err(err)
}

async fn ensure_host(app: &App) -> Result<(Arc<SelectedHost>, bool), String> {
    let mut guard = app.host.lock().await;
    if let Some(host) = guard.as_ref() {
        if !host.connection().is_closed() {
            return Ok((host.clone(), false));
        }
    }
    *guard = None;
    let selection = app.selection();
    if !selection.is_local() {
        return connect_remote(app, &selection, &mut guard).await;
    }
    let path = selection.socket();
    // Starting a daemon is endpoint-local lifecycle. Without it this only connects.
    let control = selection.process_control();
    let mut spawned = false;
    let mut replaced = false;
    let mut last = String::new();
    for _ in 0..40 {
        let mut c = match selection.connect().await {
            Ok(c) => c,
            Err(e) => {
                last = format!("{e:#}");
                // A reload failed: starting a new daemon is the user's call.
                if let Some(why) = app.failed.lock().unwrap().clone() {
                    return Err(why);
                }
                if !spawned {
                    if let Some(control) = &control {
                        spawn_daemon(control).map_err(|e| format!("start skiffd: {e:#}"))?;
                        spawned = true;
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        // Recorded for activation: the Host's identity comes from this pong.
        let info = tokio::time::timeout(HELLO_TIMEOUT, c.daemon_info()).await.ok().and_then(|r| r.ok());
        match check_info(app, c.client(), info, !replaced).await {
            Check::Use => {
                *app.failed.lock().unwrap() = None;
                let host = Arc::new(c.activate().await.map_err(err)?);
                *app.live.lock().unwrap() = Some(host.connection());
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
    let info = tokio::time::timeout(HELLO_TIMEOUT, c.daemon_info()).await.ok().and_then(|r| r.ok());
    check_info(app, c, info, may_replace).await
}

/// [`check_daemon`] with the daemon's `pong` already read; `None`: no answer.
async fn check_info(app: &App, c: &Client, info: Option<skiff_client::DaemonInfo>, may_replace: bool) -> Check {
    let Some(info) = info else {
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
    // Never a downgrade, and one try per app run. Planning runs the bundled
    // skiffd and replacing signals the daemon: endpoint-local only.
    let control = app.selection().process_control();
    let mut old_build = None;
    let planned = (order.is_lt() || other_build) && !app.reload_tried.load(Ordering::Relaxed);
    if let (true, Some(local)) = (planned, &control) {
        match reload::plan(local, &info, other_build).await {
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
    if let (true, true, Some(0), Some(pid), Some(control)) = (may_replace, stale, live, c.daemon_pid, control) {
        if stop_daemon(&control, pid, false).await {
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
async fn stop_daemon(_local: &ProcessControl, pid: i32, escalate: bool) -> bool {
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
        // A peer pid is the local socket's peer: signal it only for a local endpoint.
        let selection = app.selection();
        let control = selection.process_control().ok_or("skiffd on this Host is not managed by this app")?;
        let pid = match guard.as_ref() {
            Some(host) => host.client().daemon_pid,
            None => selection.connect().await.ok().and_then(|c| c.client().daemon_pid),
        };
        *guard = None;
        *app.live.lock().unwrap() = None;
        if let Some(pid) = pid {
            if !stop_daemon(&control, pid, true).await {
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
    let selection = app.selection();
    // A remote Host shows as its destination and socket. Its forward's peer
    // pid is this Mac's `ssh`, never the daemon: it is not shown.
    let socket = match selection.ssh_target() {
        Some(t) => format!("{}:{}", t.destination, t.socket),
        None => selection.socket().display().to_string(),
    };
    let local = selection.is_local();
    match ensure_host(app).await {
        Ok((host, spawned)) => {
            let c = host.client();
            let hello = tokio::time::timeout(HELLO_TIMEOUT, connection_status(&host.connection())).await.ok().and_then(|r| r.ok());
            DaemonStatus {
                connected: true,
                version: hello.as_ref().map(|(v, _)| v.clone()),
                protocol: hello.map(|(_, p)| p),
                socket,
                pid: c.daemon_pid.filter(|_| local),
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
    use super::{compare_versions, stop_daemon, App, ProcessControl};
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
        app.set_selection(HostSelection::new(socket.clone(), dir.join("local-host-id")));
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
            // M1e: the terminal plane sends exactly the legacy frames.
            let legacy = |id: u64, request: skiff_core::protocol::Request| serde_json::to_value(skiff_core::protocol::Envelope { id, request }).unwrap();
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), legacy(8, skiff_core::protocol::Request::Subscribe { session: "t1".into() }));
            // The snapshot and output precede the reply, as skiffd may send them.
            for frame in [
                serde_json::json!({"event":"snapshot","session":"t1","data":"AP8KDQ==","cols":80,"rows":24}),
                serde_json::json!({"event":"output","session":"other","data":"eA=="}),
                serde_json::json!({"event":"output","session":"t1","data":"bHMK","echo":{"wrote":1,"read":2}}),
                serde_json::json!({"id":8,"type":"ok"}),
            ] {
                let mut bytes = serde_json::to_vec(&frame).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            line.clear();
            stream.read_line(&mut line).await.unwrap();
            assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), legacy(9, skiff_core::protocol::Request::Write { session: "t1".into(), data: vec![0, 255, b'\n'], paste: false }));
            line.clear();
            stream.read_line(&mut line).await.unwrap();
            assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), legacy(10, skiff_core::protocol::Request::Write { session: "t1".into(), data: b"ls\n".to_vec(), paste: false }));
            for (id, request, reply) in [
                (11, skiff_core::protocol::Request::Resize { session: "t1".into(), cols: 100, rows: 30 }, serde_json::json!({"type":"ok"})),
                (12, skiff_core::protocol::Request::Resize { session: "gone".into(), cols: 1, rows: 1 }, serde_json::json!({"type":"error","message":"no such session"})),
                (13, skiff_core::protocol::Request::Unsubscribe { session: "t1".into() }, serde_json::json!({"type":"ok"})),
            ] {
                line.clear();
                stream.read_line(&mut line).await.unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), legacy(id, request));
                let mut reply = reply; reply["id"] = id.into();
                let mut bytes = serde_json::to_vec(&reply).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            // M1g: lifecycle and groups send exactly the legacy frames.
            let spec: skiff_core::session::SessionSpec = serde_json::from_value(serde_json::json!({"label":"sh","cwd":"/srv/λ","cols":90,"rows":20})).unwrap();
            let group: skiff_core::group::Group = serde_json::from_value(serde_json::json!({"name":"G","layout":{"type":"pane","session":"t1"}})).unwrap();
            let mut row = inventory[0].clone();
            row["id"] = "made".into();
            for (id, request, reply) in [
                (14, skiff_core::protocol::Request::CreateSession { spec }, serde_json::json!({"type":"session","session":row})),
                (15, skiff_core::protocol::Request::Kill { session: "gone".into() }, serde_json::json!({"type":"error","message":"no such session"})),
                (16, skiff_core::protocol::Request::ListGroups, serde_json::json!({"type":"groups","groups":[]})),
                (17, skiff_core::protocol::Request::SaveGroup { group }, serde_json::json!({"type":"group","group":{"id":"g1","name":"G","layout":{"type":"pane","session":"t1"},"focus":null}})),
            ] {
                line.clear();
                stream.read_line(&mut line).await.unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), legacy(id, request));
                let mut reply = reply; reply["id"] = id.into();
                let mut bytes = serde_json::to_vec(&reply).unwrap(); bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            }
            // Keep the one connection alive through the reuse assertions.
            line.clear();
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
        let live = app.live.lock().unwrap().clone().unwrap();
        assert!(Arc::ptr_eq(&live, &owner.connection()));
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
        // M1e: attach/output/input/resize/detach on the same selected owner.
        let terminal = host_now(&app).await.unwrap();
        assert!(Arc::ptr_eq(&terminal, &live));
        let mut output = skiff_client::host::local::legacy_output(&terminal, "t1").unwrap();
        skiff_client::host::local::legacy_attach(&terminal, "t1").await.unwrap();
        use skiff_client::host::TerminalFrame;
        assert_eq!(output.recv().await, Ok(TerminalFrame::Snapshot { data: vec![0, 255, 10, 13], cols: 80, rows: 24 }));
        let Ok(TerminalFrame::Output { data, echo: Some(echo) }) = output.recv().await else { panic!("no output") };
        assert_eq!(data, b"ls\n");
        // The latency trace reads the same JSON as before.
        assert_eq!(serde_json::to_value(echo).unwrap(), serde_json::json!({"wrote":1,"read":2}));
        // Input through the bridge, in order, with arbitrary bytes.
        assert!(matches!(app.input.write(Some(&live), "t1", vec![0, 255, b'\n']).unwrap(), crate::input::Sent::Done));
        assert!(matches!(app.input.write(Some(&live), "t1", b"ls\n".to_vec()).unwrap(), crate::input::Sent::Done));
        skiff_client::host::local::legacy_resize(&terminal, "t1", 100, 30).await.unwrap();
        assert_eq!(skiff_client::host::local::legacy_resize(&terminal, "gone", 1, 1).await.unwrap_err().to_string(), "no such session");
        skiff_client::host::local::legacy_detach(&terminal, "t1").await.unwrap();
        // Detach closes the receiver first, as unsubscribe did.
        assert_eq!(output.recv().await, Err(skiff_client::host::OutputGap::Closed));
        let mut reattached = skiff_client::host::local::legacy_output(&terminal, "t1").unwrap();
        // M1g: lifecycle and groups on the same selected owner, and the
        // capabilities the page reads.
        assert_eq!(capability_names(&terminal), ["connection_status@1", "session_screen@1", "session_inventory@1", "session_updates@1", "session_terminal@1", "session_lifecycle@1", "session_groups@1", "session_agents@1"]);
        let made = skiff_client::host::local::legacy_create(&terminal, serde_json::from_value(serde_json::json!({"label":"sh","cwd":"/srv/λ","cols":90,"rows":20})).unwrap()).await.unwrap();
        assert_eq!(made.id, "made");
        assert_eq!(skiff_client::host::local::legacy_kill(&terminal, "gone").await.unwrap_err().to_string(), "no such session");
        assert!(skiff_client::host::local::legacy_groups(&terminal).await.unwrap().is_empty());
        let saved = skiff_client::host::local::legacy_save_group(&terminal, serde_json::from_value(serde_json::json!({"name":"G","layout":{"type":"pane","session":"t1"}})).unwrap()).await.unwrap();
        assert_eq!(saved.id, "g1");
        assert!(Arc::ptr_eq(&owner, app.host.lock().await.as_ref().unwrap()));
        drop(terminal);
        drop(live);
        // Dropping the selected owner, as reconnect/restart/reload do, ends it.
        drop(owner);
        drop(client);
        *app.host.lock().await = None;
        assert!(updates.recv().await.is_none());
        assert_eq!(reattached.recv().await, Err(skiff_client::host::OutputGap::Closed));
        *app.live.lock().unwrap() = None;
        assert_eq!(std::fs::read_to_string(config).unwrap(), "# existing config\n");
        assert_eq!(std::fs::read(ledger).unwrap(), b"existing ledger fixture");
        peer.abort();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A remote Host is reached, never managed: a failing SSH endpoint gets
    /// an explanation, and the app starts, reloads and signals nothing here.
    #[test]
    fn a_remote_endpoint_never_runs_local_lifecycle_and_reports_why() {
        use super::*;
        let _env = crate::reload::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
        tokio::runtime::Runtime::new().unwrap().block_on(async {
        let dir = std::env::temp_dir().join(format!("skiff-remote-life-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        // A local "skiffd" and "ssh" that only leave a mark.
        let mark = |name: &str| {
            let script = dir.join(name);
            std::fs::write(&script, format!("#!/bin/sh\necho \"$@\" >> {}/{name}.ran\nexit 255\n", dir.display())).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            script
        };
        let old_daemon = std::env::var_os("SKIFF_DAEMON");
        std::env::set_var("SKIFF_DAEMON", mark("skiffd"));
        let app = App::default();
        let target = skiff_client::host::ssh::SshTarget { destination: "devbox".into(), socket: "/run/user/1/skiff/skiffd.sock".into() };
        // Review P2: an unknown host key from a fresh state reaches the status.
        let ssh = mark("ssh");
        let text = std::fs::read_to_string(&ssh).unwrap().replace("exit 255", "echo 'Host key verification failed.' >&2\nexit 255");
        std::fs::write(&ssh, text).unwrap();
        let options = skiff_client::host::ssh::SshOptions { program: ssh, config: None };
        app.set_selection(HostSelection::ssh(target, options, dir.join("fwd"), None).unwrap());
        assert!(app.remote());
        assert!(app.selection().process_control().is_none());
        let started = std::time::Instant::now();
        let status = status_now(&app).await;
        assert!(!status.connected && !status.spawned && status.pid.is_none());
        assert_eq!(status.socket, "devbox:/run/user/1/skiff/skiffd.sock");
        let cause = status.warning.as_ref().expect("the cause reaches the status");
        assert_eq!(cause.kind, "remote");
        assert!(cause.message.contains("does not know the host key of devbox") && !cause.message.contains("verification failed"), "{}", cause.message);
        // One ssh run, no retry loop, and a second try waits.
        assert!(started.elapsed() < Duration::from_secs(5));
        let e = ensure_host(&app).await.err().unwrap();
        assert!(e.contains("tries again"), "{e}");
        // The wait keeps the cause: the status still says what to fix.
        let again = status_now(&app).await;
        assert!(again.warning.is_some_and(|w| w.message.contains("host key")));
        assert_eq!(std::fs::read_to_string(dir.join("ssh.ran")).unwrap().lines().count(), 1);
        assert!(!dir.join("skiffd.ran").exists(), "the local daemon binary ran for a remote Host");
        // Switching back to this Mac drops the remote selection and its forward.
        replace_selection(&app, local_selection()).await;
        assert!(!app.remote());
        match old_daemon {
            Some(v) => std::env::set_var("SKIFF_DAEMON", v),
            None => std::env::remove_var("SKIFF_DAEMON"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        });
    }

    /// Review P2: each remote failure has a cause the status can show, in
    /// words that never repeat ssh's output; a wait keeps the last cause.
    #[test]
    fn remote_failures_name_a_cause_and_a_wait_keeps_the_last_one() {
        use skiff_client::host::ssh::SshError;
        let d = "devbox";
        let cause = |e: anyhow::Error| super::remote_failure(&e, d);
        for e in [
            SshError::HostKeyUnknown(d.into()),
            SshError::HostKeyChanged(d.into()),
            SshError::AuthFailed(d.into()),
            SshError::ChannelRefused { destination: d.into(), socket: "/s.sock".into() },
            SshError::ForwardingRefused(d.into()),
        ] {
            let text = e.to_string();
            let (w, message) = cause(e.into());
            assert_eq!(w.map(|w| (w.kind, w.message)), Some(("remote", text.clone())));
            assert_eq!(message, text);
        }
        let (w, _) = cause(SshError::Waiting { destination: d.into(), seconds: 4 }.into());
        assert!(w.is_none());
        let id = |s: &str| skiff_host_api::HostId::try_from(s.to_string()).unwrap();
        let (w, _) = cause(skiff_host_api::Error::IdentityChanged { expected: id("A"), received: id("B") }.into());
        assert_eq!(w.unwrap().kind, "identity");
        // An unknown failure names no path or ssh line: one line, what to check.
        let (w, _) = cause(anyhow::anyhow!("connect /var/folders/x/T/skiff-501-1/9-0/skiffd.sock: No such file"));
        let w = w.unwrap();
        assert!(w.message.contains("skiff info") && !w.message.contains("/var/folders"), "{}", w.message);
    }

    fn local_control() -> ProcessControl {
        App::default().selection().process_control().unwrap()
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
        assert!(rt.block_on(stop_daemon(&local_control(), child.id() as i32, true)));
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
        let stopped = rt.block_on(stop_daemon(&local_control(), child.id() as i32, false));
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

    /// Residual P1, failure injection: the first identity's store fails (a
    /// directory sits where the new hosts.toml is written). The connection is
    /// refused and the selection stays unpinned, on a retry too; once the
    /// store works, the retry stores the pin before it connects. A Host that
    /// then answers as B is refused here and by a selection rebuilt from the
    /// file. A fake `ssh` only (a Python Unix-socket server), never OpenSSH.
    #[test]
    fn a_first_identity_connects_only_once_it_is_stored_even_on_a_retry() {
        use super::*;
        if !std::path::Path::new("/usr/bin/python3").is_file() {
            eprintln!("skipped: no /usr/bin/python3");
            return;
        }
        // Short: the forward's socket path must fit in sun_path.
        let dir = PathBuf::from("/tmp").join(format!("sd-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let id_file = dir.join("remote-id");
        let script = format!(
            "#!/usr/bin/python3\n\
             import socket,sys,json,threading\n\
             spec=sys.argv[sys.argv.index('-L')+1]\n\
             s=socket.socket(socket.AF_UNIX); s.bind(spec.split(':')[0]); s.listen()\n\
             def serve(c):\n\
             \x20with c, c.makefile('rb') as f:\n\
             \x20 for line in f:\n\
             \x20  req=json.loads(line)\n\
             \x20  if req.get('cmd')=='ping':\n\
             \x20   hid=open('{id}').read()\n\
             \x20   data={{'id':req['id'],'type':'pong','version':'{v}','protocol':{p},'identity':{{'host_id':hid,'instance_id':'run','os':'fixture','account':'fixture'}}}}\n\
             \x20   c.sendall((json.dumps(data)+'\\n').encode())\n\
             while True:\n\
             \x20c,_=s.accept();threading.Thread(target=serve,args=(c,),daemon=True).start()\n",
            id = id_file.display(),
            v = env!("CARGO_PKG_VERSION"),
            p = skiff_core::PROTOCOL,
        );
        let ssh = dir.join("fake-ssh");
        std::fs::write(&ssh, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(&id_file, "A").unwrap();
        let file = dir.join("hosts.toml");
        std::fs::write(&file, "selected = \"fixture\"\n[[host]]\ndestination = \"fixture\"\nsocket = \"/fixture.sock\"\n").unwrap();
        let target = skiff_client::host::ssh::SshTarget { destination: "fixture".into(), socket: "/fixture.sock".into() };
        let options = skiff_client::host::ssh::SshOptions { program: ssh, config: None };
        let selection = |pin: Option<&str>| {
            let pin = pin.map(|p| skiff_host_api::HostId::try_from(p.to_string()).unwrap());
            HostSelection::ssh(target.clone(), options.clone(), dir.join("fwd"), pin).unwrap()
        };
        let disk_pin = || crate::hosts::load(&file).unwrap().hosts[0].host_id.clone();
        let changed = |r: &anyhow::Result<Arc<SelectedHost>>| matches!(r.as_ref().err().and_then(|e| e.downcast_ref()), Some(skiff_host_api::Error::IdentityChanged { .. }));
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let app = App::default();
            let first = selection(None);
            // Only the store fails: the new file cannot be written beside.
            let obstruction = file.with_extension(format!("toml.{}.tmp", std::process::id()));
            std::fs::create_dir(&obstruction).unwrap();
            for attempt in 0..2 {
                let r = try_remote(&app, &first, "fixture", file.clone()).await;
                assert!(r.is_err(), "attempt {attempt} connected with no stored pin");
                assert_eq!(first.pinned(), None, "attempt {attempt} pinned an unstored identity");
                assert_eq!(disk_pin(), None);
            }
            std::fs::remove_dir(&obstruction).unwrap();
            let retry = try_remote(&app, &first, "fixture", file.clone()).await;
            assert!(retry.is_ok(), "{:?}", retry.err());
            assert_eq!(disk_pin().as_deref(), Some("A"), "the retry connected without storing its pin");
            assert_eq!(first.pinned().map(|p| p.as_str().to_string()).as_deref(), Some("A"));
            drop(retry);
            // Another Host answers there now.
            std::fs::write(&id_file, "B").unwrap();
            assert!(changed(&try_remote(&app, &first, "fixture", file.clone()).await), "the same selection accepted B");
            drop(first);
            let rebuilt = selection(disk_pin().as_deref());
            assert!(changed(&try_remote(&app, &rebuilt, "fixture", file.clone()).await), "a rebuilt selection accepted B");
            assert_eq!(disk_pin().as_deref(), Some("A"));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
