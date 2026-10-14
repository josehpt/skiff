//! A guarded submit: text a program sends on its own never mixes with what
//! the person types. And the turn-end event, which metadata never sends.

use std::{sync::Arc, time::Duration};

use skiff_core::{
    protocol::Event,
    session::{SessionInfo, SessionSpec, SessionState},
};
use skiffd::session::SessionPool;

fn pool() -> Arc<SessionPool> {
    std::env::set_var("SHELL", "/bin/bash");
    let pool = SessionPool::new();
    pool.spawn_idle_watcher();
    pool.spawn_foreground_watcher();
    pool
}

async fn until(pool: &SessionPool, id: &str, secs: u64, what: &str, ok: impl Fn(&SessionInfo) -> bool) -> SessionInfo {
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let info = pool.get(id).unwrap().info();
        if ok(&info) {
            return info;
        }
        assert!(std::time::Instant::now() < end, "{what}: {info:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn until_text(pool: &SessionPool, id: &str, what: &str, ok: impl Fn(&[String]) -> bool) -> Vec<String> {
    let end = std::time::Instant::now() + Duration::from_secs(6);
    loop {
        let lines = pool.tail_text(id, 30).unwrap();
        if ok(&lines) {
            return lines;
        }
        assert!(std::time::Instant::now() < end, "{what}: {lines:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn bash(pool: &Arc<SessionPool>) -> SessionInfo {
    pool.create(SessionSpec {
        command: Some("/bin/bash".into()),
        args: vec!["--noprofile".into(), "--norc".into(), "-i".into()],
        cols: 80,
        rows: 24,
        ..Default::default()
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_after_the_check_refuses_the_submit() {
    let pool = pool();
    let info = bash(&pool);
    let checked = until(&pool, &info.id, 6, "shell did not go idle", |i| i.state == SessionState::Idle).await;
    // The person types between the check and the submit.
    pool.write(&info.id, b"x").unwrap();
    assert!(!pool.submit(&info.id, b"echo wake-$((6*7))", false, true, checked.input_gen).unwrap());
    tokio::time::sleep(Duration::from_millis(600)).await;
    let text = pool.tail_text(&info.id, 30).unwrap().join("\n");
    assert!(!text.contains("wake"), "a refused submit typed: {text}");

    // Read again, with the person's key cleared: it goes through.
    pool.write(&info.id, b"\x7f").unwrap();
    let now = pool.get(&info.id).unwrap().info().input_gen;
    assert!(pool.submit(&info.id, b"echo wake-$((6*7))", false, true, now).unwrap());
    until_text(&pool, &info.id, "the submit did not run", |l| l.iter().any(|l| l == "wake-42")).await;
    // Its own text counts too: a second submit on the same check is refused.
    assert!(!pool.submit(&info.id, b"echo again", false, true, now).unwrap());
    pool.kill(&info.id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_during_the_enter_delay_wait_behind_the_enter() {
    let pool = pool();
    let info = bash(&pool);
    let checked = until(&pool, &info.id, 6, "shell did not go idle", |i| i.state == SessionState::Idle).await;
    assert!(pool.submit(&info.id, b"echo one-$((1))", false, true, checked.input_gen).unwrap());
    // Typed while the submit waits to press Enter.
    pool.write(&info.id, b"echo two-$((2))\r").unwrap();
    let lines = until_text(&pool, &info.id, "both lines did not run", |l| l.iter().any(|l| l == "two-2")).await;
    assert!(lines.iter().any(|l| l == "one-1"), "the two lines mixed: {lines:?}");
    pool.kill(&info.id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_end_is_its_own_event_and_metadata_sends_none() {
    let pool = pool();
    let info = bash(&pool);
    until(&pool, &info.id, 6, "shell did not go idle", |i| i.state == SessionState::Idle).await;
    let mut events = pool.events.subscribe();
    let turn_ends = |events: &mut tokio::sync::broadcast::Receiver<Event>| {
        let mut n = 0;
        while let Ok(e) = events.try_recv() {
            if matches!(e, Event::TurnEnded { session } if session == info.id) {
                n += 1;
            }
        }
        n
    };
    pool.write(&info.id, b"sleep 1.5; echo built\r").unwrap();
    until(&pool, &info.id, 6, "the command's end did not set unread", |i| i.unread).await;
    assert_eq!(turn_ends(&mut events), 1);
    // Unread, then a rename, a theme and a resize.
    pool.rename(&info.id, "api").unwrap();
    pool.set_theme(&info.id, Some("dark".into())).unwrap();
    pool.resize(&info.id, 100, 30).await.unwrap();
    tokio::time::sleep(Duration::from_millis(3000)).await;
    assert_eq!(turn_ends(&mut events), 0, "metadata sent a turn end");
    pool.kill(&info.id).unwrap();
}
