//! Where this daemon listens and where it saves its workspace. Both are
//! fixed once per process. A reload keeps the paths of the image it
//! replaced: a debug build that adopts a release daemon still serves the
//! release socket and saves the release workspace file.

use std::path::PathBuf;
use std::sync::OnceLock;

static SOCKET: OnceLock<PathBuf> = OnceLock::new();
static STATE: OnceLock<PathBuf> = OnceLock::new();

/// The socket the listener is bound to, once it is known.
pub fn listening() -> Option<PathBuf> {
    SOCKET.get().cloned()
}

/// The socket for children's `SKIFF_SOCKET`: the listener's, or the
/// computed path before the listener is known.
pub fn socket() -> PathBuf {
    listening().unwrap_or_else(skiff_core::socket::socket_path)
}

/// Sets the socket once. A later call changes nothing. The state follows
/// it: only the live socket serves the person's files.
pub fn set_socket(path: PathBuf) {
    skiff_core::state::set_socket(path.clone());
    let _ = SOCKET.set(path);
}

/// The workspace file: the one handed over by a reload, or the computed path.
pub fn state() -> PathBuf {
    STATE.get().cloned().unwrap_or_else(crate::workspace::path)
}

/// Sets the workspace file once. A later call changes nothing.
pub fn set_state(path: PathBuf) {
    let _ = STATE.set(path);
}
