use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use super::types::Config;

/// `SKIFF_CONFIG` overrides. Otherwise `<config dir>/skiff/projects.toml`.
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("SKIFF_CONFIG") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("skiff")
        .join("projects.toml")
}

/// Held across each read-modify-write of the file, so two edits at once do
/// not drop one another.
static EDIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The edit lock: the mutex for threads in this process, and a lock on
/// `projects.toml.lock` for other processes, such as the app and the `skiff` CLI.
pub(super) struct EditLock {
    _file: Option<std::fs::File>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

pub(super) fn edit_lock() -> EditLock {
    let guard = EDIT.lock().unwrap_or_else(|e| e.into_inner());
    let mut path = config_path().into_os_string();
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
    let _edit = edit_lock();
    let file = config_path();
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
