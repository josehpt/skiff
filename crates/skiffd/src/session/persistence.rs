use super::{SessionPool, SavedSession, Workspace, SessionSpec, Arc, Path, SAVE_DELAY};
use super::groups::prune;
use crate::{paths, workspace};

impl SessionPool {
    /// Every group and session as they stand. Exited sessions count: their
    /// panes are still in the layout.
    fn workspace(&self) -> Workspace {
        let groups = self.groups.lock().unwrap();
        let sessions = self.sessions.read().unwrap();
        let mut saved: Vec<SavedSession> = sessions
            .values()
            .map(|s| s.info.lock().unwrap())
            // A peek tool lasts as long as the app's peek.
            .filter(|info| !info.peek)
            .map(|info| SavedSession::from(&*info))
            .collect();
        saved.sort_by(|a, b| a.id.cmp(&b.id));
        Workspace {
            groups: groups.clone(),
            sessions: saved,
        }
    }

    /// Writes the workspace file now. On shutdown, so the last changes count.
    pub fn save_now(&self) {
        if let Err(e) = workspace::save(&paths::state(), &self.workspace()) {
            tracing::warn!("save workspace: {e:#}");
        }
    }

    /// Writes the workspace file after changes, at most once per `SAVE_DELAY`.
    pub fn spawn_saver(self: &Arc<Self>) {
        let pool = self.clone();
        tokio::spawn(async move {
            loop {
                pool.dirty.notified().await;
                tokio::time::sleep(SAVE_DELAY).await;
                let pool = pool.clone();
                // A reload saves on its own, and must not exec halfway through a save.
                let saved = tokio::task::spawn_blocking(move || {
                    let Some(_quiet) = pool.quiet() else { return Ok(()) };
                    workspace::save(&paths::state(), &pool.workspace())
                })
                .await;
                if let Ok(Err(e)) = saved {
                    tracing::warn!("save workspace: {e:#}");
                }
            }
        });
    }

    /// Brings back the saved groups, and each saved session as a shell in its
    /// folder under its old id, so the layouts still point at it. A session
    /// that cannot start leaves its groups the way a kill would.
    pub fn restore(self: &Arc<Self>) {
        let file = paths::state();
        let ws = match workspace::load(&file) {
            Ok(ws) => ws,
            Err(e) => {
                tracing::warn!("restore workspace: {e:#}");
                return;
            }
        };
        let mut groups = ws.groups;
        let mut restored = 0;
        for saved in ws.sessions {
            // A worktree removed meanwhile: start in the home folder instead.
            let cwd = saved.cwd.is_dir().then(|| saved.cwd.clone());
            let spec = SessionSpec {
                label: String::new(),
                name: saved.name.clone(),
                role: saved.role,
                cwd,
                command: None,
                args: Vec::new(),
                cols: saved.cols,
                rows: saved.rows,
                ..Default::default()
            };
            match self.spawn(spec, Some(saved.id.clone())) {
                Ok(_) => {
                    restored += 1;
                    if saved.theme.is_some() {
                        let _ = self.set_theme(&saved.id, saved.theme);
                    }
                    if let Some(s) = self.get(&saved.id) {
                        let mut info = s.info.lock().unwrap();
                        info.resume = saved.was.clone();
                        info.was = saved.was;
                        info.agent_exit = saved.agent_exit;
                    }
                }
                Err(e) => {
                    tracing::warn!("restore session {}: {e:#}", saved.id);
                    prune(&mut groups, &saved.id, &saved.cwd);
                }
            }
        }
        // Drop panes whose session was never saved.
        {
            let sessions = self.sessions.read().unwrap();
            for g in groups.iter_mut() {
                for id in g.layout.sessions() {
                    if !skiff_core::group::is_slot(&id) && !sessions.contains_key(&id) {
                        prune(std::slice::from_mut(g), &id, Path::new(""));
                    }
                }
            }
        }
        tracing::info!("restored {} groups and {restored} sessions from {}", groups.len(), file.display());
        *self.groups.lock().unwrap() = groups;
    }
}
