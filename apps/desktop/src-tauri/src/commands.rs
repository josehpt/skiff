use std::path::PathBuf;
use skiff_core::{group::Group, project::AgentInfo, session::{SessionInfo, SessionSpec}};
use tauri::{AppHandle, Manager, State};
use crate::{connection::{App, ensure_client, err}, input::Sent};

#[tauri::command]
pub(crate) async fn list_sessions(app: State<'_, App>) -> Result<Vec<SessionInfo>, String> {
    let (c, _) = ensure_client(&app).await?;
    c.list_sessions().await.map_err(err)
}

#[tauri::command]
pub(crate) async fn create_session(app: State<'_, App>, spec: SessionSpec) -> Result<SessionInfo, String> {
    let (c, _) = ensure_client(&app).await?;
    c.create_session(spec).await.map_err(err)
}

/// Not async: it runs on the main thread, one call after another, so
/// keystrokes reach the PTY in order. It waits for no lock and no reply.
/// While a reload runs, or before a connection is up, the input waits in
/// the bridge, in order.
#[tauri::command]
pub(crate) fn pty_write(handle: AppHandle, app: State<'_, App>, session: String, data: String) -> Result<(), String> {
    let live = app.live.lock().unwrap().clone().filter(|c| !c.is_closed());
    match app.input.write(live.as_deref(), &session, data.into_bytes()).map_err(err)? {
        Sent::Done | Sent::Held => {}
        // No connection yet. Rare: connect, then send what waits.
        Sent::Connect => {
            tauri::async_runtime::spawn(async move {
                let app = handle.state::<App>();
                let c = ensure_client(&app).await.ok().map(|(c, _)| c);
                app.input.connected(c.as_deref());
            });
        }
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn pty_resize(
    app: State<'_, App>,
    session: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let (c, _) = ensure_client(&app).await?;
    c.resize(&session, cols, rows).await.map_err(err)
}

#[tauri::command]
pub(crate) async fn set_session_theme(
    app: State<'_, App>,
    session: String,
    theme: Option<String>,
) -> Result<SessionInfo, String> {
    let (c, _) = ensure_client(&app).await?;
    c.set_session_theme(&session, theme).await.map_err(err)
}

#[tauri::command]
pub(crate) async fn rename_session(
    app: State<'_, App>,
    session: String,
    name: String,
) -> Result<SessionInfo, String> {
    let (c, _) = ensure_client(&app).await?;
    c.rename_session(&session, &name).await.map_err(err)
}

/// The session's pane has the keys: its unread flag and agent exit go.
#[tauri::command]
pub(crate) async fn session_seen(app: State<'_, App>, session: String) -> Result<(), String> {
    let (c, _) = ensure_client(&app).await?;
    c.seen(&session).await.map_err(err)
}

/// Mission control's text for one session (#88).
#[tauri::command]
pub(crate) async fn session_screen(app: State<'_, App>, session: String, rows: u16) -> Result<Vec<String>, String> {
    let (c, _) = ensure_client(&app).await?;
    c.screen(&session, rows).await.map_err(err)
}

#[tauri::command]
pub(crate) async fn kill_session(app: State<'_, App>, session: String) -> Result<(), String> {
    let (c, _) = ensure_client(&app).await?;
    c.kill(&session).await.map_err(err)
}

#[tauri::command]
pub(crate) fn config_path() -> String {
    skiff_core::config::config_path().display().to_string()
}

/// Runs a call that may wait on a starting app off the async workers.
async fn blocking(f: impl FnOnce() -> anyhow::Result<()> + Send + 'static) -> Result<(), String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?.map_err(err)
}

/// Opens projects.toml in the default editor.
#[tauri::command]
pub(crate) async fn open_config() -> Result<(), String> {
    blocking(|| skiff_core::open::open_default(&skiff_core::config::config_path())).await
}

/// Opens a file or folder in the OS default app.
#[tauri::command]
pub(crate) async fn open_path(path: PathBuf) -> Result<(), String> {
    blocking(move || skiff_core::open::open_default(&path)).await
}

/// Opens a web address in the default browser.
#[tauri::command]
pub(crate) async fn open_url(url: String) -> Result<(), String> {
    blocking(move || skiff_core::open::open_url(&url)).await
}

#[tauri::command]
pub(crate) async fn list_themes() -> Result<Vec<skiff_core::theme::TerminalTheme>, String> {
    // The desktop ships its theme catalogue. A running daemon can be older.
    tokio::task::spawn_blocking(skiff_core::theme::list)
        .await.map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn import_themes(paths: Vec<PathBuf>) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || skiff_core::theme::import_files(&paths))
        .await.map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn open_theme_folder() -> Result<(), String> {
    let path = skiff_core::theme::theme_dir().ok_or("Cannot find the theme folder")?;
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    blocking(move || skiff_core::open::open_default(&path)).await
}

/// A background image's raw bytes. The page turns them into a blob URL.
#[tauri::command]
pub(crate) async fn read_image(path: PathBuf) -> Result<tauri::ipc::Response, String> {
    let bytes = tokio::fs::read(&path).await.map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// One folder for the sidebar's file tree. Read here, not in the daemon:
/// the tree is a view, and the daemon owns only sessions.
#[tauri::command]
pub(crate) async fn list_dir(path: PathBuf) -> Result<Vec<skiff_core::files::Entry>, String> {
    tokio::task::spawn_blocking(move || skiff_core::files::list_dir(&path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

/// The changed files of one worktree, for the sidebar's Changes section.
#[tauri::command]
pub(crate) async fn git_changes(path: PathBuf) -> Result<Vec<skiff_core::git::Change>, String> {
    tokio::task::spawn_blocking(move || skiff_core::git::changes(&path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

/// What happened to an open request.
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Opened {
    /// The page runs `script` in the peek, in `cwd`. It shows only if the
    /// script turns out to be a terminal tool.
    Run { script: String, cwd: PathBuf },
    /// No command is set, and the default app took the file.
    Default,
    /// No command is set, and the OS has no app for the file.
    NoApp { message: String },
}

async fn run_plan(plan: skiff_core::open::Plan, cwd: PathBuf, file: PathBuf) -> Result<Opened, String> {
    use skiff_core::open::Plan;
    tokio::task::spawn_blocking(move || match plan {
        Plan::Run(script) => Ok(Opened::Run { script, cwd }),
        Plan::Default => Ok(match skiff_core::open::open_default(&file) {
            Ok(()) => Opened::Default,
            Err(e) => Opened::NoApp { message: format!("{e:#}") },
        }),
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Shows a changed file's diff, or the whole worktree's, with `[open] diff`.
#[tauri::command]
pub(crate) async fn open_diff(path: PathBuf, file: Option<String>) -> Result<Opened, String> {
    let open = skiff_core::config::load().map(|c| c.open).unwrap_or_default();
    let dir = path.clone();
    let plan = tokio::task::spawn_blocking(move || skiff_core::open::plan_diff(&dir, file.as_deref(), &open))
        .await
        .map_err(|e| e.to_string())?;
    run_plan(plan, path.clone(), path).await
}

/// The `[open]` commands, empty for a default, and the default diff command.
#[derive(serde::Serialize)]
pub(crate) struct OpenSettings {
    peek: Vec<String>,
    diff: String,
    text: String,
    markdown: String,
    html: String,
    default_diff: &'static str,
}

fn open_settings_now() -> Result<OpenSettings, String> {
    let o = skiff_core::config::load().map_err(err)?.open;
    Ok(OpenSettings {
        peek: o.peek,
        diff: o.diff.unwrap_or_default(),
        text: o.text.unwrap_or_default(),
        markdown: o.markdown.unwrap_or_default(),
        html: o.html.unwrap_or_default(),
        default_diff: skiff_core::git::DEFAULT_DIFF,
    })
}

#[tauri::command]
pub(crate) fn open_settings() -> Result<OpenSettings, String> {
    open_settings_now()
}

/// Sets one `[open]` command. Empty: the default.
#[tauri::command]
pub(crate) fn set_open(key: String, command: String) -> Result<OpenSettings, String> {
    let c = command.trim();
    skiff_core::config::set_open(&key, (!c.is_empty()).then_some(c)).map_err(err)?;
    open_settings_now()
}

#[tauri::command]
pub(crate) fn set_open_peek(key: String, peek: bool) -> Result<OpenSettings, String> {
    skiff_core::config::set_open_peek(&key, peek).map_err(err)?;
    open_settings_now()
}

#[derive(serde::Serialize)]
pub(crate) struct FilePreview {
    script: Option<String>,
    cwd: PathBuf,
    app: String,
    peek: bool,
}

/// Plans a selection preview without starting an external app.
#[tauri::command]
pub(crate) async fn preview_file(path: PathBuf) -> Result<FilePreview, String> {
    tokio::task::spawn_blocking(move || {
        let open = skiff_core::config::load().map_err(err)?.open;
        let command = skiff_core::open::command_for(&path, &open);
        let app = command.as_deref().and_then(|c| c.split_whitespace().next())
            .and_then(|c| std::path::Path::new(c).file_name()).and_then(|c| c.to_str())
            .unwrap_or("default app").to_string();
        let script = match skiff_core::open::plan_file(&path, &open) {
            skiff_core::open::Plan::Run(script) => Some(script),
            skiff_core::open::Plan::Default => None,
        };
        let peek = skiff_core::open::previews_file(&path, &open);
        let cwd = path.parent().unwrap_or(std::path::Path::new("/")).to_path_buf();
        Ok(FilePreview { script, cwd, app, peek })
    }).await.map_err(|e| e.to_string())?
}

/// Opens a file with its `[open]` command, else in its default app.
#[tauri::command]
pub(crate) async fn open_file(path: PathBuf) -> Result<Opened, String> {
    let open = skiff_core::config::load().map(|c| c.open).unwrap_or_default();
    let file = path.clone();
    let plan = tokio::task::spawn_blocking(move || skiff_core::open::plan_file(&file, &open))
        .await
        .map_err(|e| e.to_string())?;
    let cwd = path.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    run_plan(plan, cwd, path).await
}

/// Projects with `files = true`. Read from projects.toml here, so an older
/// daemon that does not send the key still works.
#[tauri::command]
pub(crate) fn files_projects() -> Result<Vec<String>, String> {
    let cfg = skiff_core::config::load().map_err(err)?;
    Ok(cfg.projects.into_iter().filter(|p| p.files).map(|p| p.name).collect())
}

/// Projects with `changes = false`.
#[tauri::command]
pub(crate) fn hidden_changes_projects() -> Result<Vec<String>, String> {
    let cfg = skiff_core::config::load().map_err(err)?;
    Ok(cfg.projects.into_iter().filter(|p| p.changes == Some(false)).map(|p| p.name).collect())
}

#[tauri::command]
pub(crate) fn set_project_changes(project: String, on: bool) -> Result<(), String> {
    skiff_core::config::set_project_changes(&project, on).map_err(err)
}

/// Each project's own theme id, as `[name, id]`.
#[tauri::command]
pub(crate) fn project_themes() -> Result<Vec<(String, String)>, String> {
    let cfg = skiff_core::config::load().map_err(err)?;
    Ok(cfg.projects.into_iter().filter_map(|p| p.theme.map(|t| (p.name, t))).collect())
}

#[tauri::command]
pub(crate) fn set_project_theme(project: String, theme: Option<String>) -> Result<(), String> {
    skiff_core::config::set_project_theme(&project, theme.as_deref()).map_err(err)
}

#[tauri::command]
pub(crate) fn set_project_files(project: String, on: bool) -> Result<(), String> {
    skiff_core::config::set_project_files(&project, on).map_err(err)
}

#[tauri::command]
pub(crate) async fn reveal_file(path: PathBuf) -> Result<(), String> {
    blocking(move || reveal(&std::fs::canonicalize(&path)?)).await
}

/// Shows the file selected in its folder.
#[cfg(target_os = "macos")]
fn reveal(path: &std::path::Path) -> anyhow::Result<()> {
    skiff_core::open::launch("open -R", path)
}

/// Asks the file manager to select the file over D-Bus. A file manager
/// without FileManager1 opens the folder instead.
#[cfg(target_os = "linux")]
fn reveal(path: &std::path::Path) -> anyhow::Result<()> {
    use gtk::{gio::{self, prelude::FileExt}, glib::ToVariant};
    let shown = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).and_then(|bus| {
        let uri = gio::File::for_path(path).uri().to_string();
        bus.call_sync(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1",
            "ShowItems",
            Some(&(vec![uri], "").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
    });
    match shown {
        Ok(_) => Ok(()),
        Err(_) => skiff_core::open::open_default(path.parent().unwrap_or(path)),
    }
}

#[tauri::command]
pub(crate) async fn list_agents(app: State<'_, App>) -> Result<Vec<AgentInfo>, String> {
    let (c, _) = ensure_client(&app).await?;
    c.list_agents().await.map_err(err)
}

#[tauri::command]
pub(crate) async fn list_agent_sessions(app: State<'_, App>, command: Vec<String>, cwd: String) -> Result<String, String> {
    let (c, _) = ensure_client(&app).await?;
    c.list_agent_sessions(command, cwd.into()).await.map_err(err)
}

#[tauri::command]
pub(crate) async fn list_groups(app: State<'_, App>) -> Result<Vec<Group>, String> {
    let (c, _) = ensure_client(&app).await?;
    c.list_groups().await.map_err(err)
}

#[tauri::command]
pub(crate) async fn save_group(app: State<'_, App>, group: Group) -> Result<Group, String> {
    let (c, _) = ensure_client(&app).await?;
    c.save_group(group).await.map_err(err)
}

#[tauri::command]
pub(crate) async fn delete_group(app: State<'_, App>, group: String) -> Result<(), String> {
    let (c, _) = ensure_client(&app).await?;
    c.delete_group(&group).await.map_err(err)
}
