//! The Skiff agent's session (Skipper): at most one live coordinator per
//! daemon. The app and the CLI find or start it with one guarded request, so
//! two quick opens cannot start two.

use std::{sync::Arc, time::{Duration, Instant}};

use anyhow::Result;
use skiff_core::{
    protocol::Event,
    session::{Role, SessionInfo, SessionSpec, SessionState},
};

use super::{Front, Session, SessionPool};

/// A start or resume line typed into the coordinator's shell counts this
/// long: the agent is starting, so a second open types nothing.
const STARTING: Duration = Duration::from_secs(15);

pub struct Coordinated {
    pub session: SessionInfo,
    /// It started now.
    pub created: bool,
    /// A start or resume line went into its shell now.
    pub typed: bool,
}

/// What [`SessionPool::coordinator`] does.
pub struct Ask {
    /// Start a coordinator with this when none lives. `None`: only find.
    pub spec: Option<SessionSpec>,
    /// The line that starts the agent in a bare shell.
    pub start: Option<String>,
    /// The line that continues the agent's last conversation, for a shell a
    /// restore brought back from an agent.
    pub resume: Option<String>,
    /// The `input_gen` the client read. Default: the current one.
    pub expect: Option<u64>,
}

impl SessionPool {
    /// Finds the live coordinator, or starts one from `ask.spec`. A
    /// coordinator that is a bare shell gets its start or resume line. All
    /// under one lock, so a second call sees what the first did.
    pub fn coordinator(self: &Arc<Self>, ask: Ask) -> Result<Option<Coordinated>> {
        let mut typed_at = self.coordinator.lock().unwrap();
        let live = self
            .sessions
            .read()
            .unwrap()
            .values()
            .find(|s| {
                let i = s.info.lock().unwrap();
                i.coordinator && i.state != SessionState::Done
            })
            .cloned();
        if let Some(s) = live {
            let typed = self.wake_coordinator(&s, &mut typed_at, &ask)?;
            return Ok(Some(Coordinated { session: s.info(), created: false, typed }));
        }
        let Some(mut spec) = ask.spec.clone() else { return Ok(None) };
        if let Some(s) = self.unmarked(&spec) {
            {
                let _change = self.change()?;
                s.info.lock().unwrap().coordinator = true;
                tracing::info!("session {} is the coordinator: an older Skiff started it", s.info.lock().unwrap().id);
                let _ = self.events.send(Event::SessionUpdated { session: s.info() });
                self.dirty.notify_one();
            }
            // After the change guard: the submit takes its own.
            let typed = self.wake_coordinator(&s, &mut typed_at, &ask)?;
            return Ok(Some(Coordinated { session: s.info(), created: false, typed }));
        }
        let _change = self.change()?;
        spec.coordinator = true;
        let info = self.spawn(spec, None)?;
        // An exited one keeps its pane, not the role: a restore brings back one.
        for s in self.sessions.read().unwrap().values() {
            let mut i = s.info.lock().unwrap();
            if i.coordinator && i.id != info.id {
                i.coordinator = false;
                let _ = self.events.send(Event::SessionUpdated { session: i.clone() });
            }
        }
        self.dirty.notify_one();
        Ok(Some(Coordinated { session: info, created: true, typed: false }))
    }

    /// A Skiff agent session an older Skiff started, before the daemon
    /// marked it: a live agent session with the name and the folder `spec`
    /// asks for. The oldest one, when there are more. Without this, the
    /// first open after an update would start a second agent.
    fn unmarked(&self, spec: &SessionSpec) -> Option<Arc<Session>> {
        let real = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let cwd = real(spec.cwd.as_deref()?);
        let name = spec.name.as_deref()?;
        let sessions = self.sessions.read().unwrap();
        sessions
            .values()
            .filter(|s| {
                let i = s.info.lock().unwrap();
                i.state != SessionState::Done
                    && !i.peek
                    && i.role == Role::Agent
                    && i.name.as_deref() == Some(name)
                    && real(&i.cwd) == cwd
            })
            .min_by_key(|s| s.info.lock().unwrap().started_at)
            .cloned()
    }

    /// Types the start or resume line into a coordinator that is a bare
    /// shell at its prompt. True when it typed.
    fn wake_coordinator(&self, s: &Session, typed_at: &mut Option<(String, Instant)>, ask: &Ask) -> Result<bool> {
        if s.front() != Front::Shell {
            return Ok(false);
        }
        let (id, restored, gen) = {
            let i = s.info.lock().unwrap();
            (i.id.clone(), i.resume.is_some(), i.input_gen)
        };
        if typed_at.as_ref().is_some_and(|(t, at)| *t == id && at.elapsed() < STARTING) {
            return Ok(false);
        }
        let line = match (&ask.resume, &ask.start) {
            (Some(r), _) if restored => r,
            (_, Some(l)) => l,
            _ => return Ok(false),
        };
        let sent = self.submit(&id, line.as_bytes(), false, true, ask.expect.unwrap_or(gen))?;
        if sent {
            *typed_at = Some((id, Instant::now()));
        }
        Ok(sent)
    }
}
