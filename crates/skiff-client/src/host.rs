//! Host contract client, with a narrow in-process local status adapter in M1a.
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use skiff_host_api::{
    Capability, Envelope, Error, Event, Hello, Message, Request, Response, SessionUpdate,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
};

use crate::transport::Transport;

mod inventory;
pub mod local;
mod updates;

/// The one in-process M1a operation; no concrete socket/process type in the seam.
/// ScreenSource adds only the M1b text snapshot; other families stay on wire.
pub trait StatusSource: Send + Sync {
    fn status(&self) -> Pin<Box<dyn Future<Output = Result<Response, Error>> + Send + '_>>;
}

/// One read-only text snapshot, independent of socket/core session types.
pub trait ScreenSource: Send + Sync {
    fn screen(
        &self,
        session: String,
        rows: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Response, Error>> + Send + '_>>;
}

/// Read-only desktop inventory, independent of core session lifecycle types.
pub trait InventorySource: Send + Sync {
    fn inventory(&self) -> Pin<Box<dyn Future<Output = Result<Response, Error>> + Send + '_>>;
}

/// Read-only non-output session events. Each call starts a receiver at the
/// live tail: nothing earlier is replayed.
pub trait UpdateSource: Send + Sync {
    fn updates(&self) -> Box<dyn UpdateStream>;
}

/// One receiver of an UpdateSource, in the source's order.
pub trait UpdateStream: Send {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Result<SessionUpdate, UpdateGap>> + Send + '_>>;
}

/// Why a session-update receiver returned no update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateGap {
    /// A slow receiver missed this many updates. They are never replayed;
    /// the receiver continues with later ones.
    Lagged(u64),
    /// One event could not be projected. It is skipped; the receiver continues.
    Malformed(Error),
    /// The connection ended or was disconnected. Nothing more arrives.
    Closed,
}

enum UpdatesInner {
    Wire(broadcast::Receiver<Event>),
    Local {
        stream: Box<dyn UpdateStream>,
        closed: watch::Receiver<bool>,
    },
}

/// Session updates from one HostConnection, from [`HostConnection::session_updates`].
pub struct SessionUpdates(UpdatesInner);
impl SessionUpdates {
    pub async fn recv(&mut self) -> Result<SessionUpdate, UpdateGap> {
        match &mut self.0 {
            UpdatesInner::Wire(rx) => loop {
                match rx.recv().await {
                    Ok(Event::SessionUpdate { update }) => return Ok(*update),
                    // Other families have their own receivers.
                    Ok(Event::Snapshot { .. } | Event::Output { .. } | Event::SessionChanged { .. }) => {}
                    Err(broadcast::error::RecvError::Lagged(n)) => return Err(UpdateGap::Lagged(n)),
                    Err(broadcast::error::RecvError::Closed) => return Err(UpdateGap::Closed),
                }
            },
            UpdatesInner::Local { stream, closed } => {
                if *closed.borrow_and_update() {
                    return Err(UpdateGap::Closed);
                }
                tokio::select! {
                    biased;
                    // A dropped connection ends the stream like a disconnect.
                    _ = closed.changed() => Err(UpdateGap::Closed),
                    update = stream.recv() => update,
                }
            }
        }
    }
}

enum Backend {
    Wire(Engine),
    LocalReadOnly {
        source: Arc<dyn StatusSource>,
        screen: Option<Arc<dyn ScreenSource>>,
        inventory: Option<Arc<dyn InventorySource>>,
        updates: Option<Arc<dyn UpdateSource>>,
        read_slots: tokio::sync::Semaphore,
        closed: watch::Sender<bool>,
        slots: tokio::sync::Semaphore,
    },
}

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
    backend: Backend,
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
        Self::negotiated(Backend::Wire(engine), hello, previous)
    }

    fn negotiated(backend: Backend, hello: Hello, previous: Option<&Hello>) -> Result<Self, Error> {
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
            backend,
            hello,
            continuity,
        })
    }

    /// Bootstrap an in-process local status source. It is never a wire Hello
    /// from legacy skiffd, and cannot claim the session-plane capability.
    pub fn local_status(
        source: Arc<dyn StatusSource>,
        hello: Hello,
        previous: Option<&Hello>,
    ) -> Result<Self, Error> {
        Self::local_readonly(source, None, None, None, hello, previous)
    }

    /// M1b adds only session_screen; broad sessions@1 stays unavailable locally.
    pub fn local_screen(
        source: Arc<dyn StatusSource>,
        screen: Arc<dyn ScreenSource>,
        hello: Hello,
        previous: Option<&Hello>,
    ) -> Result<Self, Error> {
        Self::local_readonly(source, Some(screen), None, None, hello, previous)
    }

    /// M1c adds only the lossless read-only inventory, not sessions@1.
    pub fn local_inventory(
        source: Arc<dyn StatusSource>,
        screen: Arc<dyn ScreenSource>,
        inventory: Arc<dyn InventorySource>,
        hello: Hello,
        previous: Option<&Hello>,
    ) -> Result<Self, Error> {
        Self::local_readonly(source, Some(screen), Some(inventory), None, hello, previous)
    }

    /// M1d adds only the read-only session-update stream; no output, input,
    /// subscription request or lifecycle operation.
    pub fn local_updates(
        source: Arc<dyn StatusSource>,
        screen: Arc<dyn ScreenSource>,
        inventory: Arc<dyn InventorySource>,
        updates: Arc<dyn UpdateSource>,
        hello: Hello,
        previous: Option<&Hello>,
    ) -> Result<Self, Error> {
        Self::local_readonly(
            source,
            Some(screen),
            Some(inventory),
            Some(updates),
            hello,
            previous,
        )
    }

    fn local_readonly(
        source: Arc<dyn StatusSource>,
        screen: Option<Arc<dyn ScreenSource>>,
        inventory: Option<Arc<dyn InventorySource>>,
        updates: Option<Arc<dyn UpdateSource>>,
        hello: Hello,
        previous: Option<&Hello>,
    ) -> Result<Self, Error> {
        hello.validate()?;
        if hello.capabilities.iter().any(|c| {
            c.name != skiff_host_api::CONNECTION_STATUS
                && !(screen.is_some() && c.name == skiff_host_api::SESSION_SCREEN)
                && !(inventory.is_some() && c.name == skiff_host_api::SESSION_INVENTORY)
                && !(updates.is_some() && c.name == skiff_host_api::SESSION_UPDATES)
        }) {
            return Err(Error::MalformedContract {
                message: "local adapter cannot advertise capabilities without a source".into(),
            });
        }
        let limit = hello.limits.max_in_flight.min(MAX_PENDING);
        let slots = tokio::sync::Semaphore::new(limit);
        // Reserve one slot for status while Mission control fans out reads.
        let read_slots = tokio::sync::Semaphore::new(limit.saturating_sub(1).max(1));
        Self::negotiated(
            Backend::LocalReadOnly {
                source,
                screen,
                inventory,
                updates,
                read_slots,
                closed: watch::channel(false).0,
                slots,
            },
            hello,
            previous,
        )
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
        match &self.backend {
            Backend::Wire(engine) => {
                engine
                    .request(
                        request,
                        self.hello.limits.max_in_flight.min(MAX_PENDING),
                        self.hello.limits.max_frame_bytes.min(MAX_FRAME),
                    )
                    .await
            }
            Backend::LocalReadOnly {
                source,
                screen,
                inventory,
                read_slots,
                closed,
                slots,
                ..
            } => {
                let mut closing = closed.subscribe();
                if *closing.borrow() {
                    return Err(Error::TransportLost);
                }
                let operation: Pin<Box<dyn Future<Output = Reply> + Send + '_>> = match &request {
                    Request::ConnectionStatus => Box::pin(async {
                        let _slot = slots.try_acquire().map_err(|_| Error::HostBusy)?;
                        source.status().await
                    }),
                    Request::SessionScreen { session, rows } => {
                        let screen =
                            screen
                                .as_ref()
                                .ok_or_else(|| Error::UnsupportedCapability {
                                    capability: capability.clone(),
                                })?;
                        Box::pin(async move {
                            // Mission control fans out every visible screen. Queue
                            // inside the existing deadline instead of introducing
                            // HostBusy errors for boards with more than 64 panes.
                            let _read = read_slots
                                .acquire()
                                .await
                                .map_err(|_| Error::TransportLost)?;
                            let _slot = slots.acquire().await.map_err(|_| Error::TransportLost)?;
                            screen.screen(session.clone(), *rows).await
                        })
                    }
                    Request::SessionInventory => {
                        let inventory =
                            inventory
                                .as_ref()
                                .ok_or_else(|| Error::UnsupportedCapability {
                                    capability: capability.clone(),
                                })?;
                        Box::pin(async move {
                            let _read = read_slots
                                .acquire()
                                .await
                                .map_err(|_| Error::TransportLost)?;
                            let _slot = slots.acquire().await.map_err(|_| Error::TransportLost)?;
                            inventory.inventory().await
                        })
                    }
                    _ => return Err(Error::UnsupportedCapability { capability }),
                };
                let reply = tokio::select! {
                    biased;
                    _ = closing.changed() => return Err(Error::TransportLost),
                    reply = tokio::time::timeout(Duration::from_secs(20), operation) => reply.map_err(|_| Error::TimedOut)??,
                };
                if !request.accepts(&reply) {
                    return Err(Error::MalformedContract {
                        message: "reply does not match request".into(),
                    });
                }
                Ok(reply)
            }
        }
    }

    /// Listen before requesting Subscribe: a snapshot may precede its reply.
    /// Lagged means discard queued output and request a new subscription/snapshot.
    pub fn events(&self) -> Result<broadcast::Receiver<Event>, Error> {
        match &self.backend {
            Backend::Wire(engine) => {
                let s = engine.state.inner.lock().unwrap();
                s.events
                    .as_ref()
                    .map(|events| events.subscribe())
                    .ok_or_else(|| s.failure.clone().unwrap_or(Error::TransportLost))
            }
            Backend::LocalReadOnly { .. } => Err(Error::UnsupportedCapability {
                capability: skiff_host_api::Capability::sessions(),
            }),
        }
    }

    /// Session updates after this call, in order; take it before reading the
    /// inventory it amends. No backlog or uncertain update is replayed.
    /// Requires `session_updates@1` on both backends; never falls back.
    pub fn session_updates(&self) -> Result<SessionUpdates, Error> {
        self.hello.require(&Capability::session_updates())?;
        match &self.backend {
            Backend::Wire(engine) => {
                let s = engine.state.inner.lock().unwrap();
                s.events
                    .as_ref()
                    .map(|events| SessionUpdates(UpdatesInner::Wire(events.subscribe())))
                    .ok_or_else(|| s.failure.clone().unwrap_or(Error::TransportLost))
            }
            Backend::LocalReadOnly { updates, closed, .. } => {
                let updates = updates.as_ref().ok_or_else(|| Error::UnsupportedCapability {
                    capability: Capability::session_updates(),
                })?;
                let closed = closed.subscribe();
                if *closed.borrow() {
                    return Err(Error::TransportLost);
                }
                Ok(SessionUpdates(UpdatesInner::Local {
                    stream: updates.updates(),
                    closed,
                }))
            }
        }
    }

    pub fn disconnect(&self) {
        match &self.backend {
            Backend::Wire(engine) => {
                engine.state.fail(Error::TransportLost);
                for task in &engine.tasks {
                    task.abort();
                }
            }
            Backend::LocalReadOnly { closed, .. } => {
                closed.send_replace(true);
            }
        }
    }
}
