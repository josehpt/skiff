use std::time::Duration;

use skiff_core::{
    protocol::Event,
    session::{SessionSpec, SessionState},
};
use skiffd::session::SessionPool;
use tokio::sync::broadcast::error::RecvError;

#[tokio::test]
async fn shell_session_streams_output_and_exits() {
    let pool = SessionPool::new();
    let mut events = pool.events.subscribe();

    // The user's shell runs as is. Any other program hands the terminal to
    // that shell when it exits, and the session would not end.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let spec = SessionSpec {
        name: Some("Crew session".into()),
        command: Some(shell.clone()),
        args: vec![
            "-c".into(),
            "sleep 0.2; printf 'hello\\a'; sleep 0.2".into(),
        ],
        ..Default::default()
    };
    let info = pool.create(spec).unwrap();
    assert_eq!(Some(info.label.as_str()), shell.rsplit('/').next());
    assert_eq!(info.name.as_deref(), Some("Crew session"));
    assert_eq!(info.state, SessionState::Working);

    let session = pool.get(&info.id).unwrap();
    let mut output = session.output.subscribe();

    let mut bytes = Vec::new();
    let mut saw_waiting = false;
    let mut exit_code = None;
    let deadline = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            chunk = output.recv() => match chunk {
                Ok(c) => bytes.extend_from_slice(&c),
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => {}
            },
            ev = events.recv() => match ev {
                Ok(Event::State { session, state: SessionState::Waiting }) if session == info.id => saw_waiting = true,
                Ok(Event::Exit { session, code }) if session == info.id => { exit_code = Some(code); break; }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
            _ = &mut deadline => panic!("session did not exit in time"),
        }
    }

    assert!(String::from_utf8_lossy(&bytes).contains("hello"));
    assert!(!saw_waiting, "a shell's own BEL is no call for the user");
    assert_eq!(exit_code, Some(Some(0)));
    assert_eq!(pool.get(&info.id).unwrap().info().state, SessionState::Done);
}

#[tokio::test]
async fn write_reaches_the_process() {
    let pool = SessionPool::new();
    let spec = SessionSpec {
        command: Some("cat".into()),
        ..Default::default()
    };
    let info = pool.create(spec).unwrap();
    let mut output = pool.get(&info.id).unwrap().output.subscribe();
    pool.write(&info.id, b"ping\n").unwrap();

    let mut bytes = Vec::new();
    let deadline = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(deadline);
    while !String::from_utf8_lossy(&bytes).contains("ping") {
        tokio::select! {
            chunk = output.recv() => if let Ok(c) = chunk { bytes.extend_from_slice(&c) },
            _ = &mut deadline => panic!("no echo from cat"),
        }
    }
    pool.kill(&info.id).unwrap();
}

/// Two opens at once start one coordinator. `CreateSession` cannot set the mark.
#[tokio::test]
async fn coordinator_starts_once() {
    use skiffd::session::Ask;
    let pool = SessionPool::new();
    let ask = || Ask {
        spec: Some(SessionSpec { name: Some("Skipper".into()), command: Some("sleep".into()), args: vec!["30".into()], ..Default::default() }),
        start: None,
        resume: None,
        expect: None,
    };
    let opens: Vec<_> = (0..4)
        .map(|_| {
            let pool = pool.clone();
            std::thread::spawn(move || pool.coordinator(ask()).unwrap().unwrap())
        })
        .collect();
    let got: Vec<_> = opens.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(got.iter().all(|c| c.session.id == got[0].session.id));
    assert_eq!(got.iter().filter(|c| c.created).count(), 1);
    let marked: Vec<_> = pool.list().into_iter().filter(|s| s.coordinator).collect();
    assert_eq!(marked.len(), 1);
    // Only find: no spec, no new session.
    let found = pool.coordinator(Ask { spec: None, start: None, resume: None, expect: None }).unwrap().unwrap();
    assert_eq!(found.session.id, got[0].session.id);
    // The mark is not on the wire, so a plain create cannot claim it.
    let spec: SessionSpec = serde_json::from_str(r#"{"coordinator":true,"command":"sleep","args":["30"]}"#).unwrap();
    assert!(!pool.create(spec).unwrap().coordinator);
    for s in pool.list() {
        pool.kill(&s.id).unwrap();
    }
}
