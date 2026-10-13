//! `skiff watch`: follows the panes of the open tasks in the Skiff agent's
//! ledger, for that agent. The app starts it with the agent's session and it
//! stops when that session ends. A pane that waits notifies the person. A
//! pane that goes idle after work, exits or is gone wakes the agent: one line
//! typed into its input box while it is idle and the box is empty.
//!
//! It keeps no state of its own. The ledger, the session list and each pane's
//! unread flag are the input, so a new run rebuilds the same view. One lock
//! file per daemon socket keeps it to one copy.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use alacritty_terminal::{
    grid::Dimensions,
    index::{Column, Line},
    term::cell::Flags,
};
use anyhow::{bail, Context, Result};
use skiff_client::Client;
use skiff_core::{
    protocol::Event,
    session::{SessionInfo, SessionState},
};
use tokio::{
    sync::broadcast::error::{RecvError, TryRecvError},
    time::{interval, sleep, Instant, MissedTickBehavior},
};

use crate::{peek, print, resolve, sessions};

/// How often the ledger and the session list are read again without an event.
const TICK: Duration = Duration::from_secs(2);
/// How long the watcher waits for skiffd to come back: a reload, or a restart.
const RECONNECT_LIMIT: Duration = Duration::from_secs(600);
/// How long a new watcher waits for the lock, so an old one can finish.
const LOCK_WAIT: Duration = Duration::from_secs(10);

pub struct Options {
    /// The Skiff agent's session.
    pub skipper: String,
    /// The ledger. Default: ledger.md in the agent's folder.
    pub ledger: Option<PathBuf>,
    pub notify: bool,
}

/// An open row of the ledger.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub pane: String,
    pub task: String,
}

/// The open rows of the ledger table: the state is `sent`, `pending` or
/// `blocked`. The table columns are: pane id, project, task, sent at, state,
/// result. This is the one place that knows the ledger format.
pub fn open_tasks(text: &str) -> Vec<Task> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if !l.starts_with('|') {
                return None;
            }
            let cells: Vec<&str> = l.trim_matches('|').split('|').map(|c| c.trim()).collect();
            if cells.len() < 5 {
                return None;
            }
            let state = cells[4].trim_matches('`').to_ascii_lowercase();
            if !matches!(state.as_str(), "sent" | "pending" | "blocked") {
                return None;
            }
            let pane = cells[0].trim_matches('`').to_string();
            (!pane.is_empty()).then(|| Task { pane, task: cells[2].to_string() })
        })
        .collect()
}

/// What a ledger pane is now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Now {
    Working,
    Waiting,
    Idle { unread: bool },
    /// The program, or the agent in front of the launch shell, ended.
    Exit,
    Gone,
}

impl Now {
    /// `turned`: the daemon marked a turn's end since the last step, though
    /// the app may have marked it seen already.
    fn of(s: Option<&SessionInfo>, turned: bool) -> Now {
        let Some(s) = s else { return Now::Gone };
        match s.state {
            SessionState::Done => Now::Exit,
            _ if s.agent_exit.is_some() => Now::Exit,
            SessionState::Working => Now::Working,
            SessionState::Waiting => Now::Waiting,
            SessionState::Idle => Now::Idle { unread: s.unread || turned },
        }
    }
}

/// What the watcher tells.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fact {
    Waiting,
    Idle,
    Exit,
    Gone,
}

impl Fact {
    fn phrase(self) -> &'static str {
        match self {
            Fact::Waiting => "is waiting",
            Fact::Idle => "is idle",
            Fact::Exit => "exited",
            Fact::Gone => "is gone",
        }
    }
}

/// What this run saw of one ledger pane. Lost on a restart, by design: the
/// unread flag then brings back what still needs attention.
#[derive(Default)]
pub struct Pane {
    /// The last fact told, so a fact is told once.
    told: Option<Fact>,
    name: String,
}

impl Pane {
    /// The fact to tell now, if any. Idle counts only after work: the daemon
    /// marks the end of a turn unread, and it stays so until the agent or the
    /// person marks it seen. A start-up screen is no turn.
    pub fn observe(&mut self, now: Now) -> Option<Fact> {
        let fact = match now {
            Now::Working => {
                self.told = None;
                return None;
            }
            Now::Idle { unread: false } => {
                // A prompt the person answered with no new work.
                if self.told == Some(Fact::Waiting) {
                    self.told = None;
                }
                return None;
            }
            Now::Idle { .. } => Fact::Idle,
            Now::Waiting => Fact::Waiting,
            Now::Exit => Fact::Exit,
            Now::Gone => Fact::Gone,
        };
        if self.told == Some(fact) {
            return None;
        }
        self.told = Some(fact);
        Some(fact)
    }
}

/// One line for the agent, held until it can take it.
#[derive(Debug, Clone, PartialEq)]
pub struct Wake {
    pub fact: Fact,
    pub pane: String,
    pub name: String,
    pub id: String,
}

/// The one line the agent gets for all held wakes.
pub fn wake_line(wakes: &[Wake]) -> String {
    let parts: Vec<String> = wakes
        .iter()
        .map(|w| {
            let who = if w.name.is_empty() { String::new() } else { format!("{} ", w.name) };
            format!("pane {who}{} {}", print::short_id(&w.id), w.fact.phrase())
        })
        .collect();
    let tail = if wakes.len() == 1 { "Read it, then mark it seen." } else { "Read them, then mark each seen." };
    format!("[watch] {}. {tail}", parts.join("; "))
}

#[derive(Default)]
struct Watcher {
    /// The agent's full session id, once resolved.
    skipper: Option<String>,
    panes: HashMap<String, Pane>,
    held: Vec<Wake>,
    /// Sessions the daemon marked unread since the last step.
    turned: HashSet<String>,
}

impl Watcher {
    /// The app marks a pane seen as soon as the person looks at it, so the
    /// unread flag can be gone by the next list. The event keeps it.
    fn note(&mut self, e: &Event) {
        if let Event::SessionUpdated { session } = e {
            if session.unread {
                self.turned.insert(session.id.clone());
            }
        }
    }
}

fn log(msg: impl std::fmt::Display) {
    println!("{} {msg}", skiff_core::session::now_ms() / 1000);
}

/// Runs until the agent's session ends. Exit codes: 0 when it ends or another
/// watcher runs; 1 when skiffd does not come back.
pub async fn watch(socket: &Path, o: Options) -> Result<i32> {
    let Some(_lock) = lock(socket).await? else {
        log("another watcher runs for this daemon");
        return Ok(0);
    };
    log(format!("watching for {}", o.skipper));
    let mut w = Watcher::default();
    let mut lost: Option<Instant> = None;
    loop {
        if let Ok(c) = Client::connect(socket).await {
            lost = None;
            match follow(&c, &o, &mut w).await {
                Ok(code) => return Ok(code),
                Err(e) => log(format!("lost skiffd: {e:#}")),
            }
        }
        let since = *lost.get_or_insert_with(Instant::now);
        if since.elapsed() > RECONNECT_LIMIT {
            log("skiffd did not come back");
            return Ok(1);
        }
        sleep(Duration::from_secs(1)).await;
    }
}

/// The lock beside the socket. It frees itself when the process ends, however
/// it ends. `None` when another watcher keeps it past [`LOCK_WAIT`].
async fn lock(socket: &Path) -> Result<Option<File>> {
    let path = socket.with_extension("watch.lock");
    let f = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let until = Instant::now() + LOCK_WAIT;
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(Some(f)),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < until => sleep(Duration::from_millis(250)).await,
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(e)) => return Err(e).with_context(|| format!("lock {}", path.display())),
        }
    }
}

/// One connection: a step on each event and each tick.
async fn follow(c: &Client, o: &Options, w: &mut Watcher) -> Result<i32> {
    // Listen before the first read, so no change falls between the two.
    let mut events = c.events();
    let mut tick = interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        if let Some(code) = step(c, o, w).await? {
            return Ok(code);
        }
        tokio::select! {
            e = events.recv() => {
                match e {
                    Err(RecvError::Closed) => bail!("skiffd closed the connection"),
                    Ok(e) => w.note(&e),
                    Err(RecvError::Lagged(_)) => {}
                }
                // One step reads everything, so a burst counts once.
                loop {
                    match events.try_recv() {
                        Ok(e) => w.note(&e),
                        Err(TryRecvError::Lagged(_)) => continue,
                        Err(TryRecvError::Closed) => bail!("skiffd closed the connection"),
                        Err(TryRecvError::Empty) => break,
                    }
                }
            }
            _ = tick.tick() => {}
        }
    }
}

/// Reads the ledger and the session list, tells what is new, and delivers the
/// held lines when the agent can take them. `Some` stops the watcher.
async fn step(c: &Client, o: &Options, w: &mut Watcher) -> Result<Option<i32>> {
    let all = sessions::sorted(c).await?;
    let skipper = match &w.skipper {
        Some(id) => all.iter().find(|s| &s.id == id),
        None => resolve::session(&all, &o.skipper).ok(),
    };
    let Some(skipper) = skipper.filter(|s| s.state != SessionState::Done) else {
        log("the agent's session ended");
        return Ok(Some(0));
    };
    w.skipper = Some(skipper.id.clone());
    let ledger = o.ledger.clone().unwrap_or_else(|| skipper.cwd.join("ledger.md"));
    let tasks = open_tasks(&std::fs::read_to_string(&ledger).unwrap_or_default());

    let mut open = HashSet::new();
    for t in &tasks {
        let s = resolve::session(&all, &t.pane).ok();
        if s.is_some_and(|s| s.id == skipper.id) {
            continue;
        }
        open.insert(t.pane.clone());
        let pane = w.panes.entry(t.pane.clone()).or_default();
        if let Some(s) = s {
            pane.name = resolve::display_name(s).to_string();
        }
        let turned = s.is_some_and(|s| w.turned.contains(&s.id));
        // A new turn ended: tell it, though the last idle was told.
        if turned && pane.told == Some(Fact::Idle) {
            pane.told = None;
        }
        let Some(fact) = pane.observe(Now::of(s, turned)) else { continue };
        let id = s.map(|s| s.id.clone()).unwrap_or_else(|| t.pane.clone());
        log(format!("{} {} {}", fact.phrase(), print::short_id(&id), pane.name));
        if fact == Fact::Waiting {
            if o.notify {
                let title = format!("{} ({}) is waiting", pane.name, print::short_id(&id));
                desktop_notify(&title, "It needs you. Answer it in its pane.");
            }
            continue;
        }
        let wake = Wake { fact, pane: t.pane.clone(), name: pane.name.clone(), id };
        // The newest fact for a pane replaces an older one still held.
        w.held.retain(|h| h.pane != wake.pane);
        w.held.push(wake);
    }
    w.turned.clear();
    w.panes.retain(|k, _| open.contains(k));
    // A row the agent closed needs no line.
    w.held.retain(|h| open.contains(&h.pane));

    if !w.held.is_empty() && ready(c, skipper).await {
        let line = wake_line(&w.held);
        c.paste(&skipper.id, line.clone().into_bytes()).await?;
        sleep(sessions::ENTER_DELAY).await;
        c.write(&skipper.id, b"\r".to_vec()).await?;
        log(format!("woke the agent: {line}"));
        w.held.clear();
    }
    Ok(None)
}

/// The agent can take a line: it is idle, an agent Skiff knows is in front,
/// and its input box shows and is empty. A shell in front, or a draft, holds
/// the line.
async fn ready(c: &Client, s: &SessionInfo) -> bool {
    if s.state != SessionState::Idle || !sessions::is_agent(s) {
        return false;
    }
    let agent = s.foreground_program.as_deref().unwrap_or("");
    let Ok((data, cols, rows)) = peek::snapshot(c, &s.id).await else { return false };
    let rows = rows_of(&data, cols, rows);
    !busy_line(&rows) && input_box(&rows, agent) == Some(false)
}

/// The line an agent keeps up while it works: "esc to interrupt" and its
/// kin. The daemon calls a quiet stretch idle, so this is the second check.
pub fn busy_line(rows: &[(String, String)]) -> bool {
    let end = rows.iter().rposition(|(a, _)| !a.is_empty()).map_or(0, |i| i + 1);
    rows[end.saturating_sub(8)..end].iter().any(|(a, _)| {
        let t: String = a.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
        t.contains("tointerrupt") || t.contains("escinterrupt")
    })
}

/// The visible rows: all text, and the text without dim cells. An agent
/// draws the hint in an empty input box dim.
pub fn rows_of(data: &[u8], cols: u16, rows: u16) -> Vec<(String, String)> {
    let term = peek::term(data, cols, rows);
    let grid = term.grid();
    (0..grid.screen_lines() as i32)
        .map(|l| {
            let row = &grid[Line(l)];
            let (mut all, mut solid) = (String::new(), String::new());
            for c in 0..grid.columns() {
                let cell = &row[Column(c)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let ch = if cell.c == '\0' { ' ' } else { cell.c };
                all.push(ch);
                solid.push(if cell.flags.contains(Flags::DIM) { ' ' } else { ch });
            }
            (all.trim_end().to_string(), solid.trim_end().to_string())
        })
        .collect()
}

/// Whether the agent's input box holds a draft. `None` when no input box
/// shows: then nothing is typed.
///
/// Codex marks its input line with `›`. The others draw the box between two
/// rules of box-drawing characters (Claude Code's `───`, Gemini's `╭──╮` and
/// `╰──╯`): the last two such rows on the screen.
pub fn input_box(rows: &[(String, String)], agent: &str) -> Option<bool> {
    // An agent that draws inline leaves blank rows under a short screen.
    let end = rows.iter().rposition(|(a, _)| !a.is_empty()).map_or(0, |i| i + 1);
    let near = &rows[end.saturating_sub(16)..end];
    let content = |s: &str| {
        let t = s.trim().trim_matches(|c| c == '│' || c == '┃').trim();
        t.trim_start_matches(['>', '❯', '›']).trim().to_string()
    };
    if agent == "codex" {
        let line = near.iter().rev().find(|(all, _)| all.trim_start().starts_with('›'))?;
        return Some(!content(&line.1).is_empty());
    }
    let cols = rows.iter().map(|(a, _)| a.chars().count()).max().unwrap_or(0);
    let rule = |r: &str| {
        let t = r.trim();
        let n = t.chars().count();
        n >= 8 && n * 2 >= cols && t.chars().filter(|c| ('\u{2500}'..='\u{257f}').contains(c)).count() * 10 >= n * 8
    };
    let marks: Vec<usize> = near.iter().enumerate().filter(|(_, (a, _))| rule(a)).map(|(i, _)| i).collect();
    let [.., top, bottom] = marks[..] else { return None };
    if bottom - top < 2 {
        return None;
    }
    Some(near[top + 1..bottom].iter().any(|(_, solid)| !content(solid).is_empty()))
}

/// Best effort. A missing tool leaves the log line as the only sign.
fn desktop_notify(title: &str, body: &str) {
    let mut cmd = if cfg!(target_os = "macos") {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut c = std::process::Command::new("osascript");
        c.arg("-e").arg(format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title)));
        c
    } else {
        let mut c = std::process::Command::new("notify-send");
        c.args(["--urgency=critical", "--app-name=Skiff", "--", title, body]);
        c
    };
    if let Ok(mut ch) = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        // Reap it without blocking the watcher.
        std::thread::spawn(move || {
            let _ = ch.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEDGER: &str = "# Ledger\n\n| Pane id | Project | Task | Sent at | State | Result |\n|---|---|---|---|---|---|\n| abc12345 | app | fix bug | 10:00 | sent | |\n| def67890 | web | seo | 10:01 | done | r.md |\n| 1111aaaa | api | docs | 10:02 | pending | |\n";

    #[test]
    fn reads_only_open_rows() {
        let t = open_tasks(LEDGER);
        assert_eq!(t.iter().map(|t| t.pane.as_str()).collect::<Vec<_>>(), ["abc12345", "1111aaaa"]);
    }

    #[test]
    fn idle_counts_after_work_or_while_unread_and_is_told_once() {
        let mut p = Pane::default();
        // Quiet and seen: nothing to tell.
        assert_eq!(p.observe(Now::Idle { unread: false }), None);
        // A start-up screen: work with no turn marked.
        assert_eq!(p.observe(Now::Working), None);
        assert_eq!(p.observe(Now::Idle { unread: false }), None);
        assert_eq!(p.observe(Now::Waiting), Some(Fact::Waiting));
        assert_eq!(p.observe(Now::Waiting), None);
        assert_eq!(p.observe(Now::Working), None);
        assert_eq!(p.observe(Now::Idle { unread: true }), Some(Fact::Idle));
        assert_eq!(p.observe(Now::Idle { unread: true }), None);
        // Marked seen: nothing new until it works again.
        assert_eq!(p.observe(Now::Idle { unread: false }), None);
        // A new run: unread output still needs attention.
        assert_eq!(Pane::default().observe(Now::Idle { unread: true }), Some(Fact::Idle));
        assert_eq!(p.observe(Now::Exit), Some(Fact::Exit));
        assert_eq!(p.observe(Now::Gone), Some(Fact::Gone));
        assert_eq!(p.observe(Now::Gone), None);
    }

    #[test]
    fn held_wakes_make_one_line() {
        let w = |fact, name: &str, id: &str| Wake { fact, pane: id.into(), name: name.into(), id: id.into() };
        assert_eq!(wake_line(&[w(Fact::Idle, "api", "abc12345xyz")]), "[watch] pane api abc12345 is idle. Read it, then mark it seen.");
        assert_eq!(
            wake_line(&[w(Fact::Idle, "api", "abc12345"), w(Fact::Gone, "", "def67890")]),
            "[watch] pane api abc12345 is idle; pane def67890 is gone. Read them, then mark each seen."
        );
    }

    fn rows(lines: &[&str]) -> Vec<(String, String)> {
        lines.iter().map(|l| (l.to_string(), l.to_string())).collect()
    }

    #[test]
    fn finds_an_empty_box_and_a_draft() {
        let rule = "─".repeat(40);
        assert_eq!(input_box(&rows(&["done.", &rule, "❯", &rule, "  ? for shortcuts"]), "claude"), Some(false));
        let mut tall = rows(&[&rule, "❯ x", &rule]);
        tall.extend(rows(&[""; 30]));
        assert_eq!(input_box(&tall, "claude"), Some(true));
        assert_eq!(input_box(&rows(&["done.", &rule, "❯ fix the", "  tests", &rule]), "claude"), Some(true));
        // The dim hint is not a draft.
        let hint = vec![(rule.clone(), rule.clone()), ("❯ Try \"fix lint\"".into(), "❯".into()), (rule.clone(), rule.clone())];
        assert_eq!(input_box(&hint, "claude"), Some(false));
        assert_eq!(input_box(&rows(&["$ ls", "a b"]), "claude"), None);
        assert!(busy_line(&rows(&["* Working (2s · esc to interrupt)", &rule, "❯", &rule])));
        assert!(!busy_line(&rows(&["done.", &rule, "❯", &rule])));
        assert_eq!(input_box(&rows(&["text", "›", "", "  ? for shortcuts"]), "codex"), Some(false));
        assert_eq!(input_box(&rows(&["text", "› hello", ""]), "codex"), Some(true));
    }
}
