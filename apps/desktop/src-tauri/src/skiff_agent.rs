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

/// The marker setup leaves, so a later run knows the folder is the agent's.
const MARKER: &str = ".skiff-agent";

/// The agent's folder: `~/skiff-agent`, or the override in projects.toml when
/// it passes [`check`]. The folder is fixed, so its conversation and ledger
/// never move.
fn folder(cfg: &config::Config) -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")?;
    let Some(dir) = cfg.skiff_agent.folder.clone() else { return Ok(home.join("skiff-agent")) };
    check(&dir, &home, cfg)?;
    Ok(dir)
}

/// Refuses a folder whose instructions would reach other sessions: an agent
/// reads `CLAUDE.md` in every folder above its own, and `AGENTS.md` up to its
/// repository's root. So the folder must not be the home, the root, the config
/// folder, a project, or a folder above or inside one.
fn check(dir: &Path, home: &Path, cfg: &config::Config) -> anyhow::Result<()> {
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let d = real(dir);
    let shown = dir.display();
    if !dir.is_absolute() || d.parent().is_none() || d == real(home) {
        bail!("[skiff_agent] folder = \"{shown}\" is not allowed. Use a folder of its own, such as ~/skiff-agent");
    }
    if let Some(c) = config::config_path().parent().map(real) {
        if d.starts_with(&c) {
            bail!("[skiff_agent] folder = \"{shown}\" is in Skiff's config folder. Use a folder of its own");
        }
    }
    for p in &cfg.projects {
        let pr = real(&p.path);
        if d.starts_with(&pr) || pr.starts_with(&d) {
            bail!("[skiff_agent] folder = \"{shown}\" mixes with the project {}. Use a folder of its own", p.name);
        }
    }
    Ok(())
}

/// Writes the parts of the template that `dir` does not have yet. The folder
/// must be new, empty, or one this setup made before (it has the marker), so
/// setup never mixes the agent's files with other files. A second run keeps
/// the user's edits and puts back what is missing. Nothing is written outside
/// the folder.
///
/// ```text
/// AGENTS.md                    the instructions, with the agent's name
/// CLAUDE.md, GEMINI.md         links to AGENTS.md
/// .claude                      a link to .agents, for Claude's skills
/// .agents/skills/skiff-cli     a link to the app's copy of the skill
/// results/                     long child reports
/// ```
fn setup(res: &Path, dir: &Path, name: &str) -> anyhow::Result<PathBuf> {
    if dir.metadata().is_ok_and(|m| !m.is_dir()) {
        bail!("{} is not a folder", dir.display());
    }
    if dir.is_dir() && !dir.join(MARKER).exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!("{} holds other files. Move them, or set another folder in projects.toml: [skiff_agent] folder", dir.display());
    }
    let template = res.join("agent-template");
    let read = |f: &str| {
        let p = template.join(f);
        std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))
    };
    let agents = read("AGENTS.md.tmpl")?.replace("{{name}}", name);
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
    std::fs::create_dir_all(dir.join("results")).context("create results")?;
    let skills = dir.join(".agents").join("skills");
    std::fs::create_dir_all(&skills).context("create .agents/skills")?;
    if missing(&skills.join("skiff-cli")) {
        link(&skill, &skills.join("skiff-cli"))?;
    }
    if missing(&dir.join(".claude")) {
        link(Path::new(".agents"), &dir.join(".claude"))?;
    }
    if missing(&dir.join(MARKER)) {
        write_new(&dir.join(MARKER), "1\n")?;
    }
    Ok(dir)
}

/// Makes the agent's folder ready: writes the template parts it lacks, with
/// the name from Settings. Settings runs it when the user turns the agent on,
/// and the row before it starts a session. Returns the folder.
fn ensure(app: &AppHandle) -> anyhow::Result<PathBuf> {
    let cfg = config::load()?;
    let name = cfg.skiff_agent.name.clone().filter(|n| !n.trim().is_empty());
    setup(&resources(app)?, &folder(&cfg)?, name.as_deref().unwrap_or(DEFAULT_NAME))
}

#[tauri::command]
pub async fn ensure_skiff_agent(app: AppHandle) -> Result<String, String> {
    blocking(move || Ok(ensure(&app)?.display().to_string())).await
}

/// The `skiff` CLI beside the app binary, else the one on `PATH`.
fn cli() -> PathBuf {
    std::env::current_exe()
        .ok()
        .map(|e| e.with_file_name("skiff"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("skiff"))
}

/// Starts `skiff watch` for the agent's session, unless one runs for this
/// daemon. The watcher is its own process, out of the app's process group: it
/// outlives an app restart, connects again after a daemon reload, and stops
/// when the session ends. Its lock beside the socket keeps it to one copy, so
/// the page can call this on every start and every restore.
fn start_watch(session: &str) -> anyhow::Result<bool> {
    let socket = skiff_core::socket::socket_path();
    let lock = socket.with_extension("watch.lock");
    if let Ok(f) = std::fs::File::options().create(true).truncate(false).write(true).open(&lock) {
        if f.try_lock().is_err() {
            return Ok(false);
        }
    }
    let log = std::fs::File::create(socket.with_extension("watch.log")).context("create the watch log")?;
    let mut cmd = std::process::Command::new(cli());
    cmd.args(["watch", session])
        .env("SKIFF_SOCKET", &socket)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().with_context(|| format!("start {}", cli().display()))?;
    // Reap it when it ends while the app runs.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(true)
}

#[tauri::command]
pub async fn start_skiff_watch(session: String) -> Result<bool, String> {
    blocking(move || start_watch(&session)).await
}

/// `[skiff_agent]` as the page reads it: the folder without `~`.
#[derive(Serialize)]
pub struct SkiffAgent {
    name: Option<String>,
    agent: Option<String>,
    enabled: bool,
    /// The fixed folder, or the override. Absent when the override is refused.
    folder: Option<String>,
    /// Why the override in projects.toml is refused.
    folder_error: Option<String>,
    continue_on_restore: bool,
}

fn read() -> anyhow::Result<SkiffAgent> {
    let cfg = config::load()?;
    let (folder, folder_error) = match folder(&cfg) {
        Ok(f) => (Some(f.display().to_string()), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    let a = cfg.skiff_agent;
    Ok(SkiffAgent { name: a.name, agent: a.agent, enabled: a.enabled, folder, folder_error, continue_on_restore: a.continue_on_restore })
}

/// One edit from Settings › Skiff agent. `null` or empty takes the default.
#[derive(Deserialize)]
#[serde(tag = "key", content = "value", rename_all = "snake_case")]
pub enum Edit {
    Name(Option<String>),
    Agent(Option<String>),
    Enabled(bool),
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
    let handle = app.clone();
    let out = blocking(move || {
        match edit {
            Edit::Name(v) => config::set_skiff_agent(SkiffAgentKey::Name(v.as_deref()))?,
            Edit::Agent(v) => config::set_skiff_agent(SkiffAgentKey::Agent(v.as_deref()))?,
            Edit::ContinueOnRestore(on) => config::set_skiff_agent(SkiffAgentKey::ContinueOnRestore(on))?,
            Edit::Enabled(on) => {
                // On makes the folder ready first: a refused folder stays off.
                if on {
                    ensure(&handle)?;
                }
                config::set_skiff_agent(SkiffAgentKey::Enabled(on))?
            }
        }
        read()
    })
    .await?;
    let _ = app.emit(PROJECTS_CHANGED, ());
    Ok(out)
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
        // The ledger is the CLI's ledger.db, made on first use.
        assert!(!dir.join("ledger.md").exists() && dir.join("results").is_dir());
        // A second run fills in what is missing and keeps the user's edits.
        std::fs::write(dir.join("AGENTS.md"), "mine").unwrap();
        std::fs::remove_dir(dir.join("results")).unwrap();
        setup(&res, &dir, "Skipper").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(), "mine");
        assert!(dir.join("results").is_dir() && dir.join(MARKER).is_file());
        // A folder with other files and no marker is refused.
        let other = tmp.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("notes.md"), "x").unwrap();
        assert!(setup(&res, &other, "Skipper").unwrap_err().to_string().contains("holds other files"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn refuses_a_folder_that_mixes_with_other_work() {
        let home = std::env::temp_dir().join(format!("skiff-agent-home-{}", std::process::id()));
        let project = home.join("code/app");
        std::fs::create_dir_all(&project).unwrap();
        let cfg = config::parse(&format!("[[project]]\nname = \"app\"\npath = \"{}\"\n", project.display())).unwrap();
        for bad in [home.clone(), PathBuf::from("/"), home.join("code"), project.clone(), project.join("sub")] {
            assert!(check(&bad, &home, &cfg).is_err(), "{}", bad.display());
        }
        assert!(check(&home.join("skiff-agent"), &home, &cfg).is_ok());
        std::fs::remove_dir_all(&home).unwrap();
    }
}
