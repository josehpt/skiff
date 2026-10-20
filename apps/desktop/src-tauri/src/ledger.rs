//! Skipper's ledger for the panel beside its pane. The app reads `ledger.db`
//! read-only with the CLI's read code, and reports rows through the CLI,
//! which owns every write.

use std::{path::Path, time::Duration};

use anyhow::{bail, Context};
use serde::Serialize;
use skiff_client::ledger::{self, Row};
use tauri::{AppHandle, Emitter};

use crate::connection::err;

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

fn read() -> anyhow::Result<Vec<Entry>> {
    let file = ledger::path()?;
    if !file.exists() {
        return Ok(Vec::new());
    }
    let db = ledger::open_read(&file)?;
    let mut rows = ledger::rows(&db, "", None)?;
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

/// Marks `uids` reported with `skiff ledger report`. The app runs outside
/// any pane, so the owner check takes it for a person.
#[tauri::command]
pub async fn ledger_report(uids: Vec<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || report(&uids)).await.map_err(|e| e.to_string())?.map_err(err)
}

fn report(uids: &[String]) -> anyhow::Result<()> {
    if uids.is_empty() {
        return Ok(());
    }
    let out = std::process::Command::new(crate::skiff_agent::cli())
        .args(["ledger", "report"])
        .args(uids)
        .env("SKIFF_SOCKET", skiff_core::state::socket())
        .env("SKIFF_LEDGER", ledger::path()?)
        // An app started from a Skiff pane is still a person.
        .env_remove("SKIFF_SESSION")
        .output()
        .context("run skiff ledger report")?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        bail!("{}", msg.trim().trim_start_matches("skiff: "));
    }
    Ok(())
}

/// What the watcher compares: the size and mtime of the file and its WAL.
fn stamp(file: &Path) -> Vec<Option<(u64, std::time::SystemTime)>> {
    ["db", "db-wal"]
        .iter()
        .map(|ext| std::fs::metadata(file.with_extension(ext)).ok().and_then(|m| Some((m.len(), m.modified().ok()?))))
        .collect()
}

/// Runs at app start on its own thread. Emits [`LEDGER_CHANGED`] when
/// `ledger.db` or its WAL changes, or when the ledger moves to another folder.
pub fn watch(app: AppHandle) {
    let mut last = None;
    loop {
        if let Ok(file) = ledger::path() {
            let now = Some((stamp(&file), file));
            if last.is_some() && now != last {
                let _ = app.emit(LEDGER_CHANGED, ());
            }
            last = now;
        }
        std::thread::sleep(TICK);
    }
}
