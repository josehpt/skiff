//! status, agents, projects.

use std::path::Path;

use anyhow::Result;
use skiff_client::Client;

use crate::print;

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
    Ok(())
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

pub async fn agents(c: &Client, json: bool) -> Result<()> {
    let agents = c.list_agents().await?;
    if json {
        return print::json(&agents);
    }
    let yes = |b: bool| if b { "yes" } else { "no" }.to_string();
    let rows: Vec<Vec<String>> = agents
        .iter()
        .map(|a| vec![a.id.clone(), yes(a.installed), yes(a.enabled), a.command.clone()])
        .collect();
    print::table(&["ID", "INSTALLED", "ENABLED", "COMMAND"], &rows);
    Ok(())
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

    use super::path_warning;

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
