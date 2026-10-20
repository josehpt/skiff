//! The read side of Skipper's ledger, `ledger.db` in its folder. The `skiff
//! ledger` commands write it; the CLI and the app read it with this code.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use skiff_core::{
    session::{SessionInfo, SessionState},
    state,
};

/// Shown in place of `sent` or `blocked` when the pane restarted since the
/// send. Never stored: the pane generation tells it.
pub const INTERRUPTED: &str = "interrupted";
/// Shown in place of an open state when the pane no longer exists. Never
/// stored: the session list tells it.
pub const GONE: &str = "gone";

/// The schema version, in `user_version`. 4 added `tasks.socket` and
/// `owner`, 5 added `tasks.reported_at`, 6 added `tasks.title`, 7 added
/// `tasks.closed_at`; an older file from version 3 on takes each in place,
/// with its rows.
pub const VERSION: i64 = 7;

pub const COLUMNS: &str = "id, uid, kind, parent, pane_id, pane_gen, project, task, sent_at, state, result_path, ref, updated_at, type, socket, reported_at, title, closed_at";

/// Where work stands. Never stored: the close stamp, the task states and the
/// panes tell it. An agent is on working work now; open work waits for the
/// person or for Skipper; finished work is closed.
pub const WORKING: &str = "working";
pub const OPEN: &str = "open";
pub const FINISHED: &str = "finished";

#[derive(Serialize, Clone)]
pub struct Row {
    pub id: i64,
    /// 8 hex characters, at least one a letter, so it never reads as an id.
    pub uid: String,
    /// `task`, or `plan`: a row with no pane that groups tasks.
    pub kind: String,
    /// The plan's uid.
    pub parent: Option<String>,
    /// The full session id. A plan has none.
    pub pane_id: Option<String>,
    /// The pane's generation at the send. See [`SessionInfo::generation`].
    pub pane_gen: Option<i64>,
    pub project: String,
    pub task: String,
    pub sent_at: String,
    pub state: String,
    pub result_path: Option<String>,
    /// Free text that names the work elsewhere. Skiff never reads it.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub updated_at: String,
    /// The task type from `[skiff_agent.tasks]`. Skiff stores it and never acts on it.
    #[serde(rename = "type")]
    pub task_type: Option<String>,
    /// The daemon socket of the pane. A pane id means nothing on another socket.
    pub socket: Option<String>,
    /// When the person was told the result. `None`: not yet. Open work has none.
    pub reported_at: Option<String>,
    /// A short title, 3 to 6 words, for the app's panel. `None`: the panel
    /// takes one from the task text.
    pub title: Option<String>,
    /// When the person closed the work. Only a work head has it: a plan, or a
    /// task with no plan. A task in a plan is closed with its plan. `None`:
    /// the work is open.
    pub closed_at: Option<String>,
    /// [`WORKING`], [`OPEN`] or [`FINISHED`], for the whole work. Set by
    /// [`buckets`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<&'static str>,
    /// An agent is on this task now. Set by the caller from the sessions:
    /// see [`Row::agent_on_in`].
    #[serde(skip)]
    pub agent_on: bool,
    /// The socket is not this skiff's: no check of the pane was made.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub elsewhere: bool,
    /// The pane still exists. Absent when the daemon did not answer, and on a plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<bool>,
    /// The pane restarted since the send, so the task's process is gone.
    /// Absent when the daemon did not answer, and on a plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted: Option<bool>,
}

impl Row {
    pub fn is_plan(&self) -> bool {
        self.kind == "plan"
    }

    /// The socket of a task sent on another daemon than `here`'s. Its pane
    /// cannot be checked from here: it is neither live nor gone.
    pub fn socket_other_than(&self, here: &Path) -> Option<&str> {
        self.socket.as_deref().filter(|s| !self.is_plan() && !state::same_socket(Path::new(s), here))
    }

    /// The task ran in `s`'s pane, and that pane now runs another program
    /// than at the send: a restore brought it back as a new shell. A row
    /// with no generation (from before version 2) is never interrupted.
    pub fn interrupted_in(&self, s: Option<&SessionInfo>) -> bool {
        matches!(self.state.as_str(), "sent" | "blocked")
            && self.pane_gen.is_some()
            && s.and_then(gen).is_some_and(|g| Some(g) != self.pane_gen)
    }

    /// The uid of the work the row belongs to: its plan's, else its own.
    pub fn work(&self) -> &str {
        self.parent.as_deref().unwrap_or(&self.uid)
    }

    /// A sent task whose pane `s` still runs its program, and works or ended
    /// its turn: the result is on its way. A pane that waits for approval,
    /// exited, restarted or is gone needs the person instead.
    pub fn agent_on_in(&self, s: Option<&SessionInfo>) -> bool {
        self.state == "sent"
            && !self.interrupted_in(s)
            && s.is_some_and(|s| s.agent_exit.is_none() && matches!(s.state, SessionState::Working | SessionState::Idle))
    }

    /// The state as `ls` shows it.
    pub fn shown_state(&self) -> &str {
        if self.interrupted == Some(true) {
            INTERRUPTED
        } else if self.live == Some(false) && self.state != "done" {
            GONE
        } else {
            &self.state
        }
    }
}

/// `SKIFF_LEDGER`, else `ledger.db` in the Skiff agent's folder: the
/// `[skiff_agent] folder` override, else `~/skiff-agent`, or the folder in
/// the socket's private state.
pub fn path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("SKIFF_LEDGER") {
        return Ok(PathBuf::from(p));
    }
    Ok(crate::skipper::configured(&skiff_core::config::load()?)?.join("ledger.db"))
}

/// The pane generation as the ledger stores it.
pub fn gen(s: &SessionInfo) -> Option<i64> {
    s.generation().and_then(|g| i64::try_from(g).ok())
}

/// Opens the ledger read-only, for a reader that must not change it: no
/// migration, no new file. A file without `tasks.closed_at` (older than
/// [`VERSION`]) is refused: any `skiff ledger` command brings it up to date.
/// The columns decide, not `user_version`: an older skiff stamps its own
/// lower version on a file that has every column.
pub fn open_read(file: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .with_context(|| format!("open {}", file.display()))?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    let n: i64 = db.query_row("SELECT count(*) FROM pragma_table_info('tasks') WHERE name = 'closed_at'", [], |r| r.get(0))?;
    if n == 0 {
        bail!("the ledger is older than version {VERSION}. Run any `skiff ledger` command to update it");
    }
    Ok(db)
}

pub fn rows(db: &Connection, where_: &str, arg: Option<&dyn rusqlite::ToSql>) -> Result<Vec<Row>> {
    let sql = format!("SELECT {COLUMNS} FROM tasks {where_} ORDER BY id");
    let mut st = db.prepare(&sql)?;
    let map = |r: &rusqlite::Row| {
        Ok(Row {
            id: r.get(0)?,
            uid: r.get(1)?,
            kind: r.get(2)?,
            parent: r.get(3)?,
            pane_id: r.get(4)?,
            pane_gen: r.get(5)?,
            project: r.get(6)?,
            task: r.get(7)?,
            sent_at: r.get(8)?,
            state: r.get(9)?,
            result_path: r.get(10)?,
            reference: r.get(11)?,
            updated_at: r.get(12)?,
            task_type: r.get(13)?,
            socket: r.get(14)?,
            reported_at: r.get(15)?,
            title: r.get(16)?,
            closed_at: r.get(17)?,
            bucket: None,
            agent_on: false,
            elsewhere: false,
            live: None,
            interrupted: None,
        })
    };
    let out = match arg {
        Some(a) => st.query_map([a], map)?.collect::<rusqlite::Result<Vec<_>>>()?,
        None => st.query_map([], map)?.collect::<rusqlite::Result<Vec<_>>>()?,
    };
    Ok(out)
}

/// A plan's state from its tasks in `rows`: `done` once every task is done,
/// `sent` while one is open. `None` for a plan with no task: its own state
/// stands.
pub fn plan_state(uid: &str, rows: &[Row]) -> Option<&'static str> {
    let mut tasks = rows.iter().filter(|r| r.parent.as_deref() == Some(uid)).peekable();
    tasks.peek()?;
    Some(if tasks.all(|t| t.state == "done") { "done" } else { "sent" })
}

/// Sets each plan in `rows` to the state its tasks give it. Never stored.
pub fn roll_up(rows: &mut [Row]) {
    let states: Vec<Option<&str>> = rows.iter().map(|r| if r.is_plan() { plan_state(&r.uid, rows) } else { None }).collect();
    for (r, st) in rows.iter_mut().zip(states) {
        if let Some(st) = st {
            r.state = st.to_string();
        }
    }
}

/// Sets each row's bucket from its work: [`FINISHED`] once the work head is
/// closed, else [`WORKING`] while an agent is on one of its tasks, else
/// [`OPEN`]. Set `agent_on` first.
pub fn buckets(rows: &mut [Row]) {
    use std::collections::HashSet;
    let closed: HashSet<String> = rows.iter().filter(|r| r.closed_at.is_some()).map(|r| r.uid.clone()).collect();
    let working: HashSet<String> = rows.iter().filter(|r| r.agent_on).map(|r| r.work().to_string()).collect();
    for r in rows.iter_mut() {
        r.bucket = Some(if closed.contains(r.work()) {
            FINISHED
        } else if working.contains(r.work()) {
            WORKING
        } else {
            OPEN
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_goes_by_the_columns_and_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("skiff-ledger-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ledger.db");
        // Version 6: no closed_at. Refused.
        let db = Connection::open(&file).unwrap();
        db.execute_batch("CREATE TABLE tasks (id INTEGER PRIMARY KEY, uid TEXT, socket TEXT, reported_at TEXT, title TEXT); PRAGMA user_version = 6;").unwrap();
        assert!(open_read(&file).unwrap_err().to_string().contains("skiff ledger"));
        drop(db);
        std::fs::remove_file(&file).unwrap();
        // Every column, stamped down to 3 by an older skiff. Read.
        let db = Connection::open(&file).unwrap();
        db.execute_batch(&format!(
            "CREATE TABLE tasks (id INTEGER PRIMARY KEY, uid TEXT, kind TEXT, parent TEXT, pane_id TEXT, pane_gen INTEGER, project TEXT,
               task TEXT, sent_at TEXT, state TEXT, result_path TEXT, ref TEXT, updated_at TEXT, type TEXT, socket TEXT, reported_at TEXT, title TEXT, closed_at TEXT);
             INSERT INTO tasks (uid, kind, project, task, sent_at, state, updated_at) VALUES ('a1b2c3d4', 'plan', '', 'p', 'x', 'sent', 'x');
             INSERT INTO tasks (uid, kind, parent, project, task, sent_at, state, updated_at) VALUES ('e5f6a7b8', 'task', 'a1b2c3d4', '', 't', 'x', 'done', 'x');
             PRAGMA user_version = {};",
            3
        ))
        .unwrap();
        let r = open_read(&file).unwrap();
        let mut list = rows(&r, "", None).unwrap();
        roll_up(&mut list);
        assert_eq!(list.iter().map(|r| r.state.as_str()).collect::<Vec<_>>(), ["done", "done"]);
        assert!(r.execute("DELETE FROM tasks", []).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
