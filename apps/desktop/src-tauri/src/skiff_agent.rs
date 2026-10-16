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
    tasks::{self, TaskType},
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

/// The watch log beside the socket, for the row's Open watch log.
#[tauri::command]
pub fn skiff_watch_log() -> String {
    skiff_core::socket::socket_path().with_extension("watch.log").display().to_string()
}

/// The Skiff agent's session, and what the open did to it.
#[derive(Serialize)]
pub struct Opened {
    session: SessionInfo,
    created: bool,
    typed: bool,
    /// Why its watcher did not start, for the row's tooltip.
    watch_error: Option<String>,
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
    let watch_error = blocking(move || skipper::start_watch(&skiff_core::socket::socket_path(), &cli(), &id)).await.err();
    if let Some(e) = &watch_error {
        eprintln!("skiff watch: {e}");
    }
    Ok(Some(Opened { session: found.session, created: found.created, typed: found.typed, watch_error }))
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
    /// `[skiff_agent.tasks]` in file order, with the command each row starts.
    tasks: Vec<TaskType>,
}

fn read() -> anyhow::Result<SkiffAgent> {
    let cfg = config::load().context("read projects.toml")?;
    let (folder, folder_error) = match skipper::folder(&cfg) {
        Ok(f) => (Some(f.display().to_string()), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    let a = cfg.skiff_agent;
    // The command lines come from projects.toml, so the local list builds the same ones skiffd would.
    let tasks = tasks::resolve(&a.tasks, &skiff_core::project::agents());
    Ok(SkiffAgent { name: a.name, agent: a.agent, enabled: a.enabled, folder, folder_error, continue_on_restore: a.continue_on_restore, tasks })
}

/// One edit from Settings › Skiff agent. `null` or empty takes the default.
#[derive(Deserialize)]
#[serde(tag = "key", content = "value", rename_all = "snake_case")]
pub enum Edit {
    Name(Option<String>),
    Agent(Option<String>),
    Enabled(bool),
    ContinueOnRestore(bool),
    /// A new row at the end of Task types.
    AddTask(TaskEdit),
    /// A row's agent or model.
    Task(TaskEdit),
    RenameTask { from: String, to: String },
    RemoveTask(String),
}

/// A task type row as Settings sends it. An empty model is the agent's default.
#[derive(Deserialize)]
pub struct TaskEdit {
    #[serde(rename = "type")]
    name: String,
    agent: String,
    model: Option<String>,
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
            Edit::AddTask(t) => config::add_task_type(&t.name, &t.agent, t.model.as_deref())?,
            Edit::Task(t) => config::set_task_type(&t.name, &t.agent, t.model.as_deref())?,
            Edit::RenameTask { from, to } => config::rename_task_type(&from, &to)?,
            Edit::RemoveTask(name) => config::remove_task_type(&name)?,
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

#[cfg(test)]
mod tests {
    use super::Edit;

    /// The shapes `SkiffAgentEdit` in apps/desktop/src/appearance/skiffAgentSettings.ts sends.
    #[test]
    fn reads_the_task_type_edits_the_page_sends() {
        let edit = |v: serde_json::Value| serde_json::from_value::<Edit>(v).unwrap();
        let add = edit(serde_json::json!({ "key": "add_task", "value": { "type": "review", "agent": "claude", "model": "opus" } }));
        assert!(matches!(add, Edit::AddTask(t) if t.name == "review" && t.agent == "claude" && t.model.as_deref() == Some("opus")));
        let set = edit(serde_json::json!({ "key": "task", "value": { "type": "review", "agent": "aider", "model": null } }));
        assert!(matches!(set, Edit::Task(t) if t.model.is_none()));
        let rename = edit(serde_json::json!({ "key": "rename_task", "value": { "from": "review", "to": "code-review" } }));
        assert!(matches!(rename, Edit::RenameTask { from, to } if from == "review" && to == "code-review"));
        assert!(matches!(edit(serde_json::json!({ "key": "remove_task", "value": "review" })), Edit::RemoveTask(t) if t == "review"));
        assert!(matches!(edit(serde_json::json!({ "key": "name", "value": null })), Edit::Name(None)));
    }
}
