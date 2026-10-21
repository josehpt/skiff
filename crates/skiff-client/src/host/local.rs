//! M1a–M1g local connection ownership: status, screen, inventory, session
//! updates, the session terminal, session lifecycle and groups, and the Host
//! identity skiffd reports in `pong`. No Host service and no new skiffd requests.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use skiff_host_api::{
    Capability, Endpoint, Error, Hello, Host, HostId, InstanceId, Limits, Request, Response,
    API_MAJOR,
};
use uuid::Uuid;

use super::{
    GroupSource, HostConnection, InventorySource, LifecycleSource, LocalSources, OutputGap,
    OutputStream, ScreenSource, SessionUpdates, StatusSource, TerminalOutput, TerminalSource,
    UpdateGap, UpdateSource, UpdateStream,
};
use crate::{Client, DaemonInfo};

struct Selection {
    socket: PathBuf,
    identity_file: PathBuf,
    previous: Mutex<Option<Hello>>,
}

/// Exactly one local endpoint, fixed for this app runtime. It never starts,
/// reloads or kills a daemon; the existing desktop policy still owns that.
#[derive(Clone)]
pub struct LocalSelection(Arc<Selection>);
impl LocalSelection {
    pub fn new(socket: PathBuf, identity_file: PathBuf) -> Self {
        Self(Arc::new(Selection {
            socket,
            identity_file,
            previous: Mutex::new(None),
        }))
    }

    pub fn socket(&self) -> &Path {
        &self.0.socket
    }
    pub fn endpoint(&self) -> Endpoint {
        Endpoint::Local {
            socket: self.0.socket.to_string_lossy().into_owned(),
        }
    }

    /// Permission to start, signal or reload this endpoint's daemon, from
    /// the endpoint itself. See [`ProcessControl`].
    pub fn process_control(&self) -> Option<ProcessControl> {
        ProcessControl::for_endpoint(&self.endpoint())
    }

    /// No identity write or new handshake before existing compatibility policy
    /// accepts the legacy client. Even probes have this selected endpoint.
    pub async fn connect(&self) -> anyhow::Result<PendingLocalConnection> {
        Ok(PendingLocalConnection {
            selection: self.clone(),
            client: Arc::new(Client::connect(self.socket()).await?),
            info: None,
        })
    }
}

/// Only LocalSelection can construct a pending connection; activation cannot
/// accidentally pair a selected Host with a client from another endpoint.
pub struct PendingLocalConnection {
    selection: LocalSelection,
    client: Arc<Client>,
    info: Option<DaemonInfo>,
}
impl PendingLocalConnection {
    pub fn client(&self) -> &Arc<Client> {
        &self.client
    }

    /// The daemon's `pong` on this connection. Activation uses the last one,
    /// so checking the daemon first costs no second Ping.
    pub async fn daemon_info(&mut self) -> anyhow::Result<DaemonInfo> {
        let info = self.client.daemon_info().await?;
        self.info = Some(info.clone());
        Ok(info)
    }

    /// Takes the Host's identity from the `pong` that [`Self::daemon_info`]
    /// recorded; activation itself sends nothing. With no identity there (an
    /// older skiffd, or no recorded pong) it uses the app's bootstrap id and
    /// an adapter generation for the instance, so every reconnect is
    /// NewInstance. Never a pid or peer credentials.
    pub async fn activate(self) -> anyhow::Result<SelectedLocalHost> {
        let (version, identity) = match self.info {
            Some(info) => (Some(info.version), info.identity),
            None => (None, None),
        };
        let malformed = |message: &str| Error::MalformedContract {
            message: message.into(),
        };
        let mut service_versions = BTreeMap::from([(
            "local-status-adapter".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        )]);
        service_versions.extend(version.map(|v| ("skiffd".to_string(), v)));
        let (host, instance_id, source) = match identity {
            Some(identity) => (
                Host {
                    host_id: HostId::try_from(identity.host_id)
                        .map_err(|_| malformed("blank host id from skiffd"))?,
                    os: identity.os,
                    account: identity.account,
                },
                InstanceId::try_from(identity.instance_id)
                    .map_err(|_| malformed("blank instance id from skiffd"))?,
                IdentitySource::Host,
            ),
            None => {
                let path = self.selection.0.identity_file.clone();
                let host_id =
                    tokio::task::spawn_blocking(move || skiff_core::host_identity::load(&path))
                        .await??;
                (
                    Host {
                        host_id: HostId::try_from(host_id).map_err(anyhow::Error::msg)?,
                        os: std::env::consts::OS.into(),
                        account: skiff_core::host_identity::account(),
                    },
                    // The in-process adapter generation, NOT skiffd's PID or
                    // an assertion that a legacy daemon supplies identity.
                    InstanceId::try_from(Uuid::new_v4().to_string()).unwrap(),
                    IdentitySource::LocalBootstrap,
                )
            }
        };
        let hello = Hello {
            host,
            instance_id,
            service_versions,
            api_major: API_MAJOR,
            capabilities: vec![
                Capability::connection_status(),
                Capability::session_screen(),
                Capability::session_inventory(),
                Capability::session_updates(),
                Capability::session_terminal(),
                Capability::session_lifecycle(),
                Capability::session_groups(),
            ],
            limits: Limits {
                max_frame_bytes: 8 * 1024 * 1024,
                max_in_flight: 64,
            },
        };
        let connection = {
            let mut previous = self.selection.0.previous.lock().unwrap();
            let source = Arc::new(LegacyReadOnly(self.client.clone()));
            let connection = HostConnection::local(
                LocalSources {
                    status: source.clone(),
                    screen: Some(source.clone()),
                    inventory: Some(source.clone()),
                    updates: Some(source),
                    terminal: Some(Arc::new(LegacyTerminal(self.client.clone()))),
                    lifecycle: Some(Arc::new(LegacySessions(self.client.clone()))),
                    groups: Some(Arc::new(LegacySessions(self.client.clone()))),
                },
                hello,
                previous.as_ref(),
            )?;
            *previous = Some(connection.hello().clone());
            Arc::new(connection)
        };
        Ok(SelectedLocalHost {
            selection: self.selection,
            client: self.client,
            connection,
            source,
        })
    }
}

/// Desktop runtime owner. Existing terminal/data paths borrow the very same
/// Client until their own slices migrate; they never obtain a second one here.
pub struct SelectedLocalHost {
    selection: LocalSelection,
    client: Arc<Client>,
    connection: Arc<HostConnection>,
    source: IdentitySource,
}
impl SelectedLocalHost {
    pub fn client(&self) -> Arc<Client> {
        self.client.clone()
    }
    pub fn connection(&self) -> Arc<HostConnection> {
        self.connection.clone()
    }
    pub fn endpoint(&self) -> Endpoint {
        self.selection.endpoint()
    }
    pub fn process_control(&self) -> Option<ProcessControl> {
        self.selection.process_control()
    }
    pub fn identity_source(&self) -> IdentitySource {
        self.source
    }
}

/// Where the Hello's identity came from. A remote Host must be [`Self::Host`]:
/// only the serving Host can say who it is and which run answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySource {
    /// skiffd's `pong`: its Host id, and an instance id new on every start
    /// and reload, so a reconnect can be SameInstance.
    Host,
    /// An older skiffd: the app's bootstrap id and an adapter generation.
    /// Every reconnect is NewInstance.
    LocalBootstrap,
}
impl Drop for SelectedLocalHost {
    fn drop(&mut self) {
        self.connection.disconnect();
    }
}

/// Proof that the selected endpoint is local, so the app may start, signal,
/// reload or replace its daemon, link its CLI or manage its watcher. Only
/// [`Self::for_endpoint`] makes one, and it refuses every non-local endpoint:
/// a remote Host is reached, never managed. This is endpoint policy, not
/// daemon identity: peer credentials still only name the local socket's peer.
#[derive(Debug, Clone)]
pub struct ProcessControl(());
impl ProcessControl {
    pub fn for_endpoint(endpoint: &Endpoint) -> Option<Self> {
        endpoint.is_local().then_some(Self(()))
    }
}

struct LegacyReadOnly(Arc<Client>);
impl StatusSource for LegacyReadOnly {
    fn is_lost(&self) -> bool {
        self.0.is_closed()
    }

    fn status(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Error>> + Send + '_>>
    {
        Box::pin(async {
            match self.0.hello().await {
                Ok((version, protocol)) => Ok(Response::ConnectionStatus { version, protocol }),
                Err(e) if e.downcast_ref::<crate::ReplyTimeout>().is_some() => Err(Error::TimedOut),
                Err(_) if self.0.is_closed() => Err(Error::TransportLost),
                Err(e) => Err(Error::MalformedContract {
                    message: format!("{e:#}"),
                }),
            }
        })
    }
}

impl ScreenSource for LegacyReadOnly {
    fn screen(
        &self,
        session: String,
        rows: u16,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Error>> + Send + '_>>
    {
        Box::pin(async move {
            // Preserve the legacy request/reply mapping exactly; no new Host
            // envelope is sent to skiffd and no terminal subscription is taken.
            match self
                .0
                .request_decoded(skiff_core::protocol::Request::Screen { session, rows })
                .await
            {
                Ok(skiff_core::protocol::Response::Screen { lines }) => {
                    Ok(Response::SessionScreen { lines })
                }
                Ok(skiff_core::protocol::Response::Error { message }) => {
                    Err(Error::OperationFailed { message })
                }
                Ok(other) => Err(Error::MalformedContract {
                    message: format!("unexpected reply: {other:?}"),
                }),
                Err(e) if e.downcast_ref::<crate::ReplyTimeout>().is_some() => Err(Error::TimedOut),
                Err(e) if e.downcast_ref::<crate::ReplyDecodeError>().is_some() => {
                    Err(Error::MalformedContract {
                        message: e.to_string(),
                    })
                }
                Err(_) if self.0.is_closed() => Err(Error::TransportLost),
                Err(e) => Err(Error::MalformedContract {
                    message: format!("{e:#}"),
                }),
            }
        })
    }
}

impl InventorySource for LegacyReadOnly {
    fn inventory(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Error>> + Send + '_>>
    {
        Box::pin(async {
            match self
                .0
                .request_decoded(skiff_core::protocol::Request::ListSessions)
                .await
            {
                Ok(skiff_core::protocol::Response::Sessions { sessions }) => {
                    let sessions = sessions
                        .into_iter()
                        .map(super::inventory::project_session)
                        .collect::<Result<_, _>>()?;
                    Ok(Response::SessionInventory { sessions })
                }
                Ok(skiff_core::protocol::Response::Error { message }) => {
                    Err(Error::OperationFailed { message })
                }
                Ok(other) => Err(Error::MalformedContract {
                    message: format!("unexpected reply: {other:?}"),
                }),
                Err(e) if e.downcast_ref::<crate::ReplyTimeout>().is_some() => Err(Error::TimedOut),
                Err(e) if e.downcast_ref::<crate::ReplyDecodeError>().is_some() => {
                    Err(Error::MalformedContract {
                        message: e.to_string(),
                    })
                }
                Err(_) if self.0.is_closed() => Err(Error::TransportLost),
                Err(e) => Err(Error::MalformedContract {
                    message: format!("{e:#}"),
                }),
            }
        })
    }
}

impl UpdateSource for LegacyReadOnly {
    fn updates(&self) -> Box<dyn UpdateStream> {
        // The same broadcast the desktop read directly before M1d: the
        // Client already routes output elsewhere and skips unreadable frames.
        Box::new(LegacyUpdateStream(self.0.events()))
    }
}

struct LegacyUpdateStream(tokio::sync::broadcast::Receiver<skiff_core::protocol::Event>);
impl UpdateStream for LegacyUpdateStream {
    fn recv(
        &mut self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<skiff_host_api::SessionUpdate, UpdateGap>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async {
            use tokio::sync::broadcast::error::RecvError;
            match self.0.recv().await {
                Ok(event) => super::updates::project_update(event).map_err(UpdateGap::Malformed),
                Err(RecvError::Lagged(n)) => Err(UpdateGap::Lagged(n)),
                Err(RecvError::Closed) => Err(UpdateGap::Closed),
            }
        })
    }
}

/// Temporary desktop compatibility projection of [`SessionUpdates`]: the
/// existing page events, in order. As before M1d, missed updates are skipped
/// without replay, so the page's own refresh policy is unchanged.
pub struct LegacyUpdates(SessionUpdates);
impl LegacyUpdates {
    /// The next event, or None once the connection is gone.
    pub async fn recv(&mut self) -> Option<skiff_core::protocol::Event> {
        loop {
            match self.0.recv().await {
                Ok(update) => return Some(super::updates::restore_update(update)),
                Err(UpdateGap::Lagged(_)) => {}
                Err(UpdateGap::Malformed(e)) => tracing::warn!("skipped session update: {e}"),
                Err(UpdateGap::Closed) => return None,
            }
        }
    }
}

/// Capability/version failures are explicit, never a raw Client fallback.
pub fn legacy_updates(connection: &HostConnection) -> anyhow::Result<LegacyUpdates> {
    Ok(LegacyUpdates(connection.session_updates()?))
}

/// Preserve the existing Tauri list schema until a deliberate GUI type migration.
pub async fn legacy_inventory(
    connection: &HostConnection,
) -> anyhow::Result<Vec<skiff_core::session::SessionInfo>> {
    match connection
        .request(Request::SessionInventory)
        .await
        .map_err(legacy_read_error)?
    {
        Response::SessionInventory { sessions } => Ok(sessions
            .into_iter()
            .map(super::inventory::restore_session)
            .collect()),
        _ => Err(Error::MalformedContract {
            message: "expected session inventory".into(),
        }
        .into()),
    }
}

fn legacy_read_error(error: Error) -> anyhow::Error {
    match error {
        Error::OperationFailed { message } | Error::MalformedContract { message } => {
            anyhow::Error::msg(message)
        }
        Error::TimedOut => crate::ReplyTimeout(std::time::Duration::from_secs(20)).into(),
        Error::TransportLost => anyhow::Error::msg("skiffd connection closed"),
        other => other.into(),
    }
}

/// Temporary desktop compatibility projection. The Host request remains typed;
/// only its existing Tauri caller receives the unchanged legacy error text.
/// Capability/version failures propagate explicitly, never via legacy fallback.
pub async fn legacy_screen(
    connection: &HostConnection,
    session: &str,
    rows: u16,
) -> anyhow::Result<Vec<String>> {
    let reply = connection
        .request(Request::SessionScreen {
            session: session.into(),
            rows,
        })
        .await
        .map_err(legacy_read_error)?;
    match reply {
        Response::SessionScreen { lines } => Ok(lines),
        _ => Err(Error::MalformedContract {
            message: "expected session screen".into(),
        }
        .into()),
    }
}

/// The M1e terminal plane: the very same skiffd frames the desktop sent
/// before (subscribe, unsubscribe, resize, write), on the selected Client,
/// and its per-session output queue with lag marking. No Host envelope.
struct LegacyTerminal(Arc<Client>);
impl LegacyTerminal {
    async fn expect_ok(&self, request: skiff_core::protocol::Request) -> Result<Response, Error> {
        match self.0.request_decoded(request).await {
            Ok(skiff_core::protocol::Response::Ok) => Ok(Response::Ok),
            Ok(skiff_core::protocol::Response::Error { message }) => {
                Err(Error::OperationFailed { message })
            }
            Ok(other) => Err(Error::MalformedContract {
                message: format!("unexpected reply: {other:?}"),
            }),
            Err(e) => Err(legacy_transport_error(&self.0, e)),
        }
    }
}
type Op<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Error>> + Send + 'a>>;
impl TerminalSource for LegacyTerminal {
    fn output(&self, session: &str) -> Box<dyn OutputStream> {
        Box::new(LegacyOutput(self.0.output(session)))
    }
    fn attach(&self, session: String) -> Op<'_> {
        Box::pin(self.expect_ok(skiff_core::protocol::Request::Subscribe { session }))
    }
    fn detach(&self, session: String) -> Op<'_> {
        // As Client::unsubscribe: the receiver closes before the request.
        self.0.outputs.lock().unwrap().remove(&session);
        Box::pin(self.expect_ok(skiff_core::protocol::Request::Unsubscribe { session }))
    }
    fn resize(&self, session: String, cols: u16, rows: u16) -> Op<'_> {
        Box::pin(self.expect_ok(skiff_core::protocol::Request::Resize {
            session,
            cols,
            rows,
        }))
    }
    fn input(&self, session: &str, data: Vec<u8>, paste: bool) -> Result<(), Error> {
        // The unbounded legacy queue: a keystroke never waits, in call order.
        self.0
            .input_now(session, data, paste)
            .map_err(|_| Error::TransportLost)
    }
}

struct LegacyOutput(crate::Output);
impl OutputStream for LegacyOutput {
    fn recv(
        &mut self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<skiff_host_api::TerminalFrame, OutputGap>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async {
            use skiff_core::protocol::Event;
            use skiff_host_api::{EchoTimes, TerminalFrame};
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match self.0.recv().await {
                    Ok(Event::Snapshot {
                        data, cols, rows, ..
                    }) => return Ok(TerminalFrame::Snapshot { data, cols, rows }),
                    Ok(Event::Output { data, echo, .. }) => {
                        return Ok(TerminalFrame::Output {
                            data,
                            echo: echo.map(|e| EchoTimes {
                                wrote: e.wrote,
                                read: e.read,
                            }),
                        })
                    }
                    // The Client routes only output and snapshots here.
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => return Err(OutputGap::Lagged),
                    Err(RecvError::Closed) => return Err(OutputGap::Closed),
                }
            }
        })
    }
    fn clear(&mut self) {
        self.0.clear();
    }
}

fn legacy_transport_error(client: &Client, e: anyhow::Error) -> Error {
    if e.downcast_ref::<crate::ReplyTimeout>().is_some() {
        Error::TimedOut
    } else if e.downcast_ref::<crate::ReplyDecodeError>().is_some() {
        Error::MalformedContract {
            message: e.to_string(),
        }
    } else if client.is_closed() {
        Error::TransportLost
    } else {
        Error::MalformedContract {
            message: format!("{e:#}"),
        }
    }
}

/// Temporary desktop compatibility projection of the terminal family: typed
/// Host requests, with the existing Tauri error text. Capability/version
/// failures propagate explicitly, never via a raw Client fallback.
pub async fn legacy_attach(connection: &HostConnection, session: &str) -> anyhow::Result<()> {
    legacy_terminal_request(
        connection,
        Request::TerminalAttach {
            session: session.into(),
        },
    )
    .await
}
pub async fn legacy_detach(connection: &HostConnection, session: &str) -> anyhow::Result<()> {
    legacy_terminal_request(
        connection,
        Request::TerminalDetach {
            session: session.into(),
        },
    )
    .await
}
pub async fn legacy_resize(
    connection: &HostConnection,
    session: &str,
    cols: u16,
    rows: u16,
) -> anyhow::Result<()> {
    legacy_terminal_request(
        connection,
        Request::TerminalResize {
            session: session.into(),
            cols,
            rows,
        },
    )
    .await
}
async fn legacy_terminal_request(
    connection: &HostConnection,
    request: Request,
) -> anyhow::Result<()> {
    match connection
        .request(request)
        .await
        .map_err(legacy_read_error)?
    {
        Response::Ok => Ok(()),
        other => Err(anyhow::anyhow!("unexpected reply: {other:?}")),
    }
}
/// Keystrokes and pastes, queued without a wait; the old write_now text on loss.
pub fn legacy_input(
    connection: &HostConnection,
    session: &str,
    data: Vec<u8>,
) -> anyhow::Result<()> {
    connection
        .terminal_input(session, data, false)
        .map_err(legacy_read_error)
}
/// The session's terminal receiver; take it before [`legacy_attach`].
pub fn legacy_output(connection: &HostConnection, session: &str) -> anyhow::Result<TerminalOutput> {
    Ok(connection.terminal_output(session)?)
}

/// Match the legacy desktop status tuple without exposing core types in the
/// Host contract. A capability/version error never falls back to Client::hello.
pub async fn connection_status(connection: &HostConnection) -> Result<(String, u32), Error> {
    match connection.request(Request::ConnectionStatus).await? {
        Response::ConnectionStatus { version, protocol } => Ok((version, protocol)),
        _ => Err(Error::MalformedContract {
            message: "expected connection status".into(),
        }),
    }
}

/// M1g: session lifecycle and groups. The very same skiffd requests the
/// desktop sent before (`create_session`, `kill`, `rename_session`,
/// `set_session_theme`, `seen`, `list_groups`, `save_group`, `delete_group`)
/// on the selected Client. No Host envelope, no retry.
struct LegacySessions(Arc<Client>);
impl LegacySessions {
    async fn call(
        &self,
        request: skiff_core::protocol::Request,
    ) -> Result<skiff_core::protocol::Response, Error> {
        match self.0.request_decoded(request).await {
            Ok(skiff_core::protocol::Response::Error { message }) => {
                Err(Error::OperationFailed { message })
            }
            Ok(reply) => Ok(reply),
            Err(e) => Err(legacy_transport_error(&self.0, e)),
        }
    }

    async fn row(&self, request: skiff_core::protocol::Request) -> Result<Response, Error> {
        match self.call(request).await? {
            skiff_core::protocol::Response::Session { session } => Ok(Response::Session {
                session: Box::new(super::inventory::project_session(session)?),
            }),
            other => Err(unexpected(other)),
        }
    }

    async fn ok(&self, request: skiff_core::protocol::Request) -> Result<Response, Error> {
        match self.call(request).await? {
            skiff_core::protocol::Response::Ok => Ok(Response::Ok),
            other => Err(unexpected(other)),
        }
    }
}

fn unexpected(reply: skiff_core::protocol::Response) -> Error {
    Error::MalformedContract {
        message: format!("unexpected reply: {reply:?}"),
    }
}

impl LifecycleSource for LegacySessions {
    fn lifecycle(&self, request: Request) -> Op<'_> {
        use skiff_core::protocol::Request as Legacy;
        Box::pin(async move {
            match request {
                Request::SessionCreate { spec } => {
                    self.row(Legacy::CreateSession {
                        spec: super::lifecycle::restore_spec(spec),
                    })
                    .await
                }
                Request::SessionKill { session } => self.ok(Legacy::Kill { session }).await,
                Request::SessionRename { session, name } => {
                    self.row(Legacy::RenameSession { session, name }).await
                }
                Request::SessionSetTheme { session, theme } => {
                    self.row(Legacy::SetSessionTheme { session, theme }).await
                }
                Request::SessionSeen { session } => self.ok(Legacy::Seen { session }).await,
                _ => Err(Error::MalformedContract {
                    message: "not a session lifecycle request".into(),
                }),
            }
        })
    }
}

impl GroupSource for LegacySessions {
    fn groups(&self, request: Request) -> Op<'_> {
        use skiff_core::protocol::{Request as Legacy, Response as LegacyReply};
        Box::pin(async move {
            match request {
                Request::GroupList => match self.call(Legacy::ListGroups).await? {
                    LegacyReply::Groups { groups } => Ok(Response::Groups {
                        groups: groups
                            .into_iter()
                            .map(super::lifecycle::project_group)
                            .collect(),
                    }),
                    other => Err(unexpected(other)),
                },
                Request::GroupSave { group } => match self
                    .call(Legacy::SaveGroup {
                        group: super::lifecycle::restore_group(group),
                    })
                    .await?
                {
                    LegacyReply::Group { group } => Ok(Response::Group {
                        group: super::lifecycle::project_group(group),
                    }),
                    other => Err(unexpected(other)),
                },
                Request::GroupDelete { group } => self.ok(Legacy::DeleteGroup { group }).await,
                _ => Err(Error::MalformedContract {
                    message: "not a session group request".into(),
                }),
            }
        })
    }
}

/// Temporary desktop compatibility projections of the lifecycle and group
/// families: typed Host requests, the existing Tauri results and error text.
/// Capability/version failures propagate explicitly, never via a raw Client.
async fn legacy_request(connection: &HostConnection, request: Request) -> anyhow::Result<Response> {
    connection.request(request).await.map_err(legacy_read_error)
}

fn legacy_row(reply: Response) -> anyhow::Result<skiff_core::session::SessionInfo> {
    match reply {
        Response::Session { session } => Ok(super::inventory::restore_session(*session)),
        _ => Err(Error::MalformedContract {
            message: "expected a session".into(),
        }
        .into()),
    }
}

pub async fn legacy_create(
    connection: &HostConnection,
    spec: skiff_core::session::SessionSpec,
) -> anyhow::Result<skiff_core::session::SessionInfo> {
    let spec = super::lifecycle::project_spec(spec).map_err(legacy_read_error)?;
    legacy_row(legacy_request(connection, Request::SessionCreate { spec }).await?)
}

pub async fn legacy_kill(connection: &HostConnection, session: &str) -> anyhow::Result<()> {
    legacy_request(
        connection,
        Request::SessionKill {
            session: session.into(),
        },
    )
    .await
    .map(drop)
}

pub async fn legacy_rename(
    connection: &HostConnection,
    session: &str,
    name: &str,
) -> anyhow::Result<skiff_core::session::SessionInfo> {
    legacy_row(
        legacy_request(
            connection,
            Request::SessionRename {
                session: session.into(),
                name: name.into(),
            },
        )
        .await?,
    )
}

pub async fn legacy_set_theme(
    connection: &HostConnection,
    session: &str,
    theme: Option<String>,
) -> anyhow::Result<skiff_core::session::SessionInfo> {
    legacy_row(
        legacy_request(
            connection,
            Request::SessionSetTheme {
                session: session.into(),
                theme,
            },
        )
        .await?,
    )
}

pub async fn legacy_seen(connection: &HostConnection, session: &str) -> anyhow::Result<()> {
    legacy_request(
        connection,
        Request::SessionSeen {
            session: session.into(),
        },
    )
    .await
    .map(drop)
}

pub async fn legacy_groups(
    connection: &HostConnection,
) -> anyhow::Result<Vec<skiff_core::group::Group>> {
    match legacy_request(connection, Request::GroupList).await? {
        Response::Groups { groups } => Ok(groups
            .into_iter()
            .map(super::lifecycle::restore_group)
            .collect()),
        _ => Err(Error::MalformedContract {
            message: "expected groups".into(),
        }
        .into()),
    }
}

pub async fn legacy_save_group(
    connection: &HostConnection,
    group: skiff_core::group::Group,
) -> anyhow::Result<skiff_core::group::Group> {
    let group = super::lifecycle::project_group(group);
    match legacy_request(connection, Request::GroupSave { group }).await? {
        Response::Group { group } => Ok(super::lifecycle::restore_group(group)),
        _ => Err(Error::MalformedContract {
            message: "expected a group".into(),
        }
        .into()),
    }
}

pub async fn legacy_delete_group(connection: &HostConnection, group: &str) -> anyhow::Result<()> {
    legacy_request(
        connection,
        Request::GroupDelete {
            group: group.into(),
        },
    )
    .await
    .map(drop)
}
