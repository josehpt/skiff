//! Session commands: ls, new, send, rename, kill.

use std::{io::Read, path::PathBuf, time::Duration};

use anyhow::{bail, Context, Result};
use skiff_client::Client;
use skiff_core::{
    group::Group,
    session::{Role, SessionInfo, SessionSpec, SessionState},
};

use crate::{callsign::callsign, groups, layout::Side, print, resolve, watch};

/// Sessions in a stable order: oldest first, then by id. Tools in the app's
/// peek are not sessions to the user, so they are left out.
pub async fn sorted(c: &Client) -> Result<Vec<SessionInfo>> {
    let mut all = c.list_sessions().await?;
    all.retain(|s| !s.peek);
    all.sort_by(|a, b| a.started_at.cmp(&b.started_at).then_with(|| a.id.cmp(&b.id)));
    Ok(all)
}

pub async fn ls(c: &Client, json: bool) -> Result<()> {
    let all = sorted(c).await?;
    if json {
        return print::json(&all);
    }
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|s| {
            let mut cmd = s.command.clone();
            for a in &s.args {
                cmd.push(' ');
                cmd.push_str(a);
            }
            vec![
                print::short_id(&s.id).to_string(),
                resolve::display_name(s).to_string(),
                state(s.state).to_string(),
                role(s.role).to_string(),
                print::tilde(&s.cwd),
                cmd,
            ]
        })
        .collect();
    print::table(&["ID", "NAME", "STATE", "ROLE", "CWD", "COMMAND"], &rows);
    Ok(())
}

pub fn state(s: SessionState) -> &'static str {
    match s {
        SessionState::Working => "working",
        SessionState::Waiting => "waiting",
        SessionState::Idle => "idle",
        SessionState::Done => "done",
    }
}

pub fn role(r: Role) -> &'static str {
    match r {
        Role::Agent => "agent",
        Role::Task => "task",
        Role::Server => "server",
        Role::Shell => "shell",
    }
}

pub struct New {
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
    pub agent: Option<String>,
    pub role: Option<Role>,
    pub command: Vec<String>,
}

/// Builds the spec the way the app's `newSession` does, so the sidebar
/// shows a CLI session like one made from the + menu.
pub async fn spec(c: &Client, o: New) -> Result<SessionSpec> {
    let here = std::env::current_dir().context("current directory")?;
    let cwd = match o.cwd {
        Some(d) => here.join(d),
        None => here,
    };
    if !cwd.is_dir() {
        bail!("{} is not a directory", cwd.display());
    }
    // `SessionSpec::default()` has 0x0; serde's 120x40 default applies only
    // when decoding. Use the app's size for a pane it has not measured.
    let mut spec = SessionSpec {
        cwd: Some(cwd),
        cols: 120,
        rows: 40,
        ..SessionSpec::default()
    };
    let words: Vec<String> = if let Some(id) = &o.agent {
        let agents = c.list_agents().await?;
        let Some(a) = agents.iter().find(|a| &a.id == id) else {
            let ids: Vec<&str> = agents.iter().map(|a| a.id.as_str()).collect();
            bail!("no agent {id:?}; known: {}", ids.join(", "));
        };
        if !a.installed {
            bail!("agent {id:?} is not installed: {} is not on skiffd's PATH", a.command);
        }
        a.command.split_whitespace().map(String::from).collect()
    } else {
        o.command
    };
    spec.role = o.role.unwrap_or(if o.agent.is_some() { Role::Agent } else { Role::Shell });
    if let Some((first, rest)) = words.split_first() {
        spec.command = Some(first.clone());
        spec.args = rest.to_vec();
    }
    if spec.role == Role::Agent {
        // The app gives an agent a call sign as both label and name.
        let label = match o.name {
            Some(n) => n,
            None => {
                let live = c.list_sessions().await?;
                callsign(live.iter().map(|s| s.label.as_str()))
            }
        };
        spec.name = Some(label.clone());
        spec.label = label;
    } else {
        // Empty label: the daemon uses the program's file name.
        spec.label = o.agent.unwrap_or_default();
        spec.name = o.name;
    }
    Ok(spec)
}

/// Starts a session. With `group`, the group is found first, so a typo
/// starts nothing.
pub async fn new(c: &Client, mut o: New, group: Option<&str>, split: Option<Side>, json: bool) -> Result<()> {
    let target = match group {
        Some(arg) => {
            let all = c.list_groups().await?;
            let g = resolve::group(&all, arg)?.clone();
            groups::room(&g, &groups::Place { split, of: None })?;
            if o.cwd.is_none() {
                o.cwd = group_cwd(c, &g).await?;
            }
            Some((all, g))
        }
        None => None,
    };
    let spec = spec(c, o).await?;
    let info = c.create_session(spec).await?;
    if let Some((all, g)) = target {
        let live = c.list_sessions().await?;
        let place = groups::Place { split, of: None };
        if let Err(e) = groups::place(c, &all, g, &info.id, place, &live).await {
            eprintln!("skiff: started {} but did not place it", info.id);
            return Err(e);
        }
    }
    if json {
        return print::json(&info);
    }
    println!("{}", info.id);
    Ok(())
}

/// Where the app starts a session in `g`: the group's folder, else the
/// folder of its first session. A folder that is gone counts as none.
async fn group_cwd(c: &Client, g: &Group) -> Result<Option<PathBuf>> {
    let dir = match (&g.cwd, crate::layout::filled(&g.layout).first()) {
        (Some(d), _) => Some(PathBuf::from(d)),
        (None, Some(first)) => c.list_sessions().await?.into_iter().find(|s| &s.id == first).map(|s| s.cwd),
        (None, None) => None,
    };
    Ok(dir.filter(|d| d.is_dir()))
}


/// Why `send` must not type into `s`, as an exit code and a message.
/// `here` is `SKIFF_SESSION`; `caller_shell` is the caller's `SHELL`.
/// Checks run in a fixed order; `--shell` relaxes only the last one.
pub fn refusal(s: &SessionInfo, here: Option<&str>, shell_ok: bool, caller_shell: Option<&str>) -> Option<(i32, String)> {
    let who = format!("{} ({})", resolve::display_name(s), print::short_id(&s.id));
    if here.is_some_and(|h| h == s.id) {
        return Some((7, format!("{who} is your own session. Send to another session.")));
    }
    match s.state {
        SessionState::Waiting => return Some((3, waiting_msg(&resolve::display_name(s), &s.id))),
        SessionState::Working => {
            return Some((4, format!("{who} is busy. Skiff does not type into a working session.")))
        }
        SessionState::Done => return Some((6, format!("{who} is done. Its program exited."))),
        SessionState::Idle => {}
    }
    if shell_ok {
        return None;
    }
    let prog = s.foreground_program.as_deref().map(basename).unwrap_or("");
    if prog.is_empty() {
        return Some((
            5,
            format!("Skiff cannot identify the foreground program of {who}. Use --shell to send anyway."),
        ));
    }
    let caller = caller_shell.map(basename).unwrap_or("");
    if SHELLS.contains(&prog) || (!caller.is_empty() && prog == caller) {
        // `skiff new -- CMD` and custom agents run under a `sh` launch
        // wrapper, so their own program does not show.
        if prog == "sh" && s.role != Role::Shell {
            return Some((
                5,
                format!("Skiff cannot identify the program in front of {who}; it shows the launch shell. Use --shell to send anyway."),
            ));
        }
        return Some((5, format!("{who} has a shell ({prog}) in front. Use --shell to type a command into it.")));
    }
    None
}

/// The message for a session that needs a person: `send` and `wait` share it.
pub fn waiting_msg(name: &str, id: &str) -> String {
    format!("{name} ({}) is waiting (approval prompt or bell). Check its pane in Skiff.", print::short_id(id))
}

const SHELLS: [&str; 9] = ["bash", "zsh", "fish", "sh", "dash", "ksh", "tcsh", "csh", "nu"];

/// A program's file name. A login shell's leading `-` is dropped.
fn basename(p: &str) -> &str {
    let p = p.trim();
    let name = p.rsplit('/').next().unwrap_or(p);
    name.strip_prefix('-').unwrap_or(name)
}

pub struct Send<'a> {
    pub text: &'a [String],
    pub enter: bool,
    pub raw: bool,
    pub shell: bool,
}

/// Exit codes: 0 when sent; 3 waiting, 4 working, 5 a shell or unknown
/// program in front, 6 done, 7 own session, 9 a draft or the busy line in a
/// known agent, or input after the check. A refusal types nothing.
/// 8: Enter went to a known agent, but no turn started.
pub async fn send(c: &Client, arg: &str, o: Send<'_>) -> Result<i32> {
    let all = c.list_sessions().await?;
    let s = resolve::session(&all, arg)?;
    send_to(c, s, o).await
}

/// [`send`] to a session from a list read just now: its `input_gen` guards
/// the submit.
pub async fn send_to(c: &Client, s: &SessionInfo, o: Send<'_>) -> Result<i32> {
    let here = std::env::var("SKIFF_SESSION").ok().filter(|h| !h.is_empty());
    let caller_shell = std::env::var("SHELL").ok();
    if let Some((code, msg)) = refusal(s, here.as_deref(), o.shell, caller_shell.as_deref()) {
        eprintln!("skiff: {msg}");
        return Ok(code);
    }
    let who = format!("{} ({})", resolve::display_name(s), print::short_id(&s.id));
    let (enter, raw) = (o.enter, o.raw);
    // Only a known agent proves a turn by its state, and only its screen
    // shows a draft. A shell command can finish with no output.
    let agent = !raw && is_agent(s);
    if agent {
        if let Some(msg) = front_refusal(watch::read_front(c, s).await?) {
            eprintln!("skiff: {who} {msg}");
            return Ok(9);
        }
    }
    let mut data = if o.text == ["-"] {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf).context("read stdin")?;
        buf
    } else {
        o.text.join(" ").into_bytes()
    };
    if !raw {
        data = enter_keys(&data);
    }
    // `--enter` presses Enter once. A newline at the end of the text, as a
    // heredoc or a file has, would only add a blank line before it.
    if enter {
        while data.last() == Some(&b'\r') || data.last() == Some(&b'\n') {
            data.pop();
        }
    }
    // Listen before the submit, so no change falls before the listener.
    let events = (agent && enter).then(|| c.events());
    // A paste, so the program keeps the text's newlines as text. skiffd
    // types it only if the session took no input since the list.
    if !c.submit(&s.id, data, !raw, enter, s.input_gen).await? {
        eprintln!("skiff: {who} took input after the check. Nothing was typed.");
        return Ok(9);
    }
    let Some(events) = events else { return Ok(0) };
    if turn_started(c, &s.id, events).await? {
        return Ok(0);
    }
    // Never press Enter again: it can submit twice or add a blank line.
    eprintln!("skiff: {who}: text sent, but no turn started. It can be in the input box. Check its pane.");
    Ok(8)
}

/// Why `send` must not type into a known agent whose screen shows `f`.
pub fn front_refusal(f: watch::Front) -> Option<&'static str> {
    match f {
        watch::Front::Draft => Some("has text in its input box. Skiff does not add to a draft. The person sends or clears it."),
        watch::Front::Busy => Some("shows its busy line. Skiff does not type into a working agent."),
        watch::Front::Ready | watch::Front::NoBox => None,
    }
}

/// How long an agent has to start a turn after Enter.
const TURN_WAIT: Duration = Duration::from_secs(5);

/// The program in front is an agent Skiff knows. The daemon names a known
/// agent by its id ("claude", "codex"...), whatever binary runs it.
pub fn is_agent(s: &SessionInfo) -> bool {
    let prog = s.foreground_program.as_deref().map(basename).unwrap_or("");
    skiff_core::project::KNOWN_AGENTS.iter().any(|(id, _)| *id == prog)
}

/// True when the session leaves idle within `TURN_WAIT`: the agent took the
/// message. An exit or a removal also counts, as the program reacted.
pub async fn turn_started(
    c: &Client,
    id: &str,
    mut events: tokio::sync::broadcast::Receiver<skiff_core::protocol::Event>,
) -> Result<bool> {
    use skiff_core::protocol::Event;
    use tokio::sync::broadcast::error::RecvError;
    let deadline = tokio::time::Instant::now() + TURN_WAIT;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Err(_) => break,
            Ok(Ok(Event::State { session, state })) if session == id && state != SessionState::Idle => {
                return Ok(true)
            }
            Ok(Ok(Event::Exit { session, .. } | Event::SessionRemoved { session })) if session == id => {
                return Ok(true)
            }
            Ok(Ok(_)) => {}
            // Missed events: read the state again.
            Ok(Err(RecvError::Lagged(_))) => {
                if !still_idle(c, id).await? {
                    return Ok(true);
                }
            }
            Ok(Err(RecvError::Closed)) => bail!("skiffd closed the connection"),
        }
    }
    // The paste can start work before the listener exists. Read the state.
    Ok(!still_idle(c, id).await?)
}

async fn still_idle(c: &Client, id: &str) -> Result<bool> {
    let all = c.list_sessions().await?;
    Ok(all.iter().any(|s| s.id == id && s.state == SessionState::Idle))
}

/// Newlines as the Enter key sends them: `\r`.
fn enter_keys(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            b'\r' if data.get(i + 1) == Some(&b'\n') => {
                out.push(b'\r');
                i += 1;
            }
            b'\n' => out.push(b'\r'),
            b => out.push(b),
        }
        i += 1;
    }
    out
}

pub async fn rename(c: &Client, arg: &str, name: &str) -> Result<()> {
    let all = c.list_sessions().await?;
    let s = resolve::session(&all, arg)?;
    c.rename_session(&s.id, name).await?;
    Ok(())
}

/// Clears the session's unread mark, as focusing its pane in the app does.
pub async fn seen(c: &Client, arg: &str) -> Result<()> {
    let all = c.list_sessions().await?;
    let s = resolve::session(&all, arg)?;
    c.seen(&s.id).await
}

/// Resolves every argument first, so a typo kills nothing.
pub async fn kill(c: &Client, args: &[String]) -> Result<()> {
    let all = c.list_sessions().await?;
    let mut ids: Vec<String> = Vec::new();
    for a in args {
        let id = resolve::session(&all, a)?.id.clone();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let mut failed = 0;
    for id in &ids {
        if let Err(e) = c.kill(id).await {
            eprintln!("skiff: kill {}: {e:#}", print::short_id(id));
            failed += 1;
        }
    }
    if failed > 0 {
        bail!("{failed} of {} sessions not killed", ids.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use skiff_core::session::{SessionInfo, SessionState};

    use super::{front_refusal, refusal};

    #[test]
    fn newlines_become_enter() {
        assert_eq!(super::enter_keys(b"a\nb\r\nc\r"), b"a\rb\rc\r");
    }

    fn s(role: &str, state: SessionState, fg: Option<&str>) -> SessionInfo {
        let mut s: SessionInfo = serde_json::from_value(serde_json::json!({
            "id": "abcdef0123", "label": "x", "role": role, "cwd": "/", "command": "claude",
            "args": [], "state": "idle", "cols": 80, "rows": 24, "pid": null, "exit_code": null,
        }))
        .unwrap();
        s.state = state;
        s.foreground_program = fg.map(String::from);
        s
    }

    fn code(x: &SessionInfo, here: Option<&str>, shell: bool, caller: Option<&str>) -> i32 {
        refusal(x, here, shell, caller).map_or(0, |r| r.0)
    }

    #[test]
    fn order_and_codes() {
        use SessionState::*;
        let idle = s("agent", Idle, Some("claude"));
        assert_eq!(code(&idle, Some("abcdef0123"), true, None), 7);
        assert_eq!(code(&s("agent", Waiting, Some("claude")), Some("abcdef0123"), true, None), 7);
        assert_eq!(code(&s("agent", Waiting, Some("bash")), None, true, None), 3);
        assert_eq!(code(&s("agent", Working, None), None, true, None), 4);
        assert_eq!(code(&s("shell", Done, None), None, true, None), 6);
        assert_eq!(code(&idle, Some("other"), false, None), 0);
    }

    #[test]
    fn shell_check() {
        use SessionState::Idle;
        // An agent that exited to a shell, and a nested shell, need --shell.
        assert_eq!(code(&s("agent", Idle, Some("bash")), None, false, None), 5);
        assert_eq!(code(&s("agent", Idle, Some("/usr/bin/zsh")), None, false, None), 5);
        assert_eq!(code(&s("agent", Idle, Some("bash")), None, true, None), 0);
        // Unknown program.
        assert_eq!(code(&s("shell", Idle, None), None, false, None), 5);
        assert_eq!(code(&s("shell", Idle, Some("")), None, false, None), 5);
        assert_eq!(code(&s("shell", Idle, None), None, true, None), 0);
        // The caller's SHELL counts as a shell.
        assert_eq!(code(&s("shell", Idle, Some("xonsh")), None, false, Some("/usr/bin/xonsh")), 5);
        assert_eq!(code(&s("shell", Idle, Some("xonsh")), None, false, Some("")), 0);
        // A shell-role pane with an agent in front.
        assert_eq!(code(&s("shell", Idle, Some("codex")), None, false, Some("/bin/bash")), 0);
    }

    #[test]
    fn a_draft_or_the_busy_line_refuses_and_an_empty_box_sends() {
        use crate::watch::Front;
        assert!(front_refusal(Front::Draft).is_some());
        assert!(front_refusal(Front::Busy).is_some());
        assert!(front_refusal(Front::Ready).is_none());
        // No box to read: the state checks decide, as before.
        assert!(front_refusal(Front::NoBox).is_none());
    }

    #[test]
    fn known_agents_only() {
        use SessionState::Idle;
        for fg in ["claude", "codex", "gemini", "grok", "opencode"] {
            assert!(super::is_agent(&s("shell", Idle, Some(fg))), "{fg}");
        }
        for fg in [None, Some("bash"), Some("sh"), Some("vim"), Some("node")] {
            assert!(!super::is_agent(&s("agent", Idle, fg)), "{fg:?}");
        }
    }
}
