//! Every application exit goes through the page's workspace save guard.
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Default)]
pub(crate) struct Guard {
    ready: AtomicBool,
    approved: AtomicBool,
}

impl Guard {
    fn approve(&self) {
        self.approved.store(true, Ordering::SeqCst);
    }

    /// An approval permits one exit request, not future native quit requests.
    fn allow_exit(&self) -> bool {
        !self.ready.load(Ordering::SeqCst) || self.approved.swap(false, Ordering::SeqCst)
    }
}

pub(crate) fn requested(app: &AppHandle, api: &tauri::ExitRequestApi) {
    if !app.state::<Guard>().allow_exit() {
        api.prevent_exit();
        let _ = app.emit("skiff:quit-requested", ());
    }
}

/// Enable interception only after the page listens, before it opens a document.
/// An early boot failure must not prevent quitting an app with no open document.
#[tauri::command]
pub(crate) fn guard_quit(guard: State<'_, Guard>) {
    guard.ready.store(true, Ordering::SeqCst);
}

/// Called only after the page's Save / Don't Save / Cancel guard succeeds.
#[tauri::command]
pub(crate) fn quit_app(app: AppHandle, guard: State<'_, Guard>) {
    guard.approve();
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::Guard;
    use std::sync::atomic::Ordering;

    #[test]
    fn native_quit_needs_a_fresh_approval() {
        let guard = Guard::default();
        assert!(guard.allow_exit());
        guard.ready.store(true, Ordering::SeqCst);
        assert!(!guard.allow_exit());
        // Cancel or a failed/cancelled save never calls approve.
        assert!(!guard.allow_exit());
        guard.approve();
        assert!(guard.allow_exit());
        assert!(!guard.allow_exit());
    }
}
