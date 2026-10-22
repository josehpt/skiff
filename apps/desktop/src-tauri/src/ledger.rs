//! Skipper's ledger for the panel beside its pane, from the selected Host
//! (`ledger@1`): This Mac and an SSH Host alike. The Host reads its own
//! `ledger.db` read-only and serves a row's result as bounded text or bytes.
//! The app never opens a ledger, a result or its path, and never writes:
//! only Skipper changes rows, through `skiff ledger` on the Host.

use std::{sync::atomic::Ordering, time::Duration};

use skiff_host_api::{HostFile, HostLedgerRow};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::connection::{data_read, App};

/// The page reads the ledger again on this event.
const LEDGER_CHANGED: &str = "skiff:ledger-changed";

/// How often the watcher asks the Host for the ledger's stamp.
const TICK: Duration = Duration::from_secs(1);
/// A remote Host is asked less often: each ask crosses the link.
const REMOTE_TICK: Duration = Duration::from_secs(3);

/// Every row the panel shows, oldest first. Plans carry the state their
/// tasks give them. A Host with no `ledger.db` gives none.
#[tauri::command]
pub async fn ledger_rows(app: State<'_, App>) -> Result<Vec<HostLedgerRow>, String> {
    data_read(&app, |c| async move { skiff_client::host::data::ledger_rows(&c).await }).await
}

/// A row's result, read by the Host from the path its row records, for the
/// page's own viewer. No path is named here, and nothing is opened on this Mac.
#[tauri::command]
pub async fn ledger_result(app: State<'_, App>, uid: String) -> Result<HostFile, String> {
    data_read(&app, |c| async move { skiff_client::host::data::ledger_result(&c, &uid).await }).await
}

/// What the watcher saw last: the data connection and the Host's stamp.
type Seen = Option<(u64, String)>;

/// Runs at app start for the app's lifetime. Emits [`LEDGER_CHANGED`] when
/// the selected Host's ledger stamp changes, or when a new data connection
/// (a reconnect, a Host switch, a restarted `skiff-host`) may bring other
/// rows. It asks only over a live session plane and never starts skiffd.
pub fn watch(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last = None;
        loop {
            let state = app.state::<App>();
            let tick = if state.remote() { REMOTE_TICK } else { TICK };
            if look(&state, &mut last).await {
                let _ = app.emit(LEDGER_CHANGED, ());
            }
            tokio::time::sleep(tick).await;
        }
    });
}

/// One tick. True when the ledger may have changed since the last tick.
async fn look(app: &App, last: &mut Seen) -> bool {
    let live = app.host.lock().await.as_ref().map(|h| h.connection()).filter(|c| !c.is_closed());
    if live.is_none() {
        return false;
    }
    let stamp = data_read(app, |c| async move { skiff_client::host::data::ledger_stamp(&c).await }).await;
    let Ok(stamp) = stamp else { return false };
    let now = Some((app.data_gen.load(Ordering::SeqCst), stamp));
    let changed = last.is_some() && now != *last;
    *last = now;
    changed
}
