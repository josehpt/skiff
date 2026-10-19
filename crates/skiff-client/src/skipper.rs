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
pub(crate) const MARKER: &str = ".skiff-agent";

/// `AGENTS.md` as setup writes it for `name`.
pub(crate) fn template(name: &str) -> String {
    TEMPLATE.replace("{{name}}", name)
}

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

/// The agent's folder: [`default_folder`], or the override in projects.toml
/// when it passes [`check`]. Settings › Skiff agent › Folder changes it with
/// a copy, check and switch ([`crate::agent_folder`]), so the ledger, the
/// results and the instructions go with it. A socket other than the live
/// one never gets the live agent's folder ([`refuse_live_folder`]).
pub fn folder(cfg: &Config) -> Result<PathBuf> {
    let home = home()?;
    let dir = match cfg.skiff_agent.folder.clone() {
        Some(dir) => {
            check(&dir, &home, cfg)?;
            dir
        }
        None => default_folder()?,
    };
    refuse_live_folder(&dir)?;
    Ok(dir)
}

/// The folder projects.toml names, refused or not: the override, else
/// [`default_folder`]. A change of folder copies from it.
pub fn configured(cfg: &Config) -> Result<PathBuf> {
    match cfg.skiff_agent.folder.clone() {
        Some(dir) => Ok(dir),
        None => default_folder(),
    }
}

/// `~/skiff-agent`, or `skiff-agent` in the socket's private state (see
/// [`skiff_core::state`]).
pub fn default_folder() -> Result<PathBuf> {
    match skiff_core::state::private_dir() {
        Some(d) => Ok(d.join("skiff-agent")),
        None => Ok(home()?.join("skiff-agent")),
    }
}

/// Refuses the live Skiff agent's folder (`~/skiff-agent`, or the folder in
/// the person's projects.toml) to a process on any other socket, opted in
/// to real state or not. Two Skiff agents in one folder share one ledger,
/// and each takes the other's tasks for gone.
pub fn refuse_live_folder(dir: &Path) -> Result<()> {
    use skiff_core::state;
    let socket = state::socket();
    if state::is_live(&socket) {
        return Ok(());
    }
    let d = real(dir);
    if state::real_agent_folders().iter().any(|f| real(f) == d) {
        bail!(
            "{} is the folder of the live Skiff agent, on {}. This skiff uses {}, so it starts no Skiff agent there and follows none of its tasks. Use a folder of its own in [skiff_agent] folder of {}",
            dir.display(),
            state::live_socket().display(),
            socket.display(),
            config::config_path().display()
        );
    }
    Ok(())
}

/// The home folder, from `HOME`.
pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")
}

/// Refuses a folder whose instructions would reach other sessions: an agent
/// reads `CLAUDE.md` in every folder above its own, and `AGENTS.md` up to its
/// repository's root. So the folder must not be the home, the root, the config
/// folder, a project, a folder above or inside one, or a folder in a Git
/// repository. Nor may it be an agent's own home or a Hermes profile: Skiff
/// writes nothing in an agent's setup.
fn check(dir: &Path, home: &Path, cfg: &Config) -> Result<()> {
    let shown = dir.display();
    if !dir.is_absolute() {
        bail!("[skiff_agent] folder = \"{shown}\" is not a full path. Use one such as ~/skiff-agent");
    }
    if let Some(why) = refusal(&real(dir), home, cfg) {
        bail!("[skiff_agent] folder = \"{shown}\": {why}");
    }
    Ok(())
}

/// `p` made canonical: `.` and `..` gone and links resolved. For a path
/// that does not exist yet, the nearest existing parent is made canonical
/// and the rest added to it. `p` is absolute.
pub(crate) fn real(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => {
                out.push(c);
                // Each step resolves its link, so `..` after a link leaves the link's target.
                if let Ok(r) = out.canonicalize() {
                    out = r;
                }
            }
        }
    }
    out
}

/// The agents' own homes, from the home folder. Skiff never writes there.
const AGENT_HOMES: [(&str, &str); 5] =
    [(".claude", "claude"), (".codex", "codex"), (".gemini", "gemini"), (".config/opencode", "opencode"), (".grok", "grok")];

/// The files that make a folder a Hermes profile.
const HERMES_FILES: [&str; 5] = ["config.yaml", "SOUL.md", "state.db", "auth.json", "profile.yaml"];

/// Why `d` (absolute, links resolved) cannot be the agent's folder, in one
/// line, or `None`. Settings, `skiff skipper --move-to` and the override in
/// projects.toml share these rules. A dot-folder such as `~/.skiff-agent` is a
/// folder like any other: the agent homes match by exact path.
pub(crate) fn refusal(d: &Path, home: &Path, cfg: &Config) -> Option<String> {
    let home = real(home);
    if d.parent().is_none() {
        return Some("The root folder holds instructions for every session. Use a folder of its own.".into());
    }
    if d == home {
        return Some("Your home folder holds instructions for every session. Use a folder of its own.".into());
    }
    if let Some(c) = config::config_path().parent().map(real) {
        if d.starts_with(&c) {
            return Some("This is Skiff's config folder. Use a folder of its own.".into());
        }
    }
    let hermes = std::env::var_os("HERMES_HOME").map(PathBuf::from).into_iter().chain([home.join(".hermes")]);
    if hermes.map(|h| real(&h)).any(|h| d.starts_with(h)) || HERMES_FILES.iter().any(|f| d.join(f).exists()) {
        return Some("This is a Hermes profile. Choose another folder for Skipper.".into());
    }
    for (dir, agent) in AGENT_HOMES {
        if d.starts_with(real(&home.join(dir))) {
            return Some(format!("This is the home folder of {agent}. Skiff does not write in an agent's setup."));
        }
    }
    for p in &cfg.projects {
        let pr = real(&p.path);
        if d.starts_with(&pr) || pr.starts_with(d) {
            return Some(format!("This folder mixes with the project {}. Choose a folder outside any project.", p.name));
        }
    }
    if d.ancestors().any(|a| a.join(".git").symlink_metadata().is_ok()) {
        return Some("This folder is inside a Git repository. Choose a folder outside any project.".into());
    }
    if d.file_name().is_some_and(|n| n.to_string_lossy().contains(crate::agent_folder::STAGING)) {
        return Some("This is an unfinished copy. Skiff removes it at the next change.".into());
    }
    None
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
    setup_in(dir, name, &sync_skill()?)
}

/// [`setup`] for a caller that holds the lock. `skill` is the skill's copy.
pub(crate) fn setup_in(dir: &Path, name: &str, skill: &Path) -> Result<PathBuf> {
    if dir.metadata().is_ok_and(|m| !m.is_dir()) {
        bail!("{} is not a folder", dir.display());
    }
    if dir.is_dir() && !dir.join(MARKER).exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!("{} holds other files. Move them, or choose another folder in Settings › Skiff agent › Folder", dir.display());
    }

    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let dir = dir.canonicalize().with_context(|| format!("find {}", dir.display()))?;
    // A broken link counts as there: setup does not replace what the user made.
    let missing = |p: &Path| p.symlink_metadata().is_err();
    if missing(&dir.join("AGENTS.md")) {
        write_new(&dir.join("AGENTS.md"), &template(name))?;
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
        link(skill, &skills.join("skiff-cli"))?;
    }
    if missing(&dir.join(".claude")) {
        link(Path::new(".agents"), &dir.join(".claude"))?;
    }
    if missing(&dir.join(MARKER)) {
        write_new(&dir.join(MARKER), "1\n")?;
    }
    Ok(dir)
}

/// The setup lock. It frees itself when the process ends. A change of
/// folder holds it from the first step to the last.
pub(crate) fn lock() -> Result<std::fs::File> {
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
    // Before any request: another socket must not resume the live agent's conversation either.
    folder(&cfg)?;
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
    let lock = watch_lock(socket);
    if let Ok(f) = std::fs::File::options().create(true).truncate(false).write(true).open(&lock) {
        if f.try_lock().is_err() {
            return Ok(false);
        }
    }
    let log = fresh_log(&socket.with_extension("watch.log"))?;
    let mut cmd = outliving(cli, &unit_for("skiff-watch", socket));
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

/// The watcher that holds the lock, as it writes itself into the lock file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WatchHolder {
    pub pid: u32,
    /// The daemon socket it watches.
    pub socket: String,
    /// Unix time in milliseconds.
    pub started_at: u64,
}

/// The watch lock beside the socket, as [`watch_status`] reads it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WatchLock {
    /// A process holds the lock: a watcher runs for this daemon, registered or not.
    pub held: bool,
    /// What the holder wrote. `None` when the lock is free, or when a watcher
    /// from before this record holds it.
    pub holder: Option<WatchHolder>,
}

/// The lock file of the watcher for `socket`.
pub fn watch_lock(socket: &Path) -> PathBuf {
    socket.with_extension("watch.lock")
}

/// Writes this process into the lock file that `f` holds, for the app's
/// stale-watcher check and Restart. The lock stays on the file.
pub fn write_holder(f: &mut std::fs::File, socket: &Path) -> Result<()> {
    use std::io::{Seek, Write};
    let me = WatchHolder { pid: std::process::id(), socket: socket.display().to_string(), started_at: skiff_core::session::now_ms() };
    f.set_len(0)?;
    f.seek(std::io::SeekFrom::Start(0))?;
    f.write_all(serde_json::to_string(&me)?.as_bytes())?;
    f.flush()?;
    Ok(())
}

/// Whether a watcher holds the lock for `socket`, and which one. Reads only:
/// a free lock is let go at once.
pub fn watch_status(socket: &Path) -> Result<WatchLock> {
    let path = watch_lock(socket);
    let Ok(f) = std::fs::File::options().read(true).write(true).open(&path) else {
        return Ok(WatchLock { held: false, holder: None });
    };
    match f.try_lock() {
        Ok(()) => Ok(WatchLock { held: false, holder: None }),
        Err(std::fs::TryLockError::WouldBlock) => {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            Ok(WatchLock { held: true, holder: serde_json::from_str(text.trim()).ok() })
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e).with_context(|| format!("lock {}", path.display())),
    }
}

/// How long [`restart_watch`] waits for a signalled watcher to let go of its lock.
pub const RESTART_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// True for the command line of `skiff watch`: a program named `skiff`
/// with `watch` as its first argument. The program is the first word, or
/// the first word after `--` (systemd-run on Linux).
pub fn is_watch_command(cmd: &str) -> bool {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    let at = words.iter().position(|w| *w == "--").map_or(0, |i| i + 1);
    words.get(at).is_some_and(|w| Path::new(w).file_name().is_some_and(|n| n == "skiff")) && words.get(at + 1) == Some(&"watch")
}

/// The command line of a live process. `None` when it does not run.
#[cfg(unix)]
fn command_of(pid: u32) -> Option<String> {
    // SAFETY: signal 0 only checks that the process exists.
    if pid == 0 || unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
        return None;
    }
    let out = std::process::Command::new("ps").args(["-o", "command=", "-p", &pid.to_string()]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|c| !c.is_empty())
}

/// Why the lock holder is not ours to stop. `None`: it is a live `skiff
/// watch` for `socket`.
#[cfg(unix)]
fn not_stoppable(h: &WatchHolder, socket: &Path) -> Option<String> {
    if Path::new(&h.socket) != socket {
        return Some(format!("The watch lock names another socket ({}). Nothing was stopped.", h.socket));
    }
    match command_of(h.pid) {
        None => Some(format!("Process {} holds no watcher. Nothing was stopped.", h.pid)),
        Some(c) if !is_watch_command(&c) => Some(format!("Process {} is not skiff watch. Nothing was stopped.", h.pid)),
        Some(_) => None,
    }
}

/// Stops the watcher that holds the lock for `socket`, then starts one for
/// `session` as [`start_watch`] does. It signals only a live `skiff watch`
/// that wrote itself into the lock for this socket, with SIGTERM, and waits
/// up to [`RESTART_WAIT`] for the lock. It never sends SIGKILL, and it never
/// ends, types into or changes a session. True when it started a watcher.
#[cfg(unix)]
pub fn restart_watch(socket: &Path, cli: &Path, session: &str) -> Result<bool> {
    let st = watch_status(socket)?;
    if st.held {
        let Some(h) = st.holder else {
            bail!("A watcher holds the lock, but its pid is unknown. Stop it from a terminal, then restart.");
        };
        if let Some(why) = not_stoppable(&h, socket) {
            bail!(why);
        }
        // SAFETY: a plain syscall to a checked pid.
        if unsafe { libc::kill(h.pid as libc::pid_t, libc::SIGTERM) } != 0 {
            bail!("Cannot signal the watcher (pid {}): {}", h.pid, std::io::Error::last_os_error());
        }
        let until = std::time::Instant::now() + RESTART_WAIT;
        while watch_status(socket)?.held {
            if std::time::Instant::now() >= until {
                bail!("The watcher (pid {}) did not stop within 5 seconds. Nothing else changed.", h.pid);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    start_watch(socket, cli, session)
}

/// A new, empty watch log. The last one moves to `<log>.1`, so the log of a
/// watcher that died is still there when the next one starts.
fn fresh_log(path: &Path) -> Result<std::fs::File> {
    if path.exists() {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        std::fs::rename(path, &old).context("keep the last watch log")?;
    }
    std::fs::File::create(path).context("create the watch log")
}

/// A command for `program` that outlives the login that starts it, an SSH
/// login included. On Linux with a systemd user manager, it runs through
/// `systemd-run --user --scope --unit=<unit>`: the process leaves the
/// login's cgroup, so logind does not kill it at logout
/// (KillUserProcesses=yes). It gets DISPLAY, WAYLAND_DISPLAY,
/// DBUS_SESSION_BUS_ADDRESS and XDG_RUNTIME_DIR from the user manager, so
/// the sessions it starts see the desktop. A scope, not a service: a service
/// unit ends and kills the child when systemd-run returns. Else `program`
/// as is. Add the arguments to the result, then call [`detach`].
pub fn outliving(program: &Path, unit: &str) -> std::process::Command {
    #[cfg(target_os = "linux")]
    if let Some(env) = user_manager_env() {
        let mut cmd = std::process::Command::new("systemd-run");
        cmd.args(["--user", "--scope", "--quiet", "--collect"])
            .arg(format!("--unit={}", unit_name(unit)))
            .arg("--")
            .arg(program)
            .envs(env);
        return cmd;
    }
    let _ = unit;
    std::process::Command::new(program)
}

/// `unit-<socket name>`, in the characters a unit name takes.
pub fn unit_for(unit: &str, socket: &Path) -> String {
    let stem = socket.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    format!("{unit}-{stem}")
}

#[cfg(target_os = "linux")]
fn unit_name(unit: &str) -> String {
    unit.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect()
}

/// The desktop's variables from `systemctl --user show-environment`. `None`
/// when there is no user manager to ask.
#[cfg(target_os = "linux")]
fn user_manager_env() -> Option<Vec<(String, String)>> {
    const KEEP: [&str; 4] = ["DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"];
    let mut cmd = std::process::Command::new("systemctl");
    cmd.args(["--user", "show-environment"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // An SSH login can lack XDG_RUNTIME_DIR, and systemctl --user needs it.
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        let run = skiff_core::socket::runtime_dir();
        cmd.env("XDG_RUNTIME_DIR", run);
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let env: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| KEEP.contains(k))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    // systemd-run reaches the manager through XDG_RUNTIME_DIR too.
    env.iter().any(|(k, _)| k == "XDG_RUNTIME_DIR").then_some(env)
}

/// Runs `cmd` in a session of its own, so a hangup of the terminal that
/// started it (an SSH logout) does not reach it. On Linux it also gets a
/// session keyring of its own, linked to the user keyring as pam_keyinit
/// does: the login's keyring is revoked at logout, and every program the
/// process starts would inherit the dead one.
pub fn detach(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid and keyctl are plain syscalls and touch no memory.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                {
                    const JOIN_SESSION_KEYRING: libc::c_long = 1;
                    const LINK: libc::c_long = 8;
                    const USER_KEYRING: libc::c_long = -4;
                    const SESSION_KEYRING: libc::c_long = -3;
                    // A kernel without keys refuses both. That is no reason not to start.
                    if libc::syscall(libc::SYS_keyctl, JOIN_SESSION_KEYRING, 0 as libc::c_long) >= 0 {
                        libc::syscall(libc::SYS_keyctl, LINK, USER_KEYRING, SESSION_KEYRING);
                    }
                }
                Ok(())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lock tests run one at a time: a process another test forks holds
    /// a copy of each open file until it execs, and with it a lock.
    static LOCKS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn knows_the_watch_command() {
        assert!(is_watch_command("/Applications/Skiff.app/Contents/MacOS/skiff watch 4ac9e7a1"));
        assert!(is_watch_command("skiff watch abc"));
        assert!(is_watch_command("systemd-run --user --scope -- /usr/bin/skiff watch abc"));
        assert!(!is_watch_command("skiff ledger ls"));
        assert!(!is_watch_command("/usr/bin/skiffd"));
        assert!(!is_watch_command("vim skiff watch"));
        assert!(!is_watch_command(""));
    }

    /// A watcher that holds the lock reads as held, with what it wrote. A free
    /// lock reads as free, and the read does not keep it.
    #[test]
    fn reads_the_watch_lock_and_its_holder() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        assert_eq!(watch_status(&socket).unwrap(), WatchLock { held: false, holder: None });
        let mut f = std::fs::File::options().create(true).truncate(false).write(true).open(watch_lock(&socket)).unwrap();
        f.lock().unwrap();
        write_holder(&mut f, &socket).unwrap();
        let st = watch_status(&socket).unwrap();
        assert!(st.held);
        let h = st.holder.unwrap();
        assert_eq!((h.pid, h.socket.as_str()), (std::process::id(), socket.to_str().unwrap()));
        // A watcher from before the record: held, pid unknown.
        f.set_len(0).unwrap();
        assert_eq!(watch_status(&socket).unwrap(), WatchLock { held: true, holder: None });
        drop(f);
        assert!(!watch_status(&socket).unwrap().held);
        assert!(!watch_status(&socket).unwrap().held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Restart signals nothing that is not a live `skiff watch` for this
    /// socket: here the holder is the test itself, and it keeps running.
    #[cfg(unix)]
    #[test]
    fn restart_stops_only_a_skiff_watch() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-restart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        let cli = dir.join("no-skiff");
        let mut f = std::fs::File::options().create(true).truncate(false).write(true).open(watch_lock(&socket)).unwrap();
        f.lock().unwrap();
        let err = restart_watch(&socket, &cli, "abc").unwrap_err().to_string();
        assert!(err.contains("pid is unknown"), "{err}");
        write_holder(&mut f, &socket).unwrap();
        let err = restart_watch(&socket, &cli, "abc").unwrap_err().to_string();
        assert!(err.contains("is not skiff watch"), "{err}");
        // Another socket's record is refused before any process check.
        let other = WatchHolder { pid: std::process::id(), socket: "/elsewhere.sock".into(), started_at: 0 };
        assert!(not_stoppable(&other, &socket).unwrap().contains("another socket"));
        assert!(watch_status(&socket).unwrap().held);
        drop(f);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A watcher that holds the lock and never registers is stopped with
    /// SIGTERM, and a new one starts. The stand-in shows as `skiff watch` in
    /// `ps`: python takes the lock, then execs `cat` named `skiff` on a FIFO
    /// named `watch`, which blocks. Skipped without python3 or mkfifo.
    #[cfg(unix)]
    #[test]
    fn restart_stops_a_stale_watcher_and_starts_one() {
        let _one = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-watch-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        if !std::process::Command::new("mkfifo").arg(dir.join("watch")).status().is_ok_and(|s| s.success()) {
            return;
        }
        let script = r#"
import fcntl, json, os, sys, time
lock, sock = sys.argv[1], sys.argv[2]
fd = os.open(lock, os.O_WRONLY | os.O_CREAT)
fcntl.flock(fd, fcntl.LOCK_EX)
os.ftruncate(fd, 0)
os.write(fd, json.dumps({"pid": os.getpid(), "socket": sock, "started_at": int(time.time() * 1000)}).encode())
os.set_inheritable(fd, True)
os.execv("/bin/cat", ["skiff", "watch"])
"#;
        let Ok(mut child) = std::process::Command::new("python3")
            .args(["-c", script])
            .arg(watch_lock(&socket))
            .arg(&socket)
            .current_dir(&dir)
            .spawn()
        else {
            return;
        };
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !watch_status(&socket).unwrap().held || !command_of(child.id()).is_some_and(|c| is_watch_command(&c)) {
            assert!(std::time::Instant::now() < until, "the stand-in watcher did not start");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let st = watch_status(&socket).unwrap();
        assert_eq!(st.holder.map(|h| h.pid), Some(child.id()));
        // `true` stands in for the skiff that starts the new watcher.
        let started = restart_watch(&socket, Path::new("/usr/bin/true"), "abc").unwrap();
        assert!(started);
        let code = child.wait().unwrap();
        assert_eq!(std::os::unix::process::ExitStatusExt::signal(&code), Some(libc::SIGTERM));
        assert!(!watch_status(&socket).unwrap().held);
        assert!(socket.with_extension("watch.log").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_watch_log_keeps_the_last_one() {
        let dir = std::env::temp_dir().join(format!("skiff-watch-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("s.watch.log");
        std::io::Write::write_all(&mut fresh_log(&log).unwrap(), b"first").unwrap();
        fresh_log(&log).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("s.watch.log.1")).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
        for bad in [home.clone(), PathBuf::from("/"), home.join("code"), project.clone(), project.join("sub"), PathBuf::from("rel/x")] {
            assert!(check(&bad, &home, &cfg).is_err(), "{}", bad.display());
        }
        assert!(check(&home.join("skiff-agent"), &home, &cfg).is_ok());
        assert!(check(&home.join(".skiff-agent"), &home, &cfg).is_ok());
        // Any other dot-folder is still a folder like any other.
        assert!(check(&home.join(".skiff"), &home, &cfg).is_ok());
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
        // The person's bypass flag stays on resume.
        assert_eq!(
            continue_line("claude", "claude --dangerously-skip-permissions").as_deref(),
            Some("claude --dangerously-skip-permissions --continue")
        );
        assert_eq!(continue_line("gemini", "agy --dangerously-skip-permissions").as_deref(), Some("agy --dangerously-skip-permissions --continue"));
        // The built-in line resumes with the agent's own prompts.
        assert_eq!(continue_line("gemini", "agy").as_deref(), Some("agy --continue"));
    }

    #[test]
    fn launch_starts_the_persons_command() {
        let agent = |command: &str| AgentInfo {
            id: "codex".into(),
            command: command.into(),
            default_command: "codex".into(),
            custom: false,
            installed: true,
            enabled: true,
        };
        let s = spec(&agent("codex"), "Skipper", PathBuf::from("/tmp"));
        assert_eq!((s.command.as_deref(), s.args.len()), (Some("codex"), 0));
        let s = spec(&agent(" codex --yolo "), "Skipper", PathBuf::from("/tmp"));
        assert_eq!((s.command.as_deref(), s.args), (Some("codex"), vec!["--yolo".to_string()]));
    }
}
