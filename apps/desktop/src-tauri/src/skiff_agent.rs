//! The Skiff agent's settings, folder and session, for the page. The folder
//! setup and the find-or-start rule live in `skiff_client::skipper`, which
//! `skiff skipper` uses too, so the app and the CLI pick the same session.

use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use skiff_client::skipper;
use skiff_core::{
    config::{self, SkiffAgentKey},
    session::SessionInfo,
};
use tauri::{AppHandle, Emitter, State};

use crate::{
    config::PROJECTS_CHANGED,
    connection::{ensure_client, err, App},
};

/// Runs at app start, off the main thread. Never prompts. Copies the
/// built-in `skiff-cli` skill out when it changed.
pub fn sync() {
    if let Err(e) = skipper::sync_skill() {
        eprintln!("skiff-cli skill: {e:#}");
    }
}

/// Makes the agent's folder ready. Returns the folder.
fn ensure() -> anyhow::Result<PathBuf> {
    skipper::ensure(&config::load()?)
}

/// The `skiff` CLI beside the app binary, else the one on `PATH`.
fn cli() -> PathBuf {
    std::env::current_exe()
        .ok()
        .map(|e| e.with_file_name("skiff"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("skiff"))
}

/// Starts `skiff watch` for the agent's session, unless one runs for this
/// daemon. The page calls it on every start and every restore.
#[tauri::command]
pub async fn start_skiff_watch(session: String) -> Result<bool, String> {
    blocking(move || skipper::start_watch(&skiff_core::socket::socket_path(), &cli(), &session)).await
}

/// The Skiff agent's session, and what the open did to it.
#[derive(Serialize)]
pub struct Opened {
    session: SessionInfo,
    created: bool,
    typed: bool,
}

/// The row and the key: find the Skiff agent's session or start it, through
/// the daemon's guarded request, and start its watcher. With `restore`, the
/// `input_gen` the page read: only continue a restored shell, as Settings
/// says. `null` when there is nothing to open.
#[tauri::command]
pub async fn open_skiff_agent(app: State<'_, App>, restore: Option<u64>) -> Result<Option<Opened>, String> {
    let (c, _) = ensure_client(&app).await?;
    let how = match restore {
        Some(expect) => skipper::Open::Restore { expect },
        None => skipper::Open::Start,
    };
    let Some(found) = skipper::open(&c, how).await.map_err(err)? else { return Ok(None) };
    let id = found.session.id.clone();
    if let Err(e) = blocking(move || skipper::start_watch(&skiff_core::socket::socket_path(), &cli(), &id)).await {
        eprintln!("skiff watch: {e}");
    }
    Ok(Some(Opened { session: found.session, created: found.created, typed: found.typed }))
}

/// `[skiff_agent]` as the page reads it: the folder without `~`.
#[derive(Serialize)]
pub struct SkiffAgent {
    name: Option<String>,
    agent: Option<String>,
    enabled: bool,
    /// The fixed folder, or the override. Absent when the override is refused.
    folder: Option<String>,
    /// Why the override in projects.toml is refused.
    folder_error: Option<String>,
    continue_on_restore: bool,
}

fn read() -> anyhow::Result<SkiffAgent> {
    let cfg = config::load().context("read projects.toml")?;
    let (folder, folder_error) = match skipper::folder(&cfg) {
        Ok(f) => (Some(f.display().to_string()), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    let a = cfg.skiff_agent;
    Ok(SkiffAgent { name: a.name, agent: a.agent, enabled: a.enabled, folder, folder_error, continue_on_restore: a.continue_on_restore })
}

/// One edit from Settings › Skiff agent. `null` or empty takes the default.
#[derive(Deserialize)]
#[serde(tag = "key", content = "value", rename_all = "snake_case")]
pub enum Edit {
    Name(Option<String>),
    Agent(Option<String>),
    Enabled(bool),
    ContinueOnRestore(bool),
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn get_skiff_agent() -> Result<SkiffAgent, String> {
    blocking(read).await
}

/// Saves one setting and answers with all of them. The page reloads its
/// projects on the event, so the row follows at once.
#[tauri::command]
pub async fn set_skiff_agent(app: AppHandle, edit: Edit) -> Result<SkiffAgent, String> {
    let out = blocking(move || {
        match edit {
            Edit::Name(v) => config::set_skiff_agent(SkiffAgentKey::Name(v.as_deref()))?,
            Edit::Agent(v) => config::set_skiff_agent(SkiffAgentKey::Agent(v.as_deref()))?,
            Edit::ContinueOnRestore(on) => config::set_skiff_agent(SkiffAgentKey::ContinueOnRestore(on))?,
            Edit::Enabled(on) => {
                // On makes the folder ready first: a refused folder stays off.
                if on {
                    ensure()?;
                }
                config::set_skiff_agent(SkiffAgentKey::Enabled(on))?
            }
        }
        read()
    })
    .await?;
    let _ = app.emit(PROJECTS_CHANGED, ());
    Ok(out)
}
