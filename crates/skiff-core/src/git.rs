//! Thin wrappers over the `git` binary. Blocking: async callers use
//! `spawn_blocking`.

use crate::shell::shell_quote;

use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
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

/// The changed files of the worktree at `dir`, sorted by path.
pub fn changes(dir: &Path) -> Result<Vec<Change>> {
    let status = git(dir, &["status", "--porcelain=v1", "-z", "--no-renames", "--untracked-files=all"])?;
    // Fails in a repository with no commit yet: every file then shows no count.
    let numstat = git(dir, &["diff", "HEAD", "--numstat", "-z", "--no-renames"]).unwrap_or_default();
    let mut counts = std::collections::HashMap::new();
    for rec in numstat.split('\0').filter(|r| !r.is_empty()) {
        let mut f = rec.splitn(3, '\t');
        let (Some(a), Some(r), Some(p)) = (f.next(), f.next(), f.next()) else { continue };
        counts.insert(p.to_string(), (a.parse().ok(), r.parse().ok()));
    }
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
            None if x == b'?' => (count_lines(dir, path), Some(0)),
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

/// Lines in a new file at `rel` under the worktree `dir`, as `git diff`
/// would count them. It never leaves the worktree and never blocks:
/// - a symlink is one line, its target, as git shows it; it is not followed
/// - a path through a symlinked folder, `..` or an absolute path: `None`
/// - a FIFO, device, socket or folder: `None`, never opened for reading
/// - a binary file, or one over [`COUNT_LIMIT`] bytes: `None`, and no
///   more than the limit is read even if the file grows meanwhile
fn count_lines(dir: &Path, rel: &str) -> Option<u32> {
    use std::io::Read;
    let file = open_beneath(dir, rel)?;
    let file = match file {
        Beneath::Symlink => return Some(1),
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
    /// The last component is a symlink. It was not followed.
    Symlink,
}

/// Opens `rel` under `dir` one component at a time, each relative to the
/// folder opened before it and none followed if it is a symlink, so a folder
/// swapped for a link meanwhile cannot lead outside `dir`. The file opens
/// non-blocking: a FIFO that slipped in fails its type check, not a read.
#[cfg(unix)]
fn open_beneath(dir: &Path, rel: &str) -> Option<Beneath> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt, os::fd::FromRawFd, path::Component};
    let parts: Vec<_> = Path::new(rel).components().collect();
    if parts.is_empty() || parts.iter().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    let root = CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: plain open(2) calls on NUL-terminated paths; every fd made here
    // is closed below or owned by the returned File.
    unsafe {
        let mut fd = libc::open(root.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC);
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
                        libc::close(fd);
                        return Some(Beneath::Symlink);
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
fn open_beneath(dir: &Path, rel: &str) -> Option<Beneath> {
    let path = dir.join(rel);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_symlink() {
        return Some(Beneath::Symlink);
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
            assert_eq!(count_lines(&root, rel), None, "{rel}");
        }
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
