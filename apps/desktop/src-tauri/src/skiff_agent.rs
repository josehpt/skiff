//! The Skiff agent's settings, folder and session, for the page. The folder
//! setup and the find-or-start rule live in `skiff_client::skipper`, which
//! `skiff skipper` uses too, so the app and the CLI pick the same session.

use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use skiff_client::{
    agent_folder::{self, Kind, Mode},
    skipper,
};
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

/// Whether a watcher holds its lock, and which one, for Mission control's
/// watcher tile. Changes nothing.
#[tauri::command]
pub async fn skiff_watch_status() -> Result<skipper::WatchLock, String> {
    blocking(|| skipper::watch_status(&skiff_core::socket::socket_path())).await
}

/// Mission control's Restart watcher: stops the watcher that holds the lock
/// (SIGTERM to a checked `skiff watch` only, never SIGKILL), then starts one.
#[tauri::command]
pub async fn restart_skiff_watch(session: String) -> Result<bool, String> {
    #[cfg(unix)]
    return blocking(move || skipper::restart_watch(&skiff_core::socket::socket_path(), &cli(), &session)).await;
    #[cfg(not(unix))]
    return blocking(move || skipper::start_watch(&skiff_core::socket::socket_path(), &cli(), &session)).await;
}

/// Runs `skiff ledger ARGS` for this daemon. Its stdout, or its error text.
fn ledger(args: &[String]) -> anyhow::Result<(i32, String)> {
    let out = std::process::Command::new(cli())
        .arg("ledger")
        .args(args)
        .env("SKIFF_SOCKET", skiff_core::socket::socket_path())
        .stdin(std::process::Stdio::null())
        .output()
        .context("run skiff ledger")?;
    let code = out.status.code().unwrap_or(-1);
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().trim_start_matches("skiff: ").to_string();
    match code {
        0 => Ok((0, text(&out.stdout))),
        // `ledger send`: typed, but no turn started.
        8 if args.first().is_some_and(|a| a == "send") => Ok((8, text(&out.stderr))),
        _ => {
            let err = text(&out.stderr);
            anyhow::bail!(if err.is_empty() { format!("skiff ledger exited {code}") } else { err })
        }
    }
}

/// A uid as `ledger add` gives it: 8 hex characters.
fn uid(v: &str) -> anyhow::Result<&str> {
    if v.len() == 8 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(v)
    } else {
        anyhow::bail!("not a task uid: {v:?}")
    }
}

/// The arguments of `skiff ledger update` for Mission control's Mark done,
/// Mark resolved elsewhere… and Reassign…. Values go in `--flag=value` form,
/// so a ref that starts with `-` stays a value.
pub(crate) fn update_args(task: &str, state: Option<&str>, pane: Option<&str>, reference: Option<&str>) -> anyhow::Result<Vec<String>> {
    let mut args = vec!["update".to_string(), uid(task)?.to_string()];
    if let Some(st) = state {
        anyhow::ensure!(matches!(st, "pending" | "done"), "Mission control sets pending or done, not {st:?}");
        args.push(format!("--state={st}"));
    }
    if let Some(p) = pane {
        anyhow::ensure!(!p.is_empty() && !p.starts_with('-') && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "not a pane id: {p:?}");
        args.push(format!("--pane={p}"));
    }
    if let Some(r) = reference.map(str::trim).filter(|r| !r.is_empty()) {
        args.push(format!("--ref={r}"));
    }
    anyhow::ensure!(args.len() > 2, "nothing to change");
    Ok(args)
}

/// Every ledger row, as `skiff ledger ls --json` prints it, with LIVE and
/// interrupted marked. Mission control reads it only while it is open.
#[tauri::command]
pub async fn skiff_open_work() -> Result<serde_json::Value, String> {
    blocking(|| {
        let (_, out) = ledger(&["ls".into(), "--json".into()])?;
        serde_json::from_str(&out).context("read skiff ledger ls --json")
    })
    .await
}

/// `skiff ledger update`: the state, the pane or the ref of one task.
#[tauri::command]
pub async fn skiff_ledger_update(task: String, state: Option<String>, pane: Option<String>, reference: Option<String>) -> Result<(), String> {
    blocking(move || {
        ledger(&update_args(&task, state.as_deref(), pane.as_deref(), reference.as_deref())?)?;
        Ok(())
    })
    .await
}

/// `skiff ledger send UID --enter`, with every `send` check. Null when the
/// task went out; else why it is blocked (typed, but no turn started).
#[tauri::command]
pub async fn skiff_ledger_send(task: String) -> Result<Option<String>, String> {
    blocking(move || {
        let (code, err) = ledger(&["send".into(), uid(&task)?.to_string(), "--enter".into()])?;
        Ok((code == 8).then(|| if err.is_empty() { "No turn started. The text can be in the pane's input box.".into() } else { err }))
    })
    .await
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
    /// `~/skiff-agent`, or the override. Absent when the override is refused.
    folder: Option<String>,
    /// Why the override in projects.toml is refused.
    folder_error: Option<String>,
    /// The folder projects.toml names, refused or not, for the Folder row.
    configured: String,
    /// `ready` (a Skiff folder), `new` (setup makes it at On), `other` (it
    /// holds other files) or `refused`.
    folder_status: &'static str,
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
    let configured = skipper::configured(&cfg)?;
    let folder_status = match (&folder, agent_folder::kind(&configured)) {
        (None, _) => "refused",
        (_, Ok(Kind::Skiff)) => "ready",
        (_, Ok(Kind::New | Kind::Empty)) => "new",
        _ => "other",
    };
    let configured = configured.display().to_string();
    let a = cfg.skiff_agent;
    // The command lines come from projects.toml, so the local list builds the same ones skiffd would.
    let tasks = tasks::resolve(&a.tasks, &skiff_core::project::agents());
    Ok(SkiffAgent {
        name: a.name,
        agent: a.agent,
        enabled: a.enabled,
        folder,
        folder_error,
        configured,
        folder_status,
        continue_on_restore: a.continue_on_restore,
        tasks,
    })
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
        let (c, _) = ensure_client(&app).await?;
        c.kill(&s.id).await.map_err(err)?;
        panes.remove(&s.id);
    }
    let done = blocking(move || agent_folder::change(&config::load()?, &path, mode, &panes, anyway)).await?;
    let _ = handle.emit(PROJECTS_CHANGED, ());
    Ok(done)
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

    /// Mission control's ledger edits: only pending or done, a checked uid and
    /// pane, and a ref that cannot turn into a flag.
    #[test]
    fn builds_only_safe_ledger_updates() {
        use super::update_args;
        assert_eq!(update_args("d0e7b2f4", Some("done"), None, None).unwrap(), ["update", "d0e7b2f4", "--state=done"]);
        assert_eq!(
            update_args("d0e7b2f4", Some("done"), None, Some(" --state=sent PR #12 ")).unwrap(),
            ["update", "d0e7b2f4", "--state=done", "--ref=--state=sent PR #12"]
        );
        assert_eq!(
            update_args("d0e7b2f4", Some("pending"), Some("4ac9e7a1-77"), Some("  ")).unwrap(),
            ["update", "d0e7b2f4", "--state=pending", "--pane=4ac9e7a1-77"]
        );
        assert!(update_args("d0e7b2f4", Some("sent"), None, None).is_err());
        assert!(update_args("--help", Some("done"), None, None).is_err());
        assert!(update_args("12", Some("done"), None, None).is_err());
        assert!(update_args("d0e7b2f4", None, Some("--rm"), None).is_err());
        assert!(update_args("d0e7b2f4", None, None, None).is_err());
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
