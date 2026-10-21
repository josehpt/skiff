//! Known Hosts and which one this window shows: client-owned `hosts.toml`
//! beside `projects.toml`. "This Mac" is the local Host and needs no entry.
//! A remote Host is an SSH destination (an alias from ssh config) and the
//! skiffd socket on it. Each remote Host keeps the Skiff identity it first
//! reported (`host_id`): a different one is refused on every later
//! connection, after a new selection or app start too, until the person
//! accepts it. Each remote Host has its own clipboard opt-in for OSC 52
//! writes from its terminals, given to that identity only.

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

    /// Clipboard writes are allowed for the identity this Host must have.
    pub(crate) fn clipboard(&self) -> bool {
        self.host_id.is_some() && self.clipboard_host == self.host_id
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

/// A complete file or none: written beside, then renamed over.
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

/// Stores the identity a Host reported on its first accepted connection.
/// The file decides: when another window or run stored a different one
/// meanwhile, this one is refused as a changed identity.
pub(crate) fn pin_first(file: &Path, destination: &str, host_id: &HostId) -> anyhow::Result<()> {
    let mut hosts = load(file)?;
    let h = hosts.hosts.iter_mut().find(|h| h.destination == destination).ok_or_else(|| anyhow::anyhow!("{destination} is not a known Host"))?;
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
            save(file, &hosts)
        }
    }
}

/// Makes `host_id` the identity `destination` must report, as the person
/// accepted it. The clipboard permission of the old identity goes.
pub(crate) fn accept_identity(file: &Path, destination: &str, host_id: &HostId) -> anyhow::Result<Hosts> {
    let mut hosts = load(file)?;
    let h = hosts.hosts.iter_mut().find(|h| h.destination == destination).ok_or_else(|| anyhow::anyhow!("{destination} is not a known Host"))?;
    h.host_id = Some(host_id.as_str().to_string());
    h.clipboard_host = None;
    save(file, &hosts)?;
    Ok(hosts)
}

/// A known Host as the page sees it.
#[derive(Serialize)]
struct HostView {
    destination: String,
    socket: String,
    /// OSC 52 writes from this Host reach this Mac: allowed for its identity.
    clipboard: bool,
}

/// What the page shows and offers: the selected Host, and the known ones.
#[derive(Serialize)]
pub(crate) struct HostList {
    /// None: this Mac.
    selected: Option<String>,
    hosts: Vec<HostView>,
}

#[tauri::command]
pub(crate) async fn list_hosts(app: State<'_, App>) -> Result<HostList, String> {
    let hosts = blocking(|| load(&path())).await?;
    let selection = app.selection();
    let hosts = hosts.hosts.into_iter().map(|h| HostView { clipboard: h.clipboard(), destination: h.destination, socket: h.socket }).collect();
    Ok(HostList { selected: selection.ssh_target().map(|t| t.destination.clone()), hosts })
}

/// Adds or updates a remote Host. It does not connect.
#[tauri::command]
pub(crate) async fn add_host(destination: String, socket: String) -> Result<(), String> {
    let destination = destination.trim().to_string();
    let socket = socket.trim().to_string();
    SshTarget { destination: destination.clone(), socket: socket.clone() }.validate()?;
    blocking(move || {
        let path = path();
        let mut hosts = load(&path)?;
        match hosts.hosts.iter_mut().find(|h| h.destination == destination) {
            Some(h) => h.socket = socket,
            None => hosts.hosts.push(KnownHost::new(destination, socket)),
        }
        save(&path, &hosts)
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
        let path = path();
        let mut hosts = load(&path)?;
        hosts.hosts.retain(|h| h.destination != destination);
        save(&path, &hosts)
    })
    .await
}

/// Shows another Host in this window: the old connection, its streams and
/// any held input go first, so nothing typed for one Host reaches another.
/// The page reloads after it. `None`: this Mac.
#[tauri::command]
pub(crate) async fn select_host(handle: tauri::AppHandle, app: State<'_, App>, destination: Option<String>) -> Result<(), String> {
    let selection = blocking(move || {
        let path = path();
        let mut hosts = load(&path)?;
        if let Some(d) = &destination {
            anyhow::ensure!(hosts.find(d).is_some(), "{d} is not a known Host");
        }
        hosts.selected = destination;
        save(&path, &hosts)?;
        Ok(hosts.selection(crate::connection::local_selection))
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
/// Allowed for the identity the Host must report, so a Host that answers
/// with another identity later has no permission.
#[tauri::command]
pub(crate) async fn set_host_clipboard(destination: String, allow: bool) -> Result<(), String> {
    blocking(move || {
        let path = path();
        let mut hosts = load(&path)?;
        let h = hosts.hosts.iter_mut().find(|h| h.destination == destination).ok_or_else(|| anyhow::anyhow!("{destination} is not a known Host"))?;
        h.clipboard_host = match (allow, &h.host_id) {
            (false, _) => None,
            (true, Some(id)) => Some(id.clone()),
            (true, None) => anyhow::bail!("Skiff has not connected to {destination} yet. Connect once, then allow clipboard writes."),
        };
        save(&path, &hosts)
    })
    .await
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

        // Clipboard needs an identity, and is given to that one only.
        let mut granted = hosts.clone();
        granted.hosts[0].clipboard_host = Some("A".into());
        save(&file, &granted).unwrap();
        assert!(load(&file).unwrap().hosts[0].clipboard());
        let accepted = accept_identity(&file, "devbox", &id("B")).unwrap();
        assert_eq!(accepted.hosts[0].host_id.as_deref(), Some("B"));
        assert!(!accepted.hosts[0].clipboard(), "the old identity's permission is gone");
        assert_eq!(load(&file).unwrap().selection(local).pinned(), Some(id("B")));
        // A file whose permission names another identity allows nothing.
        let foreign = KnownHost { host_id: Some("B".into()), clipboard_host: Some("A".into()), ..KnownHost::new("x".into(), "/s".into()) };
        assert!(!foreign.clipboard());
        // The old bool key from before pinning is ignored, not trusted.
        std::fs::write(&file, "[[host]]\ndestination = \"devbox\"\nsocket = \"/s.sock\"\nclipboard = true\n").unwrap();
        assert!(!load(&file).unwrap().hosts[0].clipboard());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
