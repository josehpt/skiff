//! `Watching` registers a connection as the Skiff agent's watcher until it closes.

use std::{sync::Arc, time::Duration};

use skiff_core::{
    protocol::{Event, Response, ServerMessage},
    session::{SessionInfo, SessionSpec},
};
use skiffd::session::SessionPool;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::{unix::OwnedReadHalf, unix::OwnedWriteHalf, UnixStream},
    sync::broadcast,
};

struct Conn {
    rd: Lines<BufReader<OwnedReadHalf>>,
    wr: OwnedWriteHalf,
}

fn connect(pool: &Arc<SessionPool>) -> Conn {
    let (ours, theirs) = UnixStream::pair().unwrap();
    tokio::spawn(skiffd::server::handle(theirs, pool.clone()));
    let (rd, wr) = ours.into_split();
    Conn { rd: BufReader::new(rd).lines(), wr }
}

impl Conn {
    /// Sends one request and returns its reply, skipping events.
    async fn ask(&mut self, json: &str) -> Response {
        self.wr.write_all(format!("{json}\n").as_bytes()).await.unwrap();
        loop {
            let line = tokio::time::timeout(Duration::from_secs(5), self.rd.next_line()).await.unwrap().unwrap().unwrap();
            if let ServerMessage::Reply(r) = serde_json::from_str(&line).unwrap() {
                return r.response;
            }
        }
    }
}

/// The next `SessionUpdated` for `id`.
async fn updated(events: &mut broadcast::Receiver<Event>, id: &str) -> SessionInfo {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), events.recv()).await.expect("no SessionUpdated") {
            Ok(Event::SessionUpdated { session }) if session.id == id => return session,
            _ => {}
        }
    }
}

#[tokio::test]
async fn watching_lasts_until_the_connection_closes() {
    let pool = SessionPool::new();
    let mut events = pool.events.subscribe();
    let s = pool
        .create(SessionSpec { command: Some("sleep".into()), args: vec!["30".into()], ..Default::default() })
        .unwrap();
    let me = std::process::id();
    let watching = |failure: &str| format!(r#"{{"id":1,"cmd":"watching","session":"{}"{failure}}}"#, s.id);

    let mut a = connect(&pool);
    assert!(matches!(a.ask(&watching("")).await, Response::Ok));
    let w = updated(&mut events, &s.id).await.watch.expect("watch set");
    assert_eq!(w.pids, vec![me]);
    assert!(w.failure.is_none());
    // A watcher that does not report what it follows: unknown.
    assert!(w.follows.is_none());
    assert_eq!(pool.get(&s.id).unwrap().info().watch, Some(w.clone()));

    // A failure, then a clear, on the same connection. `since` holds.
    let failed = r#","failure":{"kind":"undelivered","message":"undelivered: x","at":7}"#;
    assert!(matches!(a.ask(&watching(failed)).await, Response::Ok));
    let f = updated(&mut events, &s.id).await.watch.unwrap();
    assert_eq!(f.failure.as_ref().map(|f| f.kind.as_str()), Some("undelivered"));
    assert_eq!(f.since, w.since);
    assert!(matches!(a.ask(&watching("")).await, Response::Ok));
    assert!(updated(&mut events, &s.id).await.watch.unwrap().failure.is_none());

    // A second watcher adds its pid.
    let mut b = connect(&pool);
    assert!(matches!(b.ask(&watching("")).await, Response::Ok));
    assert_eq!(updated(&mut events, &s.id).await.watch.unwrap().pids, vec![me, me]);
    // One reports what it follows: the union, sorted, though the other does not.
    assert!(matches!(b.ask(&watching(r#","follows":["p2","p1","p2"]"#)).await, Response::Ok));
    assert_eq!(updated(&mut events, &s.id).await.watch.unwrap().follows, Some(vec!["p1".into(), "p2".into()]));

    drop(b);
    let w = updated(&mut events, &s.id).await.watch.unwrap();
    assert_eq!(w.pids, vec![me]);
    assert!(w.follows.is_none());
    drop(a);
    assert!(updated(&mut events, &s.id).await.watch.is_none());
    assert!(pool.get(&s.id).unwrap().info().watch.is_none());

    let mut c = connect(&pool);
    match c.ask(r#"{"id":2,"cmd":"watching","session":"nope"}"#).await {
        Response::Error { message } => assert!(message.contains("no such session")),
        other => panic!("expected an error, got {other:?}"),
    }
    pool.kill(&s.id).unwrap();
}
