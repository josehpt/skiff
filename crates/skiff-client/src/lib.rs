//! Connects to `skiffd`, sends requests, and fans events out to subscribers.

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use anyhow::{anyhow, bail, Context, Result};
use skiff_core::{
    protocol::{Envelope, Event, Request, Response, ServerMessage},
    session::SessionId,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::{broadcast, mpsc, oneshot},
};

mod requests;
pub mod skipper;
pub use requests::{Coordinated, Coordinator, DaemonInfo, Refused};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>;
type Outputs = Arc<Mutex<HashMap<SessionId, OutputTx>>>;

/// Output chunks a session's receiver may hold before it counts as lagged.
const OUTPUT_QUEUE: usize = 4096;

struct OutputTx {
    tx: mpsc::Sender<Event>,
    lagged: Arc<AtomicBool>,
}

/// Output and snapshots for one session, from [`Client::output`].
pub struct Output {
    rx: mpsc::Receiver<Event>,
    lagged: Arc<AtomicBool>,
}

impl Output {
    /// The next [`Event::Output`] or [`Event::Snapshot`]. `Lagged` means
    /// chunks were dropped: subscribe again for a fresh snapshot. `Closed`
    /// means the connection is gone or a newer receiver replaced this one.
    pub async fn recv(&mut self) -> Result<Event, broadcast::error::RecvError> {
        if self.lagged.swap(false, Ordering::Relaxed) {
            return Err(broadcast::error::RecvError::Lagged(0));
        }
        self.rx.recv().await.ok_or(broadcast::error::RecvError::Closed)
    }

    /// Drops everything queued. After a lag, before subscribing again, so the
    /// new snapshot has room.
    pub fn clear(&mut self) {
        while self.rx.try_recv().is_ok() {}
        self.lagged.store(false, Ordering::Relaxed);
    }
}

/// Hands output to its session's receiver alone, without a copy per listener.
/// Never waits: a full queue marks the receiver lagged and drops the chunk.
fn route(outputs: &Outputs, e: Event) {
    let session = match &e {
        Event::Output { session, .. } | Event::Snapshot { session, .. } => session.clone(),
        _ => return,
    };
    let mut map = outputs.lock().unwrap();
    let Some(o) = map.get(&session) else { return };
    match o.tx.try_send(e) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => o.lagged.store(true, Ordering::Relaxed),
        Err(mpsc::error::TrySendError::Closed(_)) => {
            map.remove(&session);
        }
    }
}

/// A request with no reply after this long fails instead of hanging.
const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

pub struct Client {
    /// The daemon's pid, from the socket's peer credentials.
    pub daemon_pid: Option<i32>,
    /// Unbounded, so a keystroke is queued without a wait, in order.
    tx: mpsc::UnboundedSender<Vec<u8>>,
    next_id: AtomicU64,
    pending: Pending,
    /// Every event except output and snapshots, which go to [`Self::output`].
    events: broadcast::Sender<Event>,
    outputs: Outputs,
    /// Set when the daemon closes the connection.
    closed: Arc<AtomicBool>,
}

impl Client {
    /// Must be called from inside a tokio runtime.
    pub async fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .await
            .with_context(|| format!("connect {}", path.display()))?;
        let daemon_pid = stream.peer_cred().ok().and_then(|c| c.pid());
        let (rd, mut wr) = stream.into_split();

        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if wr.write_all(&line).await.is_err() {
                    break;
                }
            }
        });

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (events, _) = broadcast::channel(4096);
        let closed = Arc::new(AtomicBool::new(false));
        let outputs: Outputs = Arc::new(Mutex::new(HashMap::new()));
        {
            let pending = pending.clone();
            let outputs = outputs.clone();
            let events = events.clone();
            let closed = closed.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(rd).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    match serde_json::from_str::<ServerMessage>(&line) {
                        Ok(ServerMessage::Reply(r)) => {
                            if let Some(s) = pending.lock().unwrap().remove(&r.id) {
                                let _ = s.send(r.response);
                            }
                        }
                        Ok(ServerMessage::Event(e @ (Event::Output { .. } | Event::Snapshot { .. }))) => {
                            route(&outputs, e);
                        }
                        Ok(ServerMessage::Event(e)) => {
                            let _ = events.send(e);
                        }
                        // A newer daemon can send a reply or event this client does not know.
                        // Skip it, but fail the request it answers instead of waiting out the timeout.
                        Err(e) => {
                            tracing::warn!("bad message from skiffd: {e}");
                            let id = serde_json::from_str::<serde_json::Value>(&line)
                                .ok()
                                .and_then(|v| v.get("id").and_then(|i| i.as_u64()));
                            if let Some(s) = id.and_then(|id| pending.lock().unwrap().remove(&id)) {
                                let _ = s.send(Response::Error { message: format!("unreadable reply from skiffd: {e}") });
                            }
                        }
                    }
                }
                closed.store(true, Ordering::Relaxed);
                // Dropping the senders wakes every waiting request with an error.
                pending.lock().unwrap().clear();
                outputs.lock().unwrap().clear();
            });
        }

        Ok(Self {
            daemon_pid,
            tx,
            next_id: AtomicU64::new(1),
            pending,
            events,
            outputs,
            closed,
        })
    }

    /// True once the daemon has closed the connection. No round trip.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed) || self.tx.is_closed()
    }

    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub async fn request(&self, request: Request) -> Result<Response> {
        self.request_within(request, Some(REPLY_TIMEOUT)).await
    }

    /// [`Self::request`] with its own reply timeout. `None` waits until the
    /// daemon answers or closes the connection.
    pub(crate) async fn request_within(&self, request: Request, timeout: Option<std::time::Duration>) -> Result<Response> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (s, r) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, s);
        // After the reader cleared `pending`, nothing would answer. Checked
        // after the insert: the reader sets `closed` before it clears.
        if self.closed.load(Ordering::Relaxed) {
            self.pending.lock().unwrap().remove(&id);
            bail!("skiffd connection closed");
        }
        let mut line = serde_json::to_vec(&Envelope { id, request })?;
        line.push(b'\n');
        if self.tx.send(line).is_err() {
            self.pending.lock().unwrap().remove(&id);
            bail!("skiffd connection closed");
        }
        let Some(limit) = timeout else {
            return r.await.map_err(|_| anyhow!("skiffd connection closed"));
        };
        match tokio::time::timeout(limit, r).await {
            Ok(reply) => reply.map_err(|_| anyhow!("skiffd connection closed")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                bail!("skiffd did not answer within {}s", limit.as_secs())
            }
        }
    }

    async fn expect_ok(&self, request: Request) -> Result<()> {
        match self.request(request).await? {
            Response::Ok => Ok(()),
            Response::Error { message } => bail!(message),
            other => bail!("unexpected reply: {other:?}"),
        }
    }

    /// The receiver for one session's output. Take it before [`Self::subscribe`]:
    /// the snapshot can arrive before the reply. Replaces an earlier receiver.
    pub fn output(&self, session: &str) -> Output {
        let (tx, rx) = mpsc::channel(OUTPUT_QUEUE);
        let lagged = Arc::new(AtomicBool::new(false));
        let o = OutputTx { tx, lagged: lagged.clone() };
        self.outputs.lock().unwrap().insert(session.to_string(), o);
        Output { rx, lagged }
    }

}
