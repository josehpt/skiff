//! Project and worktree requests go to the selected Host's data plane
//! (`skiff-host`): this Mac's for This Mac, a remote Host's own for it. The
//! app writes no `[[project]]` table and runs no worktree change itself. Its
//! own settings (keys, appearance) are this computer's `client.toml`; the
//! agent settings below are still this Mac's `projects.toml` (M6).

use std::path::PathBuf;

use skiff_core::{
    config::{self, client, Appearance},
    git,
    project::AgentInfo,
};
use tauri::{AppHandle, Emitter, State};

use skiff_client::host::data;
use skiff_host_api::{HostFolder, HostProject, HostWorktree, ProjectEdit, ProjectFleet};

use crate::connection::{data_read, data_write, keep_revision, project_write, App, ensure_client, err};

/// The page reloads projects on this event.
pub(crate) const PROJECTS_CHANGED: &str = "skiff:projects-changed";

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?.map_err(err)
}

/// Tells the page after a change was tried: done, refused or unknown, it
/// reads the projects again.
fn changed<T>(handle: &AppHandle, out: Result<T, String>) -> Result<T, String> {
    let _ = handle.emit(PROJECTS_CHANGED, ());
    out
}

/// Runs a local projects.toml edit (images only), then tells the page.
async fn local_edit(handle: &AppHandle, f: impl FnOnce() -> anyhow::Result<()> + Send + 'static) -> Result<(), String> {
    changed(handle, blocking(f).await)
}

/// One key of one Host project (`project_config@1`).
async fn edit(handle: &AppHandle, app: &App, project: String, edit: ProjectEdit) -> Result<(), String> {
    let out = project_write(app, |c, revision| async move {
        data::project_edit(&c, &revision, &project, edit).await.map(|r| ((), r))
    })
    .await;
    changed(handle, out)
}

/// The selected Host's projects, from its data-plane service (`projects@1`):
/// this Mac's `skiff-host` here, a remote Host's own there. Never read here.
/// Their revision is kept for the next change.
#[tauri::command]
pub(crate) async fn list_projects(app: State<'_, App>) -> Result<Vec<HostProject>, String> {
    read_projects(&app).await
}

async fn read_projects(app: &App) -> Result<Vec<HostProject>, String> {
    data_read(app, |c| async move {
        let (rows, revision) = data::project_rows(&c).await?;
        keep_revision(app, &c, revision);
        Ok(rows)
    })
    .await
}

/// One project read again: git runs in it only, not in every project.
#[tauri::command]
pub(crate) async fn get_project(app: State<'_, App>, name: String) -> Result<HostProject, String> {
    data_read(&app, |c| async move { data::project_get(&c, &name).await }).await
}

/// A worktree on a new branch, made by the Host's git (`worktrees@1`).
#[tauri::command]
pub(crate) async fn add_worktree(
    handle: AppHandle,
    app: State<'_, App>,
    project: String,
    branch: String,
    base: Option<String>,
) -> Result<HostWorktree, String> {
    let out = data_write(&app, |c| async move { data::worktree_add(&c, &project, &branch, base.as_deref()).await }).await;
    changed(&handle, out)
}

/// The Host refuses the main worktree and one its own live sessions run
/// in. It refuses uncommitted changes unless `force`, which deletes them.
#[tauri::command]
pub(crate) async fn remove_worktree(
    handle: AppHandle,
    app: State<'_, App>,
    project: String,
    path: String,
    force: bool,
) -> Result<(), String> {
    let out = data_write(&app, |c| async move { data::worktree_remove(&c, &project, &path, force).await }).await;
    changed(&handle, out)
}

/// Adds the git repository that contains `path`, a folder on the Host.
#[tauri::command]
pub(crate) async fn add_project(
    handle: AppHandle,
    app: State<'_, App>,
    path: String,
    name: Option<String>,
    short: Option<String>,
    color: Option<String>,
) -> Result<HostProject, String> {
    let out = project_write(&app, |c, revision| async move {
        data::project_add(&c, &revision, &path, name, short, color).await
    })
    .await;
    changed(&handle, out)
}

/// Initializes the selected folder without staging files or creating a
/// commit. This Mac only: no Host offers a Git write but worktrees.
#[tauri::command]
pub(crate) async fn init_repository(path: PathBuf) -> Result<(), String> {
    blocking(move || git::init_repository(&path)).await
}

/// What `add_project` would do for `path`, a folder on the Host. Writes nothing.
#[tauri::command]
pub(crate) async fn inspect_folder(app: State<'_, App>, path: String) -> Result<HostFolder, String> {
    data_read(&app, |c| async move { data::project_inspect(&c, &path).await }).await
}

/// This Mac only: an image is a path here, and no Host takes image bytes yet.
#[tauri::command]
pub(crate) async fn set_project_icon(handle: AppHandle, project: String, icon: Option<String>) -> Result<(), String> {
    local_edit(&handle, move || config::set_project_icon(&project, icon.as_deref())).await
}

/// This Mac only, as the icon.
#[tauri::command]
pub(crate) async fn set_project_background(
    handle: AppHandle,
    project: String,
    background: Option<String>,
) -> Result<(), String> {
    local_edit(&handle, move || config::set_project_background(&project, background.as_deref())).await
}

#[tauri::command]
pub(crate) async fn set_project_fleet(handle: AppHandle, app: State<'_, App>, project: String, fleet: String) -> Result<(), String> {
    edit(&handle, &app, project, ProjectEdit::Fleet { fleet }).await
}

#[tauri::command]
pub(crate) async fn set_project_short(handle: AppHandle, app: State<'_, App>, project: String, short: String) -> Result<(), String> {
    edit(&handle, &app, project, ProjectEdit::Short { short }).await
}

#[tauri::command]
pub(crate) async fn set_project_background_opacity(
    handle: AppHandle,
    app: State<'_, App>,
    project: String,
    percent: Option<u8>,
) -> Result<(), String> {
    edit(&handle, &app, project, ProjectEdit::BackgroundOpacity { percent }).await
}

#[tauri::command]
pub(crate) async fn set_project_color(handle: AppHandle, app: State<'_, App>, project: String, color: String) -> Result<(), String> {
    edit(&handle, &app, project, ProjectEdit::Color { color }).await
}

#[tauri::command]
pub(crate) async fn set_project_changes(handle: AppHandle, app: State<'_, App>, project: String, on: bool) -> Result<(), String> {
    edit(&handle, &app, project, ProjectEdit::Changes { on }).await
}

#[tauri::command]
pub(crate) async fn set_project_files(handle: AppHandle, app: State<'_, App>, project: String, on: bool) -> Result<(), String> {
    edit(&handle, &app, project, ProjectEdit::Files { on }).await
}

#[tauri::command]
pub(crate) async fn set_project_theme(handle: AppHandle, app: State<'_, App>, project: String, theme: Option<String>) -> Result<(), String> {
    edit(&handle, &app, project, ProjectEdit::Theme { theme }).await
}

#[tauri::command]
pub(crate) async fn reorder_projects(
    handle: AppHandle,
    app: State<'_, App>,
    order: Vec<String>,
    fleets: Option<Vec<(String, String)>>,
) -> Result<(), String> {
    let fleets = fleets.unwrap_or_default().into_iter().map(|(project, fleet)| ProjectFleet { project, fleet }).collect();
    let out = project_write(&app, |c, revision| async move {
        data::project_arrange(&c, &revision, order, fleets).await.map(|r| ((), r))
    })
    .await;
    changed(&handle, out)
}

#[tauri::command]
pub(crate) async fn set_project_closed(handle: AppHandle, app: State<'_, App>, project: String, closed: bool) -> Result<(), String> {
    edit(&handle, &app, project, ProjectEdit::Closed { closed }).await
}

/// Drops the project and its settings from the Host's config. The folder stays.
#[tauri::command]
pub(crate) async fn remove_project(handle: AppHandle, app: State<'_, App>, project: String) -> Result<(), String> {
    let out = project_write(&app, |c, revision| async move {
        data::project_remove(&c, &revision, &project).await.map(|r| ((), r))
    })
    .await;
    changed(&handle, out)
}

#[tauri::command]
pub(crate) async fn get_keys() -> Result<std::collections::BTreeMap<String, String>, String> {
    blocking(|| Ok(client::load()?.keys)).await
}

#[tauri::command]
pub(crate) async fn get_appearance() -> Result<Appearance, String> {
    blocking(|| Ok(client::load()?.appearance)).await
}

/// Sets the app theme: a theme id, or `null` for Harbor.
#[tauri::command]
pub(crate) async fn set_appearance(theme: Option<String>) -> Result<Appearance, String> {
    blocking(move || {
        config::set_app_theme(theme.as_deref())?;
        Ok(client::load()?.appearance)
    })
    .await
}

/// Sets the text size: points, or `null` for the default.
#[tauri::command]
pub(crate) async fn set_font_size(size: Option<u8>) -> Result<Appearance, String> {
    blocking(move || {
        config::set_font_size(size)?;
        Ok(client::load()?.appearance)
    })
    .await
}

/// Sets Mission control's filter: only the running sessions, or all.
#[tauri::command]
pub(crate) async fn set_mission_filter(running: bool) -> Result<(), String> {
    blocking(move || config::set_mission_filter(running)).await
}

/// Runs an agent config edit, then answers with the agent list. The list
/// comes from skiffd: whether an agent is installed depends on the sessions'
/// PATH and aliases, which the app does not have.
async fn agents_after(
    app: &State<'_, App>,
    f: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
) -> Result<Vec<AgentInfo>, String> {
    blocking(f).await?;
    let (c, _) = ensure_client(app).await?;
    c.list_agents().await.map_err(err)
}

/// Saves which agents the + menu offers.
#[tauri::command]
pub(crate) async fn set_agents(app: State<'_, App>, enabled: Vec<String>) -> Result<Vec<AgentInfo>, String> {
    agents_after(&app, move || config::set_enabled_agents(&enabled)).await
}

/// Replaces an agent's command line. Empty restores the default.
#[tauri::command]
pub(crate) async fn set_agent_command(app: State<'_, App>, agent: String, command: String) -> Result<Vec<AgentInfo>, String> {
    agents_after(&app, move || config::set_agent_command(&agent, &command)).await
}

#[tauri::command]
pub(crate) async fn rename_agent(app: State<'_, App>, agent: String, name: String) -> Result<Vec<AgentInfo>, String> {
    agents_after(&app, move || config::rename_agent(&agent, &name)).await
}

#[tauri::command]
pub(crate) async fn remove_agent(app: State<'_, App>, agent: String) -> Result<Vec<AgentInfo>, String> {
    agents_after(&app, move || config::remove_agent(&agent)).await
}

/// Brings back the default agents the user removed.
#[tauri::command]
pub(crate) async fn restore_agents(app: State<'_, App>) -> Result<Vec<AgentInfo>, String> {
    agents_after(&app, config::restore_agents).await
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
