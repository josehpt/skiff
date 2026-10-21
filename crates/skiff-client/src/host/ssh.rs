//! M2: reach a remote Host's skiffd through the system `ssh`. OpenSSH forwards
//! a Unix socket in a private `0700` directory on this machine to the remote
//! skiffd socket (`-L local.sock:remote.sock`). `~/.ssh/config`, ProxyJump,
//! agents, hardware keys and `known_hosts` work as in a terminal.
//!
//! Trust is the user's own: `StrictHostKeyChecking=yes`, so an unknown or
//! changed host key fails and nothing is learned or written (`UpdateHostKeys=no`).
//! `BatchMode=yes`: there is no prompt. Skiff adds no TOFU, no auth of its own,
//! no TCP listener. The `ssh` process is this app's transport, not the Host's:
//! stopping it ends only the forward, and the remote sessions run on.
//! Identity comes from skiffd's `pong`, never from the forwarded socket's peer
//! credentials, which name `ssh`.

use std::{
    ffi::OsString,
    fmt,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

/// How long `ssh` may take to log in and listen.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
/// The most stderr kept for diagnosis.
const STDERR_KEEP: usize = 16 * 1024;

/// One remote Host as `ssh` reaches it. `destination` is what the user types
/// after `ssh`: an alias from ssh config, `host`, or `user@host`. Port, user,
/// keys and jumps come from ssh config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    pub destination: String,
    /// The remote skiffd socket, as `skiff info` prints it there.
    pub socket: String,
}

impl SshTarget {
    /// Refuses what `ssh` could read as an option, and paths `-L` cannot carry.
    pub fn validate(&self) -> Result<(), String> {
        let d = &self.destination;
        if d.is_empty()
            || d.starts_with('-')
            || d.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(
                "The SSH destination must be one word, such as an alias from ssh config.".into(),
            );
        }
        let s = &self.socket;
        if !s.starts_with('/') || s.contains(':') || s.chars().any(|c| c.is_control()) {
            return Err("The remote socket must be an absolute path with no colon.".into());
        }
        Ok(())
    }
}

/// The `ssh` program, and for test fixtures only, a private ssh config (`-F`).
#[derive(Debug, Clone)]
pub struct SshOptions {
    pub program: PathBuf,
    pub config: Option<PathBuf>,
}

impl Default for SshOptions {
    fn default() -> Self {
        // An app started from the Dock has a short PATH.
        let system = Path::new("/usr/bin/ssh");
        let program = if system.is_file() {
            system.into()
        } else {
            PathBuf::from("ssh")
        };
        Self {
            program,
            config: None,
        }
    }
}

/// The arguments for one forward. Each `-o` here wins over ssh config.
pub fn forward_args(target: &SshTarget, local: &Path, options: &SshOptions) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    if let Some(config) = &options.config {
        args.push("-F".into());
        args.push(config.into());
    }
    for (flag, value) in [
        ("-o", "BatchMode=yes"),
        ("-o", "StrictHostKeyChecking=yes"),
        ("-o", "UpdateHostKeys=no"),
        ("-o", "ExitOnForwardFailure=yes"),
        ("-o", "ServerAliveInterval=15"),
        ("-o", "ServerAliveCountMax=3"),
        ("-o", "ConnectTimeout=15"),
        ("-o", "StreamLocalBindMask=0177"),
        ("-o", "StreamLocalBindUnlink=yes"),
        // Our own connection: not a shared master, and no extra channels.
        ("-o", "ControlMaster=no"),
        ("-o", "ControlPath=none"),
        ("-o", "ForwardAgent=no"),
        ("-o", "ForwardX11=no"),
        ("-o", "PermitLocalCommand=no"),
        ("-o", "RequestTTY=no"),
        // A failed channel is logged at INFO: keep it for diagnosis.
        ("-o", "LogLevel=INFO"),
    ] {
        args.push(flag.into());
        args.push(value.into());
    }
    args.push("-N".into());
    let mut spec = OsString::from(local);
    spec.push(":");
    spec.push(&target.socket);
    args.push("-L".into());
    args.push(spec);
    args.push("--".into());
    args.push(target.destination.clone().into());
    args
}

/// Why a forward could not reach the remote skiffd. The text follows the
/// design rules; it never repeats ssh's own output, which can name key paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshError {
    HostKeyUnknown(String),
    HostKeyChanged(String),
    AuthFailed(String),
    ForwardingRefused(String),
    /// The Host refused the channel to the socket. OpenSSH reports a missing
    /// socket and disabled socket forwarding alike, so this names both.
    ChannelRefused {
        destination: String,
        socket: String,
    },
    Unreachable(String),
    Exited {
        destination: String,
        code: Option<i32>,
    },
    TimedOut(String),
    Waiting {
        destination: String,
        seconds: u64,
    },
    Setup(String),
}

impl fmt::Display for SshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostKeyUnknown(d) => write!(f, "ssh does not know the host key of {d}. Connect once with `ssh {d}` in a terminal, then try again."),
            Self::HostKeyChanged(d) => write!(f, "The host key of {d} changed. Skiff does not connect. Check the new key before you change known_hosts."),
            Self::AuthFailed(d) => write!(f, "ssh cannot log in to {d} without a prompt. Load your key into the SSH agent or name it in ssh config, then try again."),
            Self::ForwardingRefused(d) => write!(f, "{d} does not allow socket forwarding. Set AllowStreamLocalForwarding yes in its sshd_config."),
            Self::ChannelRefused { destination, socket } => write!(f, "{destination} refused the connection to {socket}. Either no skiffd listens there (run `skiff info` on that Host to see its socket), or its sshd does not allow socket forwarding (AllowStreamLocalForwarding)."),
            Self::Unreachable(d) => write!(f, "Cannot reach {d}. Check the network and the name in ssh config."),
            Self::Exited { destination, code } => match code {
                Some(c) => write!(f, "ssh to {destination} stopped (exit {c}). Run `ssh {destination}` in a terminal to see why."),
                None => write!(f, "ssh to {destination} stopped. Run `ssh {destination}` in a terminal to see why."),
            },
            Self::TimedOut(d) => write!(f, "ssh to {d} did not connect within {}s.", OPEN_TIMEOUT.as_secs()),
            Self::Waiting { destination, seconds } => write!(f, "Not connected to {destination}. Skiff tries again in {seconds}s."),
            Self::Setup(why) => write!(f, "Cannot prepare the SSH forward: {why}"),
        }
    }
}
impl std::error::Error for SshError {}

/// What ssh's stderr says, as one of the known causes. `None`: no known cause.
pub fn classify(stderr: &str, target: &SshTarget) -> Option<SshError> {
    let d = target.destination.clone();
    if stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
        || (stderr.contains("Host key for") && stderr.contains("has changed"))
    {
        Some(SshError::HostKeyChanged(d))
    } else if stderr.contains("Host key verification failed")
        || stderr.contains("No ED25519 host key is known")
        || stderr.contains("host key is known")
    {
        Some(SshError::HostKeyUnknown(d))
    } else if stderr.contains("Permission denied")
        || stderr.contains("Too many authentication failures")
    {
        Some(SshError::AuthFailed(d))
    } else if stderr.contains("administratively prohibited") {
        Some(SshError::ForwardingRefused(d))
    } else if stderr.contains("open failed: connect failed") {
        Some(SshError::ChannelRefused {
            destination: d,
            socket: target.socket.clone(),
        })
    } else if [
        "Could not resolve hostname",
        "Connection refused",
        "Connection timed out",
        "No route to host",
        "Network is unreachable",
        "Operation timed out",
    ]
    .iter()
    .any(|m| stderr.contains(m))
    {
        Some(SshError::Unreachable(d))
    } else {
        None
    }
}

/// A running forward. Dropping it stops this app's `ssh` and removes the
/// private directory; it never signals anything on the Host.
pub struct SshForward {
    target: SshTarget,
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
    stderr: Arc<Mutex<String>>,
}

impl SshForward {
    /// Starts `ssh` and waits until it listens on the local socket.
    pub async fn open(
        target: &SshTarget,
        options: &SshOptions,
        dir: &Path,
    ) -> Result<Self, SshError> {
        target.validate().map_err(SshError::Setup)?;
        private_dir(dir).map_err(|e| SshError::Setup(format!("{}: {e}", dir.display())))?;
        let socket = dir.join("skiffd.sock");
        let _ = std::fs::remove_file(&socket);
        let mut child = Command::new(&options.program)
            .args(forward_args(target, &socket, options))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SshError::Setup(format!("run {}: {e}", options.program.display())))?;
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(err) = child.stderr.take() {
            let keep = stderr.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut kept = keep.lock().unwrap();
                    kept.push_str(&line);
                    kept.push('\n');
                    if kept.len() > STDERR_KEEP {
                        let cut = kept.len() - STDERR_KEEP;
                        let cut = (cut..kept.len())
                            .find(|&i| kept.is_char_boundary(i))
                            .unwrap_or(0);
                        kept.drain(..cut);
                    }
                }
            });
        }
        let mut forward = Self {
            target: target.clone(),
            child,
            dir: dir.to_path_buf(),
            socket,
            stderr,
        };
        let until = Instant::now() + OPEN_TIMEOUT;
        loop {
            if let Ok(Some(status)) = forward.child.try_wait() {
                // Let the stderr reader catch the last lines.
                tokio::time::sleep(Duration::from_millis(50)).await;
                return Err(forward.diagnose().unwrap_or(SshError::Exited {
                    destination: target.destination.clone(),
                    code: status.code(),
                }));
            }
            if forward.socket.exists() {
                return Ok(forward);
            }
            if Instant::now() >= until {
                return Err(SshError::TimedOut(target.destination.clone()));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// The local end; a connection here is a channel to the remote socket.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// `ssh` still runs.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// The known cause in ssh's output so far, if any.
    pub fn diagnose(&self) -> Option<SshError> {
        classify(&self.stderr.lock().unwrap(), &self.target)
    }
}

impl Drop for SshForward {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// A directory only this account can enter, made if missing. An existing one
/// that is not ours or not private is refused, never repaired.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(dir)?;
    // SAFETY: geteuid takes no arguments and cannot fail.
    if !meta.is_dir()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(std::io::Error::other(
            "not a private directory of this account",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> SshTarget {
        SshTarget {
            destination: "devbox".into(),
            socket: "/run/user/501/skiff/skiffd.sock".into(),
        }
    }

    #[test]
    fn the_forward_keeps_strict_trust_and_never_takes_the_destination_as_an_option() {
        let args = forward_args(
            &target(),
            Path::new("/tmp/skiff-x/skiffd.sock"),
            &SshOptions {
                program: "ssh".into(),
                config: None,
            },
        );
        let args: Vec<String> = args.into_iter().map(|a| a.into_string().unwrap()).collect();
        for want in [
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "UpdateHostKeys=no",
            "ExitOnForwardFailure=yes",
            "ControlPath=none",
            "ForwardAgent=no",
        ] {
            let at = args
                .iter()
                .position(|a| a == want)
                .unwrap_or_else(|| panic!("no {want}: {args:?}"));
            assert_eq!(args[at - 1], "-o");
        }
        // Nothing turns host key checking off or accepts a new key.
        assert!(!args.iter().any(|a| a.contains("StrictHostKeyChecking=no")
            || a.contains("accept-new")
            || a.contains("UserKnownHostsFile")));
        assert!(!args.contains(&"-F".to_string()));
        let n = args.len();
        assert_eq!(
            &args[n - 5..],
            [
                "-N",
                "-L",
                "/tmp/skiff-x/skiffd.sock:/run/user/501/skiff/skiffd.sock",
                "--",
                "devbox"
            ]
        );
        let with = forward_args(
            &target(),
            Path::new("/x"),
            &SshOptions {
                program: "ssh".into(),
                config: Some("/fixture/ssh_config".into()),
            },
        );
        assert_eq!(
            with[..2],
            [OsString::from("-F"), OsString::from("/fixture/ssh_config")]
        );
    }

    #[test]
    fn targets_that_ssh_could_misread_are_refused() {
        assert!(target().validate().is_ok());
        assert!(SshTarget {
            destination: "me@devbox".into(),
            ..target()
        }
        .validate()
        .is_ok());
        for d in ["", "-oProxyCommand=x", "dev box", "dev\nbox"] {
            assert!(
                SshTarget {
                    destination: d.into(),
                    ..target()
                }
                .validate()
                .is_err(),
                "{d:?}"
            );
        }
        for s in ["relative.sock", "/a:b.sock", "/a\n.sock", ""] {
            assert!(
                SshTarget {
                    socket: s.into(),
                    ..target()
                }
                .validate()
                .is_err(),
                "{s:?}"
            );
        }
        // Spaces are fine: a macOS socket lives under Application Support.
        assert!(SshTarget {
            socket: "/Users/me/Library/Application Support/skiff/skiffd.sock".into(),
            ..target()
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn ssh_output_maps_to_one_cause_without_repeating_it() {
        let t = target();
        let cases = [
            ("@@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\nHost key verification failed.\n", SshError::HostKeyChanged("devbox".into())),
            ("No ED25519 host key is known for [127.0.0.1]:42022 and you have requested strict checking.\nHost key verification failed.\n", SshError::HostKeyUnknown("devbox".into())),
            ("me@devbox: Permission denied (publickey).\n", SshError::AuthFailed("devbox".into())),
            ("channel 2: open failed: administratively prohibited: open failed\n", SshError::ForwardingRefused("devbox".into())),
            // OpenSSH 9.7 prints this for a missing socket and for disabled forwarding alike.
            ("channel 1: open failed: connect failed: open failed\n", SshError::ChannelRefused { destination: "devbox".into(), socket: t.socket.clone() }),
            ("ssh: Could not resolve hostname devbox: nodename nor servname provided\n", SshError::Unreachable("devbox".into())),
        ];
        for (stderr, want) in cases {
            assert_eq!(classify(stderr, &t), Some(want), "{stderr}");
        }
        assert_eq!(
            classify("Load key \"/Users/me/.ssh/id\": bad permissions\n", &t),
            None
        );
        // The messages name the destination and never ssh's own lines.
        let text = SshError::AuthFailed("devbox".into()).to_string();
        assert!(text.contains("devbox") && !text.contains(".ssh"));
    }

    #[test]
    fn a_shared_or_foreign_directory_is_refused_not_repaired() {
        let dir = std::env::temp_dir().join(format!("skiff-ssh-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        private_dir(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&dir).is_err());
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o755
        );
        std::fs::remove_dir(&dir).unwrap();
    }
}
