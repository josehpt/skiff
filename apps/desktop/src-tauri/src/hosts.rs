//! Known Hosts and which one this window shows: client-owned `hosts.toml`
//! beside `projects.toml`. "This Mac" is the local Host and needs no entry.
//! A remote Host is an SSH destination (an alias from ssh config) and the
//! skiffd socket on it. Each remote Host keeps the Skiff identity it first
//! reported (`host_id`): a different one is refused on every later
//! connection, after a new selection or app start too, until the person
//! accepts it. Each remote Host has its own clipboard opt-in for OSC 52
//! writes from its terminals, given to that identity only, and it counts
//! only while this window's live connection answers as that identity.
//!
//! Every change to the file is one transaction for all windows and app
//! runs: [`update`] holds an exclusive `flock` from the read to the rename.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use skiff_client::host::{
    local::HostSelection,
    ssh::{SshOptions, SshTarget},
};
use skiff_host_api::HostId;
use tauri::{Manager, State};

use crate::connection::{err, App};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct KnownHost {
    /// What `ssh` takes: an alias from ssh config, `host` or `user@host`.
    pub(crate) destination: String,
    /// skiffd's socket on that Host, as `skiff info` prints it there.
    pub(crate) socket: String,
    /// The Host id skiffd reported on the first accepted connection. Only
    /// the person changes it, by accepting a new identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) host_id: Option<String>,
    /// The Host id whose OSC 52 clipboard writes reach this Mac. It counts
    /// only while it is `host_id`: a new identity has no permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) clipboard_host: Option<String>,
}

impl KnownHost {
    fn new(destination: String, socket: String) -> Self {
        Self { destination, socket, host_id: None, clipboard_host: None }
    }

    /// Clipboard writes from a connection that answers as `live` are
    /// allowed: it is the identity this Host must have, and the one the
    /// person allowed. Another window's newer pin or grant does not count.
    pub(crate) fn clipboard_for(&self, live: &HostId) -> bool {
        self.host_id.as_deref() == Some(live.as_str()) && self.clipboard_host.as_deref() == Some(live.as_str())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Hosts {
    /// The destination this window shows. None: this Mac.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selected: Option<String>,
    #[serde(default, rename = "host")]
    pub(crate) hosts: Vec<KnownHost>,
}

pub(crate) fn path() -> PathBuf {
    skiff_core::config::config_path().with_file_name("hosts.toml")
}

pub(crate) fn load(path: &Path) -> anyhow::Result<Hosts> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(toml::from_str(&text)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Hosts::default()),
        Err(e) => Err(e.into()),
    }
}

/// Changes the file as one step for every window and app run. An exclusive
/// `flock` on `hosts.toml.lock`, beside it, is held from the read to the
/// rename, so no change is lost to another, and a check such as "no pin yet"
/// still holds when the change is written. Nothing is written when `change`
/// fails or changes nothing. Every write of the file goes through here.
fn update<T>(path: &Path, change: impl FnOnce(&mut Hosts) -> anyhow::Result<T>) -> anyhow::Result<T> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let lock = std::fs::File::options().create(true).truncate(false).write(true).open(path.with_extension("toml.lock"))?;
    // Released when `lock` closes, or when the process ends.
    lock.lock()?;
    let before = load(path)?;
    let mut hosts = before.clone();
    let out = change(&mut hosts)?;
    if hosts != before {
        save(path, &hosts)?;
    }
    Ok(out)
}

/// A complete file or none: written beside, then renamed over. Only under
/// [`update`]'s lock.
fn save(path: &Path, hosts: &Hosts) -> anyhow::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    std::fs::write(&temp, toml::to_string(hosts)?)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}

impl Hosts {
    pub(crate) fn find(&self, destination: &str) -> Option<&KnownHost> {
        self.hosts.iter().find(|h| h.destination == destination)
    }

    /// The selection the file names. A selected Host that is not in the list
    /// is this Mac: the file never points at an unknown endpoint.
    pub(crate) fn selection(&self, local: impl FnOnce() -> HostSelection) -> HostSelection {
        match self.selected.as_deref().and_then(|d| self.find(d)) {
            Some(h) => remote(h).unwrap_or_else(|_| local()),
            None => local(),
        }
    }
}

/// The private directory for this destination's forwards. Each forward makes
/// its own directory inside it. Short, as a socket path must be.
fn forward_dir(destination: &str) -> PathBuf {
    let hash = destination.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    // SAFETY: geteuid takes no arguments and cannot fail.
    std::env::temp_dir().join(format!("skiff-{}-{:08x}", unsafe { libc::geteuid() }, hash as u32))
}

pub(crate) fn remote(h: &KnownHost) -> Result<HostSelection, String> {
    let target = SshTarget { destination: h.destination.clone(), socket: h.socket.clone() };
    let pin = h.host_id.clone().map(HostId::try_from).transpose()?;
    HostSelection::ssh(target, SshOptions::default(), forward_dir(&h.destination), pin)
}

fn known<'a>(hosts: &'a mut Hosts, destination: &str) -> anyhow::Result<&'a mut KnownHost> {
    hosts.hosts.iter_mut().find(|h| h.destination == destination).ok_or_else(|| anyhow::anyhow!("{destination} is not a known Host"))
}

/// Stores the identity a Host reported on its first accepted connection: a
/// compare-and-set under [`update`]'s lock. The file decides: when another
/// window or run stored a different one first, this one is refused as a
/// changed identity. Ok only once the pin is on disk.
pub(crate) fn pin_first(file: &Path, destination: &str, host_id: &HostId) -> anyhow::Result<()> {
    update(file, |hosts| {
        let h = known(hosts, destination)?;
        match &h.host_id {
            Some(pin) if pin == host_id.as_str() => Ok(()),
            Some(pin) => Err(skiff_host_api::Error::IdentityChanged {
                expected: HostId::try_from(pin.clone()).map_err(anyhow::Error::msg)?,
                received: host_id.clone(),
            }
            .into()),
            None => {
                h.host_id = Some(host_id.as_str().to_string());
                h.clipboard_host = None;
                Ok(())
            }
        }
    })
}

/// Makes `host_id` the identity `destination` must report, as the person
/// accepted it. The clipboard permission of the old identity goes.
pub(crate) fn accept_identity(file: &Path, destination: &str, host_id: &HostId) -> anyhow::Result<Hosts> {
    update(file, |hosts| {
        let h = known(hosts, destination)?;
        h.host_id = Some(host_id.as_str().to_string());
        h.clipboard_host = None;
        Ok(hosts.clone())
    })
}

/// Allows or stops OSC 52 writes from `destination` for `live`, the identity
/// the asking window's connection accepted. Allowing needs that identity to
/// be the one the file pins now: a window still connected to an identity
/// another window replaced cannot grant the new one, nor its own.
fn set_clipboard(file: &Path, destination: &str, live: Option<&HostId>, allow: bool) -> anyhow::Result<()> {
    update(file, |hosts| {
        let h = known(hosts, destination)?;
        h.clipboard_host = match live {
            _ if !allow => None,
            Some(id) if h.host_id.as_deref() == Some(id.as_str()) => Some(id.as_str().to_string()),
            Some(_) => anyhow::bail!("Skiff stored another identity for {destination} since this window connected. Pick the Host again, then allow clipboard writes."),
            None => anyhow::bail!("Skiff is not connected to {destination}. Connect once, then allow clipboard writes."),
        };
        Ok(())
    })
}

/// Clipboard writes from the shown Host are allowed for a connection that
/// answers as `live`, as the file says now.
fn clipboard_allowed(file: &Path, destination: &str, live: &HostId) -> anyhow::Result<bool> {
    Ok(load(file)?.find(destination).is_some_and(|h| h.clipboard_for(live)))
}

/// A known Host as the page sees it.
#[derive(Serialize)]
struct HostView {
    destination: String,
    socket: String,
}

/// What the page shows and offers: the selected Host, and the known ones.
#[derive(Serialize)]
pub(crate) struct HostList {
    /// None: this Mac.
    selected: Option<String>,
    hosts: Vec<HostView>,
    /// The shown remote Host's clipboard permission, for the menu: given to
    /// the identity this window accepted. Each write asks `host_clipboard`.
    clipboard: bool,
}

/// `identity`: the one this window accepted, live or pinned.
fn host_list(hosts: Hosts, selected: Option<String>, identity: Option<&HostId>) -> HostList {
    let clipboard = match (&selected, identity) {
        (Some(d), Some(id)) => hosts.find(d).is_some_and(|h| h.clipboard_for(id)),
        _ => false,
    };
    let hosts = hosts.hosts.into_iter().map(|h| HostView { destination: h.destination, socket: h.socket }).collect();
    HostList { selected, hosts, clipboard }
}

#[tauri::command]
pub(crate) async fn list_hosts(app: State<'_, App>) -> Result<HostList, String> {
    let hosts = blocking(|| load(&path())).await?;
    let selection = app.selection();
    let identity = app.live_identity().or_else(|| selection.pinned());
    Ok(host_list(hosts, selection.ssh_target().map(|t| t.destination.clone()), identity.as_ref()))
}

/// An OSC 52 write from the shown Host may reach this Mac's clipboard now:
/// asked on every write, against the identity this window's live connection
/// accepted and the file as it is now. This Mac: always.
#[tauri::command]
pub(crate) async fn host_clipboard(app: State<'_, App>) -> Result<bool, String> {
    let Some(destination) = app.selection().ssh_target().map(|t| t.destination.clone()) else {
        return Ok(true);
    };
    let Some(live) = app.live_identity() else { return Ok(false) };
    blocking(move || clipboard_allowed(&path(), &destination, &live)).await
}

/// Adds or updates a remote Host. It does not connect.
#[tauri::command]
pub(crate) async fn add_host(destination: String, socket: String) -> Result<(), String> {
    let destination = destination.trim().to_string();
    let socket = socket.trim().to_string();
    SshTarget { destination: destination.clone(), socket: socket.clone() }.validate()?;
    blocking(move || {
        update(&path(), |hosts| {
            match hosts.hosts.iter_mut().find(|h| h.destination == destination) {
                Some(h) => h.socket = socket,
                None => hosts.hosts.push(KnownHost::new(destination, socket)),
            }
            Ok(())
        })
    })
    .await
}

/// Forgets a remote Host. The shown Host cannot go: pick another first.
#[tauri::command]
pub(crate) async fn remove_host(app: State<'_, App>, destination: String) -> Result<(), String> {
    if app.selection().ssh_target().is_some_and(|t| t.destination == destination) {
        return Err("This window shows that Host. Pick another Host first.".into());
    }
    blocking(move || {
        update(&path(), |hosts| {
            hosts.hosts.retain(|h| h.destination != destination);
            Ok(())
        })
    })
    .await
}

/// Shows another Host in this window: the old connection, its streams and
/// any held input go first, so nothing typed for one Host reaches another.
/// The page reloads after it. `None`: this Mac.
#[tauri::command]
pub(crate) async fn select_host(handle: tauri::AppHandle, app: State<'_, App>, destination: Option<String>) -> Result<(), String> {
    let selection = blocking(move || {
        update(&path(), |hosts| {
            if let Some(d) = &destination {
                anyhow::ensure!(hosts.find(d).is_some(), "{d} is not a known Host");
            }
            hosts.selected = destination;
            Ok(hosts.selection(crate::connection::local_selection))
        })
    })
    .await?;
    crate::connection::replace_selection(&app, selection).await;
    // A launch on a remote Host skipped this Mac's Skipper setup.
    if !app.remote() {
        let handle = handle.clone();
        std::thread::spawn(move || crate::skiff_agent::sync(&handle.state::<App>()));
    }
    Ok(())
}

/// Allows or stops OSC 52 clipboard writes from a remote Host's terminals.
/// Allowed for the identity this window's live connection accepted, which
/// must be the one the Host must report, so a Host that answers with another
/// identity later has no permission.
#[tauri::command]
pub(crate) async fn set_host_clipboard(app: State<'_, App>, destination: String, allow: bool) -> Result<(), String> {
    let shown = app.selection().ssh_target().is_some_and(|t| t.destination == destination);
    let live = app.live_identity().filter(|_| shown);
    blocking(move || set_clipboard(&path(), &destination, live.as_ref(), allow)).await
}

/// The person accepts the new identity the shown Host reported. Only the one
/// Rust saw and refused can be accepted, not one the page names. The page
/// reloads after it.
#[tauri::command]
pub(crate) async fn accept_host_identity(app: State<'_, App>, destination: String) -> Result<(), String> {
    let changed = app.changed.lock().unwrap().clone();
    let Some((seen, received)) = changed.filter(|(d, _)| *d == destination) else {
        return Err(format!("{destination} reported no new identity to accept."));
    };
    if app.selection().ssh_target().is_none_or(|t| t.destination != seen) {
        return Err("This window shows another Host now.".into());
    }
    let selection = blocking(move || {
        let hosts = accept_identity(&path(), &seen, &received)?;
        Ok(hosts.selection(crate::connection::local_selection))
    })
    .await?;
    crate::connection::replace_selection(&app, selection).await;
    Ok(())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?.map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_round_trip_and_an_unknown_selection_is_this_mac() {
        let dir = std::env::temp_dir().join(format!("skiff-hosts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("hosts.toml");
        assert_eq!(load(&file).unwrap(), Hosts::default());
        let hosts = Hosts {
            selected: Some("devbox".into()),
            hosts: vec![KnownHost::new("devbox".into(), "/run/user/1000/skiff/skiffd.sock".into())],
        };
        save(&file, &hosts).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("selected = \"devbox\"") && text.contains("[[host]]"), "{text}");
        assert_eq!(load(&file).unwrap(), hosts);
        let local = || HostSelection::new("/l.sock".into(), "/id".into());
        let picked = hosts.selection(local);
        assert_eq!(picked.ssh_target().map(|t| t.destination.as_str()), Some("devbox"));
        assert!(picked.process_control().is_none());
        // Room under the forwards' directory for `<pid>-<n>/skiffd.sock`.
        assert!(picked.socket().as_os_str().len() + 32 < 104, "{}", picked.socket().display());
        assert_eq!(picked.pinned(), None);
        let gone = Hosts { selected: Some("gone".into()), ..hosts.clone() };
        assert!(gone.selection(local).is_local());
        let bad = Hosts { hosts: vec![KnownHost { destination: "-oProxyCommand=x".into(), ..hosts.hosts[0].clone() }], selected: Some("-oProxyCommand=x".into()) };
        assert!(bad.selection(local).is_local(), "an invalid entry never becomes an endpoint");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Review P1: accept A, then B at the same endpoint. A new selection from
    /// the file, as after picking another Host or restarting, still refuses B
    /// and A's clipboard permission does not pass to B.
    #[test]
    fn the_first_identity_is_pinned_and_only_the_person_changes_it() {
        let dir = std::env::temp_dir().join(format!("skiff-hosts-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("hosts.toml");
        let id = |s: &str| HostId::try_from(s.to_string()).unwrap();
        save(&file, &Hosts { selected: Some("devbox".into()), hosts: vec![KnownHost::new("devbox".into(), "/s.sock".into())] }).unwrap();
        pin_first(&file, "devbox", &id("A")).unwrap();
        pin_first(&file, "devbox", &id("A")).unwrap();
        let changed = pin_first(&file, "devbox", &id("B")).unwrap_err();
        assert!(matches!(changed.downcast_ref(), Some(skiff_host_api::Error::IdentityChanged { .. })), "{changed:#}");
        let hosts = load(&file).unwrap();
        assert_eq!(hosts.hosts[0].host_id.as_deref(), Some("A"));
        let local = || HostSelection::new("/l.sock".into(), "/id".into());
        assert_eq!(hosts.selection(local).pinned(), Some(id("A")));

        // Clipboard needs a live identity, and is given to that one only.
        assert!(set_clipboard(&file, "devbox", None, true).is_err());
        set_clipboard(&file, "devbox", Some(&id("A")), true).unwrap();
        assert!(load(&file).unwrap().hosts[0].clipboard_for(&id("A")));
        let accepted = accept_identity(&file, "devbox", &id("B")).unwrap();
        assert_eq!(accepted.hosts[0].host_id.as_deref(), Some("B"));
        assert!(!accepted.hosts[0].clipboard_for(&id("B")), "the old identity's permission is gone");
        assert_eq!(load(&file).unwrap().selection(local).pinned(), Some(id("B")));
        // A file whose permission names another identity allows nothing.
        let foreign = KnownHost { host_id: Some("B".into()), clipboard_host: Some("A".into()), ..KnownHost::new("x".into(), "/s".into()) };
        assert!(!foreign.clipboard_for(&id("A")) && !foreign.clipboard_for(&id("B")));
        // The old bool key from before pinning is ignored, not trusted.
        std::fs::write(&file, "[[host]]\ndestination = \"devbox\"\nsocket = \"/s.sock\"\nclipboard = true\n").unwrap();
        assert!(!load(&file).unwrap().hosts[0].clipboard_for(&id("A")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Residual P2: window 1 stays connected to B with no permission; window
    /// 2, another app run, accepts C at the same destination and allows C.
    /// Window 1's page reloads (its live B connection survives): neither the
    /// list nor a write allows B, and window 1 cannot grant B or C.
    #[test]
    fn clipboard_follows_the_live_identity_not_another_windows_grant() {
        let dir = std::env::temp_dir().join(format!("skiff-hosts-clip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("hosts.toml");
        let id = |s: &str| HostId::try_from(s.to_string()).unwrap();
        update(&file, |h| {
            h.selected = Some("devbox".into());
            h.hosts.push(KnownHost::new("devbox".into(), "/s.sock".into()));
            Ok(())
        })
        .unwrap();
        pin_first(&file, "devbox", &id("B")).unwrap();
        let live_b = id("B");
        // Window 2.
        accept_identity(&file, "devbox", &id("C")).unwrap();
        set_clipboard(&file, "devbox", Some(&id("C")), true).unwrap();
        // Window 1 after its page reloads: its live identity is still B.
        let list = host_list(load(&file).unwrap(), Some("devbox".into()), Some(&live_b));
        assert!(!list.clipboard, "C's grant allowed B's writes in the list");
        assert!(!clipboard_allowed(&file, "devbox", &live_b).unwrap(), "C's grant allowed a write from B");
        assert!(set_clipboard(&file, "devbox", Some(&live_b), true).is_err(), "window 1 granted while connected to B");
        assert_eq!(load(&file).unwrap().hosts[0].clipboard_host.as_deref(), Some("C"), "window 1 changed C's grant");
        // Window 2, live on C, may write.
        assert!(host_list(load(&file).unwrap(), Some("devbox".into()), Some(&id("C"))).clipboard);
        assert!(clipboard_allowed(&file, "devbox", &id("C")).unwrap());
        // The page's list carries no per-Host permission to fall back on.
        let json = serde_json::to_value(host_list(load(&file).unwrap(), Some("devbox".into()), Some(&live_b))).unwrap();
        assert!(json["hosts"][0].get("clipboard").is_none(), "{json}");
        // No identity yet: nothing allowed.
        assert!(!host_list(load(&file).unwrap(), Some("devbox".into()), None).clipboard);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// One store step in a child process of this test binary, released by
    /// the parent's `go` file at the same time as the others. Run only by
    /// the tests below.
    #[test]
    #[ignore]
    fn store_child() {
        let Ok(spec) = std::env::var("SKIFF_HOSTS_CHILD") else { return };
        let [file, dir, role]: [&str; 3] = spec.splitn(3, '|').collect::<Vec<_>>().try_into().unwrap();
        let (file, dir) = (Path::new(file), Path::new(dir));
        let id = |s: &str| HostId::try_from(s.to_string()).unwrap();
        std::fs::write(dir.join(format!("ready-{role}")), "").unwrap();
        while !dir.join("go").exists() {
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        let result = match role.split_once(':').unwrap() {
            ("pin", h) => pin_first(file, "devbox", &id(h)).map(drop),
            ("accept", h) => accept_identity(file, "devbox", &id(h)).map(drop),
            ("add", d) => update(file, |hosts| {
                hosts.hosts.push(KnownHost::new(d.into(), "/s.sock".into()));
                Ok(())
            }),
            _ => unreachable!(),
        };
        let out = match result {
            Ok(()) => "ok".to_string(),
            Err(e) if matches!(e.downcast_ref(), Some(skiff_host_api::Error::IdentityChanged { .. })) => "changed".into(),
            Err(e) => format!("error {e:#}"),
        };
        std::fs::write(dir.join(format!("out-{role}")), out).unwrap();
    }

    /// Runs `roles` in separate processes against one file, all released at
    /// once, and returns what each reported.
    fn race(name: &str, trial: usize, roles: &[&str]) -> (PathBuf, Vec<String>) {
        let dir = std::env::temp_dir().join(format!("skiff-hosts-race-{name}-{}-{trial}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hosts.toml");
        update(&file, |h| {
            h.hosts.push(KnownHost::new("devbox".into(), "/s.sock".into()));
            Ok(())
        })
        .unwrap();
        let exe = std::env::current_exe().unwrap();
        let children: Vec<_> = roles
            .iter()
            .map(|role| {
                std::process::Command::new(&exe)
                    .args(["--exact", "hosts::tests::store_child", "--ignored", "--test-threads=1"])
                    .env("SKIFF_HOSTS_CHILD", format!("{}|{}|{role}", file.display(), dir.display()))
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !roles.iter().all(|r| dir.join(format!("ready-{r}")).exists()) {
            assert!(std::time::Instant::now() < deadline, "children never got ready");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        std::fs::write(dir.join("go"), "").unwrap();
        for mut c in children {
            assert!(c.wait().unwrap().success());
        }
        let outs = roles.iter().map(|r| std::fs::read_to_string(dir.join(format!("out-{r}"))).unwrap()).collect();
        (dir, outs)
    }

    /// Residual P1: two app runs pin different first identities at once.
    /// Exactly one wins, the other is refused as a changed identity, and the
    /// file holds the winner's pin.
    #[test]
    fn a_first_pin_is_one_compare_and_set_across_processes() {
        for trial in 0..12 {
            let (dir, outs) = race("pin", trial, &["pin:A", "pin:B"]);
            let pin = load(&dir.join("hosts.toml")).unwrap().hosts[0].host_id.clone().unwrap();
            let winner = if pin == "A" { 0 } else { 1 };
            assert_eq!(outs[winner], "ok", "trial {trial}: {outs:?}, pin {pin}");
            assert_eq!(outs[1 - winner], "changed", "trial {trial}: both stored a first pin: {outs:?}, pin {pin}");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    /// Residual P1: a first pin and the person's accepted identity at once,
    /// beside other writers. The accepted identity is never overwritten by
    /// a first pin, and no writer's change is lost.
    #[test]
    fn identity_acceptance_and_other_writers_lose_no_update_across_processes() {
        for trial in 0..8 {
            let (dir, outs) = race("accept", trial, &["pin:A", "accept:C", "add:x", "add:y"]);
            let hosts = load(&dir.join("hosts.toml")).unwrap();
            assert_eq!(hosts.find("devbox").unwrap().host_id.as_deref(), Some("C"), "trial {trial}: {outs:?}");
            assert!(outs[0] == "ok" || outs[0] == "changed", "trial {trial}: {outs:?}");
            assert!(outs[1..].iter().all(|o| o == "ok"), "trial {trial}: {outs:?}");
            assert!(hosts.find("x").is_some() && hosts.find("y").is_some(), "trial {trial}: an added Host was lost");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
