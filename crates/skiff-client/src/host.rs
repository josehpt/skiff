//! Opt-in M0 Host client. Existing Client/Ping/Pong and app startup do not use it.
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use skiff_host_api::{Envelope, Error, Event, Hello, Message, Request, Response};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
};

use crate::transport::Transport;

const MAX_FRAME: usize = 8 * 1024 * 1024;
const MAX_PENDING: usize = 64;
const EVENT_QUEUE: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continuity {
    FirstConnection,
    SameInstance,
    NewInstance,
}

type Reply = Result<Response, Error>;
struct Shared {
    failure: Option<Error>,
    pending: HashMap<u64, oneshot::Sender<Reply>>,
    events: Option<broadcast::Sender<Event>>,
}
struct State {
    inner: Mutex<Shared>,
    closed: watch::Sender<bool>,
}
impl State {
    fn fail(&self, error: Error) {
        let mut s = self.inner.lock().unwrap();
        if s.failure.is_some() {
            return;
        }
        s.failure = Some(error.clone());
        for (_, reply) in s.pending.drain() {
            let _ = reply.send(Err(error.clone()));
        }
        s.events.take();
        self.closed.send_replace(true);
    }
}
struct Pending {
    state: Arc<State>,
    id: u64,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.state.inner.lock().unwrap().pending.remove(&self.id);
    }
}

struct Engine {
    state: Arc<State>,
    tx: mpsc::Sender<(u64, Vec<u8>)>,
    next: AtomicU64,
    tasks: Vec<JoinHandle<()>>,
    timeout: Duration,
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.state.fail(Error::TransportLost);
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl Engine {
    fn new<T: Transport>(transport: T, timeout: Duration) -> Self {
        let (rd, mut wr) = transport.split();
        let (events, _) = broadcast::channel(EVENT_QUEUE);
        let (closed, _) = watch::channel(false);
        let state = Arc::new(State {
            inner: Mutex::new(Shared {
                failure: None,
                pending: HashMap::new(),
                events: Some(events),
            }),
            closed,
        });
        let (tx, mut rx) = mpsc::channel::<(u64, Vec<u8>)>(MAX_PENDING);
        let writer_state = state.clone();
        let mut writer_closed = state.closed.subscribe();
        let writer = tokio::spawn(async move {
            loop {
                let frame = tokio::select! {
                    _ = writer_closed.changed() => break,
                    frame = rx.recv() => frame,
                };
                let Some((id, line)) = frame else { break };

                // Cancellation can prevent a queued request, never retract an in-flight write.
                if !writer_state.inner.lock().unwrap().pending.contains_key(&id) {
                    continue;
                }
                let written = tokio::select! {
                    _ = writer_closed.changed() => break,
                    result = wr.write_all(&line) => result,
                };
                if written.is_err() {
                    writer_state.fail(Error::TransportLost);
                    break;
                }
            }
        });
        let reader_state = state.clone();
        let mut reader_closed = state.closed.subscribe();
        let reader = tokio::spawn(async move {
            let mut reader = BufReader::new(rd);
            loop {
                let mut line = Vec::new();
                let frame = tokio::select! {
                    _ = reader_closed.changed() => break,
                    result = async {
                        (&mut reader).take(MAX_FRAME as u64 + 1).read_until(b'\n', &mut line).await
                    } => result,
                };
                match frame {
                    Ok(0) | Err(_) => {
                        reader_state.fail(Error::TransportLost);
                        break;
                    }
                    Ok(_) if line.len() > MAX_FRAME || line.last() != Some(&b'\n') => {
                        reader_state.fail(Error::MalformedContract {
                            message: "oversize or incomplete frame".into(),
                        });
                        break;
                    }
                    Ok(_) => {}
                }
                match serde_json::from_slice::<Message>(&line) {
                    Ok(Message::Reply { id, result }) => {
                        if let Some(reply) = reader_state.inner.lock().unwrap().pending.remove(&id)
                        {
                            let _ = reply.send(result);
                        }
                    }
                    Ok(Message::Event { event }) => {
                        if let Some(events) = &reader_state.inner.lock().unwrap().events {
                            let _ = events.send(event);
                        }
                    }
                    Err(_) => {
                        reader_state.fail(Error::MalformedContract {
                            message: "unreadable Host frame".into(),
                        });
                        break;
                    }
                }
            }
        });
        Self {
            state,
            tx,
            next: AtomicU64::new(1),
            tasks: vec![reader, writer],
            timeout,
        }
    }

    async fn request(&self, request: Request, max_pending: usize, max_frame: usize) -> Reply {
        let id = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| Error::HostBusy)?;
        let mut line = serde_json::to_vec(&Envelope {
            id,
            request: request.clone(),
        })
        .map_err(|e| Error::MalformedContract {
            message: e.to_string(),
        })?;
        line.push(b'\n');
        if line.len() > max_frame {
            return Err(Error::MalformedContract {
                message: "request exceeds frame limit".into(),
            });
        }
        let (reply, rx) = oneshot::channel();
        {
            let mut s = self.state.inner.lock().unwrap();
            if let Some(error) = &s.failure {
                return Err(error.clone());
            }
            if s.pending.len() >= max_pending {
                return Err(Error::HostBusy);
            }
            s.pending.insert(id, reply);
        }
        let _pending = Pending {
            state: self.state.clone(),
            id,
        };
        let result = tokio::time::timeout(self.timeout, async {
            self.tx
                .send((id, line))
                .await
                .map_err(|_| Error::TransportLost)?;
            rx.await.map_err(|_| Error::TransportLost)?
        })
        .await
        .map_err(|_| Error::TimedOut)??;
        if !request.accepts(&result) {
            let error = Error::MalformedContract {
                message: "reply does not match request".into(),
            };
            self.state.fail(error.clone());
            return Err(error);
        }
        Ok(result)
    }
}

/// One negotiated Host, independent of endpoint/socket/process ownership.
/// Dropping a request future cancels its local wait; it never replays a request.
/// Dropping/disconnecting this connection closes transport tasks, not host sessions.
pub struct HostConnection {
    engine: Engine,
    hello: Hello,
    continuity: Continuity,
}
impl HostConnection {
    pub async fn connect<T: Transport>(
        transport: T,
        previous: Option<&Hello>,
    ) -> Result<Self, Error> {
        Self::connect_with_timeout(transport, previous, Duration::from_secs(20)).await
    }

    pub async fn connect_with_timeout<T: Transport>(
        transport: T,
        previous: Option<&Hello>,
        timeout: Duration,
    ) -> Result<Self, Error> {
        let engine = Engine::new(transport, timeout);
        let Response::Hello { hello } = engine
            .request(Request::Hello, MAX_PENDING, MAX_FRAME)
            .await?
        else {
            unreachable!()
        };
        hello.validate()?;
        let continuity = match previous {
            None => Continuity::FirstConnection,
            Some(old) if old.host.host_id != hello.host.host_id => {
                return Err(Error::IdentityChanged {
                    expected: old.host.host_id.clone(),
                    received: hello.host.host_id.clone(),
                })
            }
            Some(old) if old.instance_id != hello.instance_id => Continuity::NewInstance,
            Some(_) => Continuity::SameInstance,
        };
        Ok(Self {
            engine,
            hello,
            continuity,
        })
    }

    pub fn hello(&self) -> &Hello {
        &self.hello
    }
    /// NewInstance requires new subscriptions/snapshots; M0 does not restore them automatically.
    pub fn continuity(&self) -> Continuity {
        self.continuity
    }

    pub async fn request(&self, request: Request) -> Reply {
        let capability = request
            .capability()
            .ok_or_else(|| Error::MalformedContract {
                message: "Hello is handshake-only".into(),
            })?;
        self.hello.require(&capability)?;
        self.engine
            .request(
                request,
                self.hello.limits.max_in_flight.min(MAX_PENDING),
                self.hello.limits.max_frame_bytes.min(MAX_FRAME),
            )
            .await
    }

    /// Listen before requesting Subscribe: a snapshot may precede its reply.
    /// Lagged means discard queued output and request a new subscription/snapshot.
    pub fn events(&self) -> Result<broadcast::Receiver<Event>, Error> {
        let s = self.engine.state.inner.lock().unwrap();
        s.events
            .as_ref()
            .map(|events| events.subscribe())
            .ok_or_else(|| s.failure.clone().unwrap_or(Error::TransportLost))
    }

    pub fn disconnect(&self) {
        self.engine.state.fail(Error::TransportLost);
        for task in &self.engine.tasks {
            task.abort();
        }
    }
}
