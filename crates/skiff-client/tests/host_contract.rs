//! Only paired in-memory streams/private UnixStream pairs. No daemon or user data.
use skiff_client::{
    host::{Continuity, HostConnection},
    transport::Transport,
};
use skiff_host_api::*;
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

struct Fake(DuplexStream);
impl Transport for Fake {
    type Reader = ReadHalf<DuplexStream>;
    type Writer = WriteHalf<DuplexStream>;
    fn split(self) -> (Self::Reader, Self::Writer) {
        tokio::io::split(self.0)
    }
}
struct Peer {
    rd: BufReader<ReadHalf<DuplexStream>>,
    wr: WriteHalf<DuplexStream>,
}
impl Peer {
    async fn request(&mut self) -> Envelope {
        let mut line = String::new();
        assert!(self.rd.read_line(&mut line).await.unwrap() > 0);
        serde_json::from_str(&line).unwrap()
    }
    async fn send(&mut self, msg: Message) {
        let mut line = serde_json::to_vec(&msg).unwrap();
        line.push(b'\n');
        self.wr.write_all(&line).await.unwrap();
    }
    async fn reply(&mut self, id: u64, result: Result<Response, Error>) {
        self.send(Message::Reply { id, result }).await;
    }
}
fn hello() -> Hello {
    serde_json::from_value(serde_json::json!({
        "host_id":"host-a","instance_id":"instance-a","os":"linux","account":"me",
        "service_versions":{"skiff-host":"2.0.0"},"api_major":1,
        "capabilities":[{"name":"sessions","version":1}],
        "limits":{"max_frame_bytes":8388608,"max_in_flight":64}
    }))
    .unwrap()
}
fn pair() -> (Fake, Peer) {
    let (client, peer) = tokio::io::duplex(32 * 1024);
    let (rd, wr) = tokio::io::split(peer);
    (
        Fake(client),
        Peer {
            rd: BufReader::new(rd),
            wr,
        },
    )
}
async fn connect(h: Hello, previous: Option<&Hello>) -> (Result<HostConnection, Error>, Peer) {
    let (transport, mut peer) = pair();
    let (result, ()) = tokio::join!(
        HostConnection::connect_with_timeout(transport, previous, Duration::from_secs(1)),
        async {
            let ask = peer.request().await;
            assert_eq!(ask.request, Request::Hello);
            peer.reply(ask.id, Ok(Response::Hello { hello: h })).await;
        }
    );
    (result, peer)
}

#[tokio::test(start_paused = true)]
async fn unknown_api_or_missing_capability_never_dispatches_an_operation() {
    let mut h = hello();
    h.api_major = 99;
    let (result, _) = connect(h, None).await;
    assert!(matches!(result, Err(Error::IncompatibleVersion { .. })));
    for caps in [
        vec![],
        vec![Capability {
            name: "sessions".into(),
            version: 2,
        }],
    ] {
        let mut h = hello();
        h.capabilities = caps;
        let (connection, mut peer) = connect(h, None).await;
        let connection = connection.unwrap();
        assert!(matches!(
            connection.request(Request::ListSessions).await,
            Err(Error::UnsupportedCapability { .. })
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), peer.request())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn replies_are_correlated_out_of_order_and_duplicate_reply_cannot_complete_twice() {
    let (connection, mut peer) = connect(hello(), None).await;
    let connection = connection.unwrap();
    let (a, b, ()) = tokio::join!(
        connection.request(Request::ListSessions),
        connection.request(Request::Resize {
            session: "s".into(),
            cols: 80,
            rows: 24
        }),
        async {
            let a = peer.request().await;
            let b = peer.request().await;
            peer.reply(b.id, Ok(Response::Ok)).await;
            peer.reply(
                b.id,
                Err(Error::Permission {
                    message: "duplicate".into(),
                }),
            )
            .await;
            peer.reply(a.id, Ok(Response::Sessions { sessions: vec![] }))
                .await;
        }
    );
    assert_eq!(a, Ok(Response::Sessions { sessions: vec![] }));
    assert_eq!(b, Ok(Response::Ok));
}

#[tokio::test]
async fn snapshot_can_precede_subscribe_reply_and_bytes_and_writes_stay_ordered() {
    let (connection, mut peer) = connect(hello(), None).await;
    let connection = connection.unwrap();
    let mut events = connection.events().unwrap();
    let data = vec![0, 255, 13, 10];
    let (result, ()) = tokio::join!(
        connection.request(Request::Subscribe {
            session: "s".into()
        }),
        async {
            let ask = peer.request().await;
            peer.send(Message::Event {
                event: Event::Snapshot {
                    session: "s".into(),
                    data: data.clone(),
                    cols: 80,
                    rows: 24,
                },
            })
            .await;
            peer.send(Message::Event {
                event: Event::Output {
                    session: "s".into(),
                    data: vec![27, 91],
                },
            })
            .await;
            peer.reply(ask.id, Ok(Response::Ok)).await;
        }
    );
    assert_eq!(result, Ok(Response::Ok));
    assert_eq!(
        events.recv().await.unwrap(),
        Event::Snapshot {
            session: "s".into(),
            data: data.clone(),
            cols: 80,
            rows: 24,
        }
    );
    assert_eq!(
        events.recv().await.unwrap(),
        Event::Output {
            session: "s".into(),
            data: vec![27, 91]
        }
    );
    for data in [data, vec![1, 2]] {
        let request = Request::Write {
            session: "s".into(),
            data,
            paste: false,
        };
        let (result, ()) = tokio::join!(connection.request(request.clone()), async {
            let ask = peer.request().await;
            assert_eq!(ask.request, request);
            peer.reply(ask.id, Ok(Response::Ok)).await;
        });
        result.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn cancellation_stall_and_timeout_remove_pending_slots() {
    let mut h = hello();
    h.limits.max_in_flight = 1;
    let (connection, mut peer) = connect(h, None).await;
    let connection = Arc::new(connection.unwrap());
    let c = connection.clone();
    let task = tokio::spawn(async move { c.request(Request::ListSessions).await });
    let cancelled = peer.request().await;
    assert_eq!(
        connection.request(Request::ListSessions).await,
        Err(Error::HostBusy)
    );
    task.abort();
    let _ = task.await;
    // A late reply to the cancelled waiter is harmless, and its slot is reusable.
    peer.reply(cancelled.id, Ok(Response::Sessions { sessions: vec![] }))
        .await;
    let c = connection.clone();
    let stalled = tokio::spawn(async move { c.request(Request::ListSessions).await });
    let timed_out = peer.request().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(stalled.await.unwrap(), Err(Error::TimedOut));
    peer.reply(timed_out.id, Ok(Response::Sessions { sessions: vec![] }))
        .await;
    let (result, ()) = tokio::join!(connection.request(Request::ListSessions), async {
        let ask = peer.request().await;
        peer.reply(ask.id, Ok(Response::Sessions { sessions: vec![] }))
            .await;
    });
    assert!(result.is_ok());
}

#[tokio::test]
async fn disconnect_and_peer_drop_complete_waiters_and_close_events() {
    for drop_peer in [false, true] {
        let (connection, mut peer) = connect(hello(), None).await;
        let connection = connection.unwrap();
        let mut events = connection.events().unwrap();
        let (result, ()) = tokio::join!(connection.request(Request::ListSessions), async {
            peer.request().await;
            if drop_peer {
                drop(peer);
            } else {
                connection.disconnect();
                connection.disconnect();
            }
        });
        assert_eq!(result, Err(Error::TransportLost));
        assert!(events.recv().await.is_err());
        assert_eq!(
            connection.request(Request::ListSessions).await,
            Err(Error::TransportLost)
        );
    }
}

#[tokio::test]
async fn reconnect_validates_identity_and_instance_without_replay_or_resubscription() {
    let old = hello();
    let mut h = old.clone();
    h.host.host_id = "host-b".to_string().try_into().unwrap();
    assert!(matches!(
        connect(h, Some(&old)).await.0,
        Err(Error::IdentityChanged { .. })
    ));
    for (h, expected) in [
        (old.clone(), Continuity::SameInstance),
        (
            {
                let mut h = old.clone();
                h.instance_id = "instance-b".to_string().try_into().unwrap();
                h
            },
            Continuity::NewInstance,
        ),
    ] {
        let (connection, mut peer) = connect(h, Some(&old)).await;
        assert_eq!(connection.unwrap().continuity(), expected);
        // Dropping this connection closes it; only Hello was written.
        let mut extra = String::new();
        assert_eq!(peer.rd.read_line(&mut extra).await.unwrap(), 0);
    }
}

#[tokio::test]
async fn malformed_or_mismatched_replies_fail_explicitly() {
    for malformed in [true, false] {
        let (connection, mut peer) = connect(hello(), None).await;
        let connection = connection.unwrap();
        let (result, ()) = tokio::join!(connection.request(Request::ListSessions), async {
            let ask = peer.request().await;
            if malformed {
                peer.wr.write_all(b"not json\n").await.unwrap();
            } else {
                peer.reply(ask.id, Ok(Response::Ok)).await;
            }
        });
        assert!(matches!(result, Err(Error::MalformedContract { .. })));
    }
}

#[tokio::test]
async fn bounded_events_report_lag_without_starving_control_replies() {
    let (connection, mut peer) = connect(hello(), None).await;
    let connection = connection.unwrap();
    let mut events = connection.events().unwrap();
    let (reply, ()) = tokio::join!(connection.request(Request::ListSessions), async {
        let ask = peer.request().await;
        for _ in 0..300 {
            peer.send(Message::Event {
                event: Event::Output {
                    session: "s".into(),
                    data: vec![1],
                },
            })
            .await;
        }
        peer.reply(ask.id, Ok(Response::Sessions { sessions: vec![] }))
            .await;
    });
    assert!(reply.is_ok());
    assert!(matches!(
        events.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Lagged(_))
    ));
}

#[tokio::test]
async fn local_unix_adapter_implements_the_same_host_contract() {
    let (local, peer) = tokio::net::UnixStream::pair().unwrap();
    let (result, ()) = tokio::join!(HostConnection::connect(local, None), async {
        let mut peer = BufReader::new(peer);
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        let ask: Envelope = serde_json::from_str(&line).unwrap();
        assert_eq!(ask.request, Request::Hello);
        let mut frame = serde_json::to_vec(&Message::Reply {
            id: ask.id,
            result: Ok(Response::Hello { hello: hello() }),
        })
        .unwrap();
        frame.push(b'\n');
        peer.get_mut().write_all(&frame).await.unwrap();
    });
    assert_eq!(result.unwrap().continuity(), Continuity::FirstConnection);
}

#[tokio::test(start_paused = true)]
async fn oversized_requests_are_rejected_before_dispatch_and_transport_stalls_are_bounded() {
    let mut h = hello();
    h.limits.max_frame_bytes = 128;
    let (connection, mut peer) = connect(h, None).await;
    let connection = connection.unwrap();
    assert!(matches!(
        connection
            .request(Request::Write {
                session: "s".into(),
                data: vec![0; 256],
                paste: false
            })
            .await,
        Err(Error::MalformedContract { .. })
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), peer.request())
            .await
            .is_err()
    );
    // No Hello reply: connecting itself has a bounded budget and no usable client.
    let (transport, _peer) = pair();
    assert!(matches!(
        HostConnection::connect_with_timeout(transport, None, Duration::from_secs(1)).await,
        Err(Error::TimedOut)
    ));
}

#[tokio::test]
async fn service_errors_are_typed_without_fallback_or_connection_replacement() {
    let (connection, mut peer) = connect(hello(), None).await;
    let connection = connection.unwrap();
    for error in [
        Error::NotFound {
            message: "missing".into(),
        },
        Error::Permission {
            message: "denied".into(),
        },
        Error::Conflict {
            message: "changed".into(),
        },
        Error::HostBusy,
    ] {
        let (result, ()) = tokio::join!(connection.request(Request::ListSessions), async {
            let ask = peer.request().await;
            assert_eq!(ask.request, Request::ListSessions);
            peer.reply(ask.id, Err(error.clone())).await;
        });
        assert_eq!(result, Err(error));
    }
    assert_eq!(connection.continuity(), Continuity::FirstConnection);
}

#[tokio::test]
async fn m1a_connection_status_also_works_over_the_negotiated_transport() {
    let mut h = hello();
    h.capabilities = vec![Capability::connection_status()];
    let (connection, mut peer) = connect(h, None).await;
    let connection = connection.unwrap();
    let (response, ()) = tokio::join!(connection.request(Request::ConnectionStatus), async {
        let request = peer.request().await;
        assert_eq!(request.request, Request::ConnectionStatus);
        peer.reply(
            request.id,
            Ok(Response::ConnectionStatus {
                version: "fixture".into(),
                protocol: 27,
            }),
        )
        .await;
    });
    assert_eq!(
        response,
        Ok(Response::ConnectionStatus {
            version: "fixture".into(),
            protocol: 27
        })
    );
    assert!(matches!(
        connection.request(Request::ListSessions).await,
        Err(Error::UnsupportedCapability { .. })
    ));
}
