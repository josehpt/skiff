//! The commands skiffd lists for the + menu, `skiff new --agent` and
//! Skipper. A fresh config starts each agent with its own permission
//! prompts. A bypass flag comes only from the person's `[agents.commands]`.
#![cfg(unix)]

use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use skiff_client::Client;

mod common;

/// Flags that turn an agent's permission prompts off.
const BYPASS: [&str; 5] = ["--dangerously-skip-permissions", "--yolo", "--dangerously-bypass-approvals-and-sandbox", "--full-auto", "--always-approve"];

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if common::stop(&mut self.child, &self.dir) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[tokio::test]
async fn fresh_config_has_no_bypass_and_an_override_applies() {
    let dir = std::env::temp_dir().join(format!("skiff-agent-commands-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("skiffd.sock");
    let config = dir.join("projects.toml");
    let child = Command::new(env!("CARGO_BIN_EXE_skiffd"))
        .env("SKIFF_SOCKET", &socket)
        .env("SKIFF_STATE", dir.join("workspace.json"))
        .env("SKIFF_CONFIG", &config)
        .env("SKIFF_LEDGER", dir.join("ledger.db"))
        .env("SKIFF_SHELL_ENV", "0")
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _daemon = Daemon { child, dir: dir.clone() };
    let c = connect(&socket).await;

    let fresh = c.list_agents().await.unwrap();
    let ids: Vec<&str> = fresh.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["claude", "codex", "gemini", "grok", "opencode"]);
    for a in &fresh {
        assert_eq!(a.command, a.default_command, "{}", a.id);
        assert!(!BYPASS.iter().any(|f| a.command.contains(f)), "{} starts with a bypass flag: {}", a.id, a.command);
    }

    std::fs::write(
        &config,
        "[agents.commands]\nclaude = 'claude --dangerously-skip-permissions'\ncodex = 'codex --yolo'\ngemini = 'agy --dangerously-skip-permissions'\n",
    )
    .unwrap();
    let mine = c.list_agents().await.unwrap();
    let command = |id: &str| mine.iter().find(|a| a.id == id).map(|a| (a.command.as_str(), a.default_command.as_str()));
    assert_eq!(command("claude"), Some(("claude --dangerously-skip-permissions", "claude")));
    assert_eq!(command("codex"), Some(("codex --yolo", "codex")));
    assert_eq!(command("gemini"), Some(("agy --dangerously-skip-permissions", "agy")));
    assert_eq!(command("grok"), Some(("grok", "grok")));
}

async fn connect(socket: &std::path::Path) -> Client {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(c) = Client::connect(socket).await {
            if c.daemon_info().await.is_ok() {
                return c;
            }
        }
        assert!(Instant::now() < until, "skiffd did not answer");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
