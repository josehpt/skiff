use crate::Client;
use anyhow::{anyhow, bail, Result};
use skiff_core::{protocol::{Envelope, Request, Response}, session::{SessionId, SessionInfo, SessionSpec}};
use std::sync::atomic::Ordering;

impl Client {
    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        match self.request(Request::ListSessions).await? {
            Response::Sessions { sessions } => Ok(sessions),
            Response::Error { message } => bail!(message),
            other => bail!("unexpected reply: {other:?}"),
        }
    }

    pub async fn create_session(&self, spec: SessionSpec) -> Result<SessionInfo> {
        match self.request(Request::CreateSession { spec }).await? {
            Response::Session { session } => Ok(session),
            Response::Error { message } => bail!(message),
            other => bail!("unexpected reply: {other:?}"),
        }
    }

    /// Finds or starts the Skiff agent's session. See [`Request::Coordinator`].
    /// `None` when `spec` is `None` and no coordinator lives.
    pub async fn coordinator(&self, ask: Coordinator) -> Result<Option<Coordinated>> {
        let req = Request::Coordinator { spec: ask.spec, start: ask.start, resume: ask.resume, expect: ask.expect };
        match self.request(req).await? {
            Response::Coordinator { session, created, typed } => Ok(Some(Coordinated { session, created, typed })),
            Response::Sessions { .. } => Ok(None),
            Response::Error { message } => bail!(message),
            other => bail!("unexpected reply: {other:?}"),
        }
    }

    pub async fn write(&self, session: &str, data: Vec<u8>) -> Result<()> {
        self.expect_ok(Request::Write {
            session: session.to_string(),
            data,
            paste: false,
        })
        .await
    }

    /// Writes `data` as a paste. See [`Request::Write`].
    pub async fn paste(&self, session: &str, data: Vec<u8>) -> Result<()> {
        self.expect_ok(Request::Write {
            session: session.to_string(),
            data,
            paste: true,
        })
        .await
    }

    /// Types `data`, and with `enter` presses Enter, only if the session
    /// took no input since its `input_gen` was `expect`. See
    /// [`Request::Submit`]. `false`: refused, nothing typed.
    pub async fn submit(&self, session: &str, data: Vec<u8>, paste: bool, enter: bool, expect: u64) -> Result<bool> {
        let req = Request::Submit {
            session: session.to_string(),
            data,
            paste,
            enter,
            expect,
        };
        match self.request(req).await? {
            Response::Ok => Ok(true),
            Response::Refused => Ok(false),
            Response::Error { message } => bail!(message),
            other => bail!("unexpected reply: {other:?}"),
        }
    }

    /// Queues input and returns at once. Calls from one thread reach the PTY
    /// in call order. The reply has no waiter, so the reader drops it.
    pub fn write_now(&self, session: &str, data: Vec<u8>) -> Result<()> {
        self.input_now(session, data, false)
    }

    /// [`Self::write_now`], or as a paste. See [`Request::Write`].
    pub fn input_now(&self, session: &str, data: Vec<u8>, paste: bool) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = Request::Write {
            session: session.to_string(),
            data,
            paste,
        };
        let mut line = serde_json::to_vec(&Envelope { id, request })?;
        line.push(b'\n');
        self.tx.send(line).map_err(|_| anyhow!("skiffd connection closed"))
    }

    pub async fn set_session_theme(&self, session: &str, theme: Option<String>) -> Result<SessionInfo> {
        let req = Request::SetSessionTheme {
            session: session.to_string(),
            theme,
        };
        match self.request(req).await? {
            Response::Session { session } => Ok(session),
            Response::Error { message } => bail!(message),
            other => bail!("unexpected reply: {other:?}"),
        }
    }

    pub async fn rename_session(&self, session: &str, name: &str) -> Result<SessionInfo> {
        let req = Request::RenameSession {
            session: session.to_string(),
            name: name.to_string(),
        };
        match self.request(req).await? {
            Response::Session { session } => Ok(session),
            Response::Error { message } => bail!(message),
            other => bail!("unexpected reply: {other:?}"),
        }
    }

    pub async fn resize(&self, session: &str, cols: u16, rows: u16) -> Result<()> {
        self.expect_ok(Request::Resize {
            session: session.to_string(),
            cols,
            rows,
        })
        .await
    }

    pub async fn subscribe(&self, session: &str) -> Result<()> {
        self.expect_ok(Request::Subscribe {
            session: session.to_string(),
        })
        .await
    }

    pub async fn unsubscribe(&self, session: &str) -> Result<()> {
        self.outputs.lock().unwrap().remove(session);
        self.expect_ok(Request::Unsubscribe {
            session: session.to_string(),
        })
        .await
    }

    /// The session's last `rows` rows of text, without the agent's input box.
    pub async fn screen(&self, session: &str, rows: u16) -> Result<Vec<String>> {
        match self.request(Request::Screen { session: session.to_string(), rows }).await? {
            Response::Screen { lines } => Ok(lines),
            Response::Error { message } => bail!(message),
            other => bail!("unexpected reply: {other:?}"),
        }
    }

    /// The session's pane has the keys. Clears its unread flag and agent exit.
    pub async fn seen(&self, session: &str) -> Result<()> {
        self.expect_ok(Request::Seen {
            session: session.to_string(),
        })
        .await
    }

    pub async fn kill(&self, session: &SessionId) -> Result<()> {
        self.expect_ok(Request::Kill {
            session: session.clone(),
        })
        .await
    }
}

/// A [`Client::coordinator`] request. See [`Request::Coordinator`].
#[derive(Default, Debug, Clone)]
pub struct Coordinator {
    pub spec: Option<SessionSpec>,
    pub start: Option<String>,
    pub resume: Option<String>,
    pub expect: Option<u64>,
}

/// The coordinator, and what the request did to it.
#[derive(Debug, Clone)]
pub struct Coordinated {
    pub session: SessionInfo,
    /// It started now.
    pub created: bool,
    /// A start or resume line went into its shell now.
    pub typed: bool,
}
