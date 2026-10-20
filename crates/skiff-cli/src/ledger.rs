//! `skiff ledger`: the Skiff agent's task record, a SQLite file in its folder.
//! The agent reads and writes it through these commands only, so a bad edit,
//! a compaction or a second writer cannot break it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, TransactionBehavior};
use skiff_client::Client;
use skiff_core::{
    session::{Role, SessionInfo},
    state,
};

use crate::{print, resolve, sessions, LedgerCmd};
pub use skiff_client::ledger::{path, Row};
use skiff_client::ledger::{gen, plan_state, roll_up, rows, GONE, INTERRUPTED, VERSION};

const STATES: [&str; 4] = ["sent", "pending", "blocked", "done"];
const TASKS: &str = "
CREATE TABLE IF NOT EXISTS tasks (
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
  type        TEXT,
  socket      TEXT,
  reported_at TEXT
);
";

/// The one Skiff agent session that changes this ledger, and its socket.
/// `skiff skipper` and the agent's `skiff watch` record it.
const OWNER: &str = "
CREATE TABLE IF NOT EXISTS owner (
  id      INTEGER PRIMARY KEY CHECK (id = 1),
  session TEXT NOT NULL,
  socket  TEXT NOT NULL,
  since   TEXT NOT NULL
);
";

/// Every uid the ledger gave out, so a removed row's uid is not given again.
const UIDS: &str = "CREATE TABLE IF NOT EXISTS uids (uid TEXT PRIMARY KEY);";

/// UTC, to the second, set by SQLite.
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%SZ', 'now')";

/// Opens the ledger, making it on first use.
fn open() -> Result<Connection> {
    open_at(&path()?)
}

fn open_at(file: &Path) -> Result<Connection> {
    if let Some(dir) = file.parent() {
        if !dir.is_dir() {
            bail!("{} does not exist. Turn on the Skiff agent in Settings first", dir.display());
        }
    }
    let mut db = connect(file)?;
    if outdated(&db)? {
        drop(db);
        let old = file.with_extension("db.old");
        std::fs::rename(file, &old).with_context(|| format!("move {} aside", file.display()))?;
        for ext in ["db-wal", "db-shm"] {
            let _ = std::fs::remove_file(file.with_extension(ext));
        }
        eprintln!("skiff: the ledger had an old format; it is now {}, and a new ledger starts empty", old.display());
        db = connect(file)?;
    }
    db.execute_batch(TASKS)?;
    db.execute_batch(UIDS)?;
    db.execute_batch(OWNER)?;
    migrate(&mut db)?;
    // Only up: an older skiff that opens the file stamps its own version, and
    // must not undo a newer one's. The columns, not the stamp, tell the shape.
    let v: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if v < VERSION {
        db.pragma_update(None, "user_version", VERSION)?;
    }
    Ok(db)
}

/// Each step adds one column in place, when the file lacks it:
///
/// - 3 to 4: `socket`. Every row before it went out on the live daemon, the
///   only one with a Skiff agent.
/// - 4 to 5: `reported_at`. Every row stays not reported: the migration
///   cannot tell what the person saw.
fn migrate(db: &mut Connection) -> Result<()> {
    add_column(db, "socket", |tx| {
        tx.execute("UPDATE tasks SET socket = ?1 WHERE kind = 'task'", [state::live_socket().display().to_string()])?;
        Ok(())
    })?;
    add_column(db, "reported_at", |_| Ok(()))
}

/// Adds the column `name TEXT` to `tasks` and runs `fill`, unless it is
/// there. One transaction, so two first opens at once add it once.
fn add_column(db: &mut Connection, name: &str, fill: impl FnOnce(&Connection) -> Result<()>) -> Result<()> {
    let has = |db: &Connection| -> rusqlite::Result<bool> {
        db.query_row("SELECT count(*) FROM pragma_table_info('tasks') WHERE name = ?1", [name], |r| r.get::<_, i64>(0)).map(|n| n > 0)
    };
    if has(db)? {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !has(&tx)? {
        tx.execute_batch(&format!("ALTER TABLE tasks ADD COLUMN {name} TEXT"))?;
        fill(&tx)?;
    }
    tx.commit()?;
    Ok(())
}

fn connect(file: &Path) -> Result<Connection> {
    let db = Connection::open(file).with_context(|| format!("open {}", file.display()))?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    Ok(db)
}

/// A `tasks` table from before version 3, which this build does not read.
fn outdated(db: &Connection) -> Result<bool> {
    let cols: i64 = db.query_row("SELECT count(*) FROM pragma_table_info('tasks') WHERE name IN ('uid', 'type')", [], |r| r.get(0))?;
    let table: i64 = db.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'tasks'", [], |r| r.get(0))?;
    Ok(table > 0 && cols < 2)
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
/// watch`. No `ledger.db` yet gives an empty list, and makes no file.
pub fn open_tasks() -> Result<Vec<Row>> {
    open_tasks_at(&path()?)
}

fn open_tasks_at(file: &Path) -> Result<Vec<Row>> {
    if !file.exists() {
        return Ok(Vec::new());
    }
    let db = open_at(file)?;
    rows(&db, "WHERE kind = 'task' AND state IN ('sent', 'pending', 'blocked')", None)
}

/// The done tasks the person was not told of yet. `skiff watch` names the
/// count at its start. No `ledger.db` yet gives 0, and makes no file.
pub fn unreported() -> Result<usize> {
    unreported_at(&path()?)
}

fn unreported_at(file: &Path) -> Result<usize> {
    if !file.exists() {
        return Ok(0);
    }
    let db = open_at(file)?;
    let n: i64 = db.query_row("SELECT count(*) FROM tasks WHERE kind = 'task' AND state = 'done' AND reported_at IS NULL", [], |r| r.get(0))?;
    Ok(usize::try_from(n).unwrap_or(0))
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

/// Fills in `live` and `interrupted` from the sessions, when the daemon
/// answers. A task of another socket is marked `elsewhere` and not checked.
async fn mark_live(socket: &Path, rows: &mut [Row]) {
    for r in rows.iter_mut() {
        r.elsewhere = r.socket_other_than(socket).is_some();
    }
    if let Some(all) = live(socket).await {
        for r in rows.iter_mut().filter(|r| !r.is_plan() && !r.elsewhere) {
            let s = r.pane_id.as_deref().and_then(|p| resolve::session(&all, p).ok());
            r.live = Some(s.is_some());
            r.interrupted = Some(r.interrupted_in(s));
        }
    }
}

/// Which rows `ls` lists.
#[derive(Debug, Clone, Copy, PartialEq)]
enum View {
    /// Open work, and done work the person was not told of.
    Open,
    All,
    /// Only the rows the person was told of.
    Reported,
}

impl View {
    fn takes(self, r: &Row) -> bool {
        match self {
            View::Open => r.state != "done" || r.reported_at.is_none(),
            View::All => true,
            View::Reported => r.reported_at.is_some(),
        }
    }
}

/// Keeps the rows in `view`. A plan stays while one of its tasks stays; a
/// plan with no task stands on its own state and stamp. Run it after
/// [`roll_up`].
fn keep(rows: &mut Vec<Row>, view: View) {
    let kept: std::collections::HashSet<String> = rows.iter().filter(|r| !r.is_plan() && view.takes(r)).filter_map(|r| r.parent.clone()).collect();
    let parents: std::collections::HashSet<String> = rows.iter().filter(|r| !r.is_plan()).filter_map(|r| r.parent.clone()).collect();
    rows.retain(|r| match (r.is_plan(), parents.contains(&r.uid)) {
        (true, true) => kept.contains(&r.uid),
        _ => view.takes(r),
    });
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

/// `reported`: add the REPORTED column.
fn print_rows(rows: &[(Row, bool)], reported: bool) {
    let yn = |l: Option<bool>| match l {
        Some(true) => "yes",
        Some(false) => "no",
        None => "?",
    };
    let refs = rows.iter().any(|(r, _)| r.reference.is_some());
    let types = rows.iter().any(|(r, _)| r.task_type.is_some());
    let sockets = rows.iter().any(|(r, _)| r.elsewhere);
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|(r, child)| {
            let mut row = vec![
                if *child { format!("└ {}", r.id) } else { r.id.to_string() },
                r.uid.clone(),
                r.shown_state().to_string(),
                if r.is_plan() { "plan".into() } else { print::short_id(r.pane_id.as_deref().unwrap_or("")).to_string() },
                if r.is_plan() { "-".into() } else if r.elsewhere { "elsewhere".into() } else { yn(r.live).to_string() },
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
            if sockets {
                row.push(if r.elsewhere { r.socket.clone().unwrap_or_default() } else { String::new() });
            }
            if reported {
                row.push(r.reported_at.clone().unwrap_or_default());
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
    if sockets {
        header.push("SOCKET");
    }
    if reported {
        header.push("REPORTED");
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
        f.push(("live", if r.elsewhere { "elsewhere".into() } else { yn(r.live).into() }));
        if let Some(s) = &r.socket {
            f.push(("socket", s.clone()));
        }
    }
    f.extend([
        ("project", r.project.clone()),
        ("sent", r.sent_at.clone()),
        ("updated", r.updated_at.clone()),
        ("reported", r.reported_at.clone().unwrap_or_else(|| "no".into())),
    ]);
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
    /// The socket of the pane.
    socket: Option<String>,
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
            "INSERT INTO tasks (uid, kind, parent, pane_id, pane_gen, project, task, sent_at, state, result_path, ref, updated_at, type, socket)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {NOW}, ?8, ?9, ?10, {NOW}, ?11, ?12)"
        ),
        params![uid, if n.plan { "plan" } else { "task" }, parent.map(|p| p.uid), n.pane_id, n.pane_gen, n.project, n.task, n.state, result, n.reference, n.task_type, n.socket],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok((id, uid, result))
}

/// The session that owns the ledger, and its socket.
#[derive(Debug, Clone, PartialEq)]
pub struct Owner {
    pub session: String,
    pub socket: String,
}

fn owner(db: &Connection) -> Result<Option<Owner>> {
    let mut st = db.prepare("SELECT session, socket FROM owner WHERE id = 1")?;
    let mut it = st.query_map([], |r| Ok(Owner { session: r.get(0)?, socket: r.get(1)? }))?;
    Ok(it.next().transpose()?)
}

fn set_owner(db: &Connection, session: &str, socket: &Path) -> Result<()> {
    db.execute(
        &format!("INSERT INTO owner (id, session, socket, since) VALUES (1, ?1, ?2, {NOW}) ON CONFLICT (id) DO UPDATE SET session = ?1, socket = ?2, since = {NOW}"),
        params![session, socket.display().to_string()],
    )?;
    Ok(())
}

/// The owner is `session` on `socket`.
fn is_owner(cur: Option<&Owner>, session: &str, socket: &Path) -> bool {
    cur.is_some_and(|o| o.session == session && state::same_socket(Path::new(&o.socket), socket))
}

/// The Skiff agent session of `socket` may take the ledger from `cur`: when
/// it has no owner, when the owner was on the same daemon (a new Skiff agent
/// session there replaces the old one), or when `socket` is the live one. A
/// daemon runs one Skiff agent, so the live agent always wins, and an agent
/// on another socket never takes the live agent's ledger.
fn may_claim(cur: Option<&Owner>, socket: &Path) -> bool {
    match cur {
        None => true,
        Some(o) => state::same_socket(Path::new(&o.socket), socket) || state::is_live(socket),
    }
}

/// Records `session`, the Skiff agent's session on `socket`, as the owner,
/// when [`may_claim`] allows it. `skiff skipper` and the agent's `skiff
/// watch` call it, so a new Skiff agent session takes over at its start.
pub fn claim(socket: &Path, session: &str) -> Result<()> {
    claim_at(&path()?, socket, session)
}

fn claim_at(file: &Path, socket: &Path, session: &str) -> Result<()> {
    if !file.parent().is_some_and(Path::is_dir) {
        return Ok(());
    }
    let db = open_at(file)?;
    let tx = db.unchecked_transaction()?;
    let cur = owner(&tx)?;
    if is_owner(cur.as_ref(), session, socket) {
        return Ok(());
    }
    if !may_claim(cur.as_ref(), socket) {
        let o = cur.expect("only an owner refuses");
        bail!("the ledger belongs to the Skiff agent session {} on {}. It stays there", print::short_id(&o.session), o.socket);
    }
    set_owner(&tx, session, socket)?;
    tx.commit()?;
    Ok(())
}

/// What a change to the ledger from `me` (`$SKIFF_SESSION`), whose session
/// on `socket` is `s`, may do.
#[derive(Debug, PartialEq)]
enum Writer {
    Yes,
    /// The Skiff agent of this socket takes the ledger first.
    Claim,
    No,
}

/// A shell outside Skiff (no `me`) and a shell pane are a person: they may.
/// The owner may. Any other pane is refused, a Skiff agent on another socket
/// included, unless [`may_claim`] lets the Skiff agent of `socket` take it.
fn writer(me: Option<&str>, s: Option<&SessionInfo>, cur: Option<&Owner>, socket: &Path) -> Writer {
    let Some(me) = me else { return Writer::Yes };
    if is_owner(cur, me, socket) {
        return Writer::Yes;
    }
    match s {
        Some(s) if s.coordinator && may_claim(cur, socket) => Writer::Claim,
        Some(s) if s.role == Role::Shell && !s.coordinator => Writer::Yes,
        _ => Writer::No,
    }
}

/// Refuses a change from a pane that is not the ledger's owner. It runs
/// before the change.
async fn check_writer(socket: &Path, db: &Connection) -> Result<()> {
    let me = std::env::var("SKIFF_SESSION").ok().filter(|s| !s.is_empty());
    let cur = owner(db)?;
    let all = match &me {
        Some(m) if !is_owner(cur.as_ref(), m, socket) => live(socket).await.unwrap_or_default(),
        _ => Vec::new(),
    };
    let s = me.as_deref().and_then(|m| all.iter().find(|s| s.id == m));
    match writer(me.as_deref(), s, cur.as_ref(), socket) {
        Writer::Yes => Ok(()),
        Writer::Claim => set_owner(db, &s.expect("a claim has a session").id, socket),
        Writer::No => bail!("{}", refusal(cur.as_ref())),
    }
}

fn refusal(cur: Option<&Owner>) -> String {
    match cur {
        Some(o) => format!(
            "this ledger belongs to the Skiff agent session {} on {}. Only that session adds, sends, updates, reports or removes tasks. Nothing changed",
            print::short_id(&o.session),
            o.socket
        ),
        None => "this ledger has no owner yet: `skiff skipper` records the Skiff agent's session, and only that session changes it. Nothing changed".into(),
    }
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
    if let Some(other) = row.socket_other_than(socket) {
        bail!("task {} went to a pane on {other}, not on {}. Nothing was sent", row.uid, socket.display());
    }
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

/// Stamps `reported_at` on the done rows `ids` name, in one transaction.
/// A plan with tasks stamps its done tasks and leaves the others; a plan with
/// none is stamped itself. A row that is not done refuses the lot. A first
/// stamp stays. `all`: every done task, and every done plan with no task.
/// Returns how many rows took a new stamp.
fn report(db: &mut Connection, ids: &[String], all: bool) -> Result<usize> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stamp = format!("UPDATE tasks SET reported_at = {NOW} WHERE state = 'done' AND reported_at IS NULL");
    let mut n = 0;
    if all {
        n += tx.execute(&format!("{stamp} AND (kind = 'task' OR NOT EXISTS (SELECT 1 FROM tasks t WHERE t.parent = tasks.uid))"), [])?;
    }
    for id in ids {
        let r = one(&tx, id)?;
        let tasks = if r.is_plan() { rows(&tx, "WHERE parent = ?1", Some(&r.uid))? } else { Vec::new() };
        if !tasks.is_empty() {
            n += tx.execute(&format!("{stamp} AND parent = ?1"), [&r.uid])?;
            continue;
        }
        if r.state != "done" {
            bail!("{} {} is {}, not done. Nothing changed", r.kind, r.uid, r.state);
        }
        n += tx.execute(&format!("{stamp} AND id = ?1"), [r.id])?;
    }
    tx.commit()?;
    Ok(n)
}

/// A row set to a state other than done opens again. That is new work: its
/// stamp goes, and `ls` shows it once it is done.
fn reopen(db: &Connection, id: i64, st: Option<&str>) -> Result<()> {
    if st.is_some_and(|s| s != "done") {
        db.execute("UPDATE tasks SET reported_at = NULL WHERE id = ?1", [id])?;
    }
    Ok(())
}

fn rows_word(n: usize) -> &'static str {
    if n == 1 {
        "row"
    } else {
        "rows"
    }
}

/// `7d`, `12h`, `30m` or `45s`, in seconds.
fn age(s: &str) -> Result<i64> {
    let unit = match s.chars().last() {
        Some('d') => 86_400,
        Some('h') => 3_600,
        Some('m') => 60,
        Some('s') => 1,
        _ => bail!("cannot read the age {s:?}. Use a number and d, h, m or s, such as 7d"),
    };
    let n: i64 = s[..s.len() - 1].parse().with_context(|| format!("cannot read the age {s:?}. Use a number and d, h, m or s, such as 7d"))?;
    Ok(n * unit)
}

/// The rows `prune` takes: done tasks reported at least `secs` ago, each
/// plan whose last tasks they are, and each done plan with no task reported
/// that long ago. Open and unreported rows never go. `yes`: delete them.
fn prune(db: &mut Connection, secs: i64, yes: bool) -> Result<Vec<Row>> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let cutoff: String = tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)", [format!("-{secs} seconds")], |r| r.get(0))?;
    let old = |r: &Row| r.state == "done" && r.reported_at.as_deref().is_some_and(|t| t <= cutoff.as_str());
    let all = rows(&tx, "", None)?;
    let gone: Vec<Row> = all
        .iter()
        .filter(|r| {
            let mut tasks = all.iter().filter(|t| !t.is_plan() && t.parent.as_deref() == Some(&r.uid)).peekable();
            match (r.is_plan(), tasks.peek().is_some()) {
                (true, true) => tasks.all(old),
                _ => old(r),
            }
        })
        .cloned()
        .collect();
    if yes {
        for r in &gone {
            // The row only: a result file stays where it is.
            tx.execute("DELETE FROM tasks WHERE id = ?1", [r.id])?;
        }
        tx.commit()?;
    }
    Ok(gone)
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
            let here = pane_id.as_ref().map(|_| socket.display().to_string());
            let new = New { plan: is_plan, parent, pane_id, pane_gen, project, task, state: st, result, reference, task_type, socket: here };
            let mut db = open_at(&file)?;
            check_writer(socket, &db).await?;
            let (id, uid, result) = insert(&mut db, &file, new)?;
            println!("id {id}\nuid {uid}\nresult {result}");
            if send {
                let row = one(&db, &uid)?;
                return send_row(socket, &db, &row, SendOpts { text: Vec::new(), enter, shell }).await;
            }
        }
        LedgerCmd::Send { id, text, enter, shell } => {
            let db = open()?;
            check_writer(socket, &db).await?;
            let row = one(&db, &id)?;
            return send_row(socket, &db, &row, SendOpts { text, enter, shell }).await;
        }
        LedgerCmd::Update { id, state: st, result, task, pane: p, project, parent, reference, task_type } => {
            let st = st.as_deref().map(state).transpose()?;
            let task_type = task_type.as_deref().map(skiff_core::tasks::check_name).transpose()?;
            let db = open()?;
            check_writer(socket, &db).await?;
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
                (None, Some("sent")) if !row.is_plan() && row.socket_other_than(socket).is_none() => {
                    let all = live(socket).await.unwrap_or_default();
                    let s = row.pane_id.as_deref().and_then(|p| resolve::session(&all, p).ok());
                    (None, s.map(gen))
                }
                _ => (None, None),
            };
            let parent = parent.as_deref().map(|p| plan(&db, p)).transpose()?.map(|p| p.uid);
            // A new pane is on this socket.
            let here = pane_id.as_ref().map(|_| socket.display().to_string());
            db.execute(
                &format!(
                    "UPDATE tasks SET state = coalesce(?2, state), result_path = coalesce(?3, result_path), task = coalesce(?4, task),
                     pane_id = coalesce(?5, pane_id), project = coalesce(?6, project), parent = coalesce(?7, parent), ref = coalesce(?8, ref),
                     pane_gen = CASE WHEN ?9 THEN ?10 ELSE pane_gen END, type = coalesce(?11, type), socket = coalesce(?12, socket), updated_at = {NOW} WHERE id = ?1"
                ),
                params![row.id, st, result, task, pane_id, project, parent, reference, gen.is_some(), gen.flatten(), task_type, here],
            )?;
            reopen(&db, row.id, st)?;
        }
        LedgerCmd::Ls { state: st, all, reported, json } => {
            if let Some(s) = st.as_deref().filter(|s| *s != INTERRUPTED && *s != GONE) {
                state(s)?;
            }
            let view = if all {
                View::All
            } else if reported {
                View::Reported
            } else {
                View::Open
            };
            let db = open()?;
            let mut list = rows(&db, "", None)?;
            mark_live(socket, &mut list).await;
            roll_up(&mut list);
            keep(&mut list, view);
            if let Some(s) = &st {
                list.retain(|r| r.shown_state() == s);
            }
            let list = tree(list);
            if json {
                print::json(&list.iter().map(|(r, _)| r).collect::<Vec<_>>())?;
            } else {
                print_rows(&list, view != View::Open);
            }
        }
        LedgerCmd::Report { ids, all } => {
            let mut db = open()?;
            check_writer(socket, &db).await?;
            let n = report(&mut db, &ids, all)?;
            println!("reported {n} {}", rows_word(n));
        }
        LedgerCmd::Prune { older_than, yes } => {
            let secs = age(&older_than)?;
            let mut db = open()?;
            check_writer(socket, &db).await?;
            let mut gone = prune(&mut db, secs, yes)?;
            if gone.is_empty() {
                println!("no row was reported more than {older_than} ago. Nothing changed");
                return Ok(0);
            }
            let n = gone.len();
            mark_live(socket, &mut gone).await;
            print_rows(&tree(gone), true);
            if yes {
                println!("removed {n} {}. The result files stay", rows_word(n));
            } else {
                println!("would remove {n} {}. Nothing changed. Run again with --yes to remove them", rows_word(n));
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
            check_writer(socket, &db).await?;
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
            socket: Some("/tmp/here.sock".into()),
        }
    }

    #[test]
    fn reads_only_open_rows_and_stores_the_type() {
        let dir = scratch("open");
        let file = dir.join("ledger.db");
        assert!(open_tasks_at(&file).unwrap().is_empty() && !file.exists());
        let mut db = open_at(&file).unwrap();
        insert(&mut db, &file, task("fix bug")).unwrap();
        insert(&mut db, &file, New { state: "done".into(), ..task("seo") }).unwrap();
        let (id, _, _) = insert(&mut db, &file, New { state: "pending".into(), task_type: Some("review".into()), ..task("docs") }).unwrap();
        let t = open_tasks_at(&file).unwrap();
        assert_eq!(t.iter().map(|r| (r.task.as_str(), r.state.as_str())).collect::<Vec<_>>(), [("fix bug", "sent"), ("docs", "pending")]);
        let r = one(&db, &id.to_string()).unwrap();
        assert_eq!(serde_json::to_value(&r).unwrap()["type"], "review");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_version_3_ledger_takes_sockets_in_place_and_keeps_its_rows() {
        let dir = scratch("v3");
        let file = dir.join("ledger.db");
        {
            let v3 = TASKS.replace("type        TEXT,\n  socket      TEXT,\n  reported_at TEXT", "type        TEXT");
            assert!(!v3.contains("socket"));
            let db = Connection::open(&file).unwrap();
            db.execute_batch(&v3).unwrap();
            db.execute_batch(UIDS).unwrap();
            db.execute_batch(
                "INSERT INTO tasks (uid, kind, pane_id, task, sent_at, state, updated_at) VALUES ('a1b2c3d4', 'task', 'abc', 'live work', 'x', 'sent', 'x');
                 INSERT INTO tasks (uid, kind, task, sent_at, state, updated_at) VALUES ('e5f6a7b8', 'plan', 'a plan', 'x', 'sent', 'x');
                 PRAGMA user_version = 3;",
            )
            .unwrap();
        }
        let db = open_at(&file).unwrap();
        let list = rows(&db, "", None).unwrap();
        assert_eq!(list.iter().map(|r| r.uid.as_str()).collect::<Vec<_>>(), ["a1b2c3d4", "e5f6a7b8"]);
        // The task went out on the live daemon; a plan has no pane.
        assert_eq!(list[0].socket.as_deref(), Some(state::live_socket().to_str().unwrap()));
        assert_eq!(list[1].socket, None);
        assert_eq!(db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), VERSION);
        assert_eq!(owner(&db).unwrap(), None);
        assert!(!dir.join("ledger.db.old").exists());
        // A newer skiff's stamp stays.
        db.pragma_update(None, "user_version", VERSION + 1).unwrap();
        drop(db);
        let db = open_at(&file).unwrap();
        assert_eq!(db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), VERSION + 1);
        // A second open changes nothing.
        drop(db);
        assert_eq!(rows(&open_at(&file).unwrap(), "", None).unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_version_4_ledger_takes_reported_at_in_place_and_keeps_its_rows() {
        let dir = scratch("v4");
        let file = dir.join("ledger.db");
        {
            let v4 = TASKS.replace(",\n  reported_at TEXT", "");
            assert!(!v4.contains("reported_at"));
            let db = Connection::open(&file).unwrap();
            db.execute_batch(&v4).unwrap();
            db.execute_batch(UIDS).unwrap();
            db.execute_batch(OWNER).unwrap();
            db.execute_batch(
                "INSERT INTO tasks (uid, kind, pane_id, task, sent_at, state, updated_at, socket) VALUES ('a1b2c3d4', 'task', 'abc', 'old work', 'x', 'done', 'x', '/tmp/s.sock');
                 INSERT INTO tasks (uid, kind, pane_id, task, sent_at, state, updated_at, socket) VALUES ('e5f6a7b8', 'task', 'abc', 'live work', 'x', 'sent', 'x', '/tmp/s.sock');
                 PRAGMA user_version = 4;",
            )
            .unwrap();
        }
        let db = open_at(&file).unwrap();
        let list = rows(&db, "", None).unwrap();
        assert_eq!(list.iter().map(|r| (r.uid.as_str(), r.reported_at.as_deref())).collect::<Vec<_>>(), [("a1b2c3d4", None), ("e5f6a7b8", None)]);
        // The v4 socket stays as it was.
        assert_eq!(list[0].socket.as_deref(), Some("/tmp/s.sock"));
        assert_eq!(db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 5);
        assert_eq!(unreported_at(&file).unwrap(), 1);
        // A second open adds nothing.
        drop(db);
        let db = open_at(&file).unwrap();
        let n: i64 = db.query_row("SELECT count(*) FROM pragma_table_info('tasks') WHERE name = 'reported_at'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        assert_eq!(rows(&db, "", None).unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The uids `ls` lists in `view`, with `state` if given, as `run` does it.
    fn listed(db: &Connection, view: View, state: Option<&str>) -> Vec<String> {
        let mut list = rows(db, "", None).unwrap();
        roll_up(&mut list);
        keep(&mut list, view);
        if let Some(s) = state {
            list.retain(|r| r.shown_state() == s);
        }
        tree(list).into_iter().map(|(r, _)| r.uid).collect()
    }

    #[test]
    fn ls_shows_open_work_and_done_work_not_yet_reported() {
        let dir = scratch("view");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let done = |t: &str| New { state: "done".into(), ..task(t) };
        let (_, open, _) = insert(&mut db, &file, task("open")).unwrap();
        let (_, told, _) = insert(&mut db, &file, done("told")).unwrap();
        let (_, untold, _) = insert(&mut db, &file, done("untold")).unwrap();
        let (_, p, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("half") }).unwrap();
        let (_, p_open, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..task("a") }).unwrap();
        let (_, p_told, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..done("b") }).unwrap();
        let (_, q, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("all told") }).unwrap();
        let (_, q_told, _) = insert(&mut db, &file, New { parent: Some(q.clone()), ..done("c") }).unwrap();
        assert_eq!(report(&mut db, &[told.clone(), p_told.clone(), q.clone()], false).unwrap(), 3);
        let s = |v: &[&String]| v.iter().map(|u| u.to_string()).collect::<Vec<_>>();

        // A plan stays while one task stays, with only that task under it.
        assert_eq!(listed(&db, View::Open, None), s(&[&open, &untold, &p, &p_open]));
        assert_eq!(listed(&db, View::All, None), s(&[&open, &told, &untold, &p, &p_open, &p_told, &q, &q_told]));
        assert_eq!(listed(&db, View::Reported, None), s(&[&told, &p, &p_told, &q, &q_told]));
        assert_eq!(listed(&db, View::Open, Some("done")), s(&[&untold]));
        assert_eq!(listed(&db, View::All, Some("done")), s(&[&told, &untold, &p_told, &q, &q_told]));

        // Interrupted and gone work is open.
        let mut list = rows(&db, "", None).unwrap();
        list.iter_mut().find(|r| r.uid == open).unwrap().live = Some(false);
        keep(&mut list, View::Open);
        assert!(list.iter().any(|r| r.uid == open && r.shown_state() == GONE));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn report_stamps_done_rows_once_and_refuses_open_ones() {
        let dir = scratch("report");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let done = |t: &str| New { state: "done".into(), ..task(t) };
        let (_, a, _) = insert(&mut db, &file, done("a")).unwrap();
        let (_, b, _) = insert(&mut db, &file, done("b")).unwrap();
        let (_, open, _) = insert(&mut db, &file, task("open")).unwrap();
        let (_, p, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("p") }).unwrap();
        let (_, p_done, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..done("c") }).unwrap();
        let (_, p_open, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..task("d") }).unwrap();
        let stamp = |db: &Connection, uid: &str| one(db, uid).unwrap().reported_at;
        assert_eq!(unreported_at(&file).unwrap(), 3);

        assert_eq!(report(&mut db, std::slice::from_ref(&a), false).unwrap(), 1);
        assert!(stamp(&db, &a).is_some());
        // A second run keeps the first stamp.
        db.execute("UPDATE tasks SET reported_at = '2020-01-01T00:00:00Z' WHERE uid = ?1", [&a]).unwrap();
        assert_eq!(report(&mut db, std::slice::from_ref(&a), false).unwrap(), 0);
        assert_eq!(stamp(&db, &a).as_deref(), Some("2020-01-01T00:00:00Z"));
        // An open task refuses the lot.
        let err = report(&mut db, &[b.clone(), open.clone()], false).unwrap_err().to_string();
        assert_eq!(err, format!("task {open} is sent, not done. Nothing changed"));
        assert!(stamp(&db, &b).is_none());
        // A plan stamps its done tasks only.
        assert_eq!(report(&mut db, std::slice::from_ref(&p), false).unwrap(), 1);
        assert!(stamp(&db, &p_done).is_some() && stamp(&db, &p_open).is_none() && stamp(&db, &p).is_none());
        // --all stamps every done task not reported, and only those.
        assert_eq!(report(&mut db, &[], true).unwrap(), 1);
        assert!(stamp(&db, &b).is_some() && stamp(&db, &open).is_none());
        assert_eq!(stamp(&db, &a).as_deref(), Some("2020-01-01T00:00:00Z"));
        assert_eq!(unreported_at(&file).unwrap(), 0);

        // A task that opens again loses its stamp.
        reopen(&db, one(&db, &b).unwrap().id, Some("done")).unwrap();
        assert!(stamp(&db, &b).is_some());
        reopen(&db, one(&db, &b).unwrap().id, None).unwrap();
        assert!(stamp(&db, &b).is_some());
        reopen(&db, one(&db, &b).unwrap().id, Some("pending")).unwrap();
        assert!(stamp(&db, &b).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prune_takes_only_old_reported_rows_and_a_plan_with_its_last_task() {
        let dir = scratch("prune");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let done = |t: &str| New { state: "done".into(), ..task(t) };
        let (_, old, _) = insert(&mut db, &file, done("old")).unwrap();
        let (_, new, _) = insert(&mut db, &file, done("new")).unwrap();
        let (_, untold, _) = insert(&mut db, &file, done("untold")).unwrap();
        let (_, open, _) = insert(&mut db, &file, task("open")).unwrap();
        let (_, p, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("p") }).unwrap();
        let (_, p_old, _) = insert(&mut db, &file, New { parent: Some(p.clone()), ..done("a") }).unwrap();
        let (_, q, _) = insert(&mut db, &file, New { plan: true, pane_id: None, ..task("q") }).unwrap();
        let (_, q_old, _) = insert(&mut db, &file, New { parent: Some(q.clone()), ..done("b") }).unwrap();
        let (_, q_untold, _) = insert(&mut db, &file, New { parent: Some(q.clone()), ..done("c") }).unwrap();
        report(&mut db, std::slice::from_ref(&new), false).unwrap();
        for uid in [&old, &p_old, &q_old] {
            db.execute("UPDATE tasks SET reported_at = '2020-01-01T00:00:00Z' WHERE uid = ?1", [uid]).unwrap();
        }
        let uids = |rows: Vec<Row>| rows.into_iter().map(|r| r.uid).collect::<Vec<_>>();
        let all = || uids(rows(&open_at(&file).unwrap(), "", None).unwrap());
        let before = all();

        // A dry run lists and changes nothing.
        let want = vec![old.clone(), p.clone(), p_old.clone(), q_old.clone()];
        assert_eq!(uids(prune(&mut db, 7 * 86_400, false).unwrap()), want);
        assert_eq!(all(), before);
        assert_eq!(uids(prune(&mut db, 7 * 86_400, true).unwrap()), want);
        // q keeps its unreported task, so q stays.
        assert_eq!(all(), [new.clone(), untold.clone(), open.clone(), q.clone(), q_untold.clone()]);
        // A short window takes the new one too.
        db.execute("UPDATE tasks SET reported_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-2 hours') WHERE uid = ?1", [&new]).unwrap();
        assert!(prune(&mut db, age("7d").unwrap(), true).unwrap().is_empty());
        assert_eq!(uids(prune(&mut db, age("1h").unwrap(), true).unwrap()), [new]);
        assert_eq!(age("30m").unwrap(), 1800);
        assert!(age("7").is_err() && age("xd").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_task_on_another_socket_is_never_gone_and_is_not_sent_from_here() {
        let dir = scratch("elsewhere");
        let file = dir.join("ledger.db");
        let mut db = open_at(&file).unwrap();
        let (_, mine, _) = insert(&mut db, &file, task("mine")).unwrap();
        let (_, theirs, _) = insert(&mut db, &file, New { socket: Some("/tmp/other.sock".into()), state: "pending".into(), ..task("theirs") }).unwrap();
        let here = Path::new("/tmp/here.sock");
        let mut list = rows(&db, "", None).unwrap();
        // No daemon answers here, as for a pane on another socket.
        mark_live(here, &mut list).await;
        let r = |uid: &str| list.iter().find(|r| r.uid == uid).unwrap().clone();
        assert!(!r(&mine).elsewhere);
        assert!(r(&theirs).elsewhere);
        assert_eq!(r(&theirs).socket_other_than(here), Some("/tmp/other.sock"));
        // Even a daemon that does not know the pane does not make it gone.
        let mut t = r(&theirs);
        t.live = None;
        assert_eq!(t.shown_state(), "pending");
        let err = send_row(here, &db, &r(&theirs), SendOpts { text: vec![], enter: true, shell: false }).await.unwrap_err().to_string();
        assert!(err.contains("/tmp/other.sock") && !err.contains("gone"), "{err}");
        assert_eq!(one(&db, &theirs).unwrap().state, "pending");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn pane(id: &str, role: &str, coordinator: bool) -> SessionInfo {
        let mut s: SessionInfo = serde_json::from_value(serde_json::json!({
            "id": id, "label": "x", "role": role, "cwd": "/", "command": "claude",
            "args": [], "state": "idle", "cols": 80, "rows": 24, "pid": null, "exit_code": null,
        }))
        .unwrap();
        s.coordinator = coordinator;
        s
    }

    #[test]
    fn only_the_owner_a_person_or_the_live_agent_changes_the_ledger() {
        let live = state::live_socket();
        let test = Path::new("/tmp/test.sock");
        let on = |session: &str, socket: &Path| Owner { session: session.into(), socket: socket.display().to_string() };
        let owner = on("96cb7de8aaaa", &live);
        let skipper = pane("96cb7de8aaaa", "agent", true);
        let worker = pane("02900b71aaaa", "agent", false);
        let shell = pane("70808970aaaa", "shell", false);
        let w = |me: &SessionInfo, cur: Option<&Owner>, socket: &Path| writer(Some(&me.id), Some(me), cur, socket);
        // The owner, a shell outside Skiff and a shell pane may.
        assert_eq!(w(&skipper, Some(&owner), &live), Writer::Yes);
        assert_eq!(writer(None, None, Some(&owner), &live), Writer::Yes);
        assert_eq!(w(&shell, Some(&owner), &live), Writer::Yes);
        // Another agent pane may not, on any socket.
        assert_eq!(w(&worker, Some(&owner), &live), Writer::No);
        assert_eq!(w(&worker, None, &live), Writer::No);
        // A second Skiff agent on a test daemon may not take the live ledger.
        let dev = pane("4d43fa26aaaa", "agent", true);
        assert_eq!(w(&dev, Some(&owner), test), Writer::No);
        // A session the daemon does not list is not a person.
        assert_eq!(writer(Some("deadbeef"), None, Some(&owner), &live), Writer::No);
        // Ownership moves: a new Skiff agent on the owner's daemon takes it,
        // the live agent takes it from any socket, and a first one takes it.
        let next = pane("f0afaa59aaaa", "agent", true);
        assert_eq!(w(&next, Some(&owner), &live), Writer::Claim);
        assert_eq!(w(&skipper, Some(&on("4d43fa26aaaa", test)), &live), Writer::Claim);
        assert_eq!(w(&dev, None, test), Writer::Claim);
        assert!(refusal(Some(&owner)).contains("96cb7de8") && refusal(Some(&owner)).contains(live.to_str().unwrap()));
    }

    #[test]
    fn claim_records_the_owner_and_keeps_the_live_one() {
        let dir = scratch("claim");
        let file = dir.join("ledger.db");
        let live = state::live_socket();
        let test = Path::new("/tmp/test.sock");
        claim_at(&file, &live, "96cb7de8aaaa").unwrap();
        let get = || owner(&open_at(&file).unwrap()).unwrap().unwrap();
        assert_eq!(get(), Owner { session: "96cb7de8aaaa".into(), socket: live.display().to_string() });
        // A Skiff agent on another socket does not take it.
        assert!(claim_at(&file, test, "4d43fa26aaaa").unwrap_err().to_string().contains("96cb7de8"));
        assert_eq!(get().session, "96cb7de8aaaa");
        // A restart on the live daemon moves it.
        claim_at(&file, &live, "f0afaa59aaaa").unwrap();
        assert_eq!(get().session, "f0afaa59aaaa");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_old_ledger_moves_aside_and_a_new_one_starts_empty() {
        let dir = scratch("old");
        let file = dir.join("ledger.db");
        Connection::open(&file).unwrap().execute_batch("CREATE TABLE tasks (id INTEGER PRIMARY KEY, pane_id TEXT, task TEXT);").unwrap();
        let db = open_at(&file).unwrap();
        assert!(rows(&db, "", None).unwrap().is_empty());
        assert!(dir.join("ledger.db.old").is_file());
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
