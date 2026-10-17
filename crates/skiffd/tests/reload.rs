//! A private daemon reloads onto its own binary. The same shell answers
//! after it, and the terminal carries on: the alternate screen, the primary
//! history under it, modes, and a sequence cut in half by the reload.
#![cfg(target_os = "linux")]

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use skiff_client::Client;
use skiff_core::{protocol::Event, session::SessionSpec};

mod common;

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let stopped = common::stop(&mut self.child, &self.dir);
        if std::thread::panicking() {
            eprintln!("{}", std::fs::read_to_string(self.dir.join("daemon.log")).unwrap_or_default());
        }
        if stopped {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

async fn start(name: &str) -> (Daemon, PathBuf) {
    let dir = std::env::temp_dir().join(format!("skiff-reload-{name}-{}", std::process::id()));
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

/// Output until `needle` shows.
async fn wait_for(out: &mut skiff_client::Output, needle: &str) {
    let mut seen = String::new();
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    while !seen.contains(needle) {
        match tokio::time::timeout_at(until, out.recv()).await {
            Ok(Ok(Event::Output { data, .. } | Event::Snapshot { data, .. })) => seen.push_str(&String::from_utf8_lossy(&data)),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("output ended ({e}) before {needle:?} in {seen:?}"),
            Err(_) => panic!("no {needle:?} in {seen:?}"),
        }
    }
}

/// A fresh snapshot of the session.
async fn snapshot(c: &Client, id: &str) -> String {
    let mut out = c.output(id);
    c.subscribe(id).await.unwrap();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), out.recv()).await.unwrap() {
            Ok(Event::Snapshot { data, .. }) => return String::from_utf8_lossy(&data).into_owned(),
            Ok(_) => continue,
            Err(e) => panic!("output ended ({e}) before the snapshot"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_keeps_the_process_and_the_whole_terminal() {
    let (daemon, socket) = start("terminal").await;
    let c = connect(&socket).await;
    let before = c.daemon_info().await.unwrap();
    assert_eq!(before.pid, Some(daemon.child.id()));
    assert!(before.reload_state.is_some());

    let s = c
        .create_session(SessionSpec {
            command: Some("/bin/sh".into()),
            cwd: Some(daemon.dir.clone()),
            cols: 40,
            rows: 6,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut out = c.output(&s.id);
    c.subscribe(&s.id).await.unwrap();
    // No echo and no prompt: only what printf prints reaches the terminal.
    c.write(&s.id, b"stty -echo; PS1=''; for i in 1 2 3 4 5 6 7 8; do echo line-$i; done\r".to_vec()).await.unwrap();
    wait_for(&mut out, "line-8").await;
    // A target that cannot adopt is refused, and the daemon carries on.
    let err = c.reload(Some("/bin/true".into())).await.unwrap_err();
    assert!(err.to_string().contains("does not support reload"), "{err:#}");
    c.write(&s.id, b"echo still-$((1+1))\r".to_vec()).await.unwrap();
    wait_for(&mut out, "still-2").await;

    // The alternate screen, paste and mouse modes, then half of ESC[31m.
    c.write(&s.id, b"printf '\\033[?1049h\\033[?2004h\\033[?1000hALT-SCREEN\\033[3'\r".to_vec()).await.unwrap();
    wait_for(&mut out, "ALT-SCREEN").await;

    let fds = open_fds(daemon.child.id());
    c.reload(None).await.unwrap();
    let c = connect(&socket).await;
    assert_eq!(open_fds(daemon.child.id()), fds, "only the PTYs and the listener crossed the exec");
    let after = c.daemon_info().await.unwrap();
    assert_eq!(after.pid, before.pid, "the daemon kept its pid");
    let sessions = c.list_sessions().await.unwrap();
    let kept = sessions.iter().find(|x| x.id == s.id).expect("the session survived");
    assert_eq!(kept.pid, s.pid);
    let leftovers: Vec<_> = std::fs::read_dir(&daemon.dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("skiffd-handover"))
        .collect();
    assert!(leftovers.is_empty(), "the handover file is gone");

    let snap = snapshot(&c, &s.id).await;
    assert!(snap.contains("line-1"), "primary history under the alternate screen: {snap:?}");
    assert!(snap.contains("\x1b[?2004h") && snap.contains("\x1b[?1000h"), "modes: {snap:?}");

    // The same shell answers, and the parser finishes ESC[31m.
    let mut out = c.output(&s.id);
    c.subscribe(&s.id).await.unwrap();
    c.write(&s.id, b"printf '1mRED\\033[0m'; echo; echo pid-$$\r".to_vec()).await.unwrap();
    wait_for(&mut out, &format!("pid-{}", s.pid.unwrap())).await;
    let snap = snapshot(&c, &s.id).await;
    assert!(snap.contains("ALT-SCREEN\x1b[0;31mRED"), "{snap:?}");

    // Leaving the alternate screen brings the primary screen back whole.
    let mut out = c.output(&s.id);
    c.subscribe(&s.id).await.unwrap();
    c.write(&s.id, b"printf '\\033[?1049l'; echo after-$((2+2))\r".to_vec()).await.unwrap();
    wait_for(&mut out, "after-4").await;
    let snap = snapshot(&c, &s.id).await;
    assert!(snap.contains("line-1") && snap.contains("line-8") && snap.contains("after-4"), "{snap:?}");
    assert!(!snap.contains("ALT-SCREEN"), "{snap:?}");
    assert!(!snap.contains("\x1b[?1049h"), "{snap:?}");
}

/// The daemon's open fds by kind, and whether every PTY master and socket
/// closes on exec.
fn open_fds(pid: u32) -> (Vec<String>, bool) {
    let mut kinds = Vec::new();
    let mut cloexec = true;
    for e in std::fs::read_dir(format!("/proc/{pid}/fd")).unwrap().flatten() {
        let target = std::fs::read_link(e.path()).map(|p| p.display().to_string()).unwrap_or_default();
        let kind = target.split(':').next().unwrap_or("").to_string();
        if kind == "/dev/ptmx" || kind == "socket" {
            let info = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{}", e.file_name().to_string_lossy())).unwrap_or_default();
            let flags = info.lines().find_map(|l| l.strip_prefix("flags:")).map(|f| u32::from_str_radix(f.trim(), 8).unwrap_or(0)).unwrap_or(0);
            cloexec &= flags & libc::O_CLOEXEC as u32 != 0;
        }
        kinds.push(kind);
    }
    kinds.sort();
    (kinds, cloexec)
}

async fn info(c: &Client, id: &str) -> skiff_core::session::SessionInfo {
    c.list_sessions().await.unwrap().into_iter().find(|s| s.id == id).unwrap()
}

async fn shell(c: &Client, dir: &Path, script: &str) -> skiff_core::session::SessionInfo {
    c.create_session(SessionSpec {
        command: Some("/bin/sh".into()),
        args: vec!["-c".into(), script.into()],
        cwd: Some(dir.to_path_buf()),
        ..Default::default()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_reaps_each_child_once_and_rolls_back_cleanly() {
    let (daemon, socket) = start("reap").await;
    let c = connect(&socket).await;
    let pid = daemon.child.id();

    // Input the program never reads: the drain times out and nothing changes.
    let stuck = shell(&c, &daemon.dir, "stty raw -echo; sleep 30").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    c.write(&stuck.id, vec![b'x'; 256 * 1024]).await.unwrap();
    let err = c.reload(None).await.unwrap_err();
    assert!(err.to_string().contains("did not reach its terminal"), "{err:#}");
    c.kill(&stuck.id).await.unwrap();

    // A target that passes preflight, reads the handover and then cannot
    // exec: every fd closes on exec again, and the daemon still starts sessions.
    let target = daemon.dir.join("skiffd-once");
    std::fs::write(&target, format!("#!/bin/sh\n[ -n \"$2\" ] && chmod -x \"$0\"\nexec {} \"$@\"\n", env!("CARGO_BIN_EXE_skiffd"))).unwrap();
    std::fs::set_permissions(&target, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let live = shell(&c, &daemon.dir, "sleep 30").await;
    let err = c.reload(Some(target)).await.unwrap_err();
    assert!(err.to_string().contains("exec"), "{err:#}");
    assert!(open_fds(pid).1, "close-on-exec is back on every PTY and socket");
    c.kill(&live.id).await.unwrap();

    // Children that exit before, during and after the handover, and one
    // killed that ignores the hang-up: each is reaped once, with its status.
    // A program that rang the bell waits for input; the reload keeps that.
    let bell = c
        .create_session(SessionSpec {
            command: Some("/usr/bin/env".into()),
            args: vec!["sh".into(), "-c".into(), "printf '\\a'; sleep 30".into()],
            cwd: Some(daemon.dir.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while info(&c, &bell.id).await.state != skiff_core::session::SessionState::Waiting {
        assert!(Instant::now() < until, "the bell set no waiting state");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let bell_before = info(&c, &bell.id).await;
    let orphan = shell(&c, &daemon.dir, "trap '' HUP; sleep 2").await;
    // Let the trap take effect before the hang-up.
    tokio::time::sleep(Duration::from_millis(200)).await;
    c.kill(&orphan.id).await.unwrap();
    let mut exiting = Vec::new();
    for i in 0..8 {
        exiting.push(shell(&c, &daemon.dir, &format!("sleep 0.{i}5; exit 7")).await);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    c.reload(None).await.unwrap();
    let c = connect(&socket).await;
    assert_eq!(c.daemon_info().await.unwrap().pid, Some(pid));
    let bell_after = info(&c, &bell.id).await;
    assert_eq!((bell_after.state, bell_after.unread, bell_after.pid), (bell_before.state, bell_before.unread, bell_before.pid));
    // A second reload while the orphan still runs hands it on again.
    assert!(Path::new(&format!("/proc/{}", orphan.pid.unwrap())).exists());
    c.reload(None).await.unwrap();
    let c = connect(&socket).await;
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let sessions = c.list_sessions().await.unwrap();
        let done = exiting.iter().all(|e| {
            let s = sessions.iter().find(|s| s.id == e.id).expect("an exited session stays");
            s.state == skiff_core::session::SessionState::Done && s.exit_code == Some(7)
        });
        let reaped = !Path::new(&format!("/proc/{}", orphan.pid.unwrap())).exists();
        if done && reaped {
            break;
        }
        assert!(Instant::now() < until, "exits after the reload: {sessions:#?}, orphan reaped: {reaped}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
