//! Config and git requests the app answers itself. They read and write
//! `projects.toml` or run git, and need no session, so a change here never
//! asks the user to restart skiffd.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};
use skiff_core::{
    config::{self, Appearance, FolderInfo},
    git,
    project::{self, AgentInfo, Project, Worktree},
    session::{SessionInfo, SessionState},
};
use tauri::{AppHandle, Emitter, State};

use crate::connection::{App, ensure_client, err};

/// The page reloads projects on this event.
const PROJECTS_CHANGED: &str = "skiff:projects-changed";

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?.map_err(err)
}

/// Runs a projects.toml or worktree edit, then tells the page.
async fn project_edit<T: Send + 'static>(
    handle: &AppHandle,
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, String> {
    let out = blocking(f).await?;
    let _ = handle.emit(PROJECTS_CHANGED, ());
    Ok(out)
}

#[tauri::command]
pub(crate) async fn list_projects() -> Result<Vec<Project>, String> {
    blocking(project::list).await
}

/// One project read again: git runs in it only, not in every project.
#[tauri::command]
pub(crate) async fn get_project(name: String) -> Result<Project, String> {
    blocking(move || Ok(Project::from_config(&project::find(&name)?))).await
}

#[tauri::command]
pub(crate) async fn add_worktree(
    handle: AppHandle,
    project: String,
    branch: String,
    base: Option<String>,
) -> Result<Worktree, String> {
    project_edit(&handle, move || project::add_worktree(&project, &branch, base.as_deref())).await
}

/// Refuses the main worktree and live sessions inside. Refuses uncommitted
/// changes unless `force`, which deletes them.
#[tauri::command]
pub(crate) async fn remove_worktree(
    handle: AppHandle,
    app: State<'_, App>,
    project: String,
    path: PathBuf,
    force: bool,
) -> Result<(), String> {
    let (c, _) = ensure_client(&app).await?;
    let sessions = c.list_sessions().await.map_err(err)?;
    project_edit(&handle, move || remove_worktree_now(&project, &path, force, &sessions)).await
}

fn remove_worktree_now(name: &str, path: &Path, force: bool, sessions: &[SessionInfo]) -> anyhow::Result<()> {
    let p = project::find(name)?;
    let wt = git::list_worktrees(&p.path)?
        .into_iter()
        .find(|w| project::same_path(&w.path, path))
        .ok_or_else(|| anyhow!("{} is not a worktree of {name}", path.display()))?;
    if wt.is_main {
        bail!("refusing to remove the main worktree");
    }
    // git reports realpaths; session cwds may go through symlinks.
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let root = real(&wt.path);
    let live = sessions
        .iter()
        .filter(|s| s.state != SessionState::Done)
        .find(|s| real(&s.cwd).starts_with(&root) || s.cwd.starts_with(path));
    if let Some(s) = live {
        bail!("session \"{}\" is still running in this worktree", s.label);
    }
    if !force && wt.path.is_dir() && git::is_dirty(&wt.path)? {
        bail!("worktree has uncommitted changes");
    }
    git::remove_worktree(&p.path, &wt.path, force)
}

/// Appends the git repository that contains `path` to projects.toml.
#[tauri::command]
pub(crate) async fn add_project(
    handle: AppHandle,
    path: PathBuf,
    name: Option<String>,
    short: Option<String>,
    color: Option<String>,
    icon: Option<String>,
    agents: Option<Vec<String>>,
) -> Result<Project, String> {
    project_edit(&handle, move || {
        let p = config::add_project(&path, name, short, color, icon, agents.unwrap_or_default())?;
        Ok(Project::from_config(&p))
    })
    .await
}

/// Initializes the selected folder without staging files or creating a commit.
#[tauri::command]
pub(crate) async fn init_repository(path: PathBuf) -> Result<(), String> {
    blocking(move || git::init_repository(&path)).await
}

/// What `add_project` would do for `path`. Writes nothing.
#[tauri::command]
pub(crate) async fn inspect_folder(path: PathBuf) -> Result<FolderInfo, String> {
    blocking(move || Ok(config::inspect_folder(&path))).await
}

#[tauri::command]
pub(crate) async fn set_project_icon(handle: AppHandle, project: String, icon: Option<String>) -> Result<(), String> {
    project_edit(&handle, move || config::set_project_icon(&project, icon.as_deref())).await
}

#[tauri::command]
pub(crate) async fn set_project_fleet(handle: AppHandle, project: String, fleet: String) -> Result<(), String> {
    project_edit(&handle, move || config::set_project_fleet(&project, &fleet)).await
}

#[tauri::command]
pub(crate) async fn set_project_short(handle: AppHandle, project: String, short: String) -> Result<(), String> {
    project_edit(&handle, move || config::set_project_short(&project, &short)).await
}

#[tauri::command]
pub(crate) async fn set_project_background(
    handle: AppHandle,
    project: String,
    background: Option<String>,
) -> Result<(), String> {
    project_edit(&handle, move || config::set_project_background(&project, background.as_deref())).await
}

#[tauri::command]
pub(crate) async fn set_project_background_opacity(
    handle: AppHandle,
    project: String,
    percent: Option<u8>,
) -> Result<(), String> {
    project_edit(&handle, move || config::set_project_background_opacity(&project, percent)).await
}

#[tauri::command]
pub(crate) async fn set_project_color(handle: AppHandle, project: String, color: String) -> Result<(), String> {
    project_edit(&handle, move || config::set_project_color(&project, &color)).await
}

#[tauri::command]
pub(crate) async fn reorder_projects(handle: AppHandle, order: Vec<String>) -> Result<(), String> {
    project_edit(&handle, move || config::reorder_projects(&order)).await
}

#[tauri::command]
pub(crate) async fn set_project_closed(handle: AppHandle, project: String, closed: bool) -> Result<(), String> {
    project_edit(&handle, move || config::set_project_closed(&project, closed)).await
}

/// Drops the project and its settings from projects.toml. The folder stays.
#[tauri::command]
pub(crate) async fn remove_project(handle: AppHandle, project: String) -> Result<(), String> {
    project_edit(&handle, move || config::remove_project(&project)).await
}

#[tauri::command]
pub(crate) async fn get_keys() -> Result<std::collections::BTreeMap<String, String>, String> {
    blocking(|| Ok(config::load()?.keys)).await
}

#[tauri::command]
pub(crate) async fn get_appearance() -> Result<Appearance, String> {
    blocking(|| Ok(config::load()?.appearance)).await
}

/// Sets the app theme: a theme id, or `null` for Harbor.
#[tauri::command]
pub(crate) async fn set_appearance(theme: Option<String>) -> Result<Appearance, String> {
    blocking(move || {
        config::set_app_theme(theme.as_deref())?;
        Ok(config::load()?.appearance)
    })
    .await
}

/// Sets the text size: points, or `null` for the default.
#[tauri::command]
pub(crate) async fn set_font_size(size: Option<u8>) -> Result<Appearance, String> {
    blocking(move || {
        config::set_font_size(size)?;
        Ok(config::load()?.appearance)
    })
    .await
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
