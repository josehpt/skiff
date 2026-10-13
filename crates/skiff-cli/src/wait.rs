//! `skiff wait`: block until sessions reach a state.

use std::{collections::HashMap, time::Duration};

use anyhow::{bail, Result};
use skiff_client::Client;
use skiff_core::{protocol::Event, session::SessionState};
use tokio::{
    sync::broadcast::error::{RecvError, TryRecvError},
    time::Instant,
};

use crate::{print, resolve, sessions};

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Target {
    Idle,
    Waiting,
    Done,
    /// Idle, waiting or done.
    AnyNotWorking,
}

impl Target {
    fn hit(self, s: SessionState) -> bool {
        match self {
            Target::Idle => s == SessionState::Idle,
            Target::Waiting => s == SessionState::Waiting,
            Target::Done => s == SessionState::Done,
            Target::AnyNotWorking => s != SessionState::Working,
        }
    }
}

/// Where one session stands.
#[derive(Clone, Copy, PartialEq)]
enum Now {
    State(SessionState),
    /// Closed, or killed.
    Gone,
}

/// Exit codes: 0 when every session got there, 1 on timeout, 2 when a
/// session went away or exited before it got there, 3 when a session is
/// waiting. Only a person answers an approval, so unless the target is
/// `waiting`, a waiting session ends the wait at once.
///
/// With `any`, the first session to get there ends the wait with 0, and
/// only that session prints. 2 then means every session went away or
/// exited first.
pub async fn wait(c: &Client, args: &[String], target: Target, any: bool, timeout: Option<f64>) -> Result<i32> {
    // Listen before the list, so no change falls between the two.
    let mut events = c.events();
    let all = sessions::sorted(c).await?;
    let mut ids: Vec<String> = Vec::new();
    let mut names = HashMap::new();
    for a in args {
        let s = match resolve::session(&all, a) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("skiff: {e:#}");
                return Ok(2);
            }
        };
        if !ids.contains(&s.id) {
            ids.push(s.id.clone());
            names.insert(s.id.clone(), resolve::display_name(s).to_string());
        }
    }
    let mut now: HashMap<String, Now> =
        all.iter().filter(|s| ids.contains(&s.id)).map(|s| (s.id.clone(), Now::State(s.state))).collect();
    let stop_on_waiting = !matches!(target, Target::Waiting);
    let needs_person = |id: &String| -> i32 {
        eprintln!("skiff: {}", sessions::waiting_msg(&names[id], id));
        3
    };
    if stop_on_waiting {
        if let Some(id) = ids.iter().find(|id| now[*id] == Now::State(SessionState::Waiting)) {
            return Ok(needs_person(id));
        }
    }

    // A timeout too large to count is no timeout.
    let deadline = timeout
        .and_then(|t| Duration::try_from_secs_f64(t.max(0.0)).ok())
        .and_then(|d| Instant::now().checked_add(d));
    // Events queued before the list reply are older than the list.
    while !matches!(events.try_recv(), Err(TryRecvError::Empty | TryRecvError::Closed)) {}
    // A session settles when it reaches the target, or can no longer reach it.
    let settled = |n: &Now| match *n {
        Now::State(s) => target.hit(s) || s == SessionState::Done,
        Now::Gone => true,
    };
    let hit = |n: &Now| matches!(*n, Now::State(s) if target.hit(s));
    let finished = |now: &HashMap<String, Now>| {
        (any && ids.iter().any(|id| hit(&now[id]))) || ids.iter().all(|id| settled(&now[id]))
    };
    let mut timed_out = false;
    while !finished(&now) {
        let next = async {
            match deadline {
                Some(d) => tokio::time::timeout_at(d, events.recv()).await.ok(),
                None => Some(events.recv().await),
            }
        };
        let Some(e) = next.await else {
            timed_out = true;
            break;
        };
        let (id, n) = match e {
            Ok(Event::State { session, state }) => (session, Now::State(state)),
            Ok(Event::Exit { session, .. }) => (session, Now::State(SessionState::Done)),
            Ok(Event::SessionRemoved { session }) => (session, Now::Gone),
            Ok(_) => continue,
            Err(RecvError::Lagged(_)) => {
                // Missed events: read the states again.
                let all = c.list_sessions().await?;
                if stop_on_waiting {
                    let waiting = |id: &&String| all.iter().any(|s| &s.id == *id && s.state == SessionState::Waiting);
                    if let Some(id) = ids.iter().find(waiting) {
                        return Ok(needs_person(id));
                    }
                }
                for id in &ids {
                    let cur = now.get_mut(id).expect("listed");
                    if !settled(cur) {
                        *cur = all.iter().find(|s| &s.id == id).map_or(Now::Gone, |s| Now::State(s.state));
                    }
                }
                continue;
            }
            Err(RecvError::Closed) => bail!("skiffd closed the connection"),
        };
        if stop_on_waiting && n == Now::State(SessionState::Waiting) && ids.contains(&id) {
            return Ok(needs_person(&id));
        }
        // Once settled, a session keeps the state it settled in.
        if let Some(cur) = now.get_mut(&id) {
            if !settled(cur) {
                *cur = n;
            }
        }
    }

    if any {
        let first = ids.iter().find_map(|id| match now[id] {
            Now::State(s) if target.hit(s) => Some((id, s)),
            _ => None,
        });
        if let Some((id, s)) = first {
            // The full id, so a script can pass it on as it is.
            let row = vec![id.clone(), names[id].clone(), sessions::state(s).to_string()];
            print::table(&["", "", ""], &[row]);
            return Ok(0);
        }
    }
    let mut code = 0;
    let rows: Vec<Vec<String>> = ids
        .iter()
        .map(|id| {
            let n = now[id];
            let state = match n {
                Now::State(s) => sessions::state(s),
                Now::Gone => "gone",
            };
            let c = match n {
                Now::State(s) if target.hit(s) => 0,
                _ if !settled(&n) => 1,
                _ => 2,
            };
            code = code.max(c);
            vec![print::short_id(id).to_string(), names[id].clone(), state.to_string()]
        })
        .collect();
    // With `any`, a session that went away first does not count while
    // another can still get there: that is a timeout, not 2.
    if any && timed_out {
        code = 1;
    }
    print::table(&["", "", ""], &rows);
    if timed_out && code == 1 {
        eprintln!("skiff: timed out");
    }
    Ok(code)
}
