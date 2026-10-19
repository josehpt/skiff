//! A daemon on any socket but the live one keeps its state beside the
//! socket. Started with only `SKIFF_SOCKET`, it leaves the person's
//! `projects.toml` and `~/skiff-agent` as they were, and its Skiff agent
//! never starts in the live agent's folder. The home, the config and the
//! runtime folder here are a test's own, so the live ones are never read.
#![cfg(target_os = "linux")]

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use skiff_client::{skipper, Client};
use skiff_core::session::SessionSpec;

mod common;

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // The socket is beside the state, not in `dir` as `common::stop` expects.
        let socket = self.dir.join("x.sock");
        let link = self.dir.join("skiffd.sock");
        let _ = std::os::unix::fs::symlink(&socket, &link);
        let stopped = common::stop(&mut self.child, &self.dir);
        if std::thread::panicking() {
            eprintln!("{}", std::fs::read_to_string(self.dir.join("daemon.log")).unwrap_or_default());
        }
        if stopped {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Every file under `dir` with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(p.clone(), std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

#[test]
fn a_test_daemon_leaves_the_person_s_state_alone() {
    let dir = std::env::temp_dir().join(format!("skiff-isolation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    let config = home.join(".config");
    let run = dir.join("run");
    let bin = dir.join("bin");
    for d in [&home, &config, &run, &bin, &home.join("skiff-agent")] {
        std::fs::create_dir_all(d).unwrap();
    }
    // The person's state: a Skiff agent on, its folder, its ledger.
    std::fs::create_dir_all(config.join("skiff")).unwrap();
    std::fs::write(config.join("skiff/projects.toml"), "[skiff_agent]\nenabled = true\n").unwrap();
    std::fs::write(home.join("skiff-agent/ledger.db"), b"the live ledger").unwrap();
    std::fs::write(home.join("skiff-agent/.skiff-agent"), b"").unwrap();
    // An agent the daemon finds on PATH.
    let claude = bin.join("claude");
    std::fs::write(&claude, "#!/bin/sh\nexec sleep 60\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    // This process reads the same home as the daemon. Set before any thread.
    for (k, v) in [("HOME", home.as_os_str()), ("XDG_CONFIG_HOME", config.as_os_str()), ("XDG_RUNTIME_DIR", run.as_os_str())] {
        std::env::set_var(k, v);
    }
    std::env::set_var("PATH", &path);
    for k in ["SKIFF_CONFIG", "SKIFF_STATE", "SKIFF_LEDGER", "SKIFF_REAL_STATE", "SKIFF_SESSION"] {
        std::env::remove_var(k);
    }
    let before = snapshot(&home);

    let socket = dir.join("x.sock");
    std::env::set_var("SKIFF_SOCKET", &socket);
    skiff_core::state::set_socket(socket.clone());
    let child = Command::new(env!("CARGO_BIN_EXE_skiffd"))
        .env("SKIFF_SHELL_ENV", "0")
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join("daemon.log")).unwrap())
        .spawn()
        .unwrap();
    let _daemon = Daemon { child, dir: dir.clone() };
    let private = dir.join("x.state");

    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let c = connect(&socket).await;
        // A session makes the daemon save its workspace.
        c.create_session(SessionSpec { command: Some("/bin/sh".into()), cwd: Some(dir.clone()), cols: 80, rows: 24, ..Default::default() })
            .await
            .unwrap();
        c.list_agents().await.unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while !private.join("workspace.json").is_file() {
            assert!(Instant::now() < until, "no private workspace.json");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // The private projects.toml has no Skiff agent: `skiff skipper` starts none.
        let off = skipper::open(&c, skipper::Open::Start).await.unwrap_err().to_string();
        assert!(off.contains("is off"), "{off}");

        // A copy of the person's file that names the live folder is refused.
        std::fs::create_dir_all(private.join("config")).unwrap();
        let file = private.join("config/projects.toml");
        std::fs::write(&file, format!("[skiff_agent]\nenabled = true\nfolder = \"{}\"\n", home.join("skiff-agent").display())).unwrap();
        let refused = skipper::open(&c, skipper::Open::Start).await.unwrap_err().to_string();
        assert!(refused.contains("folder of the live Skiff agent"), "{refused}");
        assert!(c.list_sessions().await.unwrap().iter().all(|s| !s.coordinator));

        // With no folder named, its Skiff agent starts in the private folder.
        std::fs::write(&file, "[skiff_agent]\nenabled = true\n").unwrap();
        let found = skipper::open(&c, skipper::Open::Start).await.unwrap().unwrap();
        assert_eq!(found.session.cwd, private.join("skiff-agent"));
        assert!(private.join("skiff-agent/AGENTS.md").is_file());
    });

    assert_eq!(snapshot(&home), before, "the person's home changed");
    assert!(!run.join("skiff/skiffd.state").exists());
}

async fn connect(socket: &Path) -> Client {
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
