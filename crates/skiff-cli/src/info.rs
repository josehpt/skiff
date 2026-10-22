//! status, agents, projects.

use std::path::Path;

use anyhow::Result;
use skiff_client::Client;
use skiff_core::session::SessionInfo;

use crate::{print, resolve};

pub async fn status(socket: &Path) -> Result<()> {
    let exe = std::env::current_exe().ok();
    let shown = exe.as_deref().map_or("?".to_string(), |p| p.display().to_string());
    println!("skiff     {} (skiffd {}) {shown}", env!("CARGO_PKG_VERSION"), skiff_core::VERSION);
    if let Some(w) = exe.as_deref().and_then(|e| path_warning(e, &std::env::var_os("PATH").unwrap_or_default())) {
        eprintln!("skiff: warning: {w}");
    }
    let c = crate::connect(socket).await?;
    let info = c.daemon_info().await?;
    let (version, protocol) = (info.version, info.protocol);
    if version != skiff_core::VERSION {
        eprintln!(
            "skiff: warning: skiffd is {version}, and this skiff was built for skiffd {}. Update Skiff, or run `skiff daemon reload`.",
            skiff_core::VERSION
        );
    }
    let pid = info.pid.map(|p| p as i32).or(c.daemon_pid).map_or("?".to_string(), |p| p.to_string());
    println!("skiffd    {version}");
    let note = if protocol == skiff_core::PROTOCOL {
        String::new()
    } else {
        format!(" (this skiff speaks {})", skiff_core::PROTOCOL)
    };
    println!("protocol  {protocol}{note}");
    println!("pid       {pid}");
    println!("reload    {}", info.reload_state.map_or("no".to_string(), |v| format!("yes (handover {v})")));
    println!("socket    {}", socket.display());
    let line = if protocol < 27 {
        format!("unknown (skiffd speaks {protocol})")
    } else {
        let all = crate::sessions::sorted(&c).await?;
        let agent = all.iter().find(|s| s.coordinator && s.state != skiff_core::session::SessionState::Done);
        watcher_line(agent, || watch_lock_held(socket), &socket.with_extension("watch.log"))
    };
    println!("watcher   {line}");
    Ok(())
}

/// The `watcher` line of `skiff status`, for the Skiff agent's session.
/// `lock_held` is asked only when no watcher registered: then a held lock
/// means a watcher that does not register (an older `skiff watch`) runs.
fn watcher_line(agent: Option<&SessionInfo>, lock_held: impl FnOnce() -> bool, log: &Path) -> String {
    let Some(s) = agent else { return "no Skiff agent session".into() };
    let who = format!("{} ({})", resolve::display_name(s), print::short_id(&s.id));
    let Some(w) = &s.watch else {
        return if lock_held() {
            format!("none for {who}. The watch lock is held: an older skiff watch runs. See {}", log.display())
        } else {
            format!("none for {who}")
        };
    };
    let pids = w.pids.iter().map(u32::to_string).collect::<Vec<_>>().join(" ");
    let mut line = match w.pids.len() {
        0 | 1 => format!("runs for {who} since {}, pid {}", clock(w.since), if pids.is_empty() { "?" } else { &pids }),
        n => format!("{n} run for {who}, pids {pids}"),
    };
    if let Some(f) = &w.failure {
        line.push_str(&format!("  last failure {} {}: {}", clock(f.at), f.kind, f.message));
    }
    line
}

/// Another process holds the watch lock beside the socket.
fn watch_lock_held(socket: &Path) -> bool {
    std::fs::File::open(socket.with_extension("watch.lock"))
        .is_ok_and(|f| matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)))
}

/// Unix ms as local `HH:MM`.
fn clock(ms: u64) -> String {
    let t = (ms / 1000) as libc::time_t;
    // SAFETY: localtime_r writes only into `tm`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return "?".into();
    }
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// Why the `skiff` a shell runs is not `exe`: another one comes first on
/// `path`, or none is on it. An old copy first gives old commands.
fn path_warning(exe: &Path, path: &std::ffi::OsStr) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let first = std::env::split_paths(path)
        .map(|d| d.join("skiff"))
        .find(|f| f.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0));
    let Some(first) = first else {
        return Some(format!("no skiff is on PATH. A shell runs none, and this one is {}", exe.display()));
    };
    let same = |a: &Path, b: &Path| matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b);
    (!same(&first, exe)).then(|| format!("the first skiff on PATH is {}, not this one ({}). A shell runs that one", first.display(), exe.display()))
}

/// The agents, then the task types with the command each one starts. JSON:
/// `{"agents": [...], "tasks": [...]}`, so Skipper and the skill read one source.
pub async fn agents(c: &Client, json: bool) -> Result<()> {
    let agents = c.list_agents().await?;
    let tasks = skiff_core::tasks::resolve(&skiff_core::config::load()?.skiff_agent.tasks, &agents);
    if json {
        return print::json(&serde_json::json!({ "agents": agents, "tasks": tasks }));
    }
    let yes = |b: bool| if b { "yes" } else { "no" }.to_string();
    let rows: Vec<Vec<String>> = agents
        .iter()
        .map(|a| vec![a.id.clone(), yes(a.installed), yes(a.enabled), a.command.clone()])
        .collect();
    print::table(&["ID", "INSTALLED", "ENABLED", "COMMAND"], &rows);
    if !tasks.is_empty() {
        println!();
        print::table(&["TYPE", "AGENT", "MODEL", "COMMAND"], &task_rows(&tasks));
    }
    Ok(())
}

/// A task type per row. A row that starts nothing shows why in COMMAND.
fn task_rows(tasks: &[skiff_core::tasks::TaskType]) -> Vec<Vec<String>> {
    tasks
        .iter()
        .map(|t| {
            let cmd = match (&t.command, &t.error) {
                (Some(c), None) => c.clone(),
                (Some(c), Some(e)) => format!("{c}  ({e})"),
                (None, e) => format!("error: {}", e.as_deref().unwrap_or("none")),
            };
            vec![t.name.clone(), t.agent.clone(), t.model.clone().unwrap_or_else(|| "-".into()), cmd]
        })
        .collect()
}

/// From projects.toml, read here as the app reads it. Closed projects too.
pub fn projects(json: bool) -> Result<()> {
    let list = skiff_core::project::list()?;
    if json {
        return print::json(&list);
    }
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|p| {
            let state = if p.closed {
                "closed".to_string()
            } else if let Some(e) = &p.error {
                format!("error: {e}")
            } else {
                format!("{} worktrees", p.worktrees.len())
            };
            vec![p.name.clone(), p.short.clone(), print::tilde(&p.path), p.agents.join(","), state]
        })
        .collect();
    print::table(&["NAME", "SHORT", "PATH", "AGENTS", "STATE"], &rows);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::PathBuf};

    use super::{path_warning, task_rows, watcher_line};
    use skiff_core::session::{SessionInfo, WatchFailure, WatchInfo};

    #[test]
    fn the_watcher_line_names_each_case() {
        let log = std::path::Path::new("/s.watch.log");
        assert_eq!(watcher_line(None, || false, log), "no Skiff agent session");
        let mut s: SessionInfo = serde_json::from_value(serde_json::json!({
            "id": "3a136a60ffff", "label": "claude", "name": "Skipper", "role": "agent", "cwd": "/", "command": "claude",
            "args": [], "state": "idle", "cols": 80, "rows": 24, "pid": null, "exit_code": null,
        }))
        .unwrap();
        assert_eq!(watcher_line(Some(&s), || false, log), "none for Skipper (3a136a60)");
        assert!(watcher_line(Some(&s), || true, log).ends_with("an older skiff watch runs. See /s.watch.log"));
        s.watch = Some(WatchInfo { since: 0, pids: vec![41234], failure: None, follows: None });
        assert!(watcher_line(Some(&s), || unreachable!(), log).starts_with("runs for Skipper (3a136a60) since "));
        assert!(watcher_line(Some(&s), || false, log).ends_with(", pid 41234"));
        s.watch = Some(WatchInfo {
            since: 0,
            pids: vec![41234, 41300],
            failure: Some(WatchFailure { kind: "ledger".into(), message: "busy".into(), at: 0 }),
            follows: None,
        });
        let line = watcher_line(Some(&s), || false, log);
        assert!(line.starts_with("2 run for Skipper (3a136a60), pids 41234 41300  last failure "), "{line}");
        assert!(line.ends_with(" ledger: busy"));
    }

    #[test]
    fn task_rows_show_the_command_or_why_not() {
        use skiff_core::tasks::TaskType;
        let t = |name: &str, model: Option<&str>, command: Option<&str>, error: Option<&str>| TaskType {
            name: name.into(),
            agent: "claude".into(),
            model: model.map(Into::into),
            instructions: None,
            command: command.map(Into::into),
            error: error.map(Into::into),
        };
        let rows = task_rows(&[t("review", Some("opus"), Some("claude --model opus"), None), t("test", None, Some("claude"), None), t("old", None, None, Some("no agent"))]);
        assert_eq!(rows[0], ["review", "claude", "opus", "claude --model opus"]);
        assert_eq!(rows[1][2], "-");
        assert_eq!(rows[2][3], "error: no agent");
    }

    fn bin(dir: &PathBuf) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let f = dir.join("skiff");
        std::fs::write(&f, "").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        f
    }

    #[test]
    fn warns_when_another_skiff_comes_first_on_path() {
        let root = std::env::temp_dir().join(format!("skiff-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (old, new, empty) = (root.join("old"), root.join("new"), root.join("empty"));
        let (old_bin, new_bin) = (bin(&old), bin(&new));
        std::fs::create_dir_all(&empty).unwrap();
        let path = |dirs: &[&PathBuf]| std::env::join_paths(dirs).unwrap();
        assert_eq!(path_warning(&new_bin, &path(&[&empty, &new, &old])), None);
        assert!(path_warning(&new_bin, &path(&[&old, &new])).unwrap().contains(&old_bin.display().to_string()));
        assert!(path_warning(&new_bin, &path(&[&empty])).unwrap().starts_with("no skiff"));
        // A file that cannot run is skipped, as a shell skips it.
        std::fs::set_permissions(&old_bin, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(path_warning(&new_bin, &path(&[&old, &new])), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
