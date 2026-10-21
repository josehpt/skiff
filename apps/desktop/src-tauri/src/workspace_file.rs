//! Workspace files (`.skiffw`, #154): a named view that shows some projects
//! and fleets in the tree and filters the Ledger to their work. The file
//! holds references only. It never holds a project's settings, a fleet's
//! members, a session or a ledger row. The app reads and writes it here; the
//! page owns the document (path, edits, baseline).
//!
//! Format, version 1 (TOML):
//!
//! ```toml
//! version = 1
//! name = "Client work"
//! projects = ["/home/me/code/api"]   # project folders, absolute
//! fleets = ["Clients"]               # fleet names, as in projects.toml
//!
//! [view]                             # optional
//! folded_projects = ["/home/me/code/api"]
//! folded_fleets = ["Clients"]
//! selected = "/home/me/code/api"     # the last selected included project
//! ```
//!
//! Rules:
//! - A project is named by its folder, so a new display name keeps it in.
//!   Paths are absolute. A trailing `/` is dropped; nothing else is changed.
//!   A moved project needs an explicit repair in the app.
//! - A fleet is named by its name: fleets have no other id. A fleet includes
//!   the projects in it now.
//! - Empty `projects` and `fleets` mean an empty view, never all projects.
//! - Unknown keys and any other `version` are refused, so an older Skiff
//!   never shows a newer file wrong. A later version adds `host = "<host id>"`
//!   (epic #155 §5): the projects then live on that Host. A version 1 file
//!   means the local Host.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

/// The version this Skiff reads and writes.
const VERSION: i64 = 1;

/// The file extension, without the dot.
pub(crate) const EXT: &str = "skiffw";

/// The page opens a workspace file on this event (an OS open request).
const OPEN_EVENT: &str = "skiff:open-workspace";

/// The profile as the page holds it. Lists are kept in file order.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub(crate) struct Profile {
    pub(crate) name: String,
    pub(crate) projects: Vec<String>,
    pub(crate) fleets: Vec<String>,
    pub(crate) folded_projects: Vec<String>,
    pub(crate) folded_fleets: Vec<String>,
    pub(crate) selected: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileV1 {
    version: i64,
    name: String,
    #[serde(default)]
    projects: Vec<String>,
    #[serde(default)]
    fleets: Vec<String>,
    #[serde(default, skip_serializing_if = "View::is_empty")]
    view: View,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct View {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    folded_projects: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    folded_fleets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selected: Option<String>,
}

impl View {
    fn is_empty(&self) -> bool {
        self.folded_projects.is_empty() && self.folded_fleets.is_empty() && self.selected.is_none()
    }
}

/// A project path as the file keeps it: absolute, no trailing `/`.
fn project_path(p: &str) -> Result<String, String> {
    let t = p.trim();
    if !Path::new(t).is_absolute() {
        return Err(format!("project \"{p}\" is not an absolute path"));
    }
    let t = t.trim_end_matches('/');
    Ok(if t.is_empty() { "/".into() } else { t.into() })
}

fn paths(v: Vec<String>) -> Result<Vec<String>, String> {
    v.iter().map(|p| project_path(p)).collect()
}

pub(crate) fn parse(text: &str) -> Result<Profile, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| format!("not a valid workspace file: {}", e.message()))?;
    match table.get("version").map(|v| v.as_integer()) {
        None => return Err("not a workspace file: it has no version".into()),
        Some(Some(VERSION)) => {}
        Some(Some(v)) => return Err(format!("workspace file version {v} is not supported. This Skiff reads version {VERSION}.")),
        Some(None) => return Err("not a workspace file: version is not a number".into()),
    }
    let f: FileV1 = toml::from_str(text).map_err(|e| format!("not a valid workspace file: {}", e.message()))?;
    Ok(Profile {
        name: f.name.trim().to_string(),
        projects: paths(f.projects)?,
        fleets: f.fleets,
        folded_projects: paths(f.view.folded_projects)?,
        folded_fleets: f.view.folded_fleets,
        selected: f.view.selected.map(|s| project_path(&s)).transpose()?,
    })
}

pub(crate) fn serialize(p: &Profile) -> Result<String, String> {
    let f = FileV1 {
        version: VERSION,
        name: p.name.trim().to_string(),
        projects: paths(p.projects.clone())?,
        fleets: p.fleets.clone(),
        view: View {
            folded_projects: paths(p.folded_projects.clone())?,
            folded_fleets: p.folded_fleets.clone(),
            selected: p.selected.as_deref().map(project_path).transpose()?,
        },
    };
    let body = toml::to_string(&f).map_err(|e| e.to_string())?;
    Ok(format!("# Skiff workspace. It names projects and fleets; it holds no sessions.\n{body}"))
}

/// What the page compares to see an outside change: the length and a hash of
/// the bytes. Only compared within one run.
fn revision(bytes: &[u8]) -> String {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{}-{:016x}", bytes.len(), h.finish())
}

/// The file's revision, `None` when it is not there. Another failure is an error.
fn current_revision(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(revision(&b))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

#[derive(Serialize)]
pub(crate) struct Loaded {
    path: String,
    profile: Profile,
    revision: String,
}

/// Reads and checks a workspace file. Changes nothing.
#[tauri::command]
pub(crate) async fn workspace_read(path: String) -> Result<Loaded, String> {
    let file = PathBuf::from(&path);
    let bytes = std::fs::read(&file).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{path} is not there"),
        _ => format!("cannot read {path}: {e}"),
    })?;
    let text = String::from_utf8(bytes.clone()).map_err(|_| format!("{path} is not a text file"))?;
    let profile = parse(&text).map_err(|e| format!("{path}: {e}"))?;
    Ok(Loaded { path, profile, revision: revision(&bytes) })
}

/// The file's revision now, `None` when it is gone. An error when it cannot be read.
#[tauri::command]
pub(crate) async fn workspace_revision(path: String) -> Result<Option<String>, String> {
    current_revision(Path::new(&path))
}

/// Writes the profile to `path` atomically and gives its new revision.
///
/// `expect` is the revision the page last saw there: `None` when it expects
/// no file. A different file on disk fails with `conflict:` and writes
/// nothing. The old file stays whole on every failure.
#[tauri::command]
pub(crate) async fn workspace_write(path: String, profile: Profile, expect: Option<String>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || write(Path::new(&path), &profile, expect.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

fn write(path: &Path, profile: &Profile, expect: Option<&str>) -> Result<String, String> {
    if path.extension().and_then(|e| e.to_str()) != Some(EXT) {
        return Err(format!("a workspace file ends in .{EXT}"));
    }
    let text = serialize(profile)?;
    let now = current_revision(path)?;
    if now.as_deref() != expect {
        return Err(format!("conflict: {} changed on disk", path.display()));
    }
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let done = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        // A last look: the file must still be what the page saw.
        if current_revision(path).ok().flatten().as_deref() != expect {
            return Err(std::io::Error::other("changed on disk"));
        }
        std::fs::rename(&tmp, path)?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if let Err(e) = done {
        let _ = std::fs::remove_file(&tmp);
        if e.to_string() == "changed on disk" {
            return Err(format!("conflict: {} changed on disk", path.display()));
        }
        return Err(format!("cannot save {}: {e}", path.display()));
    }
    Ok(revision(text.as_bytes()))
}

/// Workspace files the OS asked the app to open. Held until the page asks.
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A workspace path from a command-line argument or a `file://` URL.
fn as_workspace(arg: &str) -> Option<String> {
    let raw = arg.strip_prefix("file://").map(percent_decode).unwrap_or_else(|| arg.to_string());
    let p = PathBuf::from(&raw);
    if p.extension().and_then(|e| e.to_str()) != Some(EXT) {
        return None;
    }
    let abs = if p.is_absolute() { p } else { std::env::current_dir().ok()?.join(p) };
    Some(abs.to_string_lossy().into_owned())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = s.get(i + 1..i + 3).and_then(|x| u8::from_str_radix(x, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The workspace file the app was started with, else one the OS sent before
/// the page was ready. Each is given once.
#[tauri::command]
pub(crate) fn workspace_launch_file() -> Option<String> {
    static ARGS_TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !ARGS_TAKEN.swap(true, std::sync::atomic::Ordering::Relaxed) {
        if let Some(p) = std::env::args().skip(1).find_map(|a| as_workspace(&a)) {
            return Some(p);
        }
    }
    PENDING.lock().ok()?.pop()
}

/// The OS asks to open files (macOS: a double-click in Finder). The page
/// takes a workspace through its unsaved-changes guard.
#[allow(dead_code)]
pub(crate) fn os_open(app: &AppHandle, urls: impl IntoIterator<Item = String>) {
    for u in urls {
        let Some(p) = as_workspace(&u) else { continue };
        if let Ok(mut q) = PENDING.lock() {
            q.push(p.clone());
        }
        let _ = app.emit(OPEN_EVENT, p);
    }
}

/// The page took this path from the event: it is no longer pending.
#[tauri::command]
pub(crate) fn workspace_took(path: String) {
    if let Ok(mut q) = PENDING.lock() {
        q.retain(|p| p != &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Profile {
        Profile {
            name: "Client work".into(),
            projects: vec!["/home/me/api".into()],
            fleets: vec!["Clients".into()],
            folded_projects: vec!["/home/me/api".into()],
            folded_fleets: vec![],
            selected: Some("/home/me/api".into()),
        }
    }

    #[test]
    fn round_trip() {
        let p = sample();
        assert_eq!(parse(&serialize(&p).unwrap()).unwrap(), p);
        let empty = Profile { name: "e".into(), ..Default::default() };
        let text = serialize(&empty).unwrap();
        assert!(!text.contains("[view]"));
        assert_eq!(parse(&text).unwrap(), empty);
    }

    #[test]
    fn refuses_other_versions_and_keys() {
        assert!(parse("version = 2\nname = \"x\"\n").unwrap_err().contains("version 2 is not supported"));
        assert!(parse("name = \"x\"\n").unwrap_err().contains("no version"));
        assert!(parse("version = 1\nname = \"x\"\nhost = \"h\"\n").is_err());
        assert!(parse("version = 1\nname = \"x\"\nprojects = [\"rel/path\"]\n").unwrap_err().contains("absolute"));
        assert!(parse("version = 1\nname = ").is_err());
    }

    #[test]
    fn trailing_slash_dropped() {
        let p = parse("version = 1\nname = \"x\"\nprojects = [\"/a/b/\"]\n").unwrap();
        assert_eq!(p.projects, vec!["/a/b".to_string()]);
    }

    #[test]
    fn write_checks_revision() {
        let dir = std::env::temp_dir().join(format!("skiffw-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.skiffw");
        let _ = std::fs::remove_file(&file);
        let r1 = write(&file, &sample(), None).unwrap();
        // Expecting no file when one is there: refused, the file stays.
        assert!(write(&file, &sample(), None).unwrap_err().starts_with("conflict"));
        std::fs::write(&file, "version = 1\nname = \"other\"\n").unwrap();
        assert!(write(&file, &sample(), Some(&r1)).unwrap_err().starts_with("conflict"));
        assert!(std::fs::read_to_string(&file).unwrap().contains("other"));
        let r2 = current_revision(&file).unwrap();
        write(&file, &sample(), r2.as_deref()).unwrap();
        assert!(write(&dir.join("a.toml"), &sample(), None).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn launch_args() {
        assert_eq!(as_workspace("/x/y.skiffw").as_deref(), Some("/x/y.skiffw"));
        assert_eq!(as_workspace("file:///x/a%20b.skiffw").as_deref(), Some("/x/a b.skiffw"));
        assert!(as_workspace("/x/y.toml").is_none());
    }
}
