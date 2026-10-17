//! `skiff ledger`: the Skiff agent's task record, a SQLite file in its folder.
//! The agent reads and writes it through these commands only, so a bad edit,
//! a compaction or a second writer cannot break it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::Serialize;
use skiff_client::Client;
use skiff_core::session::SessionInfo;

use crate::{print, resolve, sessions, LedgerCmd};

const STATES: [&str; 4] = ["sent", "pending", "blocked", "done"];
/// Shown in place of `sent` or `blocked` when the pane restarted since the
/// send. Never stored: the pane generation tells it.
const INTERRUPTED: &str = "interrupted";
/// Shown in place of an open state when the pane no longer exists. Never
/// stored: the session list tells it.
const GONE: &str = "gone";

/// Version 1: `tasks` with a reusable id and a pane on every row. Version 2
/// adds the uid, plans, the pane generation and the ref, and never reuses an
/// id. Version 3 adds the task type.
const VERSION: i64 = 3;

const TASKS: &str = "
CREATE TABLE tasks (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  uid         TEXT NOT NULL UNIQUE,
  kind        TEXT NOT NULL DEFAULT 'task' CHECK (kind IN ('task', 'plan')),
  parent      TEXT,
  pane_id     TEXT,
  pane_gen    INTEGER,
  project     TEXT NOT NULL DEFAULT '',
  task        TEXT NOT NULL,
  sent_at     TEXT NOT NULL,
  state       TEXT NOT NULL CHECK (state IN ('sent', 'pending', 'blocked', 'done')),
  result_path TEXT,
  ref         TEXT,
  updated_at  TEXT NOT NULL,
  type        TEXT
);
";

/// Every uid the ledger gave out, so a removed row's uid is not given again.
const UIDS: &str = "CREATE TABLE IF NOT EXISTS uids (uid TEXT PRIMARY KEY);";

const COLUMNS: &str = "id, uid, kind, parent, pane_id, pane_gen, project, task, sent_at, state, result_path, ref, updated_at, type";

/// UTC, to the second, set by SQLite.
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%SZ', 'now')";

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

    /// The task ran in `s`'s pane, and that pane now runs another program
    /// than at the send: a restore brought it back as a new shell. A row
    /// with no generation (from before version 2) is never interrupted.
    pub fn interrupted_in(&self, s: Option<&SessionInfo>) -> bool {
        matches!(self.state.as_str(), "sent" | "blocked")
            && self.pane_gen.is_some()
            && s.and_then(gen).is_some_and(|g| Some(g) != self.pane_gen)
    }

    /// The state as `ls` shows it.
    fn shown_state(&self) -> &str {
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
/// `[skiff_agent] folder` override, else `~/skiff-agent`.
pub fn path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("SKIFF_LEDGER") {
        return Ok(PathBuf::from(p));
    }
    let folder = match skiff_core::config::load()?.skiff_agent.folder {
        Some(f) => f,
        None => dirs::home_dir().context("no home folder")?.join("skiff-agent"),
    };
    Ok(folder.join("ledger.db"))
}

/// Opens the ledger, making it on first use. A first use next to an old
/// `ledger.md` imports its rows and keeps the file as `ledger.md.bak`.
fn open() -> Result<Connection> {
    open_at(&path()?)
}

fn open_at(file: &Path) -> Result<Connection> {
    let fresh = !file.exists();
    if let Some(dir) = file.parent() {
        if !dir.is_dir() {
            bail!("{} does not exist. Turn on the Skiff agent in Settings first", dir.display());
        }
    }
    let mut db = Connection::open(file).with_context(|| format!("open {}", file.display()))?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    migrate(&mut db).with_context(|| format!("update {} to ledger version {VERSION}", file.display()))?;
    let md = file.with_file_name("ledger.md");
    if fresh && md.is_file() {
        let n = import(&mut db, &md)?;
        std::fs::rename(&md, md.with_file_name("ledger.md.bak")).context("keep ledger.md as ledger.md.bak")?;
        eprintln!("skiff: imported {n} rows from ledger.md; the old file is ledger.md.bak");
    }
    Ok(db)
}

/// Makes the tables, or brings a version 1 ledger up to this version in one
/// transaction: every row keeps its id and gets a uid. The columns decide,
/// not `user_version`, as an older `skiff` writes version 1 there on each open.
fn migrate(db: &mut Connection) -> Result<()> {
    migrate_v2(db)?;
    add_type(db)
}

/// Version 3: the `type` column, empty on every old row. An older `skiff`
/// names its columns, so it still reads and writes the file.
fn add_type(db: &mut Connection) -> Result<()> {
    let has = |db: &Connection| -> rusqlite::Result<bool> {
        db.query_row("SELECT count(*) FROM pragma_table_info('tasks') WHERE name = 'type'", [], |r| r.get::<_, i64>(0)).map(|n| n > 0)
    };
    if has(db)? {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !has(&tx)? {
        tx.execute_batch("ALTER TABLE tasks ADD COLUMN type TEXT;")?;
        tx.pragma_update(None, "user_version", VERSION)?;
    }
    tx.commit()?;
    Ok(())
}

fn migrate_v2(db: &mut Connection) -> Result<()> {
    let has = |db: &Connection, sql: &str| -> rusqlite::Result<bool> { db.query_row(sql, [], |r| r.get::<_, i64>(0)).map(|n| n > 0) };
    let current = "SELECT count(*) FROM pragma_table_info('tasks') WHERE name = 'uid'";
    if has(db, current)? {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Another process can have migrated while this one waited for the lock.
    if has(&tx, current)? {
        return Ok(());
    }
    let old = has(&tx, "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'tasks'")?;
    if old {
        tx.execute_batch("ALTER TABLE tasks RENAME TO tasks_v1;")?;
    }
    tx.execute_batch(TASKS)?;
    tx.execute_batch(UIDS)?;
    if old {
        let rows: Vec<(i64, String, String, String, String, String, Option<String>, String)> = tx
            .prepare("SELECT id, pane_id, project, task, sent_at, state, result_path, updated_at FROM tasks_v1 ORDER BY id")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, pane, project, task, sent_at, state, result, updated_at) in rows {
            let uid = new_uid(&tx)?;
            tx.execute(
                "INSERT INTO tasks (id, uid, pane_id, project, task, sent_at, state, result_path, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![id, uid, pane, project, task, sent_at, state, result, updated_at],
            )?;
        }
        tx.execute_batch("DROP TABLE tasks_v1;")?;
    }
    tx.pragma_update(None, "user_version", VERSION)?;
    tx.commit()?;
    Ok(())
}

/// A uid no row ever had: 8 hex characters with at least one letter.
fn new_uid(db: &Connection) -> Result<String> {
    loop {
        let uid = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        if uid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if db.execute("INSERT OR IGNORE INTO uids (uid) VALUES (?1)", [&uid])? == 1 {
            return Ok(uid);
        }
    }
}

/// The rows of the Markdown table: pane id, project, task, sent at, state,
/// result. The header and the rule under it are skipped. An unknown state
/// comes in as pending.
fn import(db: &mut Connection, md: &Path) -> Result<usize> {
    let text = std::fs::read_to_string(md).with_context(|| format!("read {}", md.display()))?;
    let tx = db.transaction()?;
    let mut n = 0;
    for line in text.lines().map(str::trim).filter(|l| l.starts_with('|')) {
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(|c| c.trim().trim_matches('`')).collect();
        if cells.len() < 6 || cells[0].eq_ignore_ascii_case("pane id") || cells[0].starts_with("---") || cells[0].is_empty() {
            continue;
        }
        let state = cells[4].to_lowercase();
        let state = if STATES.contains(&state.as_str()) { state } else { "pending".into() };
        let result = Some(cells[5]).filter(|r| !r.is_empty());
        let uid = new_uid(&tx)?;
        tx.execute(
            &format!("INSERT INTO tasks (uid, pane_id, project, task, sent_at, state, result_path, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {NOW})"),
            params![uid, cells[0], cells[1], cells[2], cells[3], state, result],
        )?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

/// Lowercase ASCII words joined by `-`, at most 40 characters. Other
/// characters split words. Empty text gives `task`.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for w in text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()) {
        let w = w.to_ascii_lowercase();
        let sep = usize::from(!out.is_empty());
        if out.len() + sep + w.len() > 40 {
            if out.is_empty() {
                out = w[..40].to_string();
            }
            break;
        }
        if sep == 1 {
            out.push('-');
        }
        out.push_str(&w);
    }
    if out.is_empty() {
        "task".into()
    } else {
        out
    }
}

/// Where a row's result goes when `add` names no path, in `results/` next to
/// the ledger:
///
/// - a plan: `<date>-<uid>-<slug>/plan.md`, a folder of its own;
/// - a task in a plan: `<uid>-<slug>.md` in the plan's folder;
/// - any other task: `<date>-<uid>-<slug>.md`.
///
/// The date is today's, local time. Nothing is made on disk.
fn result_path(ledger: &Path, date: &str, uid: &str, task: &str, plan: bool, parent: Option<&Row>) -> PathBuf {
    let results = ledger.parent().unwrap_or(Path::new(".")).join("results");
    let name = format!("{uid}-{}", slug(task));
    if plan {
        return results.join(format!("{date}-{name}")).join("plan.md");
    }
    match parent {
        Some(p) => plan_folder(&results, p).join(format!("{name}.md")),
        None => results.join(format!("{date}-{name}.md")),
    }
}

/// The folder of the plan's `plan.md`. A plan whose result is elsewhere gets
/// the folder it would have had.
fn plan_folder(results: &Path, p: &Row) -> PathBuf {
    if let Some(dir) = p.result_path.as_deref().map(Path::new).filter(|r| r.file_name() == Some("plan.md".as_ref())).and_then(Path::parent) {
        return dir.to_path_buf();
    }
    results.join(format!("{}-{}-{}", p.sent_at.get(..10).unwrap_or(&p.sent_at), p.uid, slug(&p.task)))
}

/// The open tasks (state sent, pending or blocked), oldest first. Plans are
/// left out. This is the read path for other commands, such as `skiff
/// watch`. No ledger yet (no `ledger.db` and no `ledger.md` to import) gives
/// an empty list, and makes no file.
pub fn open_tasks() -> Result<Vec<Row>> {
    open_tasks_at(&path()?)
}

fn open_tasks_at(file: &Path) -> Result<Vec<Row>> {
    if !file.exists() && !file.with_file_name("ledger.md").is_file() {
        return Ok(Vec::new());
    }
    let db = open_at(file)?;
    rows(&db, "WHERE kind = 'task' AND state IN ('sent', 'pending', 'blocked')", None)
}

fn state(s: &str) -> Result<&str> {
    match STATES.iter().find(|x| **x == s) {
        Some(x) => Ok(x),
        None => bail!("unknown state {s:?}. Use one of: {}", STATES.join(", ")),
    }
}

/// The live sessions, when the daemon answers.
async fn live(socket: &Path) -> Option<Vec<SessionInfo>> {
    let c = Client::connect(socket).await.ok()?;
    c.list_sessions().await.ok()
}

/// A pane as other commands take it, checked against the live sessions.
async fn pane(socket: &Path, arg: &str) -> Result<SessionInfo> {
    let c = crate::connect(socket).await?;
    let all = c.list_sessions().await?;
    resolve::session(&all, arg).cloned()
}

/// The project whose folder holds the pane's folder, else the folder's name.
fn project_of(s: &SessionInfo) -> String {
    let cfg = skiff_core::config::load().ok();
    cfg.iter()
        .flat_map(|c| &c.projects)
        .filter(|p| s.cwd.starts_with(&p.path))
        .max_by_key(|p| p.path.components().count())
        .map(|p| p.name.clone())
        .unwrap_or_else(|| s.cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

fn rows(db: &Connection, where_: &str, arg: Option<&dyn rusqlite::ToSql>) -> Result<Vec<Row>> {
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

/// The row `arg` names: its id (digits only) or its uid.
fn one(db: &Connection, arg: &str) -> Result<Row> {
    let found = if !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit()) {
        rows(db, "WHERE id = ?1", Some(&arg.parse::<i64>().context("id out of range")?))?
    } else {
        rows(db, "WHERE uid = ?1", Some(&arg.to_ascii_lowercase()))?
    };
    found.into_iter().next().with_context(|| format!("no ledger row {arg}"))
}

/// The plan `arg` names. A task is no plan.
fn plan(db: &Connection, arg: &str) -> Result<Row> {
    let p = one(db, arg)?;
    if !p.is_plan() {
        bail!("ledger row {arg} is a task, not a plan. Make one with `skiff ledger add --plan --task \"...\"`");
    }
    Ok(p)
}

/// The pane generation as the ledger stores it.
fn gen(s: &SessionInfo) -> Option<i64> {
    s.generation().and_then(|g| i64::try_from(g).ok())
}

/// Fills in `live` and `interrupted` from the sessions, when the daemon answers.
async fn mark_live(socket: &Path, rows: &mut [Row]) {
    if let Some(all) = live(socket).await {
        for r in rows.iter_mut().filter(|r| !r.is_plan()) {
            // Rows imported from ledger.md can hold a short id or a name.
            let s = r.pane_id.as_deref().and_then(|p| resolve::session(&all, p).ok());
            r.live = Some(s.is_some());
            r.interrupted = Some(r.interrupted_in(s));
        }
    }
}

/// A plan's state from its tasks in `rows`: `done` once every task is done,
/// `sent` while one is open. `None` for a plan with no task: its own state
/// stands.
fn plan_state(uid: &str, rows: &[Row]) -> Option<&'static str> {
    let mut tasks = rows.iter().filter(|r| r.parent.as_deref() == Some(uid)).peekable();
    tasks.peek()?;
    Some(if tasks.all(|t| t.state == "done") { "done" } else { "sent" })
}

/// Sets each plan in `rows` to the state its tasks give it. Never stored.
fn roll_up(rows: &mut [Row]) {
    let states: Vec<Option<&str>> = rows.iter().map(|r| if r.is_plan() { plan_state(&r.uid, rows) } else { None }).collect();
    for (r, st) in rows.iter_mut().zip(states) {
        if let Some(st) = st {
            r.state = st.to_string();
        }
    }
}

/// Each plan with its tasks under it, by id. A task whose plan is not in
/// `rows` stands alone. The bool marks a task shown under its plan.
fn tree(rows: Vec<Row>) -> Vec<(Row, bool)> {
    let plans: std::collections::HashSet<String> = rows.iter().filter(|r| r.is_plan()).map(|r| r.uid.clone()).collect();
    let under = |r: &Row| r.parent.as_ref().is_some_and(|p| plans.contains(p));
    let (children, top): (Vec<Row>, Vec<Row>) = rows.into_iter().partition(under);
    let mut out = Vec::new();
    for r in top {
        let uid = r.uid.clone();
        out.push((r, false));
        out.extend(children.iter().filter(|c| c.parent.as_deref() == Some(&uid)).cloned().map(|c| (c, true)));
    }
    out
}

/// A result path as the table shows it: from the agent's folder when it is in
/// there, else with `~`.
fn shown_path(p: &str) -> String {
    let p = Path::new(p);
    let base = path().ok().and_then(|l| l.parent().map(Path::to_path_buf));
    match base.as_deref().and_then(|b| p.strip_prefix(b).ok()) {
        Some(rel) => rel.display().to_string(),
        None => print::tilde(p),
    }
}

fn print_rows(rows: &[(Row, bool)]) {
    let yn = |l: Option<bool>| match l {
        Some(true) => "yes",
        Some(false) => "no",
        None => "?",
    };
    let refs = rows.iter().any(|(r, _)| r.reference.is_some());
    let types = rows.iter().any(|(r, _)| r.task_type.is_some());
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|(r, child)| {
            let mut row = vec![
                if *child { format!("└ {}", r.id) } else { r.id.to_string() },
                r.uid.clone(),
                r.shown_state().to_string(),
                if r.is_plan() { "plan".into() } else { print::short_id(r.pane_id.as_deref().unwrap_or("")).to_string() },
                if r.is_plan() { "-".into() } else { yn(r.live).to_string() },
                r.project.clone(),
                r.sent_at.clone(),
                r.result_path.as_deref().map(shown_path).unwrap_or_default(),
            ];
            if refs {
                row.push(r.reference.clone().unwrap_or_default());
            }
            if types {
                row.push(r.task_type.clone().unwrap_or_default());
            }
            row.push(r.task.clone());
            row
        })
        .collect();
    let mut header = vec!["ID", "UID", "STATE", "PANE", "LIVE", "PROJECT", "SENT", "RESULT"];
    if refs {
        header.push("REF");
    }
    if types {
        header.push("TYPE");
    }
    header.push("TASK");
    print::table(&header, &table);
}

/// One row, a field per line, as `show` prints it.
fn print_one(r: &Row) {
    let yn = |l: Option<bool>| l.map_or("?", |l| if l { "yes" } else { "no" });
    let mut f: Vec<(&str, String)> = vec![("id", r.id.to_string()), ("uid", r.uid.clone()), ("kind", r.kind.clone()), ("state", r.shown_state().to_string())];
    if let Some(p) = &r.parent {
        f.push(("plan", p.clone()));
    }
    if !r.is_plan() {
        f.push(("pane", r.pane_id.clone().unwrap_or_default()));
        f.push(("live", yn(r.live).into()));
    }
    f.extend([("project", r.project.clone()), ("sent", r.sent_at.clone()), ("updated", r.updated_at.clone())]);
    if let Some(p) = &r.result_path {
        f.push(("result", p.clone()));
    }
    if let Some(x) = &r.reference {
        f.push(("ref", x.clone()));
    }
    if let Some(x) = &r.task_type {
        f.push(("type", x.clone()));
    }
    f.push(("task", r.task.clone()));
    print::table(&["", ""], &f.into_iter().map(|(k, v)| vec![k.to_string(), v]).collect::<Vec<_>>());
}

/// A row for [`insert`].
struct New {
    plan: bool,
    /// The plan's id or uid.
    parent: Option<String>,
    pane_id: Option<String>,
    pane_gen: Option<i64>,
    project: String,
    task: String,
    state: String,
    result: Option<String>,
    reference: Option<String>,
    task_type: Option<String>,
}

/// Adds a row with a new uid, and its default result path unless it names
/// one. Returns the id, the uid and the result path.
fn insert(db: &mut Connection, file: &Path, n: New) -> Result<(i64, String, String)> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let parent = n.parent.as_deref().map(|p| plan(&tx, p)).transpose()?;
    let uid = new_uid(&tx)?;
    let result = match n.result {
        Some(r) => r,
        None => {
            let date: String = tx.query_row("SELECT date('now', 'localtime')", [], |r| r.get(0))?;
            result_path(file, &date, &uid, &n.task, n.plan, parent.as_ref()).display().to_string()
        }
    };
    tx.execute(
        &format!(
            "INSERT INTO tasks (uid, kind, parent, pane_id, pane_gen, project, task, sent_at, state, result_path, ref, updated_at, type)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {NOW}, ?8, ?9, ?10, {NOW}, ?11)"
        ),
        params![uid, if n.plan { "plan" } else { "task" }, parent.map(|p| p.uid), n.pane_id, n.pane_gen, n.project, n.task, n.state, result, n.reference, n.task_type],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok((id, uid, result))
}

/// What `ledger send` and `ledger add --send` type, and how.
pub struct SendOpts {
    /// Default: the task text and its result path.
    pub text: Vec<String>,
    pub enter: bool,
    pub shell: bool,
}

/// Why `row`, whose pane is `s` now, cannot be sent: only a pending task, or
/// one a restart interrupted, has a send to make.
fn unsendable(row: &Row, s: Option<&SessionInfo>) -> Option<String> {
    if row.is_plan() {
        return Some(format!("ledger row {} is a plan: it has no pane to send to", row.uid));
    }
    if row.state != "pending" && !row.interrupted_in(s) {
        return Some(format!("task {} is {}, not pending or interrupted. Nothing was sent", row.uid, row.state));
    }
    s.is_none().then(|| format!("the pane of task {} is gone. Nothing was sent", row.uid))
}

/// The text a task goes out with when no text is given.
fn message(row: &Row) -> String {
    let task = row.task.trim();
    match row.result_path.as_deref() {
        Some(p) => format!("{task} Write your result to {p}."),
        None => task.to_string(),
    }
}

/// The state a task takes after `send` exits with `code`. A refusal typed
/// nothing, so the task stays pending. Exit 8 typed the text but started no
/// turn: the pane needs the person.
fn state_after(code: i32) -> Option<&'static str> {
    match code {
        0 => Some("sent"),
        8 => Some("blocked"),
        _ => None,
    }
}

/// Sends the pending task `row` to its pane with `send`'s checks, then
/// records the outcome. Returns `send`'s exit code.
async fn send_row(socket: &Path, db: &Connection, row: &Row, o: SendOpts) -> Result<i32> {
    let c = crate::connect(socket).await?;
    let all = c.list_sessions().await?;
    let s = row.pane_id.as_deref().and_then(|p| resolve::session(&all, p).ok());
    if let Some(why) = unsendable(row, s) {
        bail!(why);
    }
    let s = s.expect("checked");
    let text = if o.text.is_empty() { vec![message(row)] } else { o.text };
    let code = sessions::send_to(&c, s, sessions::Send { text: &text, enter: o.enter, raw: false, shell: o.shell }).await?;
    match state_after(code) {
        Some(st) => {
            // Only from the state read: a send that raced this one keeps its own.
            db.execute(
                &format!("UPDATE tasks SET state = ?2, pane_gen = ?3, updated_at = {NOW} WHERE id = ?1 AND state = ?4"),
                params![row.id, st, gen(s), row.state],
            )?;
            if st != "sent" {
                eprintln!("skiff: task {} is {st}", row.uid);
            }
        }
        None => eprintln!("skiff: task {} stays {}", row.uid, row.state),
    }
    Ok(code)
}

/// The state `ledger add` stores. `--send` adds the task as pending and
/// sends it. Else a task waits for `ledger send`, and a plan is sent.
fn add_state(st: Option<&str>, plan: bool, send: bool) -> Result<&str> {
    match st {
        _ if send => Ok("pending"),
        Some(s) => state(s),
        None if plan => Ok("sent"),
        None => Ok("pending"),
    }
}

/// The exit code: `send`'s for `send` and `add --send`, else 0.
pub async fn run(socket: &Path, cmd: LedgerCmd) -> Result<i32> {
    match cmd {
        LedgerCmd::Add { pane: p, task, plan: is_plan, parent, project, state: st, result, reference, task_type, send, enter, shell } => {
            let task_type = task_type.as_deref().map(skiff_core::tasks::check_name).transpose()?.map(str::to_string);
            let st = add_state(st.as_deref(), is_plan, send)?.to_string();
            let (pane_id, pane_gen, project) = match p {
                Some(p) if !is_plan => {
                    let s = pane(socket, &p).await?;
                    let project = project.unwrap_or_else(|| project_of(&s));
                    (Some(s.id.clone()), gen(&s), project)
                }
                _ => (None, None, project.unwrap_or_default()),
            };
            let file = path()?;
            let new = New { plan: is_plan, parent, pane_id, pane_gen, project, task, state: st, result, reference, task_type };
            let mut db = open_at(&file)?;
            let (id, uid, result) = insert(&mut db, &file, new)?;
            println!("id {id}\nuid {uid}\nresult {result}");
            if send {
                let row = one(&db, &uid)?;
                return send_row(socket, &db, &row, SendOpts { text: Vec::new(), enter, shell }).await;
            }
        }
        LedgerCmd::Send { id, text, enter, shell } => {
            let db = open()?;
            let row = one(&db, &id)?;
            return send_row(socket, &db, &row, SendOpts { text, enter, shell }).await;
        }
        LedgerCmd::Update { id, state: st, result, task, pane: p, project, parent, reference, task_type } => {
            let st = st.as_deref().map(state).transpose()?;
            let task_type = task_type.as_deref().map(skiff_core::tasks::check_name).transpose()?;
            let db = open()?;
            let row = one(&db, &id)?;
            if row.is_plan() && (p.is_some() || parent.is_some()) {
                bail!("ledger row {id} is a plan: it has no pane and no plan above it");
            }
            // A new pane, or a new send to the same one, starts from the
            // pane's program as it is now.
            let (pane_id, gen) = match (&p, st) {
                (Some(p), _) => {
                    let s = pane(socket, p).await?;
                    (Some(s.id.clone()), Some(gen(&s)))
                }
                (None, Some("sent")) if !row.is_plan() => {
                    let all = live(socket).await.unwrap_or_default();
                    let s = row.pane_id.as_deref().and_then(|p| resolve::session(&all, p).ok());
                    (None, s.map(gen))
                }
                _ => (None, None),
            };
            let parent = parent.as_deref().map(|p| plan(&db, p)).transpose()?.map(|p| p.uid);
            db.execute(
                &format!(
                    "UPDATE tasks SET state = coalesce(?2, state), result_path = coalesce(?3, result_path), task = coalesce(?4, task),
                     pane_id = coalesce(?5, pane_id), project = coalesce(?6, project), parent = coalesce(?7, parent), ref = coalesce(?8, ref),
                     pane_gen = CASE WHEN ?9 THEN ?10 ELSE pane_gen END, type = coalesce(?11, type), updated_at = {NOW} WHERE id = ?1"
                ),
                params![row.id, st, result, task, pane_id, project, parent, reference, gen.is_some(), gen.flatten(), task_type],
            )?;
        }
        LedgerCmd::Ls { state: st, json } => {
            if let Some(s) = st.as_deref().filter(|s| *s != INTERRUPTED && *s != GONE) {
                state(s)?;
            }
            let db = open()?;
            let mut list = rows(&db, "", None)?;
            mark_live(socket, &mut list).await;
            roll_up(&mut list);
            if let Some(s) = &st {
                list.retain(|r| r.shown_state() == s);
            }
            let list = tree(list);
            if json {
                print::json(&list.iter().map(|(r, _)| r).collect::<Vec<_>>())?;
            } else {
                print_rows(&list);
            }
        }
        LedgerCmd::Show { id, json } => {
            let db = open()?;
            let mut r = [one(&db, &id)?];
            mark_live(socket, &mut r).await;
            if r[0].is_plan() {
                let tasks = rows(&db, "WHERE parent = ?1", Some(&r[0].uid))?;
                if let Some(st) = plan_state(&r[0].uid, &tasks) {
                    r[0].state = st.to_string();
                }
            }
            if json {
                print::json(&r[0])?;
            } else {
                print_one(&r[0]);
            }
        }
        LedgerCmd::Rm { id } => {
            let db = open()?;
            let row = one(&db, &id)?;
            if row.is_plan() {
                let n: i64 = db.query_row("SELECT count(*) FROM tasks WHERE parent = ?1", [&row.uid], |r| r.get(0))?;
                if n > 0 {
                    bail!("plan {id} still has {n} tasks. Remove them first");
                }
            }
            // The row only: a result file stays where it is.
            db.execute("DELETE FROM tasks WHERE id = ?1", [row.id])?;
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_ledger_md_and_reads_only_open_rows() {
        let dir = std::env::temp_dir().join(format!("skiff-ledger-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("ledger.db");
        assert!(open_tasks_at(&db).unwrap().is_empty() && !db.exists());
        std::fs::write(
            dir.join("ledger.md"),
            "# Ledger\n\n| Pane id | Project | Task | Sent at | State | Result |\n|---|---|---|---|---|---|\n| abc12345 | app | fix bug | 10:00 | sent | |\n| def67890 | web | seo | 10:01 | done | r.md |\n| 1111aaaa | api | docs | 10:02 | waiting | |\n",
        )
        .unwrap();
        let t = open_tasks_at(&db).unwrap();
        assert_eq!(t.iter().map(|r| (r.pane_id.as_deref().unwrap(), r.state.as_str())).collect::<Vec<_>>(), [("abc12345", "sent"), ("1111aaaa", "pending")]);
        assert!(dir.join("ledger.md.bak").is_file() && !dir.join("ledger.md").exists());
        // A second read does not import again.
        assert_eq!(open_tasks_at(&db).unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn add_defaults_a_task_to_pending_and_a_plan_to_sent() {
        assert_eq!(add_state(None, false, false).unwrap(), "pending");
        assert_eq!(add_state(None, true, false).unwrap(), "sent");
        assert_eq!(add_state(Some("done"), false, false).unwrap(), "done");
        assert_eq!(add_state(None, false, true).unwrap(), "pending");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("skiff-ledger-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn task(t: &str) -> New {
        New {
            plan: false,
            parent: None,
            pane_id: Some("abc123456789".into()),
            pane_gen: Some(1000),
            project: "app".into(),
            task: t.into(),
            state: "sent".into(),
            result: None,
            reference: None,
            task_type: None,
        }
    }

    #[test]
    fn a_version_2_ledger_gets_the_type_column_and_add_stores_the_type() {
        let dir = scratch("type");
        let file = dir.join("ledger.db");
        {
            let db = Connection::open(&file).unwrap();
            // The schema as version 2 made it: no type.
            db.execute_batch(&TASKS.replace(",\n  type        TEXT", "")).unwrap();
            db.execute_batch(UIDS).unwrap();
            db.execute_batch(
                "PRAGMA user_version = 2;
                 INSERT INTO tasks (uid, pane_id, project, task, sent_at, state, updated_at) VALUES ('a1b2c3d4', 'aaa', 'app', 'old', 'x', 'sent', 'x');",
            )
            .unwrap();
            assert!(rows(&db, "", None).is_err(), "the v2 file has no type column");
        }
        let mut db = open_at(&file).unwrap();
        assert_eq!(rows(&db, "", None).unwrap()[0].task_type, None);
        let v: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, VERSION);
        let (id, _, _) = insert(&mut db, &file, New { task_type: Some("review".into()), ..task("review the parser") }).unwrap();
        let r = one(&db, &id.to_string()).unwrap();
        assert_eq!(r.task_type.as_deref(), Some("review"));
        assert_eq!(serde_json::to_value(&r).unwrap()["type"], "review");
        // A second open changes nothing.
        drop(db);
        assert_eq!(one(&open_at(&file).unwrap(), &id.to_string()).unwrap().task_type.as_deref(), Some("review"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_version_1_ledger_migrates_in_place() {
        let dir = scratch("migrate");
        let file = dir.join("ledger.db");
        {
            let db = Connection::open(&file).unwrap();
            // The schema as version 1 made it.
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS tasks (
                  id INTEGER PRIMARY KEY, pane_id TEXT NOT NULL, project TEXT NOT NULL DEFAULT '', task TEXT NOT NULL,
                  sent_at TEXT NOT NULL, state TEXT NOT NULL CHECK (state IN ('sent', 'pending', 'blocked', 'done')),
                  result_path TEXT, updated_at TEXT NOT NULL);
                 PRAGMA user_version = 1;
                 INSERT INTO tasks VALUES (1, 'aaa', 'app', 'one', '2026-10-01T10:00:00Z', 'sent', NULL, '2026-10-01T10:00:00Z');
                 INSERT INTO tasks VALUES (2, 'bbb', 'web', 'two', '2026-10-01T11:00:00Z', 'done', 'results/two.md', '2026-10-01T12:00:00Z');
                 INSERT INTO tasks VALUES (4, 'ccc', 'api', 'four', '2026-10-01T13:00:00Z', 'blocked', NULL, '2026-10-01T13:00:00Z');",
            )
            .unwrap();
        }
        let db = open_at(&file).unwrap();
        let all = rows(&db, "", None).unwrap();
        let kept: Vec<_> = all.iter().map(|r| (r.id, r.pane_id.as_deref().unwrap(), r.task.as_str(), r.state.as_str(), r.result_path.as_deref(), r.sent_at.as_str())).collect();
        assert_eq!(
            kept,
            [
                (1, "aaa", "one", "sent", None, "2026-10-01T10:00:00Z"),
                (2, "bbb", "two", "done", Some("results/two.md"), "2026-10-01T11:00:00Z"),
                (4, "ccc", "four", "blocked", None, "2026-10-01T13:00:00Z"),
            ]
        );
        assert_eq!(all[1].project, "web");
        assert!(all.iter().all(|r| r.uid.len() == 8 && r.kind == "task" && r.pane_gen.is_none()));
        let uids: std::collections::HashSet<_> = all.iter().map(|r| &r.uid).collect();
        assert_eq!(uids.len(), 3);
        let v: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, VERSION);
        // An old row has no generation, so it is never interrupted.
        assert!(!all[0].interrupted_in(None));
        // A second open, and an older skiff resetting user_version, change nothing.
        db.pragma_update(None, "user_version", 1).unwrap();
        drop(db);
        let db = open_at(&file).unwrap();
        assert_eq!(rows(&db, "", None).unwrap().iter().map(|r| r.uid.clone()).collect::<Vec<_>>(), all.iter().map(|r| r.uid.clone()).collect::<Vec<_>>());
        // The next id follows the highest migrated one.
        drop(db);
        let (id, _, _) = insert(&mut open_at(&file).unwrap(), &file, task("five")).unwrap();
        assert_eq!(id, 5);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn delete_and_add_reuse_no_id_and_no_uid() {
        let dir = scratch("reuse");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut last = 0;
        for i in 0..20 {
            let (id, uid, _) = insert(&mut db, &file, task(&format!("t{i}"))).unwrap();
            assert!(id > last, "id {id} after {last}");
            assert!(seen.insert(uid.clone()), "uid {uid} again");
            assert!(uid.len() == 8 && uid.bytes().all(|b| b.is_ascii_hexdigit()) && !uid.bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(one(&db, &uid).unwrap().id, id);
            assert_eq!(one(&db, &id.to_string()).unwrap().uid, uid);
            last = id;
            // The newest row goes, as `skiff ledger rm` does it.
            db.execute("DELETE FROM tasks WHERE id = ?1", [id]).unwrap();
        }
        assert!(one(&db, &last.to_string()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_plan_lists_its_tasks_under_it_with_paths_in_its_folder() {
        let dir = scratch("plan");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let date: String = db.query_row("SELECT date('now', 'localtime')", [], |r| r.get(0)).unwrap();
        let (_, lone, lone_path) = insert(&mut db, &file, task("Fix the login bug!")).unwrap();
        let plan_row = New { plan: true, pane_id: None, pane_gen: None, reference: Some("JIRA-12".into()), ..task("Release 0.7: notes & builds") };
        let (pid, puid, plan_path) = insert(&mut db, &file, plan_row).unwrap();
        let (_, a, a_path) = insert(&mut db, &file, New { parent: Some(puid.clone()), ..task("Write the release notes") }).unwrap();
        let (_, b, b_path) = insert(&mut db, &file, New { parent: Some(pid.to_string()), ..task("Build the Linux packages") }).unwrap();
        let results = dir.join("results");
        assert_eq!(lone_path, results.join(format!("{date}-{lone}-fix-the-login-bug.md")).display().to_string());
        let folder = results.join(format!("{date}-{puid}-release-0-7-notes-builds"));
        assert_eq!(plan_path, folder.join("plan.md").display().to_string());
        assert_eq!(a_path, folder.join(format!("{a}-write-the-release-notes.md")).display().to_string());
        assert_eq!(b_path, folder.join(format!("{b}-build-the-linux-packages.md")).display().to_string());
        // `add` makes no file.
        assert!(!results.exists());
        // A task is no plan.
        assert!(insert(&mut db, &file, New { parent: Some(lone.clone()), ..task("x") }).is_err());

        let shown: Vec<_> = tree(rows(&db, "", None).unwrap()).into_iter().map(|(r, child)| (r.uid, child)).collect();
        assert_eq!(shown, [(lone.clone(), false), (puid.clone(), false), (a, true), (b, true)]);
        let p = one(&db, &puid).unwrap();
        assert!(p.is_plan() && p.pane_id.is_none() && p.reference.as_deref() == Some("JIRA-12"));
        // The watcher gets the open tasks, not the plan.
        assert_eq!(open_tasks_at(&file).unwrap().len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn slugs_are_short_lowercase_ascii() {
        assert_eq!(slug("Fix the Login bug (again)!"), "fix-the-login-bug-again");
        assert_eq!(slug("Café — über naïve"), "caf-ber-na-ve");
        assert_eq!(slug("   "), "task");
        let long = slug("one two three four five six seven eight nine ten");
        assert!(long.len() <= 40 && !long.ends_with('-'), "{long}");
        assert_eq!(long, "one-two-three-four-five-six-seven-eight");
        assert_eq!(slug(&"x".repeat(60)).len(), 40);
    }

    #[test]
    fn a_restarted_pane_interrupts_a_running_task() {
        let dir = scratch("gen");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        insert(&mut db, &file, task("t")).unwrap();
        let mut r = rows(&db, "", None).unwrap().remove(0);
        std::fs::remove_dir_all(&dir).unwrap();
        let s = |started_at: u64| -> SessionInfo {
            serde_json::from_value(serde_json::json!({
                "id": "abc123456789", "label": "x", "role": "agent", "cwd": "/", "command": "claude", "args": [],
                "state": "idle", "cols": 80, "rows": 24, "pid": 1, "exit_code": null, "started_at": started_at,
            }))
            .unwrap()
        };
        // The same program: a reload keeps the start time.
        assert!(!r.interrupted_in(Some(&s(1000))));
        // A restore starts a new shell.
        assert!(r.interrupted_in(Some(&s(2000))));
        // A pane that is gone is LIVE no, not interrupted.
        assert!(!r.interrupted_in(None));
        // A daemon too old to send a start time tells nothing.
        assert!(!r.interrupted_in(Some(&s(0))));
        // Only work that ran: a pending task was never delivered.
        r.state = "pending".into();
        assert!(!r.interrupted_in(Some(&s(2000))));
        r.state = "blocked".into();
        assert!(r.interrupted_in(Some(&s(2000))));
    }

    #[test]
    fn only_a_pending_task_is_sent_and_a_refusal_keeps_it_pending() {
        let dir = scratch("send");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, uid, result) = insert(&mut db, &file, New { state: "pending".into(), ..task("fix the tests") }).unwrap();
        let (_, plan_uid, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("p") }).unwrap();
        let mut r = one(&db, &uid).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let pane = |started_at: u64| -> SessionInfo {
            serde_json::from_value(serde_json::json!({
                "id": "abc123456789", "label": "x", "role": "agent", "cwd": "/", "command": "claude", "args": [],
                "state": "idle", "cols": 80, "rows": 24, "pid": 1, "exit_code": null, "started_at": started_at,
            }))
            .unwrap()
        };
        let (same, restarted) = (pane(1000), pane(2000));
        assert_eq!(unsendable(&r, Some(&same)), None);
        assert!(unsendable(&r, None).unwrap().contains("gone"));
        assert_eq!(message(&r), format!("fix the tests Write your result to {result}."));
        r.state = "sent".into();
        assert!(unsendable(&r, Some(&same)).unwrap().contains("not pending"));
        // A restart interrupted it: it can go again.
        assert_eq!(unsendable(&r, Some(&restarted)), None);
        assert!(unsendable(&one(&db, &plan_uid).unwrap(), Some(&same)).unwrap().contains("plan"));
        assert_eq!(state_after(0), Some("sent"));
        assert_eq!(state_after(8), Some("blocked"));
        for refused in [1, 3, 4, 5, 6, 7, 9] {
            assert_eq!(state_after(refused), None);
        }
    }

    #[test]
    fn a_gone_pane_shows_gone_and_a_plan_follows_its_tasks() {
        let dir = scratch("roll");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, p, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("p") }).unwrap();
        let (_, a, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..task("a") }).unwrap();
        insert(&mut db, &file, New { parent: Some(p.clone()), state: "done".into(), ..task("b") }).unwrap();
        let (_, empty, _) = insert(&mut db, &file, New { plan: true, pane_id: None, state: "blocked".into(), ..task("e") }).unwrap();
        let mut list = rows(&db, "", None).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let st = |list: &[Row], uid: &str| list.iter().find(|r| r.uid == uid).unwrap().shown_state().to_string();
        roll_up(&mut list);
        assert_eq!(st(&list, &p), "sent");
        // A plan with no task keeps the state it was given.
        assert_eq!(st(&list, &empty), "blocked");
        let ra = list.iter_mut().find(|r| r.uid == a).unwrap();
        ra.live = Some(false);
        assert_eq!(ra.shown_state(), GONE);
        ra.state = "done".into();
        // Done work stays done when its pane closes.
        assert_eq!(ra.shown_state(), "done");
        roll_up(&mut list);
        assert_eq!(st(&list, &p), "done");
    }
}
