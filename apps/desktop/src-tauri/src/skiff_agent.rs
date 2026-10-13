//! The Skiff agent's folder: one agent session that coordinates the others.
//! The app ships its template and the `skiff-cli` skill as resources. The
//! skill is copied out to a fixed place on every start, because the
//! AppImage mount and a moved Skiff.app change the resource path. The
//! folder links to that copy, so the skill updates with Skiff.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use skiff_core::config::{self, SkiffAgentKey};
use tauri::{AppHandle, Emitter, Manager};

use crate::config::PROJECTS_CHANGED;

/// The name until Settings has one. The app keeps the same default.
const DEFAULT_NAME: &str = "Skipper";

fn resources(app: &AppHandle) -> anyhow::Result<PathBuf> {
    app.path().resource_dir().context("find the app resources")
}

/// Where the app keeps its copy of the skill: next to `projects.toml`.
fn skill_home() -> anyhow::Result<PathBuf> {
    let config = skiff_core::config::config_path();
    let dir = config.parent().context("find the config folder")?;
    Ok(dir.join("skills").join("skiff-cli"))
}

/// Runs at app start, off the main thread. Never prompts.
pub fn sync(app: AppHandle) {
    if let Err(e) = resources(&app).and_then(|r| sync_skill(&r)) {
        eprintln!("skiff-cli skill: {e:#}");
    }
}

/// Replaces the copy of the skill when the shipped one differs. A new copy
/// is built beside the old one and renamed over it, so an agent that reads
/// the skill never sees half of it.
fn sync_skill(res: &Path) -> anyhow::Result<PathBuf> {
    let src = res.join("skills").join("skiff-cli");
    if !src.is_dir() {
        bail!("this build has no skiff-cli skill");
    }
    let dest = skill_home()?;
    if same_tree(&src, &dest) {
        return Ok(dest);
    }
    let parent = dest.parent().context("find the skills folder")?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let tmp = parent.join(".skiff-cli.new");
    let old = parent.join(".skiff-cli.old");
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&old);
    copy_tree(&src, &tmp)?;
    if dest.symlink_metadata().is_ok() {
        std::fs::rename(&dest, &old).with_context(|| format!("move {}", dest.display()))?;
    }
    std::fs::rename(&tmp, &dest).with_context(|| format!("replace {}", dest.display()))?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(dest)
}

/// True when `b` holds the same files with the same bytes as `a`.
fn same_tree(a: &Path, b: &Path) -> bool {
    let (Ok(x), Ok(y)) = (std::fs::read_dir(a), std::fs::read_dir(b)) else { return false };
    let names = |d: std::fs::ReadDir| {
        let mut v: Vec<_> = d.filter_map(|e| e.ok().map(|e| e.file_name())).collect();
        v.sort();
        v
    };
    let (x, y) = (names(x), names(y));
    x == y
        && x.iter().all(|n| {
            let (p, q) = (a.join(n), b.join(n));
            if p.is_dir() {
                q.is_dir() && same_tree(&p, &q)
            } else {
                std::fs::read(&p).ok().is_some_and(|s| std::fs::read(&q).ok().is_some_and(|d| d == s))
            }
        })
}

fn copy_tree(src: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    for e in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let e = e?;
        let to = dest.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &to)?;
        } else {
            // fs::copy keeps the mode, so the bundled scripts stay runnable.
            std::fs::copy(e.path(), &to).with_context(|| format!("copy {}", e.path().display()))?;
        }
    }
    Ok(())
}

fn expand_home(path: &str) -> anyhow::Result<PathBuf> {
    let path = path.trim();
    if path.is_empty() {
        bail!("type a folder");
    }
    let home = || std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set");
    Ok(match path.strip_prefix("~/") {
        Some(rest) => home()?.join(rest),
        None if path == "~" => home()?,
        None => PathBuf::from(path),
    })
}

/// Writes a new file. Refuses one that is there, so setup never overwrites.
fn write_new(path: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    f.write_all(text.as_bytes()).with_context(|| format!("write {}", path.display()))
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) -> anyhow::Result<()> {
    std::os::unix::fs::symlink(target, at).with_context(|| format!("link {}", at.display()))
}

#[cfg(not(unix))]
fn link(_target: &Path, _at: &Path) -> anyhow::Result<()> {
    bail!("not supported on this platform")
}

/// Writes the parts of the template that `dir` does not have yet. Any folder
/// will do; a file or link already there stays as it is, so a second run
/// keeps the user's edits and a folder set up before is used as it is.
///
/// ```text
/// AGENTS.md                    the instructions, with the agent's name
/// CLAUDE.md, GEMINI.md         links to AGENTS.md
/// .claude                      a link to .agents, for Claude's skills
/// .agents/skills/skiff-cli     a link to the app's copy of the skill
/// ledger.md                    the task ledger
/// results/                     long child reports
/// ```
fn setup(res: &Path, dir: &Path, name: &str) -> anyhow::Result<PathBuf> {
    if dir.metadata().is_ok_and(|m| !m.is_dir()) {
        bail!("{} is not a folder", dir.display());
    }
    let template = res.join("agent-template");
    let read = |f: &str| {
        let p = template.join(f);
        std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))
    };
    let agents = read("AGENTS.md.tmpl")?.replace("{{name}}", name);
    let ledger = read("ledger.md")?;
    let skill = sync_skill(res)?;

    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let dir = dir.canonicalize().with_context(|| format!("find {}", dir.display()))?;
    // A broken link counts as there: setup does not replace what the user made.
    let missing = |p: &Path| p.symlink_metadata().is_err();
    if missing(&dir.join("AGENTS.md")) {
        write_new(&dir.join("AGENTS.md"), &agents)?;
    }
    for f in ["CLAUDE.md", "GEMINI.md"] {
        if missing(&dir.join(f)) {
            link(Path::new("AGENTS.md"), &dir.join(f))?;
        }
    }
    if missing(&dir.join("ledger.md")) {
        write_new(&dir.join("ledger.md"), &ledger)?;
    }
    std::fs::create_dir_all(dir.join("results")).context("create results")?;
    let skills = dir.join(".agents").join("skills");
    std::fs::create_dir_all(&skills).context("create .agents/skills")?;
    if missing(&skills.join("skiff-cli")) {
        link(&skill, &skills.join("skiff-cli"))?;
    }
    let claude = dir.join(".claude");
    if missing(&claude) {
        link(Path::new(".agents"), &claude)?;
    } else if claude.is_dir() && !claude.symlink_metadata()?.is_symlink() {
        // The folder has its own .claude: Claude reads the skill from there.
        let at = claude.join("skills");
        std::fs::create_dir_all(&at).context("create .claude/skills")?;
        if missing(&at.join("skiff-cli")) {
            link(&skill, &at.join("skiff-cli"))?;
        }
    }
    Ok(dir)
}

/// Settings › Skiff agent › Folder. Writes the missing template files with the
/// name from Settings, then makes the folder the Skiff agent's, so its row
/// shows. Returns the folder, without `~`.
#[tauri::command]
pub async fn setup_skiff_agent(app: AppHandle, path: String) -> Result<String, String> {
    let handle = app.clone();
    let dir = blocking(move || {
        let dir = expand_home(&path)?;
        let name = config::load()?.skiff_agent.name.filter(|n| !n.trim().is_empty());
        let dir = setup(&resources(&app)?, &dir, name.as_deref().unwrap_or(DEFAULT_NAME))?;
        config::set_skiff_agent(SkiffAgentKey::Folder(Some(&dir)))?;
        Ok(dir.display().to_string())
    })
    .await?;
    let _ = handle.emit(PROJECTS_CHANGED, ());
    Ok(dir)
}

/// `[skiff_agent]` as the page reads it: the folder without `~`.
#[derive(Serialize)]
pub struct SkiffAgent {
    name: Option<String>,
    agent: Option<String>,
    folder: Option<String>,
    continue_on_restore: bool,
}

fn read() -> anyhow::Result<SkiffAgent> {
    let a = config::load()?.skiff_agent;
    Ok(SkiffAgent {
        name: a.name,
        agent: a.agent,
        folder: a.folder.map(|f| f.display().to_string()),
        continue_on_restore: a.continue_on_restore,
    })
}

/// One edit from Settings › Skiff agent. `null` or empty takes the default.
#[derive(Deserialize)]
#[serde(tag = "key", content = "value", rename_all = "snake_case")]
pub enum Edit {
    Name(Option<String>),
    Agent(Option<String>),
    Folder(Option<String>),
    ContinueOnRestore(bool),
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn get_skiff_agent() -> Result<SkiffAgent, String> {
    blocking(read).await
}

/// Saves one setting and answers with all of them. The page reloads its
/// projects on the event, so the row follows at once.
#[tauri::command]
pub async fn set_skiff_agent(app: AppHandle, edit: Edit) -> Result<SkiffAgent, String> {
    let out = blocking(move || {
        match edit {
            Edit::Name(v) => config::set_skiff_agent(SkiffAgentKey::Name(v.as_deref()))?,
            Edit::Agent(v) => config::set_skiff_agent(SkiffAgentKey::Agent(v.as_deref()))?,
            Edit::ContinueOnRestore(on) => config::set_skiff_agent(SkiffAgentKey::ContinueOnRestore(on))?,
            Edit::Folder(v) => {
                let dir = match v.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                    None => None,
                    Some(v) => {
                        let dir = expand_home(v)?;
                        if !dir.is_dir() {
                            bail!("{} is not a folder", dir.display());
                        }
                        Some(dir.canonicalize().with_context(|| format!("find {}", dir.display()))?)
                    }
                };
                config::set_skiff_agent(SkiffAgentKey::Folder(dir.as_deref()))?
            }
        }
        read()
    })
    .await?;
    let _ = app.emit(PROJECTS_CHANGED, ());
    Ok(out)
}

/// Starts a git repository in the agent's folder, even inside another
/// repository, such as a home folder kept in git. The project must be the
/// folder itself. No files are staged and nothing is committed.
#[tauri::command]
pub async fn init_skiff_agent_repository(path: PathBuf) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        if path.join(".git").exists() {
            return Ok(());
        }
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&path)
            .arg("init")
            .output()
            .context("run git")?;
        if !out.status.success() {
            bail!("git init: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e: anyhow::Error| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_template_and_fills_in_only_what_is_missing() {
        let tmp = std::env::temp_dir().join(format!("skiff-agent-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // The resources as tauri.conf.json lays them out.
        let src = Path::new(env!("CARGO_MANIFEST_DIR"));
        let res = tmp.join("res");
        copy_tree(&src.join("agent-template"), &res.join("agent-template")).unwrap();
        copy_tree(&src.join("../../../.agents/skills/skiff-cli"), &res.join("skills/skiff-cli")).unwrap();
        std::env::set_var("SKIFF_CONFIG", tmp.join("config/projects.toml"));
        let dir = setup(&res, &tmp.join("agent"), "Skipper").unwrap();
        let agents = std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
        assert!(agents.contains("You are Skipper, the Skiff agent."));
        assert_eq!(std::fs::read_to_string(dir.join("GEMINI.md")).unwrap(), agents);
        assert!(dir.join(".claude/skills/skiff-cli/SKILL.md").is_file());
        assert!(dir.join("ledger.md").is_file() && dir.join("results").is_dir());
        // A second run fills in what is missing and keeps the user's edits.
        std::fs::write(dir.join("AGENTS.md"), "mine").unwrap();
        std::fs::remove_file(dir.join("ledger.md")).unwrap();
        setup(&res, &dir, "Skipper").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(), "mine");
        assert!(dir.join("ledger.md").is_file());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
