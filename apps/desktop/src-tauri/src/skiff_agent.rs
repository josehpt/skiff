//! The Skiff agent's settings, folder and session, for the page. The folder
//! setup and the find-or-start rule live in `skiff_client::skipper`, which
//! `skiff skipper` uses too, so the app and the CLI pick the same session.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use skiff_client::{
    agent_folder::{self, Mode},
    skipper,
};
use skiff_core::{
    config::{self, SkiffAgentKey},
    session::SessionInfo,
};
use skiff_host_api::HostSkipper;
use tauri::{AppHandle, Emitter, State};

use crate::{
    config::PROJECTS_CHANGED,
    connection::{ensure_client, err, App},
};
use skiff_client::host::local::ProcessControl;

/// Runs off the main thread at app start, and again on a switch to this Mac.
/// Never prompts. Copies the built-in `skiff-cli` skill out when it changed:
/// this Mac's Skipper setup, so only while this Mac is the shown Host, and
/// once per app run. True when it ran now.
pub(crate) fn sync(app: &App) -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if app.remote() || DONE.swap(true, Ordering::SeqCst) {
        return false;
    }
    if let Err(e) = skipper::sync_skill() {
        eprintln!("skiff-cli skill: {e:#}");
    }
    true
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
/// daemon. The page calls it on every start and every restore. True when it
/// started one.
#[tauri::command]
pub async fn start_skiff_watch(app: State<'_, App>, session: String) -> Result<bool, String> {
    // A remote Host's watcher runs there; this app only reaches it.
    let Some(local) = app.selection().process_control() else { return Ok(false) };
    ensure_watch(&app, &local, cli(), session).await
}

/// One current watcher for the agent's session. A watcher of an older CLI,
/// as after an update, gets one replacement attempt per app run when it is
/// proven to be this daemon's `skiff watch`. One that is not, or that does
/// not stop, keeps running, and only the log says so: the update is not
/// finished, and nothing asks the user. One check runs at a time. It starts
/// and signals processes, so only a local endpoint may: `_local` proves it.
pub(crate) async fn ensure_watch(app: &App, _local: &ProcessControl, cli: PathBuf, session: String) -> Result<bool, String> {
    let _one = app.watch_gate.lock().await;
    let mut known = app.watch_pids.lock().unwrap().clone();
    if let Ok(list) = crate::connection::inventory_now(app).await {
        known.extend(list.iter().filter(|s| s.id == session).filter_map(|s| s.watch.as_ref()).flat_map(|w| w.pids.iter().copied()));
    }
    // Only the bundled CLI replaces a watcher: a bare `skiff` from PATH is
    // not known to be this app's build.
    let tried = cli.is_absolute().then(|| app.watch_tried.clone());
    let socket = app.selection().socket().to_path_buf();
    let out = blocking(move || skipper::ensure_watch(&socket, &cli, skipper::CLI_VERSION, &session, &known, tried.as_deref())).await?;
    match out {
        skipper::WatchOutcome::Started => Ok(true),
        skipper::WatchOutcome::Current => Ok(false),
        skipper::WatchOutcome::Replaced { pid } => {
            eprintln!("skiff watch: replaced the older watcher (pid {pid})");
            Ok(true)
        }
        skipper::WatchOutcome::Kept(why) => {
            eprintln!("skiff watch not updated: {why}");
            Ok(false)
        }
    }
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
    let watch_error = match app.selection().process_control() {
        Some(local) => ensure_watch(&app, &local, cli(), found.session.id.clone()).await.err(),
        None => None,
    };
    if let Some(e) = &watch_error {
        eprintln!("skiff watch: {e}");
    }
    Ok(Some(Opened { session: found.session, created: found.created, typed: found.typed, watch_error }))
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

/// `[skiff_agent]` and its task types, from the selected Host (`skipper@1`):
/// This Mac's and an SSH Host's alike. Nothing is read from a config here.
#[tauri::command]
pub async fn get_skiff_agent(app: State<'_, App>) -> Result<HostSkipper, String> {
    read(&app).await
}

async fn read(app: &App) -> Result<HostSkipper, String> {
    crate::connection::data_read(app, |c| async move { skiff_client::host::data::skipper_settings(&c).await }).await
}

/// Saves one setting on This Mac (the Host's config is this Mac's: the
/// command is local-only) and answers with all of them, read again through
/// the Host. The page reloads its projects on the event, so the row follows
/// at once.
#[tauri::command]
pub async fn set_skiff_agent(app: State<'_, App>, handle: AppHandle, edit: Edit) -> Result<HostSkipper, String> {
    blocking(move || {
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
        Ok(())
    })
    .await?;
    let _ = handle.emit(PROJECTS_CHANGED, ());
    read(&app).await
}

/// A preview for the Change… dialog, with Skipper's session when it runs.
#[derive(Serialize)]
pub struct FolderPreview {
    #[serde(flatten)]
    preview: agent_folder::Preview,
    /// Skipper's session runs: the change ends it first.
    skipper_runs: bool,
}

/// The chosen folder, resolved and read. A relative path resolves against
/// the home folder. Changes nothing.
#[tauri::command]
pub async fn preview_skiff_folder(app: State<'_, App>, path: String) -> Result<FolderPreview, String> {
    let (panes, skipper) = live(&app).await;
    blocking(move || {
        let home = skipper::home()?;
        let to = agent_folder::resolve(&path, &home, &home)?;
        let preview = agent_folder::preview(&config::load()?, &to, &panes)?;
        Ok(FolderPreview { preview, skipper_runs: skipper.is_some() })
    })
    .await
}

/// The live panes and Skipper's session. With no daemon, none run.
async fn live(app: &App) -> (agent_folder::Panes, Option<SessionInfo>) {
    match ensure_client(app).await {
        Ok((c, _)) => agent_folder::sessions(&c).await.unwrap_or_default(),
        Err(_) => Default::default(),
    }
}

/// Copy, check and switch, start empty, or switch to a Skiff folder. `path`
/// is the preview's resolved folder. Skipper's session ends first when
/// `end_skipper`, else a running one stops the change. `anyway` lets the
/// change run while tasks still run.
#[tauri::command]
pub async fn change_skiff_folder(
    app: State<'_, App>,
    handle: AppHandle,
    path: PathBuf,
    mode: Mode,
    end_skipper: bool,
    anyway: bool,
) -> Result<agent_folder::Done, String> {
    let (mut panes, skipper) = live(&app).await;
    if let Some(s) = skipper {
        if !end_skipper {
            return Err("Skipper runs. It ends before the change.".into());
        }
        let c = crate::connection::host_now(&app).await?;
        skiff_client::host::local::legacy_kill(&c, &s.id).await.map_err(err)?;
        panes.remove(&s.id);
    }
    let done = blocking(move || agent_folder::change(&config::load()?, &path, mode, &panes, anyway)).await?;
    let _ = handle.emit(PROJECTS_CHANGED, ());
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::Edit;

    /// Review P2: a launch with a remote Host selected leaves this Mac's
    /// Skipper skill alone (private config; the review's repro wrote it).
    #[test]
    fn a_remote_launch_does_not_set_up_this_macs_skipper() {
        let _env = crate::reload::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("skiff-remote-skill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = std::env::var_os("SKIFF_CONFIG");
        std::env::set_var("SKIFF_CONFIG", dir.join("projects.toml"));
        let app = crate::connection::App::default();
        let target = skiff_client::host::ssh::SshTarget { destination: "devbox".into(), socket: "/s.sock".into() };
        let options = skiff_client::host::ssh::SshOptions { program: "/nonexistent/ssh".into(), config: None };
        app.set_selection(skiff_client::host::local::HostSelection::ssh(target, options, dir.join("fwd"), None).unwrap());
        assert!(!super::sync(&app));
        assert!(!dir.join("skills").exists(), "a remote launch wrote this Mac's skill");
        match old {
            Some(v) => std::env::set_var("SKIFF_CONFIG", v),
            None => std::env::remove_var("SKIFF_CONFIG"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    /// The modes `Mode` in apps/desktop/src/appearance/skiffFolderDialog.ts sends.
    #[test]
    fn reads_the_folder_modes_the_page_sends() {
        use skiff_client::agent_folder::Mode;
        for (text, mode) in [("copy", Mode::Copy), ("empty", Mode::Empty), ("switch", Mode::Switch)] {
            assert_eq!(serde_json::from_value::<Mode>(serde_json::json!(text)).unwrap(), mode);
        }
    }
}
