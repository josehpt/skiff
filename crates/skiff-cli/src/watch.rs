//! `skiff watch`: follows the panes of the open tasks in the Skiff agent's
//! ledger, for that agent. The app starts it with the agent's session and it
//! stops when that session ends. A pane that waits notifies the person. A
//! pane that goes idle after work or after a prompt, exits or is gone wakes
//! the agent: one line typed into its input box while it is idle and the box
//! is empty. The daemon types it only if the person typed nothing since that
//! check. An idle pane with a pending task wakes the agent once per idle
//! stretch, and the line names the oldest pending uid and the `skiff ledger
//! send` command for it. A task whose pane
//! restarted since the send (a restore runs a new shell under the old id)
//! wakes the agent once as interrupted. Plans have no pane and are not
//! followed.
//!
//! It keeps no state of its own. The ledger, the session list and each pane's
//! unread flag are the input, so a new run rebuilds the same view. One lock
//! file per daemon socket keeps it to one copy.
//!
//! Best effort: the list, the ledger read and the submit are separate reads,
//! and the state events can lag. Held pending lines are checked against a
//! new ledger read just before the submit, and a line whose task left pending
//! is dropped. A row that changes after that read is not seen.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::Path,
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

use crate::{ledger, peek, print, resolve, sessions};

/// How often the ledger and the session list are read again without an event.
const TICK: Duration = Duration::from_secs(2);
/// How long the watcher waits for skiffd to come back: a reload, or a restart.
const RECONNECT_LIMIT: Duration = Duration::from_secs(600);
/// How long a new watcher waits for the lock, so an old one can finish.
const LOCK_WAIT: Duration = Duration::from_secs(10);

pub struct Options {
    /// The Skiff agent's session.
    pub skipper: String,
    pub notify: bool,
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
    /// The pane restarted since the task was sent.
    Interrupted,
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
    /// Idle, with a pending task for the pane.
    Pending,
    Exit,
    Gone,
    Interrupted,
}

impl Fact {
    fn phrase(self) -> &'static str {
        match self {
            Fact::Waiting => "is waiting",
            Fact::Idle | Fact::Pending => "is idle",
            Fact::Exit => "exited",
            Fact::Gone => "is gone",
            Fact::Interrupted => "restarted",
        }
    }
}

/// What this run saw of one ledger pane. Lost on a restart, by design: the
/// unread flag and the pending rows then bring back what still needs
/// attention.
#[derive(Default)]
pub struct Pane {
    /// The last fact told, so a fact is told once.
    told: Option<Fact>,
    name: String,
    /// The pane showed a prompt and has not been told idle since.
    waited: bool,
    /// The pending task told for this idle stretch.
    pending_told: Option<String>,
    /// The pane's generation when last seen. See [`SessionInfo::generation`].
    generation: Option<u64>,
}

impl Pane {
    /// The fact to tell now, if any. Idle counts after work: the daemon marks
    /// the end of a turn unread, and it stays so until the agent or the
    /// person marks it seen. A start-up screen is no turn. Idle also counts
    /// once after a prompt, answered or dismissed, though no turn followed:
    /// the pane can take the next step.
    pub fn observe(&mut self, now: Now) -> Option<Fact> {
        let fact = match now {
            Now::Working => {
                self.told = None;
                return None;
            }
            // The prompt left the screen. Told though an older idle was.
            Now::Idle { .. } if self.waited => {
                self.waited = false;
                self.told = Some(Fact::Idle);
                return Some(Fact::Idle);
            }
            Now::Idle { unread: false } => return None,
            Now::Idle { .. } => Fact::Idle,
            Now::Waiting => {
                self.waited = true;
                Fact::Waiting
            }
            Now::Exit => Fact::Exit,
            Now::Gone => Fact::Gone,
            Now::Interrupted => Fact::Interrupted,
        };
        if self.told == Some(fact) {
            return None;
        }
        self.told = Some(fact);
        if fact != Fact::Waiting {
            self.waited = false;
        }
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
    /// The task's uid, for an interrupted or a pending task.
    pub task: Option<String>,
}

impl Wake {
    fn tail(&self) -> String {
        match (&self.task, self.fact) {
            (Some(t), Fact::Pending) => format!(": task {t} is pending: skiff ledger send {t} --enter"),
            (Some(t), _) => format!(": task {t} is interrupted"),
            (None, _) => String::new(),
        }
    }
}

/// The one line the agent gets for all held wakes.
pub fn wake_line(wakes: &[Wake]) -> String {
    let parts: Vec<String> = wakes
        .iter()
        .map(|w| {
            let who = if w.name.is_empty() { String::new() } else { format!("{} ", w.name) };
            format!("pane {who}{} {}{}", print::short_id(&w.id), w.fact.phrase(), w.tail())
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
    /// Sessions that showed a prompt since the last step.
    prompted: HashSet<String>,
    /// Sessions that worked or showed a prompt since the last step.
    busy: HashSet<String>,
}

/// What came of a line typed into the agent's pane.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Delivery {
    /// A turn started: the agent took it.
    Taken,
    /// The person typed in the pane after the check. Nothing was typed.
    Refused,
    /// Typed and Enter pressed, but no turn started.
    NoTurn,
}

/// The open rows that name one live pane, or one pane that no session
/// resolves to.
struct Group<'a> {
    /// The resolved session id, else the ledger's pane value.
    key: String,
    s: Option<&'a SessionInfo>,
    /// Its pending uids, oldest first.
    pending: Vec<String>,
}

/// The open rows by pane, and the interrupted tasks apart. Rows that resolve
/// to one session (a full id and an imported prefix) make one group. The
/// agent's own pane is skipped.
fn group<'a>(tasks: &'a [ledger::Row], all: &'a [SessionInfo], skipper: &str) -> (Vec<Group<'a>>, Vec<(&'a str, &'a SessionInfo)>) {
    let (mut groups, mut interrupted) = (Vec::<Group>::new(), Vec::new());
    for t in tasks {
        let Some(pane) = t.pane_id.as_deref() else { continue };
        let s = resolve::session(all, pane).ok();
        if s.is_some_and(|s| s.id == skipper) {
            continue;
        }
        if let Some(s) = s.filter(|s| t.interrupted_in(Some(s))) {
            interrupted.push((t.uid.as_str(), s));
            continue;
        }
        let key = s.map_or(pane, |s| s.id.as_str());
        let i = match groups.iter().position(|g| g.key == key) {
            Some(i) => i,
            None => {
                groups.push(Group { key: key.to_string(), s, pending: Vec::new() });
                groups.len() - 1
            }
        };
        if t.state == "pending" {
            groups[i].pending.push(t.uid.clone());
        }
    }
    (groups, interrupted)
}

impl Watcher {
    /// The app marks a pane seen as soon as the person looks at it, so the
    /// unread flag can be gone by the next list. The daemon's turn-end event
    /// keeps it. A rename, a theme or a resize is no turn. A prompt or work
    /// between two lists shows only in the state events, so they are kept
    /// too. The list stays the authority for what the pane is now.
    fn note(&mut self, e: &Event) {
        match e {
            Event::TurnEnded { session } => {
                self.turned.insert(session.clone());
            }
            Event::State { session, state: SessionState::Waiting } => {
                self.prompted.insert(session.clone());
                self.busy.insert(session.clone());
            }
            Event::State { session, state: SessionState::Working } => {
                self.busy.insert(session.clone());
            }
            _ => {}
        }
    }

    /// Takes the state of the pane `key` now, with its pending uids, oldest
    /// first. Returns the fact to tell, if any, and holds a wake for the
    /// agent unless the pane waits. An idle pane with a pending task is told
    /// once per idle stretch with the oldest uid, in place of a plain idle.
    fn track(&mut self, key: &str, s: Option<&SessionInfo>, pending: &[String]) -> Option<Wake> {
        let pane = self.panes.entry(key.to_string()).or_default();
        if let Some(s) = s {
            pane.name = resolve::display_name(s).to_string();
            // A restore runs a new program under the old id. Also across a
            // reconnect: what this run saw of the old one does not count.
            let generation = s.generation();
            if pane.generation.is_some() && pane.generation != generation {
                pane.waited = false;
                pane.pending_told = None;
            }
            pane.generation = generation;
        }
        let seen = |set: &HashSet<String>| s.is_some_and(|s| set.contains(&s.id));
        let turned = seen(&self.turned);
        // A new turn ended: tell it, though the last idle was told.
        if turned && pane.told == Some(Fact::Idle) {
            pane.told = None;
        }
        if seen(&self.prompted) {
            pane.waited = true;
        }
        let now = Now::of(s, turned);
        // An idle line is old news once the pane works or waits again, and
        // the next idle stretch tells the pending task again.
        if seen(&self.busy) || matches!(now, Now::Working | Now::Waiting) {
            pane.pending_told = None;
            self.held.retain(|h| !(h.pane == key && matches!(h.fact, Fact::Idle | Fact::Pending)));
        }
        // The told task was sent, closed or removed: the next one can go.
        if pane.pending_told.as_ref().is_some_and(|u| !pending.contains(u)) {
            pane.pending_told = None;
        }
        let mut fact = pane.observe(now);
        let mut task = None;
        if let (Now::Idle { .. }, Some(uid)) = (now, pending.first()) {
            if pane.pending_told.as_ref() != Some(uid) {
                pane.pending_told = Some(uid.clone());
                pane.waited = false;
                // It covers the turn too: no plain idle line after it.
                pane.told = Some(Fact::Idle);
                fact = Some(Fact::Pending);
                task = Some(uid.clone());
            } else if fact == Some(Fact::Idle) {
                // A late turn end in the same idle stretch.
                fact = None;
            }
        }
        let fact = fact?;
        let id = s.map(|s| s.id.clone()).unwrap_or_else(|| key.to_string());
        let wake = Wake { fact, pane: key.to_string(), name: pane.name.clone(), id, task };
        if fact != Fact::Waiting {
            // The newest fact for a pane replaces an older one still held.
            self.held.retain(|h| h.pane != key);
            self.held.push(wake.clone());
        }
        Some(wake)
    }

    /// Takes a task whose pane restarted since the send. It is followed by
    /// its uid, not its pane: the pane's new program is not the task. Told
    /// once per run.
    fn interrupted(&mut self, uid: &str, s: &SessionInfo) -> Option<Wake> {
        let key = format!("task:{uid}");
        let pane = self.panes.entry(key.clone()).or_default();
        pane.name = resolve::display_name(s).to_string();
        let fact = pane.observe(Now::Interrupted)?;
        let wake = Wake { fact, pane: key, name: pane.name.clone(), id: s.id.clone(), task: Some(uid.to_string()) };
        self.held.push(wake.clone());
        Some(wake)
    }

    /// One step's read of the rows: tracks each pane and each interrupted
    /// task, then settles the held lines. Returns the facts to log.
    fn take(&mut self, groups: &[Group], interrupted: &[(&str, &SessionInfo)]) -> Vec<Wake> {
        let mut open = HashSet::new();
        let mut wakes = Vec::new();
        for (uid, s) in interrupted {
            open.insert(format!("task:{uid}"));
            wakes.extend(self.interrupted(uid, s));
        }
        for g in groups {
            open.insert(g.key.clone());
            wakes.extend(self.track(&g.key, g.s, &g.pending));
        }
        self.turned.clear();
        self.prompted.clear();
        self.busy.clear();
        self.panes.retain(|k, _| open.contains(k));
        // A row the agent closed needs no line.
        self.held.retain(|h| open.contains(&h.pane));
        self.drop_stale(groups);
        wakes
    }

    /// Drops a held pending line whose task is no longer pending on its pane
    /// in this ledger read.
    fn drop_stale(&mut self, groups: &[Group]) {
        self.held.retain(|h| {
            h.fact != Fact::Pending
                || groups.iter().any(|g| g.key == h.pane && h.task.as_ref().is_some_and(|t| g.pending.contains(t)))
        });
    }

    /// Settles the held lines after a delivery, and says what to log. A line
    /// that started no turn can sit in the input box: Enter is not pressed
    /// again, as it could submit twice.
    fn delivered(&mut self, d: Delivery, line: &str) -> String {
        let msg = match d {
            Delivery::Refused => return format!("held: the person typed in the agent's pane: {line}"),
            Delivery::Taken => format!("woke the agent: {line}"),
            Delivery::NoTurn => format!("undelivered: no turn started; the line can be in the agent's input box: {line}"),
        };
        self.held.clear();
        msg
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
    // A read that fails (the file busy past its timeout) skips this step: the
    // next tick reads again. An empty list would forget the open panes.
    let tasks = match ledger::open_tasks() {
        Ok(t) => t,
        Err(e) => {
            log(format!("cannot read the ledger: {e:#}"));
            return Ok(None);
        }
    };

    let (groups, interrupted) = group(&tasks, &all, &skipper.id);
    for wake in w.take(&groups, &interrupted) {
        log(format!("{} {} {}{}", wake.fact.phrase(), print::short_id(&wake.id), wake.name, wake.tail()));
        if wake.fact == Fact::Waiting && o.notify {
            let title = format!("{} ({}) is waiting", wake.name, print::short_id(&wake.id));
            desktop_notify(&title, "It needs you. Answer it in its pane.");
        }
    }

    if !w.held.is_empty() && ready(c, skipper).await {
        // The check above took time. Read the rows again, so a task sent
        // meanwhile is not named.
        match ledger::open_tasks() {
            Ok(tasks) => w.drop_stale(&group(&tasks, &all, &skipper.id).0),
            Err(e) => {
                log(format!("cannot read the ledger: {e:#}"));
                return Ok(None);
            }
        }
        if w.held.is_empty() {
            return Ok(None);
        }
        let line = wake_line(&w.held);
        // The check above read the screen. The daemon refuses the line if
        // the person typed after the list this step read.
        let events = c.events();
        let d = if !c.submit(&skipper.id, line.clone().into_bytes(), true, true, skipper.input_gen).await? {
            Delivery::Refused
        } else if sessions::turn_started(c, &skipper.id, events).await? {
            Delivery::Taken
        } else {
            Delivery::NoTurn
        };
        log(w.delivered(d, &line));
    }
    Ok(None)
}

/// The agent can take a line: it is idle, an agent Skiff knows is in front,
/// and its input box shows and is empty. A shell in front, or a draft, holds
/// the line.
async fn ready(c: &Client, s: &SessionInfo) -> bool {
    s.state == SessionState::Idle && sessions::is_agent(s) && read_front(c, s).await.ok() == Some(Front::Ready)
}

/// What a known agent's screen shows at its input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Front {
    /// An empty input box, and no busy line.
    Ready,
    /// The busy line: the agent works, though the pane is quiet.
    Busy,
    /// Text in the input box.
    Draft,
    /// No input box shows.
    NoBox,
}

pub fn front(rows: &[(String, String)], agent: &str) -> Front {
    if busy_line(rows) {
        return Front::Busy;
    }
    match input_box(rows, agent) {
        Some(false) => Front::Ready,
        Some(true) => Front::Draft,
        None => Front::NoBox,
    }
}

/// Reads the screen of session `s`, a known agent, for [`front`].
pub async fn read_front(c: &Client, s: &SessionInfo) -> Result<Front> {
    let agent = s.foreground_program.as_deref().unwrap_or("");
    let (data, cols, rows) = peek::snapshot(c, &s.id).await?;
    Ok(front(&rows_of(&data, cols, rows), agent))
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
        let w = |fact, name: &str, id: &str| Wake { fact, pane: id.into(), name: name.into(), id: id.into(), task: None };
        assert_eq!(wake_line(&[w(Fact::Idle, "api", "abc12345xyz")]), "[watch] pane api abc12345 is idle. Read it, then mark it seen.");
        assert_eq!(
            wake_line(&[w(Fact::Idle, "api", "abc12345"), w(Fact::Gone, "", "def67890")]),
            "[watch] pane api abc12345 is idle; pane def67890 is gone. Read them, then mark each seen."
        );
        let mut w = Watcher::default();
        let s = info("abc12345xyz", "idle", true);
        assert!(w.interrupted("3f9a01bc", &s).is_some());
        // Once per run, however long the row stays open.
        assert!(w.interrupted("3f9a01bc", &s).is_none());
        assert_eq!(wake_line(&w.held), "[watch] pane abc12345xyz abc12345 restarted: task 3f9a01bc is interrupted. Read it, then mark it seen.");
    }

    fn info(id: &str, state: &str, unread: bool) -> SessionInfo {
        serde_json::from_value(serde_json::json!({
            "id": id, "label": id, "role": "agent", "cwd": "/", "command": "claude", "args": [],
            "state": state, "cols": 80, "rows": 24, "pid": null, "exit_code": null, "unread": unread,
        }))
        .unwrap()
    }

    #[test]
    fn only_a_turn_end_counts_as_a_turn() {
        let mut w = Watcher::default();
        // A rename, a theme or a resize on an unread pane.
        w.note(&Event::SessionUpdated { session: info("a", "idle", true) });
        assert!(w.turned.is_empty());
        w.note(&Event::TurnEnded { session: "a".into() });
        assert!(w.turned.contains("a"));

        // Told idle once; metadata events after it wake nothing again.
        let mut w = Watcher::default();
        let s = info("a", "idle", true);
        assert_eq!(w.track("a", Some(&s), &[]).map(|k| k.fact), Some(Fact::Idle));
        w.held.clear();
        for _ in 0..3 {
            w.note(&Event::SessionUpdated { session: s.clone() });
            assert_eq!(w.track("a", Some(&s), &[]), None);
            w.turned.clear();
        }
        assert!(w.held.is_empty());
        // A real second turn is told.
        w.note(&Event::TurnEnded { session: "a".into() });
        assert_eq!(w.track("a", Some(&s), &[]).map(|k| k.fact), Some(Fact::Idle));
    }

    #[test]
    fn a_held_idle_line_goes_when_the_pane_works_or_waits() {
        for state in ["working", "waiting"] {
            let mut w = Watcher::default();
            w.track("a", Some(&info("a", "idle", true)), &[]);
            w.track("b", Some(&info("b", "idle", true)), &[]);
            assert_eq!(w.held.len(), 2);
            w.track("a", Some(&info("a", state, false)), &[]);
            assert_eq!(w.held.iter().map(|h| h.pane.as_str()).collect::<Vec<_>>(), ["b"], "{state}");
        }
        // An exit stays held while the pane is gone.
        let mut w = Watcher::default();
        w.track("a", None, &[]);
        w.track("a", None, &[]);
        assert_eq!(w.held.len(), 1);
    }

    #[test]
    fn a_refused_line_is_held_and_one_with_no_turn_is_undelivered() {
        let mut w = Watcher::default();
        w.track("a", Some(&info("a", "idle", true)), &[]);
        let line = wake_line(&w.held);
        assert!(w.delivered(Delivery::Refused, &line).starts_with("held:"));
        assert_eq!(w.held.len(), 1);
        assert!(w.delivered(Delivery::NoTurn, &line).starts_with("undelivered:"));
        assert!(w.held.is_empty());
        w.track("b", Some(&info("b", "idle", true)), &[]);
        assert!(w.delivered(Delivery::Taken, &line).starts_with("woke the agent"));
        assert!(w.held.is_empty());
    }

    fn row(id: i64, uid: &str, pane: &str, state: &str, gen: Option<i64>) -> ledger::Row {
        ledger::Row {
            id,
            uid: uid.into(),
            kind: "task".into(),
            parent: None,
            pane_id: Some(pane.into()),
            pane_gen: gen,
            project: String::new(),
            task: "t".into(),
            sent_at: String::new(),
            state: state.into(),
            result_path: None,
            reference: None,
            updated_at: String::new(),
            live: None,
            interrupted: None,
        }
    }

    fn born(mut s: SessionInfo, started_at: u64) -> SessionInfo {
        s.started_at = started_at;
        s
    }

    /// One step with these rows and sessions, as `step` runs it.
    fn run(w: &mut Watcher, rows: &[ledger::Row], all: &[SessionInfo]) -> Vec<Fact> {
        let (groups, interrupted) = group(rows, all, "skipper");
        w.take(&groups, &interrupted).iter().map(|k| k.fact).collect()
    }

    fn held(w: &Watcher) -> Vec<(Fact, Option<&str>)> {
        w.held.iter().map(|h| (h.fact, h.task.as_deref())).collect()
    }

    fn state(w: &mut Watcher, id: &str, states: &[SessionState]) {
        for &state in states {
            w.note(&Event::State { session: id.into(), state });
        }
    }

    #[test]
    fn a_prompt_then_idle_is_told_once() {
        let seq = |nows: &[Now]| {
            let mut p = Pane::default();
            nows.iter().filter_map(|&n| p.observe(n)).collect::<Vec<_>>()
        };
        let seen = Now::Idle { unread: false };
        let unread = Now::Idle { unread: true };
        assert_eq!(seq(&[Now::Waiting, Now::Working, seen, seen]), [Fact::Waiting, Fact::Idle]);
        // The working step not seen.
        assert_eq!(seq(&[Now::Waiting, seen]), [Fact::Waiting, Fact::Idle]);
        // Each prompt notifies the person; one idle after the last.
        assert_eq!(
            seq(&[Now::Waiting, Now::Working, Now::Waiting, Now::Working, seen]),
            [Fact::Waiting, Fact::Waiting, Fact::Idle]
        );
        // A turn after the prompt: one idle, not one more for the prompt.
        assert_eq!(seq(&[Now::Waiting, Now::Working, unread, seen]), [Fact::Waiting, Fact::Idle]);
        assert_eq!(seq(&[Now::Waiting, Now::Exit, seen]), [Fact::Waiting, Fact::Exit]);
        assert_eq!(seq(&[Now::Waiting, Now::Gone]), [Fact::Waiting, Fact::Gone]);
        // A start-up screen is still no turn.
        assert_eq!(seq(&[Now::Working, seen, seen]), []);
    }

    #[test]
    fn an_idle_pane_with_a_pending_task_wakes_once_per_idle_stretch() {
        let mut w = Watcher::default();
        let rows = [row(1, "7c2e91ab", "a", "pending", None)];
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), [Fact::Pending]);
        assert_eq!(wake_line(&w.held), "[watch] pane a a is idle: task 7c2e91ab is pending: skiff ledger send 7c2e91ab --enter. Read it, then mark it seen.");
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), []);
        assert_eq!(w.held.len(), 1);
        // It works: the held line goes. The next idle tells it again.
        assert_eq!(run(&mut w, &rows, &[info("a", "working", false)]), []);
        assert!(w.held.is_empty());
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), [Fact::Pending]);
        // Waiting drops it too.
        assert_eq!(run(&mut w, &rows, &[info("a", "waiting", false)]), [Fact::Waiting]);
        assert!(w.held.is_empty());
        // Joined with other parts as today.
        let b = info("b", "idle", true);
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", false), b]), [Fact::Pending]);
        let all = [info("a", "idle", false), info("b", "idle", true)];
        let rows = [row(1, "7c2e91ab", "a", "pending", None), row(2, "9d01ffe2", "b", "sent", None)];
        assert_eq!(run(&mut w, &rows, &all), [Fact::Idle]);
        assert_eq!(
            wake_line(&w.held),
            "[watch] pane a a is idle: task 7c2e91ab is pending: skiff ledger send 7c2e91ab --enter; pane b b is idle. Read them, then mark each seen."
        );
    }

    #[test]
    fn a_pending_task_on_an_unread_pane_is_one_part() {
        let mut w = Watcher::default();
        let rows = [row(1, "u1", "a", "pending", None)];
        w.note(&Event::TurnEnded { session: "a".into() });
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", true)]), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        // A prompt answered with a pending row: one part for both.
        let mut w = Watcher::default();
        assert_eq!(run(&mut w, &rows, &[info("a", "waiting", false)]), [Fact::Waiting]);
        assert_eq!(run(&mut w, &rows, &[info("a", "working", false)]), []);
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        assert_eq!(run(&mut w, &rows, &[info("a", "idle", false)]), []);
    }

    #[test]
    fn a_late_turn_end_does_not_repeat_a_pending_line() {
        let rows = [row(1, "u1", "a", "pending", None)];
        let a = info("a", "idle", true);
        // Delivered, then the turn's end comes in.
        let mut w = Watcher::default();
        assert_eq!(run(&mut w, &rows, std::slice::from_ref(&a)), [Fact::Pending]);
        w.delivered(Delivery::Taken, "");
        w.note(&Event::TurnEnded { session: "a".into() });
        assert_eq!(run(&mut w, &rows, std::slice::from_ref(&a)), []);
        assert!(w.held.is_empty());
        // Still held: it stays one part.
        let mut w = Watcher::default();
        run(&mut w, &rows, std::slice::from_ref(&a));
        w.note(&Event::TurnEnded { session: "a".into() });
        assert_eq!(run(&mut w, &rows, std::slice::from_ref(&a)), []);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
    }

    #[test]
    fn a_new_pending_row_upgrades_a_held_idle_line() {
        let mut w = Watcher::default();
        let a = [info("a", "idle", true)];
        assert_eq!(run(&mut w, &[row(1, "s1", "a", "sent", None)], &a), [Fact::Idle]);
        // A row is added; the pane stays unread with no new state.
        let rows = [row(1, "s1", "a", "sent", None), row(2, "u1", "a", "pending", None)];
        assert_eq!(run(&mut w, &rows, &a), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        // The oldest uid changes: u1 removed, u2 added.
        let rows = [row(1, "s1", "a", "sent", None), row(3, "u2", "a", "pending", None)];
        assert_eq!(run(&mut w, &rows, &a), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u2"))]);
    }

    #[test]
    fn the_next_pending_task_goes_once_the_told_one_leaves_pending() {
        let a = [info("a", "idle", false)];
        let both = [row(1, "u1", "a", "pending", None), row(2, "u2", "a", "pending", None)];
        // u1 removed while u2 stays pending: u2 at once.
        let mut w = Watcher::default();
        run(&mut w, &both, &a);
        w.delivered(Delivery::Taken, "");
        assert_eq!(run(&mut w, &both[1..], &a), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u2"))]);
        // u1 sent, then pending again with no state change: told again.
        let mut w = Watcher::default();
        let u1 = |state| [row(1, "u1", "a", state, None)];
        run(&mut w, &u1("pending"), &a);
        w.delivered(Delivery::Taken, "");
        assert_eq!(run(&mut w, &u1("sent"), &a), []);
        assert_eq!(run(&mut w, &u1("pending"), &a), [Fact::Pending]);
        // u1 cancelled (done or removed) while held: no line, no idle part.
        let mut w = Watcher::default();
        run(&mut w, &u1("pending"), &a);
        assert_eq!(run(&mut w, &[], &a), []);
        assert!(w.held.is_empty());
        // u1 sent before watch sees work: the held line goes.
        let mut w = Watcher::default();
        run(&mut w, &u1("pending"), &a);
        assert_eq!(run(&mut w, &u1("sent"), &a), []);
        assert!(w.held.is_empty());
    }

    #[test]
    fn state_events_between_two_lists_count() {
        use SessionState::{Idle, Waiting, Working};
        let seen = [info("a", "idle", false)];
        // A prompt shown and answered between two seen-idle lists, no turn.
        let mut w = Watcher::default();
        let sent = [row(1, "s1", "a", "sent", None)];
        assert_eq!(run(&mut w, &sent, &seen), []);
        state(&mut w, "a", &[Waiting, Working, Idle]);
        assert_eq!(run(&mut w, &sent, &seen), [Fact::Idle]);
        assert_eq!(run(&mut w, &sent, &seen), []);
        // A told pending task is told again after unseen work.
        let mut w = Watcher::default();
        let rows = [row(1, "u1", "a", "pending", None)];
        assert_eq!(run(&mut w, &rows, &seen), [Fact::Pending]);
        w.delivered(Delivery::Taken, "");
        state(&mut w, "a", &[Working, Idle]);
        assert_eq!(run(&mut w, &rows, &seen), [Fact::Pending]);
        // A held one goes with the work, and comes back as one part.
        state(&mut w, "a", &[Waiting, Working, Idle]);
        assert_eq!(run(&mut w, &rows, &seen), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        // An idle event alone is no work.
        state(&mut w, "a", &[Idle]);
        assert_eq!(run(&mut w, &rows, &seen), []);
    }

    #[test]
    fn a_row_changed_after_the_wake_is_dropped_before_the_line() {
        let mut w = Watcher::default();
        let all = [info("a", "idle", false), info("b", "idle", true)];
        let rows = [row(1, "u1", "a", "pending", None), row(2, "s1", "b", "sent", None)];
        run(&mut w, &rows, &all);
        assert_eq!(w.held.len(), 2);
        // The read after `ready`: u1 was sent meanwhile.
        let rows = [row(1, "u1", "a", "sent", None), row(2, "s1", "b", "sent", None)];
        w.drop_stale(&group(&rows, &all, "skipper").0);
        assert_eq!(held(&w), [(Fact::Idle, None)]);
    }

    #[test]
    fn a_new_generation_forgets_the_pane() {
        let rows = [row(1, "u1", "a", "pending", None)];
        let mut w = Watcher::default();
        assert_eq!(run(&mut w, &rows, &[born(info("a", "idle", false), 1)]), [Fact::Pending]);
        w.delivered(Delivery::Taken, "");
        // A reload keeps the generation: nothing new.
        assert_eq!(run(&mut w, &rows, &[born(info("a", "idle", false), 1)]), []);
        // A restore, seen only after a reconnect, already seen-idle.
        assert_eq!(run(&mut w, &rows, &[born(info("a", "idle", false), 2)]), [Fact::Pending]);
        // A sent row from the old generation is interrupted beside it.
        let mut w = Watcher::default();
        let rows = [row(1, "s1", "a", "sent", Some(1)), row(2, "u1", "a", "pending", None)];
        assert_eq!(run(&mut w, &rows, &[born(info("a", "idle", false), 2)]), [Fact::Interrupted, Fact::Pending]);
        assert_eq!(
            wake_line(&w.held),
            "[watch] pane a a restarted: task s1 is interrupted; pane a a is idle: task u1 is pending: skiff ledger send u1 --enter. Read them, then mark each seen."
        );
    }

    #[test]
    fn rows_that_name_one_pane_make_one_group() {
        let mut w = Watcher::default();
        let all = [info("abc12345xyz", "idle", false)];
        // An imported prefix row and a full-id row, the newer one first in id order.
        let rows = [row(4, "u1", "abc12345", "pending", None), row(7, "u2", "abc12345xyz", "pending", None)];
        let (groups, _) = group(&rows, &all, "skipper");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].pending, ["u1", "u2"]);
        assert_eq!(run(&mut w, &rows, &all), [Fact::Pending]);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        // The agent's own pane is skipped.
        let all = [info("skipper", "idle", false)];
        assert!(group(&[row(1, "u1", "skipper", "pending", None)], &all, "skipper").0.is_empty());
    }

    #[test]
    fn exit_and_gone_win_over_a_pending_task() {
        let rows = [row(1, "u1", "a", "pending", None)];
        let mut w = Watcher::default();
        assert_eq!(run(&mut w, &rows, &[info("a", "done", false)]), [Fact::Exit]);
        assert_eq!(wake_line(&w.held), "[watch] pane a a exited. Read it, then mark it seen.");
        let mut w = Watcher::default();
        run(&mut w, &rows, &[info("a", "idle", false)]);
        // Gone after a held pending line: the gone line replaces it.
        assert_eq!(run(&mut w, &rows, &[]), [Fact::Gone]);
        assert_eq!(held(&w), [(Fact::Gone, None)]);
    }

    #[test]
    fn a_pending_line_is_held_when_refused_and_dropped_with_no_turn() {
        let rows = [row(1, "u1", "a", "pending", None)];
        let a = [info("a", "idle", false)];
        let mut w = Watcher::default();
        run(&mut w, &rows, &a);
        let line = wake_line(&w.held);
        w.delivered(Delivery::Refused, &line);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        // A failed ledger read skips the step: the next one keeps the line.
        assert_eq!(run(&mut w, &rows, &a), []);
        assert_eq!(held(&w), [(Fact::Pending, Some("u1"))]);
        w.delivered(Delivery::NoTurn, &line);
        assert!(w.held.is_empty());
        // Not told again in this idle stretch.
        assert_eq!(run(&mut w, &rows, &a), []);
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
        assert_eq!(front(&rows(&["done.", &rule, "❯", &rule]), "claude"), Front::Ready);
        assert_eq!(front(&rows(&["done.", &rule, "❯ fix the", &rule]), "claude"), Front::Draft);
        // The busy line wins over an empty box.
        assert_eq!(front(&rows(&["* Working (2s · esc to interrupt)", &rule, "❯", &rule]), "claude"), Front::Busy);
        assert_eq!(front(&rows(&["$ ls", "a b"]), "claude"), Front::NoBox);
    }
}
