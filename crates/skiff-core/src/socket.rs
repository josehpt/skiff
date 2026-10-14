use std::path::PathBuf;

/// Where `skiffd` listens. `SKIFF_SOCKET` overrides. Otherwise
/// `<runtime dir>/skiff/skiffd.sock` (see [`runtime_dir`]). Debug builds use
/// `skiffd-dev.sock`, so a daemon from a checkout never serves the installed
/// app.
pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("SKIFF_SOCKET") {
        return PathBuf::from(p);
    }
    let name = if cfg!(debug_assertions) { "skiffd-dev.sock" } else { "skiffd.sock" };
    runtime_dir().join("skiff").join(name)
}

/// The user's runtime folder, the same from the desktop and from an SSH
/// login, where the environment can differ. On Linux, `$XDG_RUNTIME_DIR`,
/// else `/run/user/<uid>` when it exists. On macOS, the per-user temp folder
/// the system names, which is the desktop's `$TMPDIR`. Else the temp dir.
fn runtime_dir() -> PathBuf {
    if let Some(d) = dirs::runtime_dir() {
        return d;
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = std::fs::metadata("/proc/self") {
            let d = PathBuf::from(format!("/run/user/{}", m.uid()));
            if d.is_dir() {
                return d;
            }
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(d) = darwin_user_temp() {
        return d;
    }
    std::env::temp_dir()
}

/// `confstr(_CS_DARWIN_USER_TEMP_DIR)`: what launchd sets `$TMPDIR` to for
/// the desktop. An SSH login can lack `$TMPDIR`.
#[cfg(target_os = "macos")]
fn darwin_user_temp() -> Option<PathBuf> {
    let mut buf = vec![0u8; 1024];
    // SAFETY: confstr writes at most `buf.len()` bytes, NUL included.
    let n = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr().cast(), buf.len()) };
    if n == 0 || n > buf.len() {
        return None;
    }
    buf.truncate(n - 1);
    String::from_utf8(buf).ok().map(PathBuf::from)
}
