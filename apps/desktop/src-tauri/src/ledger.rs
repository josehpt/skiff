//! Skipper's ledger for the panel beside its pane. The app reads `ledger.db`
//! read-only with the CLI's read code. It never writes: only Skipper changes
//! rows, through the CLI.

use std::{path::Path, time::Duration};

use serde::Serialize;
use skiff_client::ledger::{self, Row};
use tauri::{AppHandle, Emitter, Manager};

use crate::connection::{err, App};

/// The page reads the ledger again on this event.
const LEDGER_CHANGED: &str = "skiff:ledger-changed";

/// How often the watcher looks at the files.
const TICK: Duration = Duration::from_secs(1);

/// A row as the panel takes it.
#[derive(Serialize)]
pub struct Entry {
    #[serde(flatten)]
    row: Row,
    /// The result file is on disk.
    result_exists: bool,
}

/// Every row, reported ones included, oldest first. Plans carry the state
/// their tasks give them. No `ledger.db` yet gives none.
#[tauri::command]
pub async fn ledger_rows() -> Result<Vec<Entry>, String> {
    tauri::async_runtime::spawn_blocking(read).await.map_err(|e| e.to_string())?.map_err(err)
}

/// Finished shows this many closed items, newest first. Keep in step with
/// `KEEP` in `workspace/ledger.ts`.
const KEEP: i64 = 100;

/// The rows the panel shows: all work not closed, and the last [`KEEP`]
/// closed work heads with their tasks. Older closed work stays in the file.
const PANEL: &str = "WHERE coalesce(parent, uid) NOT IN (SELECT uid FROM tasks WHERE closed_at IS NOT NULL) \
     OR coalesce(parent, uid) IN (SELECT uid FROM tasks WHERE closed_at IS NOT NULL ORDER BY closed_at DESC, id DESC LIMIT ?1)";

fn read() -> anyhow::Result<Vec<Entry>> {
    let file = ledger::path()?;
    if !file.exists() {
        return Ok(Vec::new());
    }
    let db = ledger::open_read(&file)?;
    let mut rows = ledger::rows(&db, PANEL, Some(&KEEP))?;
    ledger::roll_up(&mut rows);
    let here = skiff_core::state::socket();
    Ok(rows
        .into_iter()
        .map(|mut row| {
            row.elsewhere = row.socket_other_than(&here).is_some();
            let result_exists = row.result_path.as_deref().is_some_and(|p| Path::new(p).is_file());
            Entry { row, result_exists }
        })
        .collect())
}

/// What the watcher compares: the size and mtime of the file and its WAL.
fn stamp(file: &Path) -> Vec<Option<(u64, std::time::SystemTime)>> {
    ["db", "db-wal"]
        .iter()
        .map(|ext| std::fs::metadata(file.with_extension(ext)).ok().and_then(|m| Some((m.len(), m.modified().ok()?))))
        .collect()
}

/// What the watcher saw last: the stamps and the file.
type Seen = Option<(Vec<Option<(u64, std::time::SystemTime)>>, std::path::PathBuf)>;

/// Runs at app start on its own thread, for the app's lifetime. Emits
/// [`LEDGER_CHANGED`] when `ledger.db` or its WAL changes, or when the ledger
/// moves to another folder. The ledger is this Mac's: while a remote Host is
/// shown it reads nothing, and starts over on a switch back.
pub fn watch(app: AppHandle) {
    let mut last = None;
    loop {
        if look(app.state::<App>().remote(), &mut last) {
            let _ = app.emit(LEDGER_CHANGED, ());
        }
        std::thread::sleep(TICK);
    }
}

/// One tick. True when the ledger changed since the last local tick.
fn look(remote: bool, last: &mut Seen) -> bool {
    if remote {
        *last = None;
        return false;
    }
    let Ok(file) = ledger::path() else { return false };
    let now = Some((stamp(&file), file));
    let changed = last.is_some() && now != *last;
    *last = now;
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review P2: while a remote Host is shown, the watcher reads nothing of
    /// this Mac's ledger, and a switch back starts from a fresh look.
    #[test]
    fn the_ledger_watcher_reads_nothing_while_a_remote_host_is_shown() {
        let _env = crate::reload::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-ledger-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = std::env::var_os("SKIFF_LEDGER");
        // A private ledger that changes between ticks.
        let file = dir.join("ledger.db");
        std::env::set_var("SKIFF_LEDGER", &file);
        let mut last = None;
        assert!(!look(false, &mut last));
        assert!(last.is_some());
        std::fs::write(&file, "changed").unwrap();
        assert!(!look(true, &mut last), "a remote tick reports nothing");
        assert!(last.is_none(), "a remote tick keeps nothing of this Mac's ledger");
        assert!(!look(false, &mut last), "the first local tick after a switch is a fresh look");
        std::fs::write(&file, "changed again").unwrap();
        assert!(look(false, &mut last));
        match old {
            Some(v) => std::env::set_var("SKIFF_LEDGER", v),
            None => std::env::remove_var("SKIFF_LEDGER"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
