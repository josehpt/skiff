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
        local::{
            legacy_agents, legacy_attach, legacy_create, legacy_delete_group, legacy_groups, legacy_kill, legacy_output,
            legacy_rename, legacy_save_group, HostSelection, IdentitySource, SelectedHost,
        },
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
        HostSelection::ssh(target, self.options(config), self.dir.join(forward), None).unwrap()
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
    // The same protocol: terminals, and M3's lifecycle, groups and agents.
    for cap in [Capability::session_terminal(), Capability::session_lifecycle(), Capability::session_groups(), Capability::session_agents()] {
        assert!(hello.capabilities.contains(&cap), "{cap:?}");
    }
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
    // Each forward's own directory goes; the shared private parent stays.
    let left: Vec<_> = std::fs::read_dir(f.dir.join("fwd")).unwrap().collect();
    assert!(left.is_empty(), "the forward's directory is removed: {left:?}");
    let after = host_side.list_sessions().await.unwrap().into_iter().find(|s| s.id == shell.id).unwrap();
    assert_eq!((after.pid, after.state == skiff_core::session::SessionState::Done), (shell.pid, false));
}

/// M3 over the real forward: the agent list is the remote account's own (its
/// config, its PATH); a session starts in a folder on the Host and runs
/// there; rename, groups and end act on the remote skiffd. The app's own
/// config is never read for it.
#[tokio::test]
async fn remote_lifecycle_groups_and_agents_come_from_the_host_over_a_real_forward() {
    let Some(f) = fixture("m") else { return };
    let host_side = local_client(&f.socket()).await;
    std::fs::create_dir_all(f.dir.join("config")).unwrap();
    f.write("config/projects.toml", "[agents.commands]\nhostonly = \"/bin/sh -i\"\n");
    let work = f.dir.join("host λ");
    std::fs::create_dir_all(&work).unwrap();
    let work = work.canonicalize().unwrap();

    let selection = f.selection("ssh_config", &f.socket(), "fwd");
    let host = select(&selection).await.unwrap();
    let conn = host.connection();
    let agents = legacy_agents(&conn).await.unwrap();
    let mine = agents.iter().find(|a| a.id == "hostonly").expect("the Host account's own agent");
    assert_eq!((mine.command.as_str(), mine.custom, mine.installed, mine.enabled), ("/bin/sh -i", true, true, true));

    let words: Vec<String> = mine.command.split_whitespace().map(String::from).collect();
    let spec = SessionSpec {
        label: "hostonly".into(),
        name: Some("on the Host".into()),
        cwd: Some(work.clone()),
        command: Some(words[0].clone()),
        args: words[1..].to_vec(),
        cols: 80,
        rows: 24,
        ..Default::default()
    };
    let created = legacy_create(&conn, spec).await.unwrap();
    assert_eq!(created.cwd, work);
    let mut output = legacy_output(&conn, &created.id).unwrap();
    legacy_attach(&conn, &created.id).await.unwrap();
    skiff_client::host::local::legacy_input(&conn, &created.id, b"echo AT-$(pwd)\n".to_vec()).unwrap();
    until_seen(&mut output, &format!("AT-{}", work.display())).await;

    let renamed = legacy_rename(&conn, &created.id, "renamed on the Host").await.unwrap();
    assert_eq!(renamed.name.as_deref(), Some("renamed on the Host"));
    let group: skiff_core::group::Group = serde_json::from_value(serde_json::json!({
        "name":"Remote","layout":{"type":"pane","session":created.id},"cwd":work.to_str().unwrap()}))
    .unwrap();
    let saved = legacy_save_group(&conn, group).await.unwrap();
    assert!(!saved.id.is_empty());
    // The Host keeps it: its own CLI sees the same group.
    let theirs = host_side.list_groups().await.unwrap();
    assert!(theirs.iter().any(|g| g.id == saved.id && g.cwd.as_deref() == work.to_str()));
    assert!(legacy_groups(&conn).await.unwrap().iter().any(|g| g.id == saved.id));
    legacy_delete_group(&conn, &saved.id).await.unwrap();

    legacy_kill(&conn, &created.id).await.unwrap();
    let left = host_side.list_sessions().await.unwrap();
    assert!(!left.iter().any(|s| s.id == created.id), "the remote session ended on the Host");
    assert!(selection.process_control().is_none());
}

/// Resize contention, with the app over the forward and a client on the Host
/// (as `skiff attach` from Termius). A session has one PTY size. skiffd
/// applies each resize in the order it receives them: the last one applied
/// is the size for every client. It tells every client each new size, so the
/// app learns of a size it did not send. Nobody re-asserts on that event.
#[tokio::test]
async fn the_last_applied_resize_is_the_size_for_every_attached_client() {
    let Some(f) = fixture("r") else { return };
    let host_side = local_client(&f.socket()).await;
    let shell = host_side
        .create_session(SessionSpec { label: "shared".into(), command: Some("/bin/sh".into()), cols: 80, rows: 24, ..Default::default() })
        .await
        .unwrap();
    let selection = f.selection("ssh_config", &f.socket(), "fwd");
    let host = select(&selection).await.unwrap();
    let conn = host.connection();
    let mut updates = skiff_client::host::local::legacy_updates(&conn).unwrap();
    legacy_attach(&conn, &shell.id).await.unwrap();
    let size = || async {
        let s = host_side.list_sessions().await.unwrap().into_iter().find(|s| s.id == shell.id).unwrap();
        (s.cols, s.rows)
    };
    // The app sizes it, then the Host-side client, then the app again.
    skiff_client::host::local::legacy_resize(&conn, &shell.id, 100, 30).await.unwrap();
    assert_eq!(size().await, (100, 30));
    host_side.resize(&shell.id, 120, 40).await.unwrap();
    assert_eq!(size().await, (120, 40), "the later resize wins");
    // The app hears the size it did not send.
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(until, updates.recv()).await {
            Ok(Some(skiff_core::protocol::Event::SessionUpdated { session })) if session.id == shell.id && (session.cols, session.rows) == (120, 40) => break,
            Ok(Some(_)) => {}
            other => panic!("no foreign size event: {other:?}"),
        }
    }
    // Nothing re-asserts: the size stays until a client resizes again.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(size().await, (120, 40));
    skiff_client::host::local::legacy_resize(&conn, &shell.id, 90, 20).await.unwrap();
    assert_eq!(size().await, (90, 20));
    // Two at once: whichever skiffd applies last is the size, and both see it.
    let (a, b) = tokio::join!(
        skiff_client::host::local::legacy_resize(&conn, &shell.id, 70, 21),
        host_side.resize(&shell.id, 71, 22)
    );
    a.unwrap();
    b.unwrap();
    let last = size().await;
    assert!(last == (70, 21) || last == (71, 22), "{last:?}");
    let screen_size = host_side.list_sessions().await.unwrap().into_iter().find(|s| s.id == shell.id).map(|s| (s.cols, s.rows));
    assert_eq!(screen_size, Some(last));
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

/// M4 over a real forward: the Host's `skiff-host` (a real process with the
/// Host's private state, started on the Host side by the test, never by the
/// app) serves that Host's projects and Git status through a second forward
/// to the socket beside its skiffd. Both planes report one Host. Stopping
/// the data plane leaves the remote terminal working.
#[tokio::test]
async fn remote_projects_and_git_status_come_from_the_hosts_data_plane_over_a_real_forward() {
    use skiff_client::host::data::{self, DataError};
    let Some(f) = fixture("d") else { return };
    let _ = local_client(&f.socket()).await;
    let repo = f.dir.join("host repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-C"])
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::create_dir_all(f.dir.join("config")).unwrap();
    f.write("config/projects.toml", &format!("[[project]]\nname = \"on the Host\"\npath = \"{}\"\nfleet = \"far\"\n", repo.display()));

    let binary = std::env::var_os("SKIFF_HOST_BIN").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_BIN_EXE_skiffd")).with_file_name("skiff-host"));
    assert!(binary.is_file(), "build skiff-host first: cargo build -p skiff-host (or set SKIFF_HOST_BIN)");
    let start = || {
        Command::new(&binary)
            .arg("--session-socket")
            .arg(f.socket())
            .env("SKIFF_CONFIG", f.dir.join("config/projects.toml"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::options().create(true).append(true).open(f.dir.join("skiff-host.log")).unwrap())
            .spawn()
            .unwrap()
    };
    struct Service(Child);
    impl Drop for Service {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let service = Service(start());
    let data_socket = skiff_core::socket::data_socket(&f.socket());
    let until = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(&data_socket).is_err() {
        assert!(Instant::now() < until, "skiff-host did not listen");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let selection = f.selection("ssh_config", &f.socket(), "fwd");
    let host = select(&selection).await.unwrap();
    let sessions = host.connection();
    let host_id = sessions.hello().host.host_id.clone();
    let plane = selection.connect_data(&host_id).await.unwrap();
    let c = plane.connection();
    assert_eq!(c.hello().host, sessions.hello().host, "one Host on both forwards");
    assert_ne!(c.hello().instance_id, sessions.hello().instance_id);

    let projects = data::project_list(&c).await.unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!((projects[0].name.as_str(), projects[0].fleet.as_deref()), ("on the Host", Some("far")));
    let worktree = projects[0].worktrees[0].path.clone();
    assert_eq!(Path::new(&worktree).canonicalize().unwrap(), repo.canonicalize().unwrap());
    let changes = data::git_changes(&c, &worktree).await.unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!((changes[0].path.as_str(), changes[0].added, changes[0].removed), ("a.txt", Some(1), Some(0)));

    // A terminal on the session plane, then the data plane stops on the Host.
    let spec = SessionSpec { cwd: Some(repo.clone()), cols: 80, rows: 24, ..Default::default() };
    let s = legacy_create(&sessions, spec).await.unwrap();
    let mut output = legacy_output(&sessions, &s.id).unwrap();
    legacy_attach(&sessions, &s.id).await.unwrap();
    // SAFETY: the test's own child.
    unsafe { libc::kill(service.0.id() as i32, libc::SIGTERM) };
    drop(service);
    assert!(matches!(data::project_list(&c).await, Err(skiff_host_api::Error::TransportLost)));
    drop(plane);
    let down = selection.connect_data(&host_id).await.err().unwrap();
    assert!(matches!(down.downcast_ref(), Some(DataError::NotRunning)), "{down:#}");
    skiff_client::host::local::legacy_input(&sessions, &s.id, b"echo STILL-$((7*6))\n".to_vec()).unwrap();
    until_seen(&mut output, "STILL-42").await;
    assert!(!sessions.is_closed());
    legacy_kill(&sessions, &s.id).await.unwrap();
}
