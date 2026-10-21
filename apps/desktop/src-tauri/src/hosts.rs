//! Known Hosts and which one this window shows: client-owned `hosts.toml`
//! beside `projects.toml`. "This Mac" is the local Host and needs no entry.
//! A remote Host is an SSH destination (an alias from ssh config) and the
//! skiffd socket on it. Each remote Host has its own clipboard opt-in for
//! OSC 52 writes from its terminals.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use skiff_client::host::{
    local::HostSelection,
    ssh::{SshOptions, SshTarget},
};
use tauri::State;

use crate::connection::{err, App};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct KnownHost {
    /// What `ssh` takes: an alias from ssh config, `host` or `user@host`.
    pub(crate) destination: String,
    /// skiffd's socket on that Host, as `skiff info` prints it there.
    pub(crate) socket: String,
    /// OSC 52 clipboard writes from this Host's terminals reach this Mac.
    #[serde(default)]
    pub(crate) clipboard: bool,
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

/// The forward's private directory: short, as a socket path must be.
fn forward_dir(destination: &str) -> PathBuf {
    let hash = destination.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    // SAFETY: geteuid takes no arguments and cannot fail.
    std::env::temp_dir().join(format!("skiff-{}-{:08x}", unsafe { libc::geteuid() }, hash as u32))
}

pub(crate) fn remote(h: &KnownHost) -> Result<HostSelection, String> {
    let target = SshTarget { destination: h.destination.clone(), socket: h.socket.clone() };
    HostSelection::ssh(target, SshOptions::default(), forward_dir(&h.destination))
}

/// What the page shows and offers: the selected Host, and the known ones.
#[derive(Serialize)]
pub(crate) struct HostList {
    /// None: this Mac.
    selected: Option<String>,
    hosts: Vec<KnownHost>,
}

#[tauri::command]
pub(crate) async fn list_hosts(app: State<'_, App>) -> Result<HostList, String> {
    let hosts = blocking(|| load(&path())).await?;
    let selection = app.selection();
    Ok(HostList { selected: selection.ssh_target().map(|t| t.destination.clone()), hosts: hosts.hosts })
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
            None => hosts.hosts.push(KnownHost { destination, socket, clipboard: false }),
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
pub(crate) async fn select_host(app: State<'_, App>, destination: Option<String>) -> Result<(), String> {
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
    Ok(())
}

/// Allows or stops OSC 52 clipboard writes from a remote Host's terminals.
#[tauri::command]
pub(crate) async fn set_host_clipboard(destination: String, allow: bool) -> Result<(), String> {
    blocking(move || {
        let path = path();
        let mut hosts = load(&path)?;
        let h = hosts.hosts.iter_mut().find(|h| h.destination == destination).ok_or_else(|| anyhow::anyhow!("{destination} is not a known Host"))?;
        h.clipboard = allow;
        save(&path, &hosts)
    })
    .await
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
            hosts: vec![KnownHost { destination: "devbox".into(), socket: "/run/user/1000/skiff/skiffd.sock".into(), clipboard: false }],
        };
        save(&file, &hosts).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("selected = \"devbox\"") && text.contains("[[host]]"), "{text}");
        assert_eq!(load(&file).unwrap(), hosts);
        let local = || HostSelection::new("/l.sock".into(), "/id".into());
        let picked = hosts.selection(local);
        assert_eq!(picked.ssh_target().map(|t| t.destination.as_str()), Some("devbox"));
        assert!(picked.process_control().is_none());
        // A forward path short enough for a Unix socket.
        assert!(picked.socket().as_os_str().len() < 100, "{}", picked.socket().display());
        let gone = Hosts { selected: Some("gone".into()), ..hosts.clone() };
        assert!(gone.selection(local).is_local());
        let bad = Hosts { hosts: vec![KnownHost { destination: "-oProxyCommand=x".into(), ..hosts.hosts[0].clone() }], selected: Some("-oProxyCommand=x".into()) };
        assert!(bad.selection(local).is_local(), "an invalid entry never becomes an endpoint");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
