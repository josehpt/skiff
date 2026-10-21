//! M2 over a real SSH stream-local forward: a private skiffd, a private
//! loopback sshd with throwaway keys and known_hosts, and the system `ssh`
//! with a private config (`-F`). Nothing reads or writes the user's ~/.ssh,
//! and no other host is contacted. Skips when this machine has no usable sshd.
#![cfg(unix)]

use std::{
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use skiff_client::{
    host::{
        local::{legacy_attach, legacy_output, HostSelection, IdentitySource, SelectedHost},
        ssh::{SshError, SshOptions, SshTarget},
        Continuity, OutputGap, TerminalFrame,
    },
    Client,
};
use skiff_core::session::SessionSpec;
use skiff_host_api::Capability;

mod common;

const SSHD: &str = "/usr/sbin/sshd";
const SSH: &str = "/usr/bin/ssh";

struct Fixture {
    dir: PathBuf,
    daemon: Child,
    sshd: Option<Child>,
    port: u16,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop_sshd();
        // Our own `ssh` clients, by their private config path only.
        let _ = Command::new("pkill").arg("-f").arg(self.dir.join("ssh_config")).status();
        let stopped = common::stop(&mut self.daemon, &self.dir);
        if std::thread::panicking() {
            for log in ["daemon.log", "sshd.log"] {
                eprintln!("--- {log}\n{}", std::fs::read_to_string(self.dir.join(log)).unwrap_or_default());
            }
        }
        if stopped {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

impl Fixture {
    fn socket(&self) -> PathBuf {
        self.dir.join("skiffd.sock")
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.dir.join(name), text).unwrap();
    }

    fn sshd_config(&self, name: &str, forwarding: bool) {
        let d = self.dir.display();
        self.write(name, &format!(
            "Port {}\nListenAddress 127.0.0.1\nHostKey {d}/host_key\nAuthorizedKeysFile {d}/authorized_keys\n\
             PidFile {d}/sshd.pid\nStrictModes no\nUsePAM no\nPasswordAuthentication no\n\
             KbdInteractiveAuthentication no\nAllowStreamLocalForwarding {}\nLogLevel VERBOSE\n",
            self.port,
            if forwarding { "yes" } else { "no" }
        ));
    }

    /// Starts sshd; false when this machine cannot run one unprivileged.
    fn start_sshd(&mut self, config: &str) -> bool {
        self.stop_sshd();
        let mut child = Command::new(SSHD)
            .arg("-D")
            .arg("-f")
            .arg(self.dir.join(config))
            .arg("-E")
            .arg(self.dir.join("sshd.log"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if std::net::TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                self.sshd = Some(child);
                return true;
            }
            if child.try_wait().unwrap().is_some() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        false
    }

    /// Stops the listener and every connection it serves: its process group.
    fn stop_sshd(&mut self) {
        if let Some(mut child) = self.sshd.take() {
            // SAFETY: a plain signal to this test's own sshd process group.
            unsafe { libc::kill(-(child.id() as i32), libc::SIGTERM) };
            let _ = child.wait();
        }
    }

    fn options(&self, config: &str) -> SshOptions {
        SshOptions { program: SSH.into(), config: Some(self.dir.join(config)) }
    }

    fn selection(&self, config: &str, socket: &Path, forward: &str) -> HostSelection {
        let target = SshTarget { destination: "fixture".into(), socket: socket.to_string_lossy().into_owned() };
        HostSelection::ssh(target, self.options(config), self.dir.join(forward)).unwrap()
    }
}

fn keygen(path: &Path) {
    let ok = Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", ""]).arg("-f").arg(path).status().unwrap();
    assert!(ok.success());
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn fixture(name: &str) -> Option<Fixture> {
    fixture_with(name, false)
}

fn fixture_with(name: &str, no_identity: bool) -> Option<Fixture> {
    if !Path::new(SSHD).is_file() || !Path::new(SSH).is_file() {
        eprintln!("skipped: no {SSHD} or {SSH}");
        return None;
    }
    // Short: the forward's socket path must fit sun_path.
    let dir = std::env::temp_dir().join(format!("skssh-{name}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    if no_identity {
        // A store skiffd refuses to read: its pong then reports no identity,
        // as an older skiffd's does.
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config/local-host-id"), "not an id").unwrap();
        std::fs::set_permissions(dir.join("config/local-host-id"), std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let log = std::fs::File::create(dir.join("daemon.log")).unwrap();
    let daemon = Command::new(env!("CARGO_BIN_EXE_skiffd"))
        .env("SKIFF_SOCKET", dir.join("skiffd.sock"))
        .env("SKIFF_STATE", dir.join("workspace.json"))
        .env("SKIFF_CONFIG", dir.join("config/projects.toml"))
        .env("SKIFF_LEDGER", dir.join("ledger.db"))
        .env("SKIFF_SHELL_ENV", "0")
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    let mut f = Fixture { dir, daemon, sshd: None, port: free_port() };
    keygen(&f.dir.join("host_key"));
    keygen(&f.dir.join("other_host_key"));
    keygen(&f.dir.join("client_key"));
    std::fs::copy(f.dir.join("client_key.pub"), f.dir.join("authorized_keys")).unwrap();
    let key = |name: &str| {
        let text = std::fs::read_to_string(f.dir.join(name)).unwrap();
        text.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
    };
    let known = format!("[127.0.0.1]:{} {}\n", f.port, key("host_key.pub"));
    let changed = format!("[127.0.0.1]:{} {}\n", f.port, key("other_host_key.pub"));
    f.write("known_hosts", &known);
    f.write("changed_hosts", &changed);
    f.write("empty_hosts", "");
    for (config, hosts) in [("ssh_config", "known_hosts"), ("unknown_config", "empty_hosts"), ("changed_config", "changed_hosts")] {
        let d = f.dir.display();
        f.write(config, &format!(
            "Host fixture\n  HostName 127.0.0.1\n  Port {}\n  IdentityFile {d}/client_key\n  IdentitiesOnly yes\n  \
             UserKnownHostsFile {d}/{hosts}\n  GlobalKnownHostsFile /dev/null\n",
            f.port
        ));
    }
    f.sshd_config("sshd_config", true);
    f.sshd_config("sshd_noforward", false);
    if !f.start_sshd("sshd_config") {
        eprintln!("skipped: {SSHD} does not run unprivileged here");
        return None;
    }
    Some(f)
}

async fn local_client(socket: &Path) -> Client {
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

async fn select(selection: &HostSelection) -> anyhow::Result<SelectedHost> {
    let mut pending = selection.connect().await?;
    pending.daemon_info().await?;
    pending.activate().await
}

async fn until_seen(output: &mut skiff_client::host::TerminalOutput, needle: &str) {
    let mut seen = String::new();
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    while !seen.contains(needle) {
        match tokio::time::timeout_at(until, output.recv()).await {
            Ok(Ok(TerminalFrame::Snapshot { data, .. } | TerminalFrame::Output { data, .. })) => {
                seen.push_str(&String::from_utf8_lossy(&data))
            }
            Ok(Err(e)) => panic!("output ended ({e:?}) before {needle:?} in {seen:?}"),
            Err(_) => panic!("no {needle:?} in {seen:?}"),
        }
    }
}

fn ssh_error(e: &anyhow::Error) -> Option<&SshError> {
    e.downcast_ref::<SshError>()
}

#[tokio::test]
async fn a_remote_terminal_over_a_real_forward_survives_a_dropped_link() {
    let Some(mut f) = fixture("a") else { return };
    // The remote CLI starts the work, as M2 expects.
    let host_side = local_client(&f.socket()).await;
    let shell = host_side
        .create_session(SessionSpec { label: "remote".into(), command: Some("/bin/sh".into()), cols: 80, rows: 24, ..Default::default() })
        .await
        .unwrap();

    let selection = f.selection("ssh_config", &f.socket(), "fwd");
    assert!(!selection.is_local());
    assert!(selection.process_control().is_none(), "a remote endpoint is never managed");
    let host = select(&selection).await.unwrap();
    assert_eq!(host.identity_source(), IdentitySource::Host);
    let hello = host.connection().hello().clone();
    assert_eq!(host.connection().continuity(), Continuity::FirstConnection);
    // Sessions and terminals only: create, end, rename and groups are M3.
    assert!(hello.capabilities.contains(&Capability::session_terminal()));
    assert!(!hello.capabilities.contains(&Capability::session_lifecycle()));
    assert!(!hello.capabilities.contains(&Capability::session_groups()));
    // The forward's peer is `ssh`: its pid is never the daemon's.
    assert_ne!(host.client().daemon_pid.map(|p| p as u32), f.daemon.id().into());

    let conn = host.connection();
    let listed = skiff_client::host::local::legacy_inventory(&conn).await.unwrap();
    assert!(listed.iter().any(|s| s.id == shell.id));
    let mut output = legacy_output(&conn, &shell.id).unwrap();
    legacy_attach(&conn, &shell.id).await.unwrap();
    skiff_client::host::local::legacy_input(&conn, &shell.id, b"echo M2-$((40+2))\n".to_vec()).unwrap();
    until_seen(&mut output, "M2-42").await;

    // The link drops: ssh ends. The stream closes and input is refused.
    f.stop_sshd();
    let _ = Command::new("pkill").arg("-f").arg(f.dir.join("ssh_config")).status();
    let until = Instant::now() + Duration::from_secs(10);
    while !conn.is_closed() {
        assert!(Instant::now() < until, "the dropped link was not noticed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(output.recv().await.err(), Some(OutputGap::Closed));
    assert!(skiff_client::host::local::legacy_input(&conn, &shell.id, b"echo LOST\n".to_vec()).is_err());
    // The remote session runs on, the same child.
    let same = host_side.list_sessions().await.unwrap().into_iter().find(|s| s.id == shell.id).unwrap();
    assert_eq!(same.pid, shell.pid);
    drop(conn);
    drop(host);

    // While the link is down, connecting fails and waits before ssh runs again.
    let down = select(&selection).await.err().unwrap();
    assert!(matches!(ssh_error(&down), Some(SshError::Unreachable(_) | SshError::Exited { .. })), "{down:#}");
    let waiting = select(&selection).await.err().unwrap();
    assert!(matches!(ssh_error(&waiting), Some(SshError::Waiting { .. })), "{waiting:#}");

    // Back up: the same skiffd run answers, with a fresh subscription and snapshot.
    assert!(f.start_sshd("sshd_config"));
    let until = Instant::now() + Duration::from_secs(10);
    let again = loop {
        match select(&selection).await {
            Ok(host) => break host,
            Err(e) => {
                assert!(Instant::now() < until, "no reconnect: {e:#}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    assert_eq!(again.connection().continuity(), Continuity::SameInstance);
    assert_eq!(again.connection().hello().host.host_id, hello.host.host_id);
    let conn = again.connection();
    let mut output = legacy_output(&conn, &shell.id).unwrap();
    legacy_attach(&conn, &shell.id).await.unwrap();
    // The snapshot redraws what ran before; LOST was never sent.
    until_seen(&mut output, "M2-42").await;
    skiff_client::host::local::legacy_input(&conn, &shell.id, b"echo BACK-$((1+1))\n".to_vec()).unwrap();
    until_seen(&mut output, "BACK-2").await;
    let screen = host_side.screen(&shell.id, 24).await.unwrap().join("\n");
    assert!(!screen.contains("LOST"), "{screen}");

    // The app quits: its forward goes, and the remote session stays.
    drop(conn);
    drop(again);
    drop(selection);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!f.dir.join("fwd").exists(), "the private forward directory is removed");
    let after = host_side.list_sessions().await.unwrap().into_iter().find(|s| s.id == shell.id).unwrap();
    assert_eq!((after.pid, after.state == skiff_core::session::SessionState::Done), (shell.pid, false));
}

#[tokio::test]
async fn unknown_or_changed_keys_closed_forwarding_and_a_wrong_socket_are_explained_and_refused() {
    let Some(mut f) = fixture("b") else { return };
    let _ = local_client(&f.socket()).await;
    let unknown = select(&f.selection("unknown_config", &f.socket(), "u")).await.err().unwrap();
    assert!(matches!(ssh_error(&unknown), Some(SshError::HostKeyUnknown(_))), "{unknown:#}");
    let changed = select(&f.selection("changed_config", &f.socket(), "c")).await.err().unwrap();
    assert!(matches!(ssh_error(&changed), Some(SshError::HostKeyChanged(_))), "{changed:#}");
    // Strict checking learned nothing: the fixture's known_hosts files are unchanged.
    assert_eq!(std::fs::read_to_string(f.dir.join("empty_hosts")).unwrap(), "");

    let missing = select(&f.selection("ssh_config", Path::new("/nonexistent/skiffd.sock"), "m")).await.err().unwrap();
    assert!(matches!(ssh_error(&missing), Some(SshError::ChannelRefused { .. })), "{missing:#}");

    assert!(f.start_sshd("sshd_noforward"));
    let refused = select(&f.selection("ssh_config", &f.socket(), "r")).await.err().unwrap();
    // This sshd says no more than for a missing socket; the text names both.
    assert!(matches!(ssh_error(&refused), Some(SshError::ChannelRefused { .. } | SshError::ForwardingRefused(_))), "{refused:#}");
    assert!(refused.to_string().contains("AllowStreamLocalForwarding"), "{refused:#}");
}

#[tokio::test]
async fn a_remote_skiffd_without_identity_is_refused_and_no_local_identity_stands_in() {
    let Some(f) = fixture_with("c", true) else { return };
    let info = local_client(&f.socket()).await.daemon_info().await.unwrap();
    assert_eq!(info.identity, None);
    let selection = f.selection("ssh_config", &f.socket(), "i");
    let refused = select(&selection).await.err().unwrap();
    assert!(refused.to_string().contains("reports no Host identity"), "{refused:#}");
    // No bootstrap file was read or made for the remote Host.
    assert!(!f.dir.join("i/local-host-id").exists());
    assert!(std::fs::read_dir(&f.dir).unwrap().all(|e| e.unwrap().file_name() != "local-host-id"));
}
