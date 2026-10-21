//! M0 Host contract. No I/O, service implementation, application state or core types.
//! This contract is not sent by the existing desktop/CLI connection.
use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};

pub mod inventory;
pub use inventory::InventorySession;

pub const API_MAJOR: u32 = 1;
pub const SESSIONS: &str = "sessions";
pub const CONNECTION_STATUS: &str = "connection_status";
pub const SESSION_SCREEN: &str = "session_screen";
pub const SESSION_INVENTORY: &str = "session_inventory";

macro_rules! identity {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl TryFrom<String> for $name {
            type Error = String;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                if value.trim().is_empty() {
                    return Err(concat!(stringify!($name), " must not be empty").into());
                }
                Ok(Self(value))
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}
identity!(HostId);
identity!(InstanceId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub host_id: HostId,
    pub os: String,
    pub account: String,
}

/// A locator, never identity or an instruction to launch a process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Endpoint {
    Local {
        socket: String,
    },
    /// Descriptor only; M0 has no SSH implementation.
    Ssh {
        hostname: String,
        account: String,
        port: u16,
        socket: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    /// Feature major; unrelated to API_MAJOR and skiffd's PROTOCOL.
    pub version: u32,
}
impl Capability {
    pub fn session_inventory() -> Self {
        Self {
            name: SESSION_INVENTORY.into(),
            version: 1,
        }
    }

    /// Read-only text snapshot; does not grant session lifecycle or events.
    pub fn session_screen() -> Self {
        Self {
            name: SESSION_SCREEN.into(),
            version: 1,
        }
    }
    pub fn connection_status() -> Self {
        Self {
            name: CONNECTION_STATUS.into(),
            version: 1,
        }
    }
    pub fn sessions() -> Self {
        Self {
            name: SESSIONS.into(),
            version: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub max_frame_bytes: usize,
    pub max_in_flight: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    #[serde(flatten)]
    pub host: Host,
    pub instance_id: InstanceId,
    pub service_versions: BTreeMap<String, String>,
    pub api_major: u32,
    pub capabilities: Vec<Capability>,
    pub limits: Limits,
}
impl Hello {
    pub fn validate(&self) -> Result<(), Error> {
        if self.api_major != API_MAJOR {
            return Err(Error::IncompatibleVersion {
                supported: API_MAJOR,
                received: self.api_major,
            });
        }
        if self.host.os.is_empty()
            || self.host.account.is_empty()
            || self.limits.max_frame_bytes == 0
            || self.limits.max_in_flight == 0
            || self
                .capabilities
                .iter()
                .any(|c| c.name.is_empty() || c.version == 0)
            || self
                .capabilities
                .iter()
                .enumerate()
                .any(|(i, c)| self.capabilities[..i].contains(c))
        {
            return Err(Error::MalformedContract {
                message: "invalid Hello metadata or limits".into(),
            });
        }
        Ok(())
    }

    pub fn require(&self, capability: &Capability) -> Result<(), Error> {
        if self.capabilities.contains(capability) {
            Ok(())
        } else {
            Err(Error::UnsupportedCapability {
                capability: capability.clone(),
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Error {
    UnsupportedCapability {
        capability: Capability,
    },
    IncompatibleVersion {
        supported: u32,
        received: u32,
    },
    MalformedContract {
        message: String,
    },
    /// Legacy service rejection without a structured cause; never guess a
    /// not-found/permission code from message text.
    OperationFailed {
        message: String,
    },
    NotFound {
        message: String,
    },
    Conflict {
        message: String,
    },
    Permission {
        message: String,
    },
    HostBusy,
    TransportLost,
    Cancelled,
    TimedOut,
    IdentityChanged {
        expected: HostId,
        received: HostId,
    },
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// M0 session projection only. No config, filesystem, ledger or core re-exports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub cwd: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Hello,
    ConnectionStatus,
    SessionInventory,
    SessionScreen {
        session: String,
        rows: u16,
    },
    ListSessions,
    Write {
        session: String,
        #[serde(with = "bytes")]
        data: Vec<u8>,
        #[serde(default)]
        paste: bool,
    },
    Resize {
        session: String,
        cols: u16,
        rows: u16,
    },
    Subscribe {
        session: String,
    },
    Unsubscribe {
        session: String,
    },
}
impl Request {
    pub fn capability(&self) -> Option<Capability> {
        match self {
            Self::Hello => None,
            Self::ConnectionStatus => Some(Capability::connection_status()),
            Self::SessionInventory => Some(Capability::session_inventory()),
            Self::SessionScreen { .. } => Some(Capability::session_screen()),
            _ => Some(Capability::sessions()),
        }
    }
    pub fn accepts(&self, reply: &Response) -> bool {
        matches!(
            (self, reply),
            (Self::Hello, Response::Hello { .. })
                | (Self::ListSessions, Response::Sessions { .. })
                | (Self::ConnectionStatus, Response::ConnectionStatus { .. })
                | (Self::SessionScreen { .. }, Response::SessionScreen { .. })
                | (Self::SessionInventory, Response::SessionInventory { .. })
        ) || (!matches!(
            self,
            Self::Hello
                | Self::ListSessions
                | Self::ConnectionStatus
                | Self::SessionScreen { .. }
                | Self::SessionInventory
        ) && matches!(reply, Response::Ok))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: u64,
    pub request: Request,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    SessionInventory { sessions: Vec<InventorySession> },
    SessionScreen { lines: Vec<String> },
    ConnectionStatus { version: String, protocol: u32 },
    Hello { hello: Hello },
    Sessions { sessions: Vec<Session> },
    Ok,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Snapshot {
        session: String,
        #[serde(with = "bytes")]
        data: Vec<u8>,
        cols: u16,
        rows: u16,
    },
    Output {
        session: String,
        #[serde(with = "bytes")]
        data: Vec<u8>,
    },
    SessionChanged {
        session: Session,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Reply {
        id: u64,
        result: Result<Response, Error>,
    },
    Event {
        event: Event,
    },
}

mod bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        STANDARD
            .decode(String::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}
