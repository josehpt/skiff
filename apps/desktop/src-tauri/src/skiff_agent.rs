//! The Skiff agent's folder: one agent session that coordinates the others.
//! The app ships its template and the `skiff-cli` skill as resources. The
//! skill is copied out to a fixed place on every start, because the
//! AppImage mount and a moved Skiff.app change the resource path. The
//! folder links to that copy, so the skill updates with Skiff.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use tauri::{AppHandle, Manager};

/// Written into `AGENTS.md` until Settings has a name for the agent.
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

/// Writes the template into `dir`. The folder must be empty or not exist.
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
    if let Ok(meta) = dir.metadata() {
        if !meta.is_dir() {
            bail!("{} is not a folder", dir.display());
        }
        if std::fs::read_dir(dir)?.next().is_some() {
            bail!("{} is not empty. Pick an empty folder", dir.display());
        }
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
    write_new(&dir.join("AGENTS.md"), &agents)?;
    link(Path::new("AGENTS.md"), &dir.join("CLAUDE.md"))?;
    link(Path::new("AGENTS.md"), &dir.join("GEMINI.md"))?;
    write_new(&dir.join("ledger.md"), &ledger)?;
    std::fs::create_dir(dir.join("results")).context("create results")?;
    let skills = dir.join(".agents").join("skills");
    std::fs::create_dir_all(&skills).context("create .agents/skills")?;
    link(&skill, &skills.join("skiff-cli"))?;
    link(Path::new(".agents"), &dir.join(".claude"))?;
    Ok(dir)
}

/// The palette command. Returns the folder it wrote, without `~`.
#[tauri::command]
pub async fn setup_skiff_agent(app: AppHandle, path: String, name: Option<String>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = expand_home(&path)?;
        let name = name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(DEFAULT_NAME);
        setup(&resources(&app)?, &dir, name).map(|d| d.display().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("{e:#}"))
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
    fn writes_the_template_and_refuses_a_full_folder() {
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
        assert!(setup(&res, &dir, "Skipper").unwrap_err().to_string().contains("not empty"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
