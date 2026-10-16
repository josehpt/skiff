//! `skiff skipper`: attach to the Skiff agent, or start it first. From any
//! terminal, an SSH login included.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use skiff_client::{
    agent_folder::{self, Kind, Mode, Preview},
    skipper, Client,
};

use crate::attach;

/// How long a daemon this command started may take to answer.
const START: Duration = Duration::from_secs(10);

pub async fn run(socket: &Path, no_attach: bool) -> Result<()> {
    let c = connect_or_start(socket).await?;
    let info = c.daemon_info().await?;
    if info.protocol < skiff_core::PROTOCOL {
        bail!("skiffd {} is too old for this skiff. Run `skiff daemon reload`", info.version);
    }
    let Some(found) = skipper::open(&c, skipper::Open::Start).await? else {
        bail!("skiffd did not start the Skiff agent");
    };
    let s = found.session;
    let name = s.name.clone().unwrap_or_else(|| s.label.clone());
    let exe = std::env::current_exe().context("find this skiff binary")?;
    if let Err(e) = skipper::start_watch(socket, &exe, &s.id) {
        eprintln!("skiff: {name} runs, but its watcher did not start: {e:#}");
    }
    if no_attach {
        println!("{}", s.id);
        return Ok(());
    }
    if found.created {
        eprintln!("skiff: started {name}");
    } else if found.typed {
        eprintln!("skiff: started the agent in {name}'s shell");
    }
    attach::attach(&c, &s.id).await
}

/// Connects. With no daemon at the socket, starts one, as the app does.
async fn connect_or_start(socket: &Path) -> Result<Client> {
    if let Ok(c) = Client::connect(socket).await {
        return Ok(c);
    }
    // One start at a time: a second `skiff skipper` waits here, then finds
    // the daemon the first one started.
    let _lock = start_lock(socket).await?;
    if let Ok(c) = Client::connect(socket).await {
        return Ok(c);
    }
    start_daemon(socket)?;
    let since = Instant::now();
    loop {
        if let Ok(c) = Client::connect(socket).await {
            if c.daemon_info().await.is_ok() {
                return Ok(c);
            }
        }
        if since.elapsed() > START {
            bail!("skiffd did not start at {}; see {}", socket.display(), socket.with_extension("log").display());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The lock beside the socket that a daemon start holds. It frees itself
/// when this process ends.
async fn start_lock(socket: &Path) -> Result<std::fs::File> {
    let path = socket.with_extension("start.lock");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let f = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let since = Instant::now();
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(f),
            Err(std::fs::TryLockError::WouldBlock) if since.elapsed() < START => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(std::fs::TryLockError::WouldBlock) => bail!("another skiff is starting skiffd at {}", socket.display()),
            Err(std::fs::TryLockError::Error(e)) => return Err(e).with_context(|| format!("lock {}", path.display())),
        }
    }
}

/// `SKIFF_DAEMON`, else the `skiffd` beside this binary, else the one on `PATH`.
fn daemon_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("SKIFF_DAEMON") {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .map(|e| e.with_file_name("skiffd"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("skiffd"))
}

/// Starts skiffd on `socket` apart from this login: its own scope, session
/// and keyring (see [`skipper::outliving`] and [`skipper::detach`]), so it
/// outlives this terminal and an SSH logout. Logs beside the socket, as the app does.
fn start_daemon(socket: &Path) -> Result<()> {
    let log = socket.with_extension("log");
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    // Keep the last daemon's log: it saw what the new one restores.
    let _ = std::fs::rename(&log, log.with_extension("log.1"));
    let file = std::fs::File::create(&log).with_context(|| format!("create {}", log.display()))?;
    let bin = daemon_binary();
    let mut cmd = skipper::outliving(&bin, &skipper::unit_for("skiffd", socket));
    cmd.env("SKIFF_SOCKET", socket).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(file));
    skipper::detach(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("start {}", bin.display()))?;
    // It forks nothing: reap it if it ends while this command runs.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// `skiff skipper --move-to PATH`: the Change… dialog of Settings ›
/// Skiff agent › Folder as text. Prints the preview, asks, then copies,
/// checks and switches (or switches to a Skiff folder). Never without a
/// terminal to ask in: there is no `--yes`.
pub async fn move_to(socket: &Path, path: &Path, empty: bool, dry_run: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("find this folder")?;
    let to = agent_folder::resolve(&path.to_string_lossy(), &cwd, &skipper::home()?)?;
    // A daemon that does not run has no Skipper and no running tasks.
    let c = Client::connect(socket).await.ok();
    let (panes, running) = match &c {
        Some(c) => agent_folder::sessions(c).await?,
        None => Default::default(),
    };
    let cfg = skiff_core::config::load()?;
    let p = agent_folder::preview(&cfg, &to, &panes)?;
    print!("{}", preview_text(&p, running.is_some()));
    let mode = match p.kind {
        Kind::Current => bail!("Skipper uses this folder now"),
        Kind::Refused => bail!("{}", p.refused.as_deref().unwrap_or("Skiff cannot use this folder")),
        Kind::Skiff if empty => bail!("{} is a Skiff folder. Skiff switches to it as it is; --empty does not apply", p.to.display()),
        Kind::Skiff => Mode::Switch,
        _ if empty || !p.can_copy => Mode::Empty,
        _ => {
            if let Some(why) = &p.copy_refused {
                bail!("{why} Add --empty to start empty");
            }
            Mode::Copy
        }
    };
    println!(
        "Skiff will {}.",
        match mode {
            Mode::Copy => "copy the current folder here, check the copy, then switch",
            Mode::Empty => "set up a new folder here with a new ledger, then switch",
            Mode::Switch => "switch to this folder",
        }
    );
    if dry_run {
        return Ok(());
    }
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        bail!("--move-to asks before it changes the folder. Run it in a terminal, or use --dry-run");
    }
    let anyway = !p.running.is_empty();
    if anyway && !ask("Tasks still run. Their results stay in the old folder. Change anyway?")? {
        return Ok(());
    }
    let what = match mode {
        Mode::Copy => "Copy and switch?",
        Mode::Empty => "Start empty and switch?",
        Mode::Switch => "Switch to this folder?",
    };
    if !ask(what)? {
        return Ok(());
    }
    if let (Some(c), Some(s)) = (&c, &running) {
        c.kill(&s.id).await.context("end Skipper's session")?;
    }
    let mut panes = panes;
    if let Some(s) = &running {
        panes.remove(&s.id);
    }
    let d = agent_folder::change(&cfg, &to, mode, &panes, anyway)?;
    println!("Skipper now uses {}.", d.to.display());
    if let Some(n) = &d.copied {
        println!("Copied and checked: {} tasks, {} results ({}).", n.tasks, n.results, agent_folder::size(n.bytes));
    }
    if let Some(note) = &d.moved_note {
        println!("The old folder stays, with every file. Skiff added {}.", note.display());
    }
    if let Some(w) = &d.warning {
        eprintln!("skiff: {w}");
    }
    println!("Skipper starts in the new folder at the next `skiff skipper`, with a new conversation.");
    Ok(())
}

/// The preview as the dialog shows it.
fn preview_text(p: &Preview, skipper_runs: bool) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "From  {}\nTo    {}", p.from.display(), p.to.display());
    let kind = match p.kind {
        Kind::Current => "in use now",
        Kind::New => "a new folder",
        Kind::Empty => "an empty folder",
        Kind::Skiff => "a Skiff folder: switch to it, no copy, no merge",
        Kind::Refused => "refused",
    };
    let _ = writeln!(s, "The folder is {kind}.");
    // The caller says why a folder is refused.
    if p.refused.is_some() {
        return s;
    }
    if let (true, Some(c)) = (matches!(p.kind, Kind::New | Kind::Empty) && p.can_copy, &p.from_counts) {
        let _ = writeln!(
            s,
            "Copy: AGENTS.md{}, the ledger ({} tasks: {} open, {} done), results/ ({} files, {}){}.",
            if c.agents_edited { " (edited)" } else { "" },
            c.tasks,
            c.open,
            c.done,
            c.results,
            agent_folder::size(c.bytes),
            if c.other.is_empty() { String::new() } else { format!(", and {}", c.other.join(", ")) }
        );
        let _ = writeln!(s, "Setup makes the links again. The old folder stays, with every file, and gets MOVED.md.");
    }
    if let (Kind::Skiff, Some(t)) = (p.kind, &p.to_counts) {
        let _ = writeln!(s, "This folder: {} tasks, {} open. Skiff does not merge two ledgers.", t.tasks, t.open);
    }
    if p.moved.is_some() {
        let _ = writeln!(s, "Skipper moved away from this folder before. Tasks added since then are only in the other folder.");
    }
    if p.kind != Kind::Current {
        let _ = writeln!(s, "The conversation does not move.");
    }
    if skipper_runs {
        let _ = writeln!(s, "Skipper runs in {}. Its session ends before the change.", p.from.display());
    }
    for r in &p.running {
        let _ = writeln!(s, "Still runs: {}  {}", r.uid, r.task);
    }
    if let Some(l) = &p.ledger_env {
        let _ = writeln!(s, "SKIFF_LEDGER is set to {l}. The change does not touch it.");
    }
    s
}

/// A yes or no on the terminal. No is the default.
fn ask(question: &str) -> Result<bool> {
    use std::io::Write;
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}
