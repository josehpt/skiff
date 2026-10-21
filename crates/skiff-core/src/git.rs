//! Thin wrappers over the `git` binary. Blocking: async callers use
//! `spawn_blocking`.

use crate::shell::shell_quote;

use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};

use crate::project::Worktree;

/// Runs `git -C <dir> <args>` and returns stdout. A non-zero exit becomes an
/// error that carries git's stderr.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("run git")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if stderr.is_empty() {
            bail!("git {} failed: {}", args.join(" "), out.status);
        }
        bail!(stderr);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Initializes an existing folder. A folder inside a repository stays as it is.
pub fn init_repository(dir: &Path) -> Result<()> {
    if toplevel(dir).is_ok() {
        return Ok(());
    }
    git(dir, &["init"]).map(drop)
}

pub fn list_worktrees(repo: &Path) -> Result<Vec<Worktree>> {
    Ok(parse_worktrees(&git(
        repo,
        &["worktree", "list", "--porcelain", "-z"],
    )?))
}

/// Parses `git worktree list --porcelain -z`: NUL after every field, and an
/// empty field after every record. The first record is the main worktree.
pub fn parse_worktrees(out: &str) -> Vec<Worktree> {
    let mut list = Vec::new();
    let mut cur: Option<Worktree> = None;
    for field in out.split('\0') {
        if field.is_empty() {
            list.extend(cur.take());
            continue;
        }
        let (key, val) = field.split_once(' ').unwrap_or((field, ""));
        if key == "worktree" {
            list.extend(cur.take());
            cur = Some(Worktree {
                path: PathBuf::from(val),
                branch: None,
                head: String::new(),
                is_main: list.is_empty(),
                locked: false,
                prunable: false,
            });
            continue;
        }
        let Some(wt) = cur.as_mut() else { continue };
        match key {
            "HEAD" => wt.head = val.to_string(),
            "branch" => {
                wt.branch = Some(val.strip_prefix("refs/heads/").unwrap_or(val).to_string())
            }
            "locked" => wt.locked = true,
            "prunable" => wt.prunable = true,
            _ => {}
        }
    }
    list.extend(cur);
    list
}

pub fn add_worktree(repo: &Path, branch: &str, path: &Path, base: Option<&str>) -> Result<()> {
    let path = path.to_str().ok_or_else(|| anyhow!("path is not UTF-8"))?;
    // `--` keeps a base like `--force` from being read as an option.
    let mut args = vec!["worktree", "add", "-b", branch, "--", path];
    args.extend(base);
    git(repo, &args).map(drop)
}

/// Without `force`, git refuses a dirty worktree. A locked one is refused either way.
pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> Result<()> {
    let path = path.to_str().ok_or_else(|| anyhow!("path is not UTF-8"))?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.extend(["--", path]);
    git(repo, &args).map(drop)
}

/// The branch the main checkout normally holds: origin's HEAD, else a local
/// `main` or `master`. `None` when none of them exists.
pub fn default_branch(repo: &Path) -> Option<String> {
    if let Ok(r) = git(repo, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]) {
        if let Some(b) = r.trim().strip_prefix("origin/") {
            return Some(b.to_string());
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|b| git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]).is_ok())
        .map(String::from)
}

/// The top-level directory of the repository that contains `dir`.
pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    let out = git(dir, &["rev-parse", "--show-toplevel"])
        .map_err(|_| anyhow!("{} is not inside a git repository", dir.display()))?;
    Ok(PathBuf::from(out.trim()))
}

pub fn is_dirty(dir: &Path) -> Result<bool> {
    Ok(!git(dir, &["status", "--porcelain"])?.trim().is_empty())
}

/// One changed file in a worktree, against HEAD: staged and unstaged together.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Relative to the worktree root.
    pub path: String,
    /// `M` modified, `A` added or untracked, `D` deleted, `U` unmerged.
    pub status: char,
    /// Lines added and removed. `None` for a binary file.
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

/// Untracked files larger than this show no line count.
const COUNT_LIMIT: u64 = 1 << 20;

/// The worktree folder held open, so a count reads the folder that was
/// checked even if its path is swapped for another folder or a link.
pub struct Root {
    dir: std::fs::File,
    #[cfg(not(unix))]
    path: PathBuf,
}

impl Root {
    /// Opened before `dir` is known to be a worktree: anything but a folder
    /// fails, and a FIFO there never blocks.
    pub fn open(dir: &Path) -> Result<Root> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_DIRECTORY | libc::O_NONBLOCK);
        let file = options.open(dir).with_context(|| format!("open {}", dir.display()))?;
        if !file.metadata()?.is_dir() {
            bail!("{} is not a folder", dir.display());
        }
        Ok(Root {
            dir: file,
            #[cfg(not(unix))]
            path: dir.to_path_buf(),
        })
    }

    /// Fails unless `dir` still names this folder.
    fn check(&self, dir: &Path) -> Result<()> {
        if !same_file(&self.dir.metadata()?, &std::fs::metadata(dir)?) {
            bail!("{} was replaced while it was read", dir.display());
        }
        Ok(())
    }
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    (a.dev(), a.ino()) == (b.dev(), b.ino())
}

#[cfg(not(unix))]
fn same_file(_: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    b.is_dir()
}

/// The changed files of the worktree at `dir`, sorted by path.
pub fn changes(dir: &Path) -> Result<Vec<Change>> {
    changes_in(&Root::open(dir)?, dir)
}

/// [`changes`] of `root`, opened before `dir` was checked as a worktree.
/// Git runs at `dir`, which must name `root` before and after; untracked
/// files are counted under `root` itself.
pub fn changes_in(root: &Root, dir: &Path) -> Result<Vec<Change>> {
    root.check(dir)?;
    let status = git(dir, &["status", "--porcelain=v1", "-z", "--no-renames", "--untracked-files=all"])?;
    // Fails in a repository with no commit yet: every file then shows no count.
    let numstat = git(dir, &["diff", "HEAD", "--numstat", "-z", "--no-renames"]).unwrap_or_default();
    let mut counts = std::collections::HashMap::new();
    for rec in numstat.split('\0').filter(|r| !r.is_empty()) {
        let mut f = rec.splitn(3, '\t');
        let (Some(a), Some(r), Some(p)) = (f.next(), f.next(), f.next()) else { continue };
        counts.insert(p.to_string(), (a.parse().ok(), r.parse().ok()));
    }
    // Swapped while git ran: its output may be another folder's.
    root.check(dir)?;
    let mut list = Vec::new();
    for rec in status.split('\0').filter(|r| r.len() > 3) {
        let (xy, path) = rec.split_at(3);
        let (x, y) = (xy.as_bytes()[0], xy.as_bytes()[1]);
        let status = match (x, y) {
            (b'?', _) => 'A',
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => 'U',
            (b'D', _) | (_, b'D') => 'D',
            (b'A', _) => 'A',
            _ => 'M',
        };
        let (added, removed) = match counts.get(path) {
            Some(&c) => c,
            None if x == b'?' => (count_lines(root, path), Some(0)),
            None => (None, None),
        };
        list.push(Change { path: path.to_string(), status, added, removed });
    }
    list.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(list)
}

/// The diff command when `[open] diff` sets none.
pub const DEFAULT_DIFF: &str = "git diff --color=always";

/// A changed file's diff against HEAD, or the whole worktree's, printed by
/// `command` in the worktree at `width` columns. `{target}` in the command
/// becomes what to compare; without it, the target goes at the end. An
/// untracked file compares with an empty file. The whole-worktree view leaves
/// untracked files out.
pub fn diff_text(dir: &Path, file: Option<&str>, command: Option<&str>, width: u16) -> Result<String> {
    let script = diff_script(dir, file, command);
    let out = Command::new("sh")
        .args(["-c", &script])
        .current_dir(dir)
        // Printed to a pipe, tools drop color and guess the width. Tell them both.
        .env("COLUMNS", width.to_string())
        .env("DFT_COLOR", "always")
        .env("DFT_WIDTH", width.to_string())
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.quotePath")
        .env("GIT_CONFIG_VALUE_0", "false")
        .stdin(Stdio::null())
        .output()
        .context("run the diff command")?;
    // Diff tools exit 1 when the files differ.
    if !out.status.success() && out.status.code() != Some(1) {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!("{script} failed: {}", if err.is_empty() { out.status.to_string() } else { err });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The shell script that shows a changed file's diff against HEAD, or the
/// whole worktree's, with `command`, else `git diff`. `{target}` in the
/// command becomes what to compare; without it, the target goes at the end.
/// An untracked file compares with an empty file.
pub fn diff_script(dir: &Path, file: Option<&str>, command: Option<&str>) -> String {
    let target = match file {
        None => "HEAD".to_string(),
        Some(f) if git(dir, &["ls-files", "--error-unmatch", "--", f]).is_ok() => format!("HEAD -- {}", shell_quote(f)),
        Some(f) => format!("--no-index -- /dev/null {}", shell_quote(f)),
    };
    let command = command.map(str::trim).filter(|c| !c.is_empty()).unwrap_or(DEFAULT_DIFF);
    if command.contains("{target}") {
        command.replace("{target}", &target)
    } else {
        format!("{command} {target}")
    }
}

/// The most diff text [`diff_in`] answers with, in bytes.
pub const DIFF_LIMIT: usize = 512 * 1024;
/// How long one git run for [`diff_in`] may take before it is killed.
const DIFF_TIMEOUT: Duration = Duration::from_secs(20);

/// A diff as plain text, for a viewer: never colored, never from an
/// external diff tool or a textconv filter, never over [`DIFF_LIMIT`].
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    /// Unified diff text. Empty: no change.
    pub text: String,
    /// The diff is longer than the limit: `text` is its start, cut at a line end.
    pub truncated: bool,
    /// The file is binary: `text` holds git's note only.
    pub binary: bool,
    /// The file is untracked: it is compared with an empty file.
    pub untracked: bool,
}

/// `rel` is a path below a worktree: relative, with no `..`, `.` or root.
pub fn is_relative_file(rel: &str) -> bool {
    use std::path::Component;
    let parts: Vec<_> = Path::new(rel).components().collect();
    !parts.is_empty() && parts.iter().all(|c| matches!(c, Component::Normal(_)))
}

/// One changed file's diff against HEAD, or the whole worktree's, as
/// [`changes_in`] reads it: git runs at `dir`, which must name `root` before
/// and after, and an untracked file is read under `root` itself, never
/// followed if it is a link, never opened if it is not a regular file. The
/// whole-worktree diff leaves untracked files out, as `git diff` does.
pub fn diff_in(root: &Root, dir: &Path, file: Option<&str>) -> Result<Diff> {
    root.check(dir)?;
    let diff = match file {
        None => tracked_diff(dir, None)?,
        Some(f) => {
            if !is_relative_file(f) {
                bail!("{f} is not a file path in this worktree");
            }
            // Literal: a `*` or `:` in a name is not a pattern.
            let spec = format!(":(literal){f}");
            let status = git(dir, &["status", "--porcelain=v1", "-z", "--no-renames", "--untracked-files=all", "--", &spec])?;
            match status.split('\0').find(|r| r.len() > 3 && &r[3..] == f) {
                None => Diff::default(),
                Some(r) if r.starts_with("??") => untracked_diff(root, f)?,
                Some(_) => tracked_diff(dir, Some(&spec))?,
            }
        }
    };
    // Swapped while git ran: its output may be another folder's.
    root.check(dir)?;
    Ok(diff)
}

fn tracked_diff(dir: &Path, spec: Option<&str>) -> Result<Diff> {
    // No commit yet: everything is compared with the empty tree.
    let base = match git(dir, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]) {
        Ok(head) => head,
        Err(_) => git(dir, &["hash-object", "-t", "tree", "/dev/null"])?,
    };
    let base = base.trim();
    let mut args = vec!["-c", "core.quotePath=false", "diff", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", base];
    if let Some(spec) = spec {
        args.extend(["--", spec]);
    }
    let (out, truncated) = git_bounded(dir, &args, DIFF_LIMIT)?;
    let (text, truncated) = cap(out, truncated);
    // Content lines start with ` `, `+` or `-`: this line is git's own.
    let binary = spec.is_some() && text.lines().any(|l| l.starts_with("Binary files "));
    Ok(Diff { text, truncated, binary, untracked: false })
}

/// A new file's diff, as `git diff --no-index /dev/null <file>` shows it,
/// read under `root` with the same rules as its line count.
fn untracked_diff(root: &Root, rel: &str) -> Result<Diff> {
    use std::io::Read;
    let not_shown = || anyhow!("{rel} is not a regular file or a link, so it is not shown");
    let (mode, body, truncated) = match open_beneath(root, rel).ok_or_else(not_shown)? {
        Beneath::Symlink(target) => ("120000", target, false),
        Beneath::File(file) => {
            let meta = file.metadata()?;
            if !meta.is_file() {
                return Err(not_shown());
            }
            #[cfg(unix)]
            let exec = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
            #[cfg(not(unix))]
            let exec = false;
            let mut bytes = Vec::new();
            file.take(DIFF_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
            let truncated = bytes.len() > DIFF_LIMIT;
            bytes.truncate(DIFF_LIMIT);
            (if exec { "100755" } else { "100644" }, bytes, truncated)
        }
    };
    let mut text = format!("diff --git a/{rel} b/{rel}\nnew file mode {mode}\n");
    // git's test: a NUL in the first 8000 bytes.
    if body[..body.len().min(8000)].contains(&0) {
        text += &format!("Binary files /dev/null and b/{rel} differ\n");
        return Ok(Diff { text, truncated: false, binary: true, untracked: true });
    }
    let body = String::from_utf8_lossy(&body);
    let lines: Vec<&str> = body.split_inclusive('\n').collect();
    if !lines.is_empty() {
        let count = if lines.len() == 1 { String::new() } else { format!(",{}", lines.len()) };
        text += &format!("--- /dev/null\n+++ b/{rel}\n@@ -0,0 +1{count} @@\n");
        for line in &lines {
            text.push('+');
            text.push_str(line);
        }
        if !body.ends_with('\n') && !truncated {
            text += "\n\\ No newline at end of file\n";
        }
    }
    let (text, cut) = cap(text.into_bytes(), truncated);
    Ok(Diff { text, truncated: cut, binary: false, untracked: true })
}

/// At most [`DIFF_LIMIT`] bytes of text. Cut text ends at its last whole line.
fn cap(mut out: Vec<u8>, truncated: bool) -> (String, bool) {
    let truncated = truncated || out.len() > DIFF_LIMIT;
    if truncated {
        out.truncate(DIFF_LIMIT);
        let end = out.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        out.truncate(end);
    }
    (String::from_utf8_lossy(&out).into_owned(), truncated)
}

/// Runs `git -C <dir> <args>` with [`bounded`] and [`DIFF_TIMEOUT`].
fn git_bounded(dir: &Path, args: &[&str], limit: usize) -> Result<(Vec<u8>, bool)> {
    let mut cmd = Command::new("git");
    // A read: git takes no optional lock and writes no index.
    cmd.arg("-C").arg(dir).args(args).env("GIT_OPTIONAL_LOCKS", "0");
    bounded(cmd, limit, DIFF_TIMEOUT).map_err(|e| match e.downcast_ref::<Failed>() {
        Some(Failed(err)) if err.is_empty() => anyhow!("git {} failed", args.join(" ")),
        Some(Failed(err)) => anyhow!("{err}"),
        None => e,
    })
}

/// A command that exited with an error, and the start of what it printed there.
#[derive(Debug)]
struct Failed(String);
impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Failed {}

/// Runs `cmd` and keeps at most `limit` bytes of its output, and the start
/// of its errors. More output, or a run past `timeout`, kills it. True: the
/// output was cut.
fn bounded(mut cmd: Command, limit: usize, timeout: Duration) -> Result<(Vec<u8>, bool)> {
    use std::io::Read;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start the command")?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    // Drained to the end, so git never waits on a full pipe; the start is kept.
    let errors = std::thread::spawn(move || {
        let mut kept = Vec::new();
        let _ = (&mut stderr).take(4096).read_to_end(&mut kept);
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        kept
    });
    // The child stays unreaped until `try_wait` below, so a kill never
    // reaches another process.
    let child = Arc::new(Mutex::new(child));
    let timed_out = Arc::new(AtomicBool::new(false));
    let (done, finished) = mpsc::channel::<()>();
    let watchdog = {
        let (child, timed_out) = (child.clone(), timed_out.clone());
        std::thread::spawn(move || {
            if let Err(mpsc::RecvTimeoutError::Timeout) = finished.recv_timeout(timeout) {
                timed_out.store(true, Ordering::SeqCst);
                let _ = child.lock().unwrap().kill();
            }
        })
    };
    let mut out = Vec::new();
    let read = (&mut stdout).take(limit as u64 + 1).read_to_end(&mut out);
    let truncated = out.len() > limit;
    out.truncate(limit);
    drop(stdout);
    if truncated {
        let _ = child.lock().unwrap().kill();
    }
    let status = loop {
        if let Some(status) = child.lock().unwrap().try_wait().context("wait for git")? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let _ = done.send(());
    let _ = watchdog.join();
    let err = String::from_utf8_lossy(&errors.join().unwrap_or_default()).trim().to_string();
    if timed_out.load(Ordering::SeqCst) {
        bail!("git took longer than {}s", timeout.as_secs_f32());
    }
    read.context("read git output")?;
    if !truncated && !status.success() {
        return Err(Failed(err).into());
    }
    Ok((out, truncated))
}

/// Lines in a new file at `rel` under the worktree `root`, as `git diff`
/// would count them. It never leaves the worktree and never blocks:
/// - a symlink is one line, its target, as git shows it; it is not followed
/// - a path through a symlinked folder, `..` or an absolute path: `None`
/// - a FIFO, device, socket or folder: `None`, never opened for reading
/// - a binary file, or one over [`COUNT_LIMIT`] bytes: `None`, and no
///   more than the limit is read even if the file grows meanwhile
fn count_lines(root: &Root, rel: &str) -> Option<u32> {
    use std::io::Read;
    let file = open_beneath(root, rel)?;
    let file = match file {
        Beneath::Symlink(_) => return Some(1),
        Beneath::File(f) => f,
    };
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > COUNT_LIMIT {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(COUNT_LIMIT + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > COUNT_LIMIT || bytes.contains(&0) {
        return None;
    }
    let n = bytes.iter().filter(|&&b| b == b'\n').count();
    let last = !bytes.is_empty() && !bytes.ends_with(b"\n");
    Some((n + last as usize) as u32)
}

enum Beneath {
    File(std::fs::File),
    /// The last component is a symlink, with its target. It was not followed.
    Symlink(Vec<u8>),
}

/// Opens `rel` under the open folder `root` one component at a time, each
/// relative to the folder opened before it and none followed if it is a
/// symlink, so a folder swapped for a link meanwhile cannot lead outside
/// `root`. The file opens non-blocking: a FIFO that slipped in fails its
/// type check, not a read.
#[cfg(unix)]
fn open_beneath(root: &Root, rel: &str) -> Option<Beneath> {
    use std::{ffi::CString, os::fd::AsRawFd, os::unix::ffi::OsStrExt, os::fd::FromRawFd, path::Component};
    let parts: Vec<_> = Path::new(rel).components().collect();
    if parts.is_empty() || parts.iter().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    // SAFETY: plain open(2) calls on NUL-terminated paths; every fd made here
    // is closed below or owned by the returned File. `root` stays open.
    unsafe {
        let mut fd = libc::fcntl(root.dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0);
        if fd < 0 {
            return None;
        }
        for (i, part) in parts.iter().enumerate() {
            let Ok(name) = CString::new(part.as_os_str().as_bytes()) else {
                libc::close(fd);
                return None;
            };
            let last = i + 1 == parts.len();
            if last {
                // Its type before any open: a link is not followed, and a
                // FIFO or device is never opened.
                let mut st: libc::stat = std::mem::zeroed();
                if libc::fstatat(fd, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) != 0 {
                    libc::close(fd);
                    return None;
                }
                match st.st_mode & libc::S_IFMT {
                    libc::S_IFREG => {}
                    libc::S_IFLNK => {
                        let mut target = vec![0u8; 4096];
                        let n = libc::readlinkat(fd, name.as_ptr(), target.as_mut_ptr().cast(), target.len());
                        libc::close(fd);
                        target.truncate(n.max(0) as usize);
                        return Some(Beneath::Symlink(target));
                    }
                    _ => {
                        libc::close(fd);
                        return None;
                    }
                }
            }
            // Swapped meanwhile: O_NOFOLLOW refuses a link, O_NONBLOCK keeps a
            // FIFO from blocking, and the caller checks the type again.
            let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | if last { libc::O_NONBLOCK } else { libc::O_DIRECTORY };
            let next = libc::openat(fd, name.as_ptr(), flags);
            libc::close(fd);
            if next < 0 {
                return None;
            }
            fd = next;
        }
        Some(Beneath::File(std::fs::File::from_raw_fd(fd)))
    }
}

#[cfg(not(unix))]
fn open_beneath(root: &Root, rel: &str) -> Option<Beneath> {
    let path = root.path.join(rel);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&path).map(|t| t.to_string_lossy().into_owned().into_bytes()).unwrap_or_default();
        return Some(Beneath::Symlink(target));
    }
    meta.is_file().then(|| std::fs::File::open(path).ok().map(Beneath::File)).flatten()
}

/// Which of `paths`, relative to `dir`, git ignores. A folder needs a
/// trailing `/` to match a pattern such as `target/`.
pub fn ignored(dir: &Path, paths: &[String]) -> Result<HashSet<String>> {
    if paths.is_empty() {
        return Ok(HashSet::new());
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["check-ignore", "-z", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run git")?;
    let mut input = Vec::new();
    for p in paths {
        input.extend_from_slice(p.as_bytes());
        input.push(0);
    }
    // Written from a thread: git may fill its stdout pipe before it reads all of stdin.
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output().context("run git")?;
    let _ = writer.join();
    // 1 means nothing is ignored. 128 is a real error, such as no repository.
    if !out.status.success() && out.status.code() != Some(1) {
        bail!(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_repository_keeps_files_and_supports_an_unborn_branch() {
        let root = std::env::temp_dir().join(format!("skiff-init-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("notes.txt"), "keep me\n").unwrap();
        init_repository(&root).unwrap();
        assert_eq!(toplevel(&root).unwrap(), root.canonicalize().unwrap());
        assert_eq!(std::fs::read_to_string(root.join("notes.txt")).unwrap(), "keep me\n");
        assert_eq!(git(&root, &["status", "--porcelain"]).unwrap().trim(), "?? notes.txt");
        assert!(git(&root, &["rev-parse", "--verify", "HEAD"]).is_err());
        let worktrees = list_worktrees(&root).unwrap();
        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].is_main);
        assert!(worktrees[0].branch.is_some());
        init_repository(&root.join("src")).unwrap();
        assert!(!root.join("src/.git").exists());
        assert!(init_repository(&root.join("missing")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changes_counts_lines_per_file() {
        let root = std::env::temp_dir().join(format!("skiff-changes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "1\n2\n3\n").unwrap();
        std::fs::write(root.join("gone.txt"), "x\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        std::fs::write(root.join("a.txt"), "1\ntwo\n3\n4\n").unwrap();
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/new.rs"), "fn main() {}\nlast").unwrap();
        std::fs::write(root.join("blob.bin"), [0u8, 1, 2]).unwrap();

        let got = changes(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let c = |path: &str, status, added, removed| Change { path: path.into(), status, added, removed };
        assert_eq!(
            got,
            [
                c("a.txt", 'M', Some(2), Some(1)),
                c("blob.bin", 'A', None, Some(0)),
                c("gone.txt", 'D', Some(0), Some(1)),
                c("src/new.rs", 'A', Some(2), Some(0)),
            ]
        );
    }

    /// Review M4 P2: an untracked link to a FIFO outside the worktree once
    /// blocked the count and read the outside data. Now a link counts as git
    /// counts it (one line, its target), and nothing outside, no FIFO and no
    /// more than the limit is ever read.
    #[cfg(unix)]
    #[test]
    fn untracked_links_fifos_and_large_files_are_never_followed_or_read() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("skiff-count-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        struct Gone(PathBuf);
        impl Drop for Gone {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _gone = Gone(base.clone());
        let (root, outside) = (base.join("repo"), base.join("outside"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
        run(&["init", "-q"]);
        std::fs::write(outside.join("secret.txt"), "1\n2\n3\n").unwrap();
        let mkfifo = |p: &Path| {
            let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: a NUL-terminated path.
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        };
        mkfifo(&outside.join("fifo"));
        mkfifo(&root.join("pipe"));
        symlink(outside.join("secret.txt"), root.join("link-out")).unwrap();
        symlink(outside.join("fifo"), root.join("link-fifo")).unwrap();
        symlink(&outside, root.join("dir-link")).unwrap();
        std::fs::write(root.join("big.txt"), "x\n".repeat((COUNT_LIMIT as usize / 2) + 1)).unwrap();
        std::fs::write(root.join("small.txt"), "a\nb").unwrap();

        // Off this thread: a regression would block on the FIFO.
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = root.clone();
        std::thread::spawn(move || tx.send(changes(&dir)).unwrap());
        let got = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("changes blocked").unwrap();
        let count = |p: &str| got.iter().find(|c| c.path == p).map(|c| c.added);
        // git's own count for a new link is one line: its target.
        let out = Command::new("git").arg("-C").arg(&root).args(["diff", "--no-index", "--numstat", "--", "/dev/null", "link-out"]).output().unwrap();
        let git_count = String::from_utf8_lossy(&out.stdout);
        assert!(git_count.starts_with("1\t0"), "{git_count}");
        assert_eq!(count("link-out"), Some(Some(1)));
        assert_eq!(count("link-fifo"), Some(Some(1)));
        assert_eq!(count("dir-link"), Some(Some(1)));
        assert_eq!(count("big.txt"), Some(None));
        assert_eq!(count("small.txt"), Some(Some(2)));
        // Directly: a FIFO, a path through a link, `..` and absolute paths.
        for rel in ["pipe", "dir-link/secret.txt", "../outside/secret.txt", "/etc/hosts", ""] {
            assert_eq!(count_lines(&Root::open(&root).unwrap(), rel), None, "{rel}");
        }
    }

    /// Review M4 P2: the folder held open is the one counted. A path that
    /// no longer names it, swapped for a link or another folder, is refused.
    #[cfg(unix)]
    #[test]
    fn a_root_whose_path_names_another_folder_is_refused() {
        let base = std::env::temp_dir().join(format!("skiff-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, other) = (base.join("repo"), base.join("other"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let held = Root::open(&root).unwrap();
        let first = held.check(&root).is_ok();
        std::fs::rename(&root, base.join("moved")).unwrap();
        std::os::unix::fs::symlink(&other, &root).unwrap();
        let swapped = changes_in(&held, &root).map_err(|e| e.to_string());
        let other_folder = changes_in(&held, &other).map_err(|e| e.to_string());
        let file = Root::open(&base.join("moved/missing"));
        std::fs::remove_dir_all(&base).unwrap();
        assert!(first);
        assert!(swapped.is_err_and(|e| e.contains("replaced")));
        assert!(other_folder.is_err_and(|e| e.contains("replaced")));
        assert!(file.is_err());
    }

    /// M4 diff text for a viewer: git's own text for a tracked file, a new
    /// file read under the held root, explicit binary, untracked and cut
    /// states, and nothing read outside the worktree or through a FIFO.
    #[cfg(unix)]
    #[test]
    fn diff_text_is_bounded_plain_and_stays_in_the_worktree() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("skiff-difftext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        struct Gone(PathBuf);
        impl Drop for Gone {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _gone = Gone(base.clone());
        let (root, outside) = (base.join("repo"), base.join("outside"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
        run(&["init", "-q"]);
        // No commit yet: an added file is compared with the empty tree.
        std::fs::write(root.join("a.txt"), "1\n2\n").unwrap();
        run(&["add", "a.txt"]);
        let held = Root::open(&root).unwrap();
        let unborn = diff_in(&held, &root, Some("a.txt")).unwrap();
        assert!(unborn.text.contains("+1\n+2\n") && !unborn.untracked, "{unborn:?}");
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);

        // A tracked change: exactly git's own plain text.
        std::fs::write(root.join("a.txt"), "1\ntwo\n").unwrap();
        let got = diff_in(&held, &root, Some("a.txt")).unwrap();
        let git_text = git(&root, &["diff", "--no-color", "--no-ext-diff", "HEAD", "--", "a.txt"]).unwrap();
        assert_eq!(got, Diff { text: git_text, ..Diff::default() });
        assert!(!got.text.contains('\x1b'));
        // Unchanged, and a name that is a pattern elsewhere: nothing, literally.
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        run(&["add", "b.txt"]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "b"]);
        assert_eq!(diff_in(&held, &root, Some("b.txt")).unwrap(), Diff::default());
        assert_eq!(diff_in(&held, &root, Some("*.txt")).unwrap(), Diff::default());

        // Untracked: compared with an empty file, read under the root.
        std::fs::write(root.join("new.txt"), "x\ny").unwrap();
        let new = diff_in(&held, &root, Some("new.txt")).unwrap();
        assert!(new.untracked && !new.binary && !new.truncated, "{new:?}");
        assert!(new.text.ends_with("@@ -0,0 +1,2 @@\n+x\n+y\n\\ No newline at end of file\n"), "{}", new.text);
        // A link shows its target and is never followed.
        std::fs::write(outside.join("secret.txt"), "secret\n").unwrap();
        symlink(outside.join("secret.txt"), root.join("link")).unwrap();
        let link = diff_in(&held, &root, Some("link")).unwrap();
        assert!(link.text.contains("new file mode 120000") && link.text.contains(&format!("+{}", outside.join("secret.txt").display())));
        assert!(!link.text.contains("+secret"), "{}", link.text);
        // A FIFO never blocks the diff and is never opened for reading.
        let fifo = std::ffi::CString::new(root.join("pipe").as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let (dir, r2) = (root.clone(), Root::open(&root).unwrap());
        std::thread::spawn(move || tx.send(diff_in(&r2, &dir, Some("pipe")).map_err(|e| e.to_string())).unwrap());
        let pipe = rx.recv_timeout(Duration::from_secs(10)).expect("diff blocked on a FIFO");
        // git lists no FIFO as untracked: no change, and nothing opened.
        assert_eq!(pipe, Ok(Diff::default()));
        let direct = untracked_diff(&held, "pipe").map_err(|e| e.to_string());
        assert!(direct.is_err_and(|e| e.contains("not a regular file")));
        std::fs::remove_file(root.join("pipe")).unwrap();
        // Binary: git's note only.
        std::fs::write(root.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
        let blob = diff_in(&held, &root, Some("blob.bin")).unwrap();
        assert!(blob.binary && blob.untracked && blob.text.contains("Binary files /dev/null and b/blob.bin differ"));
        std::fs::write(root.join("a.txt"), [0u8, 9, 9]).unwrap();
        let tracked_bin = diff_in(&held, &root, Some("a.txt")).unwrap();
        assert!(tracked_bin.binary && !tracked_bin.untracked, "{tracked_bin:?}");

        // Over the limit, tracked or untracked: cut at a line end, and said so.
        let big = "0123456789abcdef\n".repeat(DIFF_LIMIT / 8);
        std::fs::write(root.join("a.txt"), &big).unwrap();
        std::fs::write(root.join("big.txt"), &big).unwrap();
        for f in ["a.txt", "big.txt"] {
            let d = diff_in(&held, &root, Some(f)).unwrap();
            assert!(d.truncated && d.text.len() <= DIFF_LIMIT && d.text.ends_with('\n'), "{f}: {}", d.text.len());
        }
        let all = diff_in(&held, &root, None).unwrap();
        assert!(all.truncated && all.text.len() <= DIFF_LIMIT && !all.text.contains("big.txt"));

        // Not a path below the worktree.
        for bad in ["../outside/secret.txt", "/etc/hosts", "", "./a.txt", "link/../a.txt"] {
            assert!(diff_in(&held, &root, Some(bad)).is_err(), "{bad}");
        }
        // The root swapped for a link: refused, never the other folder's.
        std::fs::rename(&root, base.join("moved")).unwrap();
        symlink(&outside, &root).unwrap();
        assert!(diff_in(&held, &root, Some("new.txt")).is_err_and(|e| e.to_string().contains("replaced")));
    }

    /// The diff's subprocess: a run past its time is killed, and output past
    /// the limit is cut and the process killed, not read to the end.
    #[cfg(unix)]
    #[test]
    fn a_diff_process_is_bounded_in_time_and_output() {
        let sh = |script: &str| {
            let mut c = Command::new("sh");
            c.args(["-c", script]);
            c
        };
        let started = std::time::Instant::now();
        let slow = bounded(sh("sleep 30"), 10, Duration::from_millis(300));
        assert!(slow.is_err_and(|e| e.to_string().contains("longer than")));
        assert!(started.elapsed() < Duration::from_secs(10));
        let started = std::time::Instant::now();
        let (out, cut) = bounded(sh("yes"), 4096, Duration::from_secs(20)).unwrap();
        assert!(cut && out.len() == 4096 && started.elapsed() < Duration::from_secs(10));
        let failed = bounded(sh("echo why >&2; exit 3"), 10, Duration::from_secs(20));
        assert!(failed.is_err_and(|e| e.to_string() == "why"));
        assert_eq!(bounded(sh("printf ok"), 10, Duration::from_secs(20)).unwrap(), (b"ok".to_vec(), false));
    }

    #[test]
    fn diff_command_gets_the_target() {
        let root = std::env::temp_dir().join(format!("skiff-diffcmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(root.join("a b.txt"), "1\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        std::fs::write(root.join("a b.txt"), "2\n").unwrap();
        std::fs::write(root.join("new.txt"), "n\n").unwrap();

        let plain = diff_text(&root, Some("a b.txt"), Some("git diff"), 80).unwrap();
        let untracked = diff_text(&root, Some("new.txt"), None, 80).unwrap();
        let placed = diff_text(&root, Some("a b.txt"), Some("git diff {target} | wc -l"), 80).unwrap();
        let failed = diff_text(&root, None, Some("false-command-that-is-missing"), 80);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(plain.contains("-1") && plain.contains("+2"), "{plain}");
        assert!(untracked.contains("+++ b/new.txt"), "{untracked}");
        assert!(placed.trim().parse::<u32>().unwrap() > 3, "{placed}");
        assert!(failed.is_err());
    }

    #[test]
    fn parses_porcelain_z() {
        let out = "worktree /code/app\0HEAD aaa\0branch refs/heads/main\0\0\
                   worktree /code/app-worktrees/feat-x\0HEAD bbb\0branch refs/heads/feat/x\0locked\0\0\
                   worktree /tmp/gone\0HEAD ccc\0detached\0prunable gitdir file points to non-existent location\0\0";
        let wts = parse_worktrees(out);
        assert_eq!(wts.len(), 3);
        assert!(wts[0].is_main && !wts[1].is_main && !wts[2].is_main);
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert_eq!(wts[1].path, PathBuf::from("/code/app-worktrees/feat-x"));
        assert_eq!(wts[1].branch.as_deref(), Some("feat/x"));
        assert!(wts[1].locked && !wts[1].prunable);
        assert_eq!(wts[2].head, "ccc");
        assert_eq!(wts[2].branch, None);
        assert!(wts[2].prunable);
    }
}
