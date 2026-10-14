//! What stops a private daemon. SIGTERM or SIGINT from a process in one of
//! its sessions does not; the log names the sender. The same signal from
//! outside does, and so does a `shutdown` request. A reload keeps the guard.
#![cfg(target_os = "linux")]

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use skiff_client::Client;
use skiff_core::{protocol::Event, session::SessionSpec};

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Daemon {
    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("daemon.log")).unwrap_or_default()
    }

    /// The exit status once it exits, within 10 s.
    async fn exited(&mut self) -> ExitStatus {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < until, "skiffd did not stop:\n{}", self.log());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            eprintln!("{}", self.log());
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn start(name: &str) -> (Daemon, PathBuf) {
    let dir = std::env::temp_dir().join(format!("skiff-stop-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("skiffd.sock");
    let log = std::fs::File::create(dir.join("daemon.log")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_skiffd"))
        .env("SKIFF_SOCKET", &socket)
        .env("SKIFF_STATE", dir.join("workspace.json"))
        .env("SKIFF_CONFIG", dir.join("projects.toml"))
        .env("SKIFF_LEDGER", dir.join("ledger.db"))
        .env("SKIFF_SHELL_ENV", "0")
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    let daemon = Daemon { child, dir };
    connect(&socket).await;
    (daemon, socket)
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

/// Runs `line` in a new shell session and waits until it finished.
async fn run_in_session(c: &Client, cwd: &Path, line: &str) {
    let s = c
        .create_session(SessionSpec {
            command: Some("/bin/sh".into()),
            cwd: Some(cwd.to_path_buf()),
            cols: 120,
            rows: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut out = c.output(&s.id);
    c.subscribe(&s.id).await.unwrap();
    // The quotes keep the echoed input from matching the marker.
    c.write(&s.id, format!("{line}; echo 'fin''ished'\r").into_bytes()).await.unwrap();
    let mut seen = String::new();
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    while !seen.contains("finished") {
        match tokio::time::timeout_at(until, out.recv()).await {
            Ok(Ok(Event::Output { data, .. } | Event::Snapshot { data, .. })) => seen.push_str(&String::from_utf8_lossy(&data)),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("output ended ({e}) in {seen:?}"),
            Err(_) => panic!("{line:?} did not finish: {seen:?}"),
        }
    }
}

/// The daemon is alive and answers on a new connection.
async fn answers(daemon: &mut Daemon, socket: &Path) {
    // Give a signal that would stop it time to do so.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(daemon.child.try_wait().unwrap().is_none(), "skiffd stopped:\n{}", daemon.log());
    let c = connect(socket).await;
    assert_eq!(c.daemon_info().await.unwrap().pid, Some(daemon.child.id()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signal_from_a_session_is_ignored_and_one_from_outside_stops() {
    let (mut daemon, socket) = start("signal").await;
    let pid = daemon.child.id();
    let c = connect(&socket).await;

    // The shell's own kill, and a short-lived kill binary as pkill would be.
    run_in_session(&c, &daemon.dir, &format!("kill -TERM {pid}")).await;
    run_in_session(&c, &daemon.dir, &format!("/bin/kill -TERM {pid}")).await;
    run_in_session(&c, &daemon.dir, &format!("sh -c 'kill -INT {pid}'")).await;
    answers(&mut daemon, &socket).await;
    let log = daemon.log();
    assert_eq!(log.matches("ignored SIGTERM from pid").count(), 2, "{log}");
    assert!(log.contains("ignored SIGINT from pid"), "{log}");
    assert!(log.contains("skiff daemon stop"), "{log}");

    // A reload keeps the guard: the new image installs it again.
    c.reload(None).await.unwrap();
    drop(c);
    let c = connect(&socket).await;
    run_in_session(&c, &daemon.dir, &format!("kill -TERM {pid}")).await;
    answers(&mut daemon, &socket).await;
    assert_eq!(daemon.log().matches("ignored SIGTERM from pid").count(), 3, "{}", daemon.log());

    // This test process is outside the session tree.
    assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGTERM) }, 0);
    assert!(daemon.exited().await.success());
    let log = daemon.log();
    assert!(log.contains(&format!("SIGTERM from pid {}", std::process::id())), "{log}");
    assert!(log.contains("skiffd shutting down"), "{log}");
    assert!(!socket.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shutdown_request_stops_it() {
    let (mut daemon, socket) = start("request").await;
    let c = connect(&socket).await;
    c.shutdown().await.unwrap();
    assert!(daemon.exited().await.success());
    assert!(daemon.log().contains("shutdown requested over the socket"));
    assert!(!socket.exists());
}
