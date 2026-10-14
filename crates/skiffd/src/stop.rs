//! How skiffd learns to stop: SIGTERM, SIGINT or a `shutdown` request.
//!
//! A signal from a process inside one of skiffd's own sessions is ignored. A
//! pane can run `pkill -f skiffd` meant for some other daemon; that must not
//! end the panes around it. A deliberate stop from a pane goes through the
//! socket (`skiff daemon stop`). A signal from anyone else stops the daemon
//! as before: logout, systemd, a shell outside Skiff, the app.
//!
//! The handler reads the sender from `siginfo_t` (`SA_SIGINFO`) and walks
//! its parents while the handler still runs. A short-lived sender such as
//! `pkill` may exit right after it sends; the walk starts before it can be
//! reaped, in all but a rare race. A sender that is gone counts as ours.

use std::{os::raw::c_int, sync::atomic::{AtomicI32, Ordering}};

use anyhow::{bail, Result};
use tokio::sync::{mpsc, Notify};

/// What made the daemon stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    Signal { signal: c_int, sender: i32 },
    Request,
}

/// Where the sender of a signal stands to this daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum Sender {
    /// The kernel or a terminal (`si_pid` 0), or a process outside the
    /// session tree.
    Outside = 0,
    /// A process that descends from this daemon: something in a pane.
    Inside = 1,
    /// Gone before the walk could read it.
    Gone = 2,
}

static WRITE_FD: AtomicI32 = AtomicI32::new(-1);
static REQUESTED: Notify = Notify::const_new();

/// Asks the daemon to stop, as the `shutdown` request does.
pub fn request() {
    REQUESTED.notify_one();
}

/// Installs the SIGTERM and SIGINT handlers. Call once, before any session
/// starts. Returns a stream of stop causes: each signal from outside the
/// session tree and each `shutdown` request. Signals from inside are logged
/// and dropped here.
pub fn install() -> Result<mpsc::UnboundedReceiver<Cause>> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: pipe writes two fds into the array we pass.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        bail!("pipe: {}", std::io::Error::last_os_error());
    }
    let [read, write] = fds;
    for fd in fds {
        // Neither end may reach a session or a reloaded image.
        // SAFETY: fcntl on fds we own.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // The handler must never block. A full pipe drops a signal, and the
    // pipe holds hundreds.
    // SAFETY: as above.
    unsafe { libc::fcntl(write, libc::F_SETFL, libc::fcntl(write, libc::F_GETFL) | libc::O_NONBLOCK) };
    WRITE_FD.store(write, Ordering::SeqCst);

    for signal in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: a zeroed sigaction with our handler and an empty mask.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = on_signal as extern "C" fn(c_int, *mut libc::siginfo_t, *mut libc::c_void) as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        unsafe { libc::sigemptyset(&mut action.sa_mask) };
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
            bail!("sigaction: {}", std::io::Error::last_os_error());
        }
    }

    let (tx, rx) = mpsc::unbounded_channel();
    let signals = tx.clone();
    std::thread::Builder::new().name("signals".into()).spawn(move || {
        let mut buf = [0u8; 12];
        loop {
            // SAFETY: read into a buffer we own. Twelve bytes are under
            // PIPE_BUF, so each write arrives whole.
            let n = unsafe { libc::read(read, buf.as_mut_ptr().cast(), buf.len()) };
            if n != buf.len() as isize {
                if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            let word = |i: usize| i32::from_ne_bytes(buf[i * 4..i * 4 + 4].try_into().unwrap());
            let (signal, sender, verdict) = (word(0), word(1), word(2));
            let name = if signal == libc::SIGINT { "SIGINT" } else { "SIGTERM" };
            if verdict == Sender::Outside as i32 {
                tracing::info!("{name} from pid {sender} ({}); stopping", command(sender));
                if signals.send(Cause::Signal { signal, sender }).is_err() {
                    return;
                }
            } else {
                let what = if verdict == Sender::Gone as i32 { "a process that is gone" } else { "a process in a session" };
                tracing::warn!(
                    "ignored {name} from pid {sender} ({}): {what}. To stop skiffd from a pane, run `skiff daemon stop`",
                    command(sender)
                );
            }
        }
    })?;
    tokio::spawn(async move {
        loop {
            REQUESTED.notified().await;
            if tx.send(Cause::Request).is_err() {
                return;
            }
        }
    });
    Ok(rx)
}

/// The sender's command line for the log, or `?` when it is gone.
fn command(pid: i32) -> String {
    let argv = crate::session::proc_argv(pid as u32);
    if argv.is_empty() { "?".into() } else { argv.join(" ") }
}

extern "C" fn on_signal(signal: c_int, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
    // Only async-signal-safe calls from here on: getpid, open, read, close,
    // write, and errno kept as it was.
    let errno = errno();
    // SAFETY: the kernel passes a valid siginfo_t with SA_SIGINFO.
    let sender = if info.is_null() { 0 } else { unsafe { (*info).si_pid() } };
    let verdict = classify(sender);
    let mut buf = [0u8; 12];
    buf[0..4].copy_from_slice(&signal.to_ne_bytes());
    buf[4..8].copy_from_slice(&sender.to_ne_bytes());
    buf[8..12].copy_from_slice(&(verdict as i32).to_ne_bytes());
    let fd = WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: write from a buffer we own.
        unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
    }
    set_errno(errno);
}

/// Walks the sender's parents up to this daemon or to init.
fn classify(sender: i32) -> Sender {
    if sender <= 0 {
        return Sender::Outside;
    }
    // SAFETY: getpid has no preconditions.
    let me = unsafe { libc::getpid() };
    let mut pid = sender;
    for _ in 0..128 {
        if pid == me {
            return Sender::Inside;
        }
        if pid <= 1 {
            return Sender::Outside;
        }
        match parent(pid) {
            Some(p) => pid = p,
            // The sender, or a parent of it, exited before the walk got
            // there. The chain cannot be told.
            None => return Sender::Gone,
        }
    }
    Sender::Outside
}

/// The parent pid, read with no allocation. `None` when the process is gone.
#[cfg(target_os = "linux")]
fn parent(pid: i32) -> Option<i32> {
    // "/proc/" + up to 10 digits + "/stat" + NUL.
    let mut path = [0u8; 32];
    let mut len = 0;
    for b in b"/proc/" {
        path[len] = *b;
        len += 1;
    }
    let mut digits = [0u8; 10];
    let (mut n, mut d) = (pid as u32, 0);
    loop {
        digits[d] = b'0' + (n % 10) as u8;
        d += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    while d > 0 {
        d -= 1;
        path[len] = digits[d];
        len += 1;
    }
    for b in b"/stat" {
        path[len] = *b;
        len += 1;
    }
    // SAFETY: a NUL-terminated path in a buffer we own.
    let fd = unsafe { libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    let mut stat = [0u8; 512];
    // SAFETY: read into a buffer we own, then close our fd.
    let n = unsafe { libc::read(fd, stat.as_mut_ptr().cast(), stat.len()) };
    unsafe { libc::close(fd) };
    if n <= 0 {
        return None;
    }
    parse_ppid(&stat[..n as usize])
}

/// The ppid from `/proc/<pid>/stat`: `pid (comm) state ppid ...`, where comm
/// can hold spaces and parens.
#[cfg(target_os = "linux")]
fn parse_ppid(stat: &[u8]) -> Option<i32> {
    let close = stat.iter().rposition(|b| *b == b')')?;
    let mut fields = stat[close + 1..].split(|b| *b == b' ').filter(|f| !f.is_empty());
    fields.next()?;
    let mut ppid: i32 = 0;
    let field = fields.next()?;
    for b in field {
        if !b.is_ascii_digit() {
            return None;
        }
        ppid = ppid.checked_mul(10)?.checked_add((b - b'0') as i32)?;
    }
    Some(ppid)
}

#[cfg(target_os = "macos")]
fn parent(pid: i32) -> Option<i32> {
    // proc_pidinfo is a thin wrapper on the proc_info system call.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let want = std::mem::size_of::<libc::proc_bsdinfo>() as c_int;
    // SAFETY: the buffer is a proc_bsdinfo of the size we pass.
    let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&mut info as *mut libc::proc_bsdinfo).cast(), want) };
    (got == want).then_some(info.pbi_ppid as i32)
}

#[cfg(target_os = "linux")]
fn errno() -> c_int {
    // SAFETY: the calling thread's errno.
    unsafe { *libc::__errno_location() }
}

#[cfg(target_os = "linux")]
fn set_errno(v: c_int) {
    unsafe { *libc::__errno_location() = v };
}

#[cfg(target_os = "macos")]
fn errno() -> c_int {
    unsafe { *libc::__error() }
}

#[cfg(target_os = "macos")]
fn set_errno(v: c_int) {
    unsafe { *libc::__error() = v };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_to_this_process() {
        let child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        assert_eq!(classify(child.id() as i32), Sender::Inside);
        let mut child = child;
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(classify(child.id() as i32), Sender::Gone);
        // This process's own parent is outside its tree.
        assert_eq!(classify(std::os::unix::process::parent_id() as i32), Sender::Outside);
        assert_eq!(classify(0), Sender::Outside);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_ppid_past_odd_names() {
        assert_eq!(parse_ppid(b"42 (a) b (c) S 7 42 42 0"), Some(7));
        assert_eq!(parse_ppid(b"42 (sleep) S 1234 42"), Some(1234));
    }
}
