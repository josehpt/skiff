//! The Skiff agent (Skipper): one agent session that coordinates the others.
//! The app and the `skiff skipper` command share this code, so they set up
//! the same folder and find the same session.
//!
//! The daemon marks the session as the coordinator and finds or starts it in
//! one guarded request ([`Client::coordinator`]). Nothing here guesses it from
//! the folder.
//!
//! The template and the `skiff-cli` skill are built in. The skill is copied
//! out next to `projects.toml`, and the folder links to that copy, so the
//! skill updates with Skiff.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use skiff_core::{
    config::{self, Config},
    project::AgentInfo,
    session::{Role, SessionSpec},
};

use crate::{Client, Coordinated, Coordinator};

/// The name until Settings has one. The app keeps the same default.
pub const DEFAULT_NAME: &str = "Skipper";

/// The folder's instructions. `{{name}}` becomes the agent's name.
const TEMPLATE: &str = include_str!("../agent-template/AGENTS.md.tmpl");

/// The `skiff-cli` skill, file by file: (path, bytes, runnable).
/// `skill_files_match_the_repo` keeps this list in step with the folder.
const SKILL: &[(&str, &[u8], bool)] = &[
    ("SKILL.md", include_bytes!("../../../.agents/skills/skiff-cli/SKILL.md"), false),
    ("scripts/session_report.py", include_bytes!("../../../.agents/skills/skiff-cli/scripts/session_report.py"), true),
];

/// The marker setup leaves, so a later run knows the folder is the agent's.
const MARKER: &str = ".skiff-agent";

/// The configured name, or the default.
pub fn name(cfg: &Config) -> String {
    cfg.skiff_agent.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(DEFAULT_NAME).to_string()
}

/// The agent the Skiff agent starts: the one from Settings while it is
/// installed and on (a custom agent is on), else the first enabled one.
/// Settings shows the same.
pub fn choose<'a>(agents: &'a [AgentInfo], id: Option<&str>) -> Option<&'a AgentInfo> {
    agents
        .iter()
        .find(|a| Some(a.id.as_str()) == id && a.installed && (a.enabled || a.custom))
        .or_else(|| agents.iter().find(|a| a.enabled))
}

/// The line that continues the newest conversation in the folder, from each
/// CLI's help (2026-10-13). Only the Skiff agent's own folder runs it: it
/// holds no other conversation. A custom agent has none. Keep in step with
/// `CONTINUE` in apps/desktop/src/terminal/resume.ts, which Settings shows.
pub fn continue_line(agent: &str, cmd: &str) -> Option<String> {
    let cmd = cmd.trim();
    // A subcommand goes after the program and before the user's flags.
    let sub = |words: &str| match cmd.split_once(char::is_whitespace) {
        Some((program, flags)) => format!("{program} {words} {}", flags.trim()),
        None => format!("{cmd} {words}"),
    };
    match agent {
        "claude" | "opencode" | "grok" | "gemini" => Some(format!("{cmd} --continue")),
        "codex" => Some(sub("resume --last")),
        _ => None,
    }
}

/// Where the skill is copied: next to `projects.toml`.
pub fn skill_home() -> Result<PathBuf> {
    let config = config::config_path();
    let dir = config.parent().context("find the config folder")?;
    Ok(dir.join("skills").join("skiff-cli"))
}

/// Replaces the copy of the skill when the built-in one differs. A new copy
/// is built beside the old one and renamed over it, so an agent that reads
/// the skill never sees half of it.
pub fn sync_skill() -> Result<PathBuf> {
    let dest = skill_home()?;
    if SKILL.iter().all(|(f, bytes, _)| std::fs::read(dest.join(f)).is_ok_and(|b| b == *bytes)) {
        return Ok(dest);
    }
    let parent = dest.parent().context("find the skills folder")?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    // Named by process, so two copies at once do not share a folder.
    let pid = std::process::id();
    let tmp = parent.join(format!(".skiff-cli.new.{pid}"));
    let old = parent.join(format!(".skiff-cli.old.{pid}"));
    let _ = std::fs::remove_dir_all(&tmp);
    for (f, bytes, run) in SKILL {
        let p = tmp.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).with_context(|| format!("create {}", p.display()))?;
        std::fs::write(&p, bytes).with_context(|| format!("write {}", p.display()))?;
        #[cfg(unix)]
        if *run {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    if dest.symlink_metadata().is_ok() {
        std::fs::rename(&dest, &old).with_context(|| format!("move {}", dest.display()))?;
    }
    std::fs::rename(&tmp, &dest).with_context(|| format!("replace {}", dest.display()))?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(dest)
}

/// The agent's folder: `~/skiff-agent`, or the override in projects.toml when
/// it passes [`check`]. The folder is fixed, so its conversation and ledger
/// never move.
pub fn folder(cfg: &Config) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")?;
    let Some(dir) = cfg.skiff_agent.folder.clone() else { return Ok(home.join("skiff-agent")) };
    check(&dir, &home, cfg)?;
    Ok(dir)
}

/// Refuses a folder whose instructions would reach other sessions: an agent
/// reads `CLAUDE.md` in every folder above its own, and `AGENTS.md` up to its
/// repository's root. So the folder must not be the home, the root, the config
/// folder, a project, or a folder above or inside one.
fn check(dir: &Path, home: &Path, cfg: &Config) -> Result<()> {
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

/// Makes the agent's folder ready: writes the template parts it lacks, with
/// the name from Settings. Settings runs it when the user turns the agent on,
/// and every open runs it before it starts a session. Returns the folder.
pub fn ensure(cfg: &Config) -> Result<PathBuf> {
    setup(&folder(cfg)?, &name(cfg))
}

/// Writes the parts of the template that `dir` does not have yet. The folder
/// must be new, empty, or one this setup made before (it has the marker), so
/// setup never mixes the agent's files with other files. A second run keeps
/// the user's edits and puts back what is missing. Nothing is written outside
/// the folder and the skill's copy. A lock beside `projects.toml` keeps two
/// setups at once (the app and the CLI) apart.
///
/// ```text
/// AGENTS.md                    the instructions, with the agent's name
/// CLAUDE.md, GEMINI.md         links to AGENTS.md
/// .claude                      a link to .agents, for Claude's skills
/// .agents/skills/skiff-cli     a link to the copy of the skill
/// results/                     long child reports
/// ```
pub fn setup(dir: &Path, name: &str) -> Result<PathBuf> {
    let _lock = lock()?;
    if dir.metadata().is_ok_and(|m| !m.is_dir()) {
        bail!("{} is not a folder", dir.display());
    }
    if dir.is_dir() && !dir.join(MARKER).exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!("{} holds other files. Move them, or set another folder in projects.toml: [skiff_agent] folder", dir.display());
    }
    let skill = sync_skill()?;

    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let dir = dir.canonicalize().with_context(|| format!("find {}", dir.display()))?;
    // A broken link counts as there: setup does not replace what the user made.
    let missing = |p: &Path| p.symlink_metadata().is_err();
    if missing(&dir.join("AGENTS.md")) {
        write_new(&dir.join("AGENTS.md"), &TEMPLATE.replace("{{name}}", name))?;
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

/// The setup lock. It frees itself when the process ends.
fn lock() -> Result<std::fs::File> {
    let config = config::config_path();
    let dir = config.parent().context("find the config folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(".skiff-agent.lock");
    let f = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    f.lock().with_context(|| format!("lock {}", path.display()))?;
    Ok(f)
}

/// Writes a new file. Refuses one that is there, so setup never overwrites.
fn write_new(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    f.write_all(text.as_bytes()).with_context(|| format!("write {}", path.display()))
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, at).with_context(|| format!("link {}", at.display()))
}

#[cfg(not(unix))]
fn link(_target: &Path, _at: &Path) -> Result<()> {
    bail!("not supported on this platform")
}

/// What [`open`] does with the coordinator it finds.
pub enum Open {
    /// Find it, or start it. A bare shell gets the agent: its last
    /// conversation when Settings says to continue on restore, else a new one.
    Start,
    /// A restore brought it back as a shell: continue its last conversation,
    /// when Settings says so. Never starts a session or a new conversation.
    /// `expect` is the `input_gen` the app read, so a line the person
    /// typed first stays theirs.
    Restore { expect: u64 },
}

/// Finds the live Skiff agent session, or starts it, through the daemon's
/// guarded request. `None` only for [`Open::Restore`] with no coordinator.
pub async fn open(c: &Client, how: Open) -> Result<Option<Coordinated>> {
    let cfg = config::load()?;
    let a = &cfg.skiff_agent;
    if !a.enabled {
        bail!("The Skiff agent is off. Turn it on in Settings › Skiff agent");
    }
    let agents = c.list_agents().await?;
    let agent = choose(&agents, a.agent.as_deref());
    let resume = agent.filter(|_| a.continue_on_restore).and_then(|g| continue_line(&g.id, &g.command));
    let ask = match how {
        Open::Restore { expect } => Coordinator { resume, expect: Some(expect), ..Default::default() },
        Open::Start => {
            let Some(agent) = agent else { bail!("No agent is on. Turn one on in Settings › Agents") };
            // The folder is made ready first: files the user deleted come back.
            let dir = ensure(&cfg)?;
            Coordinator { spec: Some(spec(agent, &name(&cfg), dir)), start: Some(agent.command.trim().to_string()), resume, expect: None }
        }
    };
    c.coordinator(ask).await
}

/// A new session of `agent` in `dir`, as the app's + menu starts one.
fn spec(agent: &AgentInfo, name: &str, dir: PathBuf) -> SessionSpec {
    let mut words = agent.command.split_whitespace().map(str::to_string);
    SessionSpec {
        label: name.to_string(),
        name: Some(name.to_string()),
        role: Role::Agent,
        cwd: Some(dir),
        command: words.next(),
        args: words.collect(),
        ..Default::default()
    }
}

/// Starts `skiff watch` for the agent's session, unless one runs for this
/// daemon. `cli` is the `skiff` binary to run. The watcher is its own
/// process, out of the caller's process group and session: it outlives the
/// app or the terminal that started it, connects again after a daemon
/// reload, and stops when the session ends. Its lock beside the socket keeps
/// it to one copy. True when it started one.
pub fn start_watch(socket: &Path, cli: &Path, session: &str) -> Result<bool> {
    let lock = socket.with_extension("watch.lock");
    if let Ok(f) = std::fs::File::options().create(true).truncate(false).write(true).open(&lock) {
        if f.try_lock().is_err() {
            return Ok(false);
        }
    }
    let log = std::fs::File::create(socket.with_extension("watch.log")).context("create the watch log")?;
    let mut cmd = std::process::Command::new(cli);
    cmd.args(["watch", session])
        .env("SKIFF_SOCKET", socket)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    detach(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("start {}", cli.display()))?;
    // Reap it when it ends while the caller runs.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(true)
}

/// Runs `cmd` in a session of its own, so a hangup of the terminal that
/// started it (an SSH logout) does not reach it.
pub fn detach(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches no memory.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_template_and_fills_in_only_what_is_missing() {
        let tmp = std::env::temp_dir().join(format!("skiff-agent-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("SKIFF_CONFIG", tmp.join("config/projects.toml"));
        let dir = setup(&tmp.join("agent"), "Skipper").unwrap();
        let agents = std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
        assert!(agents.contains("You are Skipper, the Skiff agent."));
        assert_eq!(std::fs::read_to_string(dir.join("GEMINI.md")).unwrap(), agents);
        assert!(dir.join(".claude/skills/skiff-cli/SKILL.md").is_file());
        // The ledger is the CLI's ledger.db, made on first use.
        assert!(!dir.join("ledger.md").exists() && dir.join("results").is_dir());
        // A second run fills in what is missing and keeps the user's edits.
        std::fs::write(dir.join("AGENTS.md"), "mine").unwrap();
        std::fs::remove_dir(dir.join("results")).unwrap();
        std::fs::remove_file(dir.join("GEMINI.md")).unwrap();
        setup(&dir, "Skipper").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(dir.join("GEMINI.md")).unwrap(), "mine");
        assert!(dir.join("results").is_dir() && dir.join(MARKER).is_file());
        // A folder with other files and no marker is refused.
        let other = tmp.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("notes.md"), "x").unwrap();
        assert!(setup(&other, "Skipper").unwrap_err().to_string().contains("holds other files"));
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

    #[test]
    fn skill_files_match_the_repo() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.agents/skills/skiff-cli");
        let mut found = Vec::new();
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    found.push(p.strip_prefix(&dir).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        found.sort();
        let mut listed: Vec<String> = SKILL.iter().map(|(f, _, _)| f.to_string()).collect();
        listed.sort();
        assert_eq!(found, listed, "add the new skill files to SKILL");
    }

    #[test]
    fn continue_lines() {
        assert_eq!(continue_line("claude", "claude --model x").as_deref(), Some("claude --model x --continue"));
        assert_eq!(continue_line("codex", "codex --yolo").as_deref(), Some("codex resume --last --yolo"));
        assert_eq!(continue_line("codex", "codex").as_deref(), Some("codex resume --last"));
        assert_eq!(continue_line("mine", "mine").as_deref(), None);
    }
}
