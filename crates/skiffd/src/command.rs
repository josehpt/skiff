//! Bounded collection of a command's stdout, including pipes held by descendants.

use std::{io::{self, Read}, os::fd::AsRawFd, process::{Child, ExitStatus}, time::{Duration, Instant}};

pub(crate) fn output(child: &mut Child, timeout: Duration) -> io::Result<(ExitStatus, Vec<u8>)> {
    let mut stdout = child.stdout.take().ok_or_else(|| io::Error::other("command stdout is not piped"))?;
    let fd = stdout.as_raw_fd();
    // SAFETY: stdout owns this fd; change only its file status flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags == -1 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    let start = Instant::now();
    let mut out = Vec::new();
    let mut status = None;
    let mut eof = false;
    let mut bytes = [0; 16 * 1024];
    loop {
        let mut read = false;
        if !eof {
            match stdout.read(&mut bytes) {
                Ok(0) => eof = true,
                Ok(n) => {
                    out.extend_from_slice(&bytes[..n]);
                    read = true;
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                Err(e) => return Err(e),
            }
        }
        if status.is_none() {
            status = child.try_wait()?;
        }
        if let Some(status) = status.filter(|_| eof) {
            return Ok((status, out));
        }
        if start.elapsed() >= timeout {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "command stdout did not close before timeout"));
        }
        if !read {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn output_keeps_all_bytes_and_the_exit_status() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "head -c 65536 /dev/zero; exit 7"])
            .stdout(Stdio::piped())
            .spawn().unwrap();
        let (status, bytes) = output(&mut child, Duration::from_secs(3)).unwrap();
        assert_eq!(status.code(), Some(7));
        assert_eq!(bytes, vec![0; 65536]);
    }

    #[test]
    fn a_descendant_holding_stdout_cannot_extend_the_deadline() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 1 & printf done"])
            .stdout(Stdio::piped())
            .spawn().unwrap();
        let start = Instant::now();
        let error = output(&mut child, Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(child.try_wait().unwrap().unwrap().success(), "the parent already exited");
        assert!(start.elapsed() < Duration::from_secs(1), "stdout held by the descendant did not bound the wait");
    }
}
