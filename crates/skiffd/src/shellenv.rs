//! The login shell's environment.
//!
//! A desktop launcher starts the app with the session's environment, which
//! skips `~/.bashrc` and friends. PATH then misses `~/.local/bin`, nvm and
//! cargo, so agents are not found, and EDITOR is unset, so an agent's
//! "open in editor" key (Ctrl+G in Claude Code) opens the wrong editor.
//! Started from a terminal, the environment is already complete.

use std::{
    process::{Command, Stdio},
    sync::OnceLock,
    time::Duration,
};

const MARK: &str = "__SKIFF_ENV__";
const TIMEOUT: Duration = Duration::from_secs(3);

/// Variables that describe the probe shell itself, not the user's setup.
const SKIP: &[&str] = &["_", "PWD", "OLDPWD", "SHLVL", "TERM", "COLORTERM", "PS1"];

/// The `alias` output the aliases came from. A reload hands it on: exec keeps
/// the environment, but not what the probe found besides.
static ALIASES: OnceLock<String> = OnceLock::new();

pub fn aliases() -> &'static str {
    ALIASES.get().map_or("", String::as_str)
}

/// Sets the aliases without a probe, as a reload found them.
pub fn set_aliases(text: &str) {
    skiff_core::alias::set(text);
    let _ = ALIASES.set(text.to_string());
}

/// Copies the login shell's variables into this process. Call it before any
/// thread starts: it changes the process environment.
pub fn import() {
    if std::env::var("SKIFF_SHELL_ENV").as_deref() == Ok("0") {
        return;
    }
    // Started from a terminal, the environment is complete. The aliases still need the probe.
    let from_launcher = std::env::var_os("TERM").is_none();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    match probe(&shell) {
        Some((vars, aliases)) => {
            set_aliases(&aliases);
            if from_launcher {
                for (k, v) in vars {
                    if !SKIP.contains(&k.as_str()) {
                        std::env::set_var(k, v);
                    }
                }
            }
        }
        None if from_launcher => eprintln!("skiffd: could not read the environment of {shell}; using the launcher's"),
        None => {}
    }
}

fn probe(shell: &str) -> Option<(Vec<(String, String)>, String)> {
    // -i reads the rc file (bash reads ~/.bashrc only when interactive), -l the profile.
    // The mark skips anything the rc files print.
    // An interactive shell that shares the daemon's terminal stops on job control.
    // A new session gives it no terminal at all, as under a desktop launcher.
    let mut cmd = Command::new(shell);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches no memory.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let mut child = cmd
        .args(["-l", "-i", "-c", &format!("printf '{MARK}'; env -0; printf '{MARK}'; alias")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let (_, buf) = match crate::command::output(&mut child, TIMEOUT) {
        Ok(output) => output,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    let text = String::from_utf8_lossy(&buf);
    let body = &text[text.find(MARK)? + MARK.len()..];
    let (body, aliases) = body.split_once(MARK).unwrap_or((body, ""));
    let vars: Vec<_> = body
        .split('\0')
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, _)| !k.is_empty() && !k.contains('\n'))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    vars.iter().any(|(k, _)| k == "PATH").then(|| (vars, aliases.to_string()))
}
