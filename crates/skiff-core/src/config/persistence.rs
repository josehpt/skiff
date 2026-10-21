use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use super::types::Config;

/// `SKIFF_CONFIG` overrides. Otherwise `<config dir>/skiff/projects.toml`,
/// or the socket's private copy (see [`crate::state`]).
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("SKIFF_CONFIG") {
        return PathBuf::from(p);
    }
    match crate::state::private_dir() {
        Some(d) => d.join("config").join("projects.toml"),
        None => crate::state::real_config_path(),
    }
}

/// Held across each read-modify-write of the file, so two edits at once do
/// not drop one another.
static EDIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    /// The files whose edit lock this thread holds, outermost first. A nested
    /// edit of the same file, such as one checked against a revision, runs
    /// under the outer lock.
    static HELD: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The edit lock: the mutex for threads in this process, and a lock on
/// `projects.toml.lock` for other processes, such as the app and the `skiff` CLI.
pub(super) struct EditLock {
    _file: Option<std::fs::File>,
    _guard: Option<std::sync::MutexGuard<'static, ()>>,
}

impl Drop for EditLock {
    fn drop(&mut self) {
        HELD.with(|h| h.borrow_mut().pop());
    }
}

pub(super) fn edit_lock() -> EditLock {
    edit_lock_for(&config_path())
}

/// The edit lock, with the lock file beside `file`. Taken again on a thread
/// that holds it for `file`, it is already held; for another file, that
/// file's lock is taken under the mutex this thread already holds.
pub(super) fn edit_lock_for(file: &Path) -> EditLock {
    let (same, nested) = HELD.with(|h| {
        let h = h.borrow();
        (h.iter().any(|f| f == file), !h.is_empty())
    });
    HELD.with(|h| h.borrow_mut().push(file.to_path_buf()));
    if same {
        return EditLock { _file: None, _guard: None };
    }
    let guard = (!nested).then(|| EDIT.lock().unwrap_or_else(|e| e.into_inner()));
    let mut path = file.as_os_str().to_owned();
    path.push(".lock");
    // Without the lock file, edits still exclude each other inside this process.
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .ok()
        .filter(|f| f.lock().is_ok());
    EditLock { _file: file, _guard: guard }
}

/// The `[[project]]` tables changed after they were read: a Host write
/// made against that read is refused, not applied over the change.
#[derive(Debug)]
pub struct Stale;

impl std::fmt::Display for Stale {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("The projects changed on this Host since they were read.")
    }
}
impl std::error::Error for Stale {}

/// Why a project edit was refused, by kind, so the Host can answer with a
/// typed error. Its text is the message the person reads.
#[derive(Debug)]
pub enum Refused {
    /// The value is not one the key takes.
    Invalid(String),
    /// No project has that name.
    NoProject(String),
    /// The name or folder is already a project.
    Taken(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Taken(m) => f.write_str(m),
            Self::NoProject(name) => write!(f, "no such project: {name}"),
        }
    }
}
impl std::error::Error for Refused {}

/// The projects as read, as a short token: equal tokens, equal `[[project]]`
/// tables. The rest of the file (keys, appearance, agents) does not count.
pub fn projects_revision(projects: &[super::types::ProjectConfig]) -> String {
    // FNV-1a: the same token from every build, so a restarted service agrees.
    let bytes = serde_json::to_vec(projects).unwrap_or_default();
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3));
    format!("{hash:016x}")
}

/// Longest name, fleet or theme id a project takes, in characters.
pub const LABEL_MAX: usize = 64;

/// A one-line label: trimmed, at most [`LABEL_MAX`] characters, no control characters.
pub fn check_label<'a>(what: &str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    if value.chars().count() > LABEL_MAX || value.chars().any(char::is_control) {
        return Err(Refused::Invalid(format!("the {what} takes one line of at most {LABEL_MAX} characters")).into());
    }
    Ok(value)
}

/// Runs `edit` under the edit lock, only if the `[[project]]` tables are
/// still at `revision`; else [`Stale`] and nothing is written. Answers what
/// `edit` returns and the revision after it.
pub fn edit_projects<T>(revision: &str, edit: impl FnOnce() -> Result<T>) -> Result<(T, String)> {
    let _edit = edit_lock();
    if projects_revision(&load()?.projects) != revision {
        return Err(Stale.into());
    }
    let out = edit()?;
    Ok((out, projects_revision(&load()?.projects)))
}

/// Writes a temp file beside the target and renames it over, so a reader
/// never sees a half-written file. Writes through a symlink to its target.
pub fn write_atomic(file: &Path, text: &str) -> Result<()> {
    let file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = file.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &file).with_context(|| format!("write {}", file.display()))
}

/// A missing file is an empty config, not an error.
pub fn load() -> Result<Config> {
    load_from(&config_path())
}

pub fn load_from(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    parse(&text).with_context(|| format!("parse {}", path.display()))
}

pub fn parse(text: &str) -> Result<Config> {
    let mut cfg: Config = toml::from_str(text)?;
    for p in &mut cfg.projects {
        p.path = expand_home(&p.path);
    }
    if let Some(f) = &mut cfg.skiff_agent.folder {
        *f = expand_home(f);
    }
    Ok(cfg)
}

/// Opens a top-level table in the config file, creating it above the projects.
pub(super) fn edit_table(name: &str, f: impl FnOnce(&mut toml_edit::Table) -> Result<()>) -> Result<()> {
    edit_table_in(&config_path(), name, f)
}

/// [`edit_table`] in another file of the same format, under its own lock.
pub(super) fn edit_table_in(file: &Path, name: &str, f: impl FnOnce(&mut toml_edit::Table) -> Result<()>) -> Result<()> {
    let _edit = edit_lock_for(file);
    let file = file.to_path_buf();
    let text = if file.exists() {
        std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?
    } else {
        String::new()
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parse {}", file.display()))?;
    let created = !doc.contains_key(name);
    let item = doc
        .entry(name)
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let table = item
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`{name}` in {} is not a table", file.display()))?;
    f(table)?;
    if created {
        // A new table goes above the [[project]] list, not after it.
        table.set_position(Some(-1));
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(&file, &doc.to_string())
}

/// `~/x` for paths under the home directory, so the file moves between machines.
pub(super) fn home_relative(p: &Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

pub(super) fn expand_home(p: &Path) -> PathBuf {
    let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("~/")) else {
        return p.to_path_buf();
    };
    match dirs::home_dir() {
        Some(home) => home.join(rest),
        None => p.to_path_buf(),
    }
}
