//! M6: a `SKIFF_LEDGER` that is relative, or not named `.db`, read through
//! skiff-host on a private socket, home and working folder. A relative name
//! is taken from the working folder, so results are held to the ledger's
//! folder there, never to `/`; the stamp follows the named file and its
//! `-wal`, whatever the name. Its own test binary: HOME, the working folder
//! and SKIFF_LEDGER are process-wide.
#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use skiff_client::host::{data, local::HostSelection};
use skiff_host_api::{Error, Hello};
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

const SCHEMA: &str = "CREATE TABLE tasks (id INTEGER PRIMARY KEY, uid TEXT UNIQUE, kind TEXT, parent TEXT, pane_id TEXT, pane_gen INTEGER,
    project TEXT, task TEXT, sent_at TEXT, state TEXT, result_path TEXT, ref TEXT, updated_at TEXT, type TEXT, socket TEXT,
    reported_at TEXT, title TEXT, closed_at TEXT); PRAGMA user_version = 7;";

fn add(db: &rusqlite::Connection, uid: &str, result: Option<&Path>) {
    db.execute(
        "INSERT INTO tasks (uid, kind, pane_id, pane_gen, project, task, sent_at, state, result_path, updated_at)
         VALUES (?1, 'task', 'pane-' || ?1, 1, 'web', 'Task ' || ?1, '2026-10-21 10:00:00', 'done', ?2, '2026-10-21 10:00:00')",
        rusqlite::params![uid, result.map(|p| p.to_string_lossy().into_owned())],
    )
    .unwrap();
}

/// A ledger at `file` with one result beside it and one outside its folder.
fn ledger_with_results(file: &Path, outside: &Path) {
    let results = file.parent().unwrap().join("results");
    std::fs::create_dir_all(&results).unwrap();
    std::fs::write(results.join("in.md"), "# Inside\n").unwrap();
    let db = rusqlite::Connection::open(file).unwrap();
    db.execute_batch(SCHEMA).unwrap();
    add(&db, "1111aaaa", Some(&results.join("in.md")));
    add(&db, "2222bbbb", Some(outside));
}

#[test]
fn a_relative_or_oddly_named_ledger_keeps_its_results_in_its_folder_and_its_stamp() {
    let dir = PathBuf::from("/tmp").join(format!("shlp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let _guard = Guard(dir.clone());
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    for k in ["XDG_CONFIG_HOME", "SKIFF_CONFIG", "SKIFF_STATE", "SKIFF_LEDGER", "SKIFF_REAL_STATE", "SKIFF_SESSION"] {
        std::env::remove_var(k);
    }
    assert!(skiff_core::state::real_config_path().starts_with(&home), "the person's config");
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::env::set_current_dir(&work).unwrap();
    // One worker: SKIFF_LEDGER changes only between requests.
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(paths(dir, work));
}

async fn paths(dir: PathBuf, work: PathBuf) {
    let socket = dir.join("skiffd.sock");
    skiff_core::state::set_socket(socket.clone());
    assert!(skiff_core::state::private_dir().is_some(), "never the live state");
    let hello = skiff_host::hello().unwrap();
    let host = hello.host.host_id.clone();
    let _service = start(&skiff_core::socket::data_socket(&socket), hello);
    let selection = HostSelection::new(socket.clone(), dir.join("bootstrap-id"));
    let plane = selection.connect_data(&host).await.unwrap();
    let c = plane.connection();

    // An outside result, a sibling of the working folder.
    let outside = dir.join("outside.md");
    std::fs::write(&outside, "outside").unwrap();

    // A bare name and a relative folder: both held to their folder in `work`.
    for (name, file) in [("ledger.db", work.join("ledger.db")), ("sub/ledger.db", work.join("sub/ledger.db"))] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        ledger_with_results(&file, &outside);
        let before = std::fs::read(&file).unwrap();
        std::env::set_var("SKIFF_LEDGER", name);
        assert_eq!(skiff_client::ledger::path().unwrap(), file, "{name}");
        let rows = data::ledger_rows(&c).await.unwrap();
        let exists = |uid: &str| rows.iter().find(|r| r.uid == uid).unwrap().result_exists;
        assert!(exists("1111aaaa"), "{name}: the result beside it");
        assert!(!exists("2222bbbb"), "{name}: a result outside its folder");
        assert_eq!(data::ledger_result(&c, "1111aaaa").await.unwrap().text.as_deref(), Some("# Inside\n"), "{name}");
        let got = data::ledger_result(&c, "2222bbbb").await;
        assert!(matches!(got, Err(Error::NotFound { .. })), "{name}: {got:?}");
        assert_eq!(std::fs::read(&file).unwrap(), before, "{name}: a read wrote the ledger");
        std::fs::remove_file(&file).unwrap();
    }

    // Any name: the stamp follows the file and `<name>-wal`. WAL mode with
    // the writer still open keeps the commit in the WAL, so only the
    // sidecar changes.
    for name in ["custom.sqlite", "ledgerfile"] {
        let file = work.join(name);
        std::env::set_var("SKIFF_LEDGER", name);
        let none = data::ledger_stamp(&c).await.unwrap();
        let db = rusqlite::Connection::open(&file).unwrap();
        db.execute_batch(SCHEMA).unwrap();
        let made = data::ledger_stamp(&c).await.unwrap();
        assert_ne!(made, none, "{name}: a new ledger");
        db.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(())).unwrap();
        add(&db, "3333cccc", None);
        let first = data::ledger_stamp(&c).await.unwrap();
        assert_eq!(data::ledger_stamp(&c).await.unwrap(), first, "{name}: stable while nothing changes");
        let main = std::fs::read(&file).unwrap();
        let mut wal = file.as_os_str().to_owned();
        wal.push("-wal");
        assert!(Path::new(&wal).is_file(), "{name}: the write is in the WAL");
        add(&db, "4444dddd", None);
        assert_eq!(std::fs::read(&file).unwrap(), main, "{name}: only the WAL changed");
        assert_ne!(data::ledger_stamp(&c).await.unwrap(), first, "{name}: a WAL write changes the stamp");
        assert_eq!(data::ledger_rows(&c).await.unwrap().len(), 2, "{name}");
        drop(db);
    }
    std::env::remove_var("SKIFF_LEDGER");
}
