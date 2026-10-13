//! `skiff ledger`: the Skiff agent's task record, a SQLite file in its folder.
//! The agent reads and writes it through these commands only, so a bad edit,
//! a compaction or a second writer cannot break it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use skiff_client::Client;
use skiff_core::session::SessionInfo;

use crate::{print, resolve, LedgerCmd};

const STATES: [&str; 4] = ["sent", "pending", "blocked", "done"];

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tasks (
  id          INTEGER PRIMARY KEY,
  pane_id     TEXT NOT NULL,
  project     TEXT NOT NULL DEFAULT '',
  task        TEXT NOT NULL,
  sent_at     TEXT NOT NULL,
  state       TEXT NOT NULL CHECK (state IN ('sent', 'pending', 'blocked', 'done')),
  result_path TEXT,
  updated_at  TEXT NOT NULL
);
PRAGMA user_version = 1;
";

/// UTC, to the second, set by SQLite.
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%SZ', 'now')";

#[derive(Serialize)]
pub struct Row {
    pub id: i64,
    /// The full session id.
    pub pane_id: String,
    pub project: String,
    pub task: String,
    pub sent_at: String,
    pub state: String,
    pub result_path: Option<String>,
    pub updated_at: String,
    /// The pane still exists. Absent when the daemon did not answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<bool>,
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
    db.execute_batch(SCHEMA)?;
    let md = file.with_file_name("ledger.md");
    if fresh && md.is_file() {
        let n = import(&mut db, &md)?;
        std::fs::rename(&md, md.with_file_name("ledger.md.bak")).context("keep ledger.md as ledger.md.bak")?;
        eprintln!("skiff: imported {n} rows from ledger.md; the old file is ledger.md.bak");
    }
    Ok(db)
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
        tx.execute(
            &format!("INSERT INTO tasks (pane_id, project, task, sent_at, state, result_path, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, {NOW})"),
            params![cells[0], cells[1], cells[2], cells[3], state, result],
        )?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

/// The open tasks (state sent, pending or blocked), oldest first. This is the
/// read path for other commands, such as `skiff watch`. No ledger yet (no
/// `ledger.db` and no `ledger.md` to import) gives an empty list, and makes
/// no file.
pub fn open_tasks() -> Result<Vec<Row>> {
    open_tasks_at(&path()?)
}

fn open_tasks_at(file: &Path) -> Result<Vec<Row>> {
    if !file.exists() && !file.with_file_name("ledger.md").is_file() {
        return Ok(Vec::new());
    }
    let db = open_at(file)?;
    rows(&db, "WHERE state IN ('sent', 'pending', 'blocked')", None)
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
    let sql = format!("SELECT id, pane_id, project, task, sent_at, state, result_path, updated_at FROM tasks {where_} ORDER BY id");
    let mut st = db.prepare(&sql)?;
    let map = |r: &rusqlite::Row| {
        Ok(Row {
            id: r.get(0)?,
            pane_id: r.get(1)?,
            project: r.get(2)?,
            task: r.get(3)?,
            sent_at: r.get(4)?,
            state: r.get(5)?,
            result_path: r.get(6)?,
            updated_at: r.get(7)?,
            live: None,
        })
    };
    let out = match arg {
        Some(a) => st.query_map([a], map)?.collect::<rusqlite::Result<Vec<_>>>()?,
        None => st.query_map([], map)?.collect::<rusqlite::Result<Vec<_>>>()?,
    };
    Ok(out)
}

fn one(db: &Connection, id: i64) -> Result<Row> {
    rows(db, "WHERE id = ?1", Some(&id))?.pop().with_context(|| format!("no ledger row {id}"))
}

async fn mark_live(socket: &Path, rows: &mut [Row]) {
    if let Some(all) = live(socket).await {
        for r in rows {
            // Rows imported from ledger.md can hold a short id or a name.
            r.live = Some(resolve::session(&all, &r.pane_id).is_ok());
        }
    }
}

fn print_rows(rows: &[Row]) {
    let yn = |l: Option<bool>| match l {
        Some(true) => "yes",
        Some(false) => "no",
        None => "?",
    };
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                r.id.to_string(),
                r.state.clone(),
                print::short_id(&r.pane_id).to_string(),
                yn(r.live).to_string(),
                r.project.clone(),
                r.sent_at.clone(),
                r.result_path.clone().unwrap_or_default(),
                r.task.clone(),
            ]
        })
        .collect();
    print::table(&["ID", "STATE", "PANE", "LIVE", "PROJECT", "SENT", "RESULT", "TASK"], &table);
}

pub async fn run(socket: &Path, cmd: LedgerCmd) -> Result<()> {
    match cmd {
        LedgerCmd::Add { pane: p, task, project, state: st, result } => {
            let st = state(&st)?.to_string();
            let s = pane(socket, &p).await?;
            let project = project.unwrap_or_else(|| project_of(&s));
            let db = open()?;
            db.execute(
                &format!("INSERT INTO tasks (pane_id, project, task, sent_at, state, result_path, updated_at) VALUES (?1, ?2, ?3, {NOW}, ?4, ?5, {NOW})"),
                params![s.id, project, task, st, result],
            )?;
            println!("{}", db.last_insert_rowid());
        }
        LedgerCmd::Update { id, state: st, result, task, pane: p, project } => {
            let st = st.as_deref().map(state).transpose()?;
            let pane_id = match p {
                Some(p) => Some(pane(socket, &p).await?.id),
                None => None,
            };
            let db = open()?;
            one(&db, id)?;
            let n = db.execute(
                &format!(
                    "UPDATE tasks SET state = coalesce(?2, state), result_path = coalesce(?3, result_path), task = coalesce(?4, task),
                     pane_id = coalesce(?5, pane_id), project = coalesce(?6, project), updated_at = {NOW} WHERE id = ?1"
                ),
                params![id, st, result, task, pane_id, project],
            )?;
            if n == 0 {
                bail!("no ledger row {id}");
            }
        }
        LedgerCmd::Ls { state: st, json } => {
            let db = open()?;
            let mut list = match st {
                Some(s) => rows(&db, "WHERE state = ?1", Some(&state(&s)?.to_string()))?,
                None => rows(&db, "", None)?,
            };
            mark_live(socket, &mut list).await;
            if json {
                print::json(&list)?;
            } else {
                print_rows(&list);
            }
        }
        LedgerCmd::Show { id, json } => {
            let db = open()?;
            let mut r = [one(&db, id)?];
            mark_live(socket, &mut r).await;
            if json {
                print::json(&r[0])?;
            } else {
                print_rows(&r);
            }
        }
        LedgerCmd::Rm { id } => {
            let db = open()?;
            if db.execute("DELETE FROM tasks WHERE id = ?1", [id])? == 0 {
                bail!("no ledger row {id}");
            }
        }
    }
    Ok(())
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
        assert_eq!(t.iter().map(|r| (r.pane_id.as_str(), r.state.as_str())).collect::<Vec<_>>(), [("abc12345", "sent"), ("1111aaaa", "pending")]);
        assert!(dir.join("ledger.md.bak").is_file() && !dir.join("ledger.md").exists());
        // A second read does not import again.
        assert_eq!(open_tasks_at(&db).unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
