//! M6: Skipper's ledger, results and settings through skiff-host, on a
//! private socket and private state (never the live socket, config, ledger
//! or Host id), reached as the app reaches it: over this Mac's socket and
//! over a fake `ssh` forward. Both routes must answer what the CLI's read
//! code reads; a result is served only by its row, inside the ledger's
//! folder, bounded, without following a link; nothing writes the ledger.
#![cfg(unix)]
#![allow(clippy::too_many_arguments)]

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use skiff_client::host::{
    data,
    local::HostSelection,
    ssh::{SshOptions, SshTarget},
};
use skiff_core::files;
use skiff_host_api::{Capability, Error, FileContent, Hello};
use tokio::{net::UnixListener, task::JoinHandle};

struct Guard(PathBuf);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn start(socket: &Path, hello: Hello) -> JoinHandle<()> {
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        let _ = skiff_host::serve(listener, Arc::new(hello)).await;
    })
}

/// A fake ssh: binds the -L path and pipes each connection to the remote
/// path, which here is a socket on this machine.
fn fake_ssh(dir: &Path) -> Option<PathBuf> {
    if !Path::new("/usr/bin/python3").is_file() {
        eprintln!("skipped the SSH route: no /usr/bin/python3");
        return None;
    }
    let script = r#"#!/usr/bin/python3
import socket,sys,threading
spec=sys.argv[sys.argv.index('-L')+1]
local,remote=spec.split(':',1)
s=socket.socket(socket.AF_UNIX); s.bind(local); s.listen()
def pipe(a,b):
 try:
  while True:
   d=a.recv(65536)
   if not d: break
   b.sendall(d)
 except OSError: pass
 for x in (a,b):
  try: x.shutdown(socket.SHUT_RDWR)
  except OSError: pass
while True:
 c,_=s.accept()
 r=socket.socket(socket.AF_UNIX)
 try: r.connect(remote)
 except OSError:
  c.close(); continue
 threading.Thread(target=pipe,args=(c,r),daemon=True).start()
 threading.Thread(target=pipe,args=(r,c),daemon=True).start()
"#;
    let ssh = dir.join("fake-ssh");
    std::fs::write(&ssh, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    Some(ssh)
}

const SCHEMA: &str = "CREATE TABLE tasks (id INTEGER PRIMARY KEY, uid TEXT UNIQUE, kind TEXT, parent TEXT, pane_id TEXT, pane_gen INTEGER,
    project TEXT, task TEXT, sent_at TEXT, state TEXT, result_path TEXT, ref TEXT, updated_at TEXT, type TEXT, socket TEXT,
    reported_at TEXT, title TEXT, closed_at TEXT); PRAGMA user_version = 7;";

/// One row, as `skiff ledger add` leaves it.
fn add(db: &rusqlite::Connection, uid: &str, kind: &str, parent: Option<&str>, state: &str, result: Option<&str>, socket: Option<&str>, closed: Option<&str>) {
    db.execute(
        "INSERT INTO tasks (uid, kind, parent, pane_id, pane_gen, project, task, sent_at, state, result_path, updated_at, socket, closed_at)
         VALUES (?1, ?2, ?3, ?4, 1, 'web', 'Task ' || ?1, '2026-10-21 10:00:00', ?5, ?6, '2026-10-21 10:00:00', ?7, ?8)",
        rusqlite::params![uid, kind, parent, (kind == "task").then(|| format!("pane-{uid}")), state, result, socket, closed],
    )
    .unwrap();
}

fn sum(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_default()
}

/// A private home for this process, set before any thread runs. The live
/// folder guard ([`skiff_client::skipper::refuse_live_folder`]) compares
/// against the person's projects.toml and `~/skiff-agent`; with this home
/// both are the fixture's own, so no test reads the person's config.
fn private_home(dir: &Path) {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    for k in ["XDG_CONFIG_HOME", "SKIFF_CONFIG", "SKIFF_STATE", "SKIFF_LEDGER", "SKIFF_REAL_STATE", "SKIFF_SESSION"] {
        std::env::remove_var(k);
    }
    assert!(skiff_core::state::real_config_path().starts_with(&home), "the person's config");
    assert!(skiff_core::state::real_agent_folders().iter().all(|f| f.starts_with(&home)), "the person's agent folder");
}

#[test]
fn the_ledger_results_and_skipper_settings_come_from_the_host_read_only_and_bounded() {
    let dir = PathBuf::from("/tmp").join(format!("shl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = Guard(dir.clone());
    private_home(&dir);
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(host_ledger(dir));
}

async fn host_ledger(dir: PathBuf) {
    let socket = dir.join("skiffd.sock");
    skiff_core::state::set_socket(socket.clone());
    let private = skiff_core::state::private_dir().expect("never the live state");
    assert!(std::env::var_os("SKIFF_LEDGER").is_none(), "the test reads only the private ledger");

    // Private config: Skipper on, two task types.
    let config = skiff_core::config::config_path();
    assert!(config.starts_with(&private));
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        "[skiff_agent]\nname = \"Mate\"\nagent = \"claude\"\nenabled = true\n\n[skiff_agent.tasks]\nreview = { agent = \"claude\", model = \"opus\" }\nghost = { agent = \"nope\" }\n",
    )
    .unwrap();

    let hello = skiff_host::hello().unwrap();
    for c in [Capability::ledger_read(), Capability::skipper_settings()] {
        assert!(hello.capabilities.contains(&c), "{c:?}");
    }
    let host = hello.host.host_id.clone();
    let data_socket = skiff_core::socket::data_socket(&socket);
    let service = start(&data_socket, hello);
    let selection = HostSelection::new(socket.clone(), dir.join("bootstrap-id"));
    let local = selection.connect_data(&host).await.unwrap();
    let l = local.connection();
    let ssh = HostSelection::ssh(
        SshTarget { destination: "devbox".into(), socket: socket.to_string_lossy().into_owned() },
        SshOptions { program: fake_ssh(&dir).unwrap_or_else(|| "/nonexistent/ssh".into()), config: None },
        dir.join("fwd"),
        Some(host.clone()),
    )
    .unwrap();
    let remote = ssh.connect_data(&host).await.ok();
    assert_eq!(remote.is_some(), Path::new("/usr/bin/python3").is_file(), "the SSH route ran");
    let r = remote.as_ref().map(|p| p.connection()).unwrap_or_else(|| l.clone());

    // No ledger yet: no rows, nothing made on disk.
    let ledger = skiff_client::ledger::path().unwrap();
    assert!(ledger.starts_with(&private), "{}", ledger.display());
    for c in [&l, &r] {
        assert_eq!(data::ledger_rows(c).await.unwrap(), vec![]);
        assert!(matches!(data::ledger_result(c, "a1b2c3d4").await, Err(Error::NotFound { .. })));
    }
    assert!(!ledger.exists(), "a read made the ledger");
    let empty = data::ledger_stamp(&r).await.unwrap();

    // Results: in the folder, past the text limit, a link, a FIFO, outside,
    // through `..`, relative and missing.
    let folder = ledger.parent().unwrap().to_path_buf();
    let results = folder.join("results");
    std::fs::create_dir_all(results.join("plan-a")).unwrap();
    std::fs::write(results.join("ok.md"), "# Done\n").unwrap();
    std::fs::write(results.join("plan-a/plan.md"), "# Plan\n").unwrap();
    let line = "z".repeat(999) + "\n";
    std::fs::write(results.join("long.md"), line.repeat(files::TEXT_LIMIT / 1000 + 50)).unwrap();
    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.md"), "secret").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.md"), results.join("link.md")).unwrap();
    std::os::unix::fs::symlink(&outside, results.join("dirlink")).unwrap();
    assert!(Command::new("mkfifo").arg(results.join("pipe.md")).status().unwrap().success());
    let at = |p: &str| results.join(p).to_string_lossy().into_owned();
    let other = dir.join("other.sock").to_string_lossy().into_owned();

    let db = rusqlite::Connection::open(&ledger).unwrap();
    db.execute_batch(SCHEMA).unwrap();
    // A plan whose tasks are all done rolls up to done.
    add(&db, "aaaa0001", "plan", None, "sent", Some(&at("plan-a/plan.md")), None, None);
    add(&db, "aaaa0002", "task", Some("aaaa0001"), "done", Some(&at("ok.md")), Some(&socket.to_string_lossy()), None);
    add(&db, "aaaa0003", "task", Some("aaaa0001"), "done", Some(&at("long.md")), Some(&other), None);
    let refused = [
        ("bbbb0001", at("link.md")),
        ("bbbb0002", at("dirlink/secret.md")),
        ("bbbb0003", at("pipe.md")),
        ("bbbb0004", outside.join("secret.md").to_string_lossy().into_owned()),
        ("bbbb0005", format!("{}/../../outside/secret.md", results.display())),
        ("bbbb0006", "results/ok.md".into()),
        ("bbbb0007", at("missing.md")),
        ("bbbb0008", at("plan-a")),
    ];
    for (uid, path) in &refused {
        add(&db, uid, "task", None, "sent", Some(path), None, None);
    }
    add(&db, "bbbb0009", "task", None, "pending", None, None, None);
    // 102 closed items: Finished keeps the newest 100.
    for i in 0..102 {
        add(&db, &format!("cccc{i:04}"), "task", None, "done", None, None, Some(&format!("2026-10-20 10:{:02}:{:02}", i / 60, i % 60)));
    }
    drop(db);
    let before = (sum(&ledger), sum(&ledger.with_extension("db-wal")));

    // Rows: the CLI's read code, on both routes.
    let core = skiff_client::ledger::panel(&ledger).unwrap();
    for c in [&l, &r] {
        let rows = data::ledger_rows(c).await.unwrap();
        assert_eq!(rows.len(), core.len());
        assert_eq!(rows.iter().map(|r| (&r.uid, &r.state)).collect::<Vec<_>>(), core.iter().map(|r| (&r.uid, &r.state)).collect::<Vec<_>>());
        let row = |uid: &str| rows.iter().find(|r| r.uid == uid).unwrap();
        assert_eq!(row("aaaa0001").state, "done", "a plan rolls up");
        assert!(!row("aaaa0002").elsewhere && row("aaaa0003").elsewhere, "elsewhere is the Host's skiffd");
        for uid in ["aaaa0001", "aaaa0002", "aaaa0003"] {
            assert!(row(uid).result_exists, "{uid}");
        }
        for (uid, _) in &refused {
            assert!(!row(uid).result_exists, "{uid}");
        }
        assert!(!row("bbbb0009").result_exists);
        assert_eq!(rows.iter().filter(|r| r.closed_at.is_some()).count(), 100, "Finished keeps 100");
        assert!(rows.iter().all(|r| r.uid != "cccc0000" && r.uid != "cccc0001"), "the oldest closed stay in the file");
        assert_eq!(rows, data::ledger_rows(&l).await.unwrap(), "both routes agree");
    }

    // Results by uid only: text, bounded, and refusals that never block.
    for c in [&l, &r] {
        let ok = data::ledger_result(c, "aaaa0002").await.unwrap();
        assert_eq!((ok.content, ok.text.as_deref()), (FileContent::Text, Some("# Done\n")));
        assert_eq!(data::ledger_result(c, "aaaa0001").await.unwrap().text.as_deref(), Some("# Plan\n"));
        let long = data::ledger_result(c, "aaaa0003").await.unwrap();
        assert!(long.truncated && long.text.unwrap().len() <= files::TEXT_LIMIT);
        let started = Instant::now();
        for (uid, path) in &refused {
            let got = data::ledger_result(c, uid).await;
            assert!(matches!(got, Err(Error::NotFound { .. })), "{uid} {path}: {got:?}");
        }
        for uid in ["bbbb0009", "nope", "", "'; DROP TABLE tasks; --"] {
            assert!(matches!(data::ledger_result(c, uid).await, Err(Error::NotFound { .. })), "{uid}");
        }
        assert!(started.elapsed() < Duration::from_secs(10), "a refusal waited");
    }
    // Reads change nothing; a stamp is stable until the ledger changes.
    assert_eq!((sum(&ledger), sum(&ledger.with_extension("db-wal"))), before, "a read wrote the ledger");
    let stamp = data::ledger_stamp(&r).await.unwrap();
    assert_ne!(stamp, empty);
    assert_eq!(data::ledger_stamp(&l).await.unwrap(), stamp);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let db = rusqlite::Connection::open(&ledger).unwrap();
    add(&db, "dddd0001", "task", None, "sent", None, None, None);
    drop(db);
    assert_ne!(data::ledger_stamp(&r).await.unwrap(), stamp, "a write changes the stamp");
    assert!(data::ledger_rows(&r).await.unwrap().iter().any(|r| r.uid == "dddd0001"));

    // Settings: the Host's own config, as the CLI's code reads it.
    let want = skiff_client::skipper::settings(&skiff_core::config::load().unwrap()).unwrap();
    for c in [&l, &r] {
        let got = data::skipper_settings(c).await.unwrap();
        assert_eq!(got, want);
        assert_eq!((got.name.as_deref(), got.enabled), (Some("Mate"), true));
        assert_eq!(got.tasks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["review", "ghost"]);
        assert!(got.tasks[1].error.is_some(), "a gone agent says why");
        assert!(got.folder.as_deref().is_some_and(|f| Path::new(f).starts_with(&private)));
    }

    // An older ledger is refused, typed, and left as it is.
    std::fs::remove_file(&ledger).unwrap();
    let _ = std::fs::remove_file(ledger.with_extension("db-wal"));
    let old = rusqlite::Connection::open(&ledger).unwrap();
    old.execute_batch("CREATE TABLE tasks (id INTEGER PRIMARY KEY, uid TEXT); PRAGMA user_version = 6;").unwrap();
    drop(old);
    let old = sum(&ledger);
    assert!(matches!(data::ledger_rows(&r).await, Err(Error::Invalid { .. })));
    assert!(matches!(data::ledger_result(&r, "aaaa0002").await, Err(Error::Invalid { .. })));
    assert_eq!(sum(&ledger), old, "the reader migrated an old ledger");

    // A restarted service is a new instance of the same Host: reconnect and read again.
    let first = l.hello().instance_id.clone();
    service.abort();
    let _ = service.await;
    drop((local, remote));
    let again = skiff_host::hello().unwrap();
    assert_eq!(again.host.host_id, host);
    let _service = start(&data_socket, again);
    let plane = selection.connect_data(&host).await.unwrap();
    let c = plane.connection();
    assert_ne!(c.hello().instance_id, first);
    assert!(matches!(data::ledger_rows(&c).await, Err(Error::Invalid { .. })), "the same Host's ledger, read again");

    // A Host without the families: the client refuses before sending.
    let mut bare = skiff_host::hello().unwrap();
    bare.capabilities.retain(|c| *c != Capability::ledger_read() && *c != Capability::skipper_settings());
    let bare_socket = dir.join("bare.sock");
    let _bare = start(&bare_socket, bare);
    let stream = tokio::net::UnixStream::connect(&bare_socket).await.unwrap();
    let b = skiff_client::host::HostConnection::connect_with_timeout(stream, None, Duration::from_secs(5)).await.unwrap();
    assert!(matches!(data::ledger_rows(&b).await, Err(Error::UnsupportedCapability { .. })));
    assert!(matches!(data::skipper_settings(&b).await, Err(Error::UnsupportedCapability { .. })));
}
