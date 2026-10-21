//! M1a/M1b local connection ownership and read-only screen snapshots. No Host service and no new skiffd messages.
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{bail, Context};
use skiff_host_api::{
    Capability, Endpoint, Error, Hello, Host, HostId, InstanceId, Limits, Request, Response,
    API_MAJOR,
};
use uuid::Uuid;

use super::{HostConnection, InventorySource, ScreenSource, StatusSource};
use crate::Client;

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

    /// No identity write or new handshake before existing compatibility policy
    /// accepts the legacy client. Even probes have this selected endpoint.
    pub async fn connect(&self) -> anyhow::Result<PendingLocalConnection> {
        Ok(PendingLocalConnection {
            selection: self.clone(),
            client: Arc::new(Client::connect(self.socket()).await?),
        })
    }
}

/// Only LocalSelection can construct a pending connection; activation cannot
/// accidentally pair a selected Host with a client from another endpoint.
pub struct PendingLocalConnection {
    selection: LocalSelection,
    client: Arc<Client>,
}
impl PendingLocalConnection {
    pub fn client(&self) -> &Arc<Client> {
        &self.client
    }

    pub async fn activate(self) -> anyhow::Result<SelectedLocalHost> {
        let path = self.selection.0.identity_file.clone();
        let host_id = tokio::task::spawn_blocking(move || load_identity(&path)).await??;
        let hello = Hello {
            host: Host {
                host_id,
                os: std::env::consts::OS.into(),
                // Numeric local account identity, not a user-controlled label.
                account: format!("uid:{}", unsafe { libc::geteuid() }),
            },
            // This is the in-process adapter generation, NOT skiffd's PID or
            // an assertion that a legacy daemon supplies durable identity.
            instance_id: InstanceId::try_from(Uuid::new_v4().to_string()).unwrap(),
            service_versions: BTreeMap::from([(
                "local-status-adapter".into(),
                env!("CARGO_PKG_VERSION").into(),
            )]),
            api_major: API_MAJOR,
            capabilities: vec![
                Capability::connection_status(),
                Capability::session_screen(),
                Capability::session_inventory(),
            ],
            limits: Limits {
                max_frame_bytes: 8 * 1024 * 1024,
                max_in_flight: 64,
            },
        };
        let connection = {
            let mut previous = self.selection.0.previous.lock().unwrap();
            let source = Arc::new(LegacyReadOnly(self.client.clone()));
            let connection = HostConnection::local_inventory(
                source.clone(),
                source.clone(),
                source,
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
        })
    }
}

/// Desktop runtime owner. Existing terminal/data paths borrow the very same
/// Client until their own slices migrate; they never obtain a second one here.
pub struct SelectedLocalHost {
    selection: LocalSelection,
    client: Arc<Client>,
    connection: Arc<HostConnection>,
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
}
impl Drop for SelectedLocalHost {
    fn drop(&mut self) {
        self.connection.disconnect();
    }
}

struct LegacyReadOnly(Arc<Client>);
impl StatusSource for LegacyReadOnly {
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

/// Additive bootstrap metadata beside config, never edits config/daemon state.
/// Publish complete bytes with a no-clobber hard link: concurrent app launches
/// agree on one UUID, and a crash cannot leave a partially written identity.
fn load_identity(path: &Path) -> anyhow::Result<HostId> {
    match read_identity(path) {
        Ok(id) => return Ok(id),
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
        Err(e) => return Err(e),
    }
    let parent = path.parent().context("local Host identity has no parent")?;
    // A bare SKIFF_CONFIG filename yields an empty parent, which is valid
    // for path joins but not for opening the directory to sync publication.
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    fs::create_dir_all(parent)?;
    let uuid = Uuid::new_v4();
    let temp = parent.join(format!(".local-host-id-{uuid}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let result = (|| {
        writeln!(file, "{uuid}")?;
        file.sync_all()?;
        match fs::hard_link(&temp, path) {
            Ok(()) => {
                std::fs::File::open(parent)?.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        read_identity(path)
    })();
    let _ = fs::remove_file(temp);
    result
}

fn read_identity(path: &Path) -> anyhow::Result<HostId> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("read local Host identity {}", path.display()))?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.len() > 128
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
    {
        bail!("local Host identity must be a private regular file owned by this account");
    }
    let mut text = String::new();
    file.take(128).read_to_string(&mut text)?;
    let id = Uuid::parse_str(text.trim())
        .context("invalid local Host identity; refusing to replace it")?;
    HostId::try_from(id.to_string()).map_err(anyhow::Error::msg)
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
