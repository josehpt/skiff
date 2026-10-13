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

    /// Queues input and returns at once. Calls from one thread reach the PTY
    /// in call order. The reply has no waiter, so the reader drops it.
    pub fn write_now(&self, session: &str, data: Vec<u8>) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = Request::Write {
            session: session.to_string(),
            data,
            paste: false,
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
