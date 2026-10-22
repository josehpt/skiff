//! Local copies of a Host's files, for Open with (M5). A remote Host's path
//! is never opened here: the app fetches the file's bytes (bounded by the
//! Host, `files@1` with the copy purpose) and writes them to a copy it
//! manages, and only the copy goes to an app on this Mac.
//!
//! - One private folder per account (`$TMPDIR/skiff-copies-<uid>`, `0700`,
//!   owned by this account and not a link, else refused), one folder per app
//!   process in it, one folder per copy, so two files with one name never meet.
//! - A copy is read-only (`0400`) and never executable. On macOS it carries
//!   the quarantine flag, as a download does. Types that launch or install
//!   something when opened are refused.
//! - At most [`KEEP`] copies per process: the oldest go. The process's folder
//!   goes when the app quits, and the folders of processes that no longer
//!   run go when the app starts.
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{bail, Context, Result};

/// Copies kept per app process.
const KEEP: usize = 32;

/// File types that run, install or redirect something when opened: never
/// handed to an app from a Host.
const REFUSED: &[&str] = &[
    "app", "command", "tool", "terminal", "workflow", "action", "scpt", "scptd", "applescript", "osascript",
    "pkg", "mpkg", "dmg", "jar", "webloc", "inetloc", "fileloc", "url", "desktop", "prefpane", "kext",
    "plugin", "bundle", "shortcut", "service", "mobileconfig", "appimage", "deb", "rpm", "run", "sh", "bash",
    "zsh", "csh", "ksh", "fish",
];

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The account's folder for copies.
pub(crate) fn base() -> PathBuf {
    // SAFETY: geteuid takes no arguments and cannot fail.
    std::env::temp_dir().join(format!("skiff-copies-{}", unsafe { libc::geteuid() }))
}

/// `base`, made if missing, and checked: a folder of this account, not a
/// link, that no one else can open.
fn private(base: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(base) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).with_context(|| format!("create {}", base.display())),
    }
    let meta = std::fs::symlink_metadata(base)?;
    // SAFETY: as in `base`.
    let me = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.uid() != me || meta.permissions().mode() & 0o077 != 0 {
        bail!("{} is not a private folder of this account, so Skiff makes no copy there", base.display());
    }
    Ok(())
}

/// A name for the copy from the Host file's own: its last component, with
/// no path separator or control character, at most 120 bytes, its
/// extension kept. Refused for a type that runs something when opened.
pub(crate) fn copy_name(host_file: &str) -> Result<String> {
    let last = host_file.rsplit('/').next().unwrap_or_default();
    let mut name: String = last.chars().map(|c| if c.is_control() || c == '\\' { '_' } else { c }).collect();
    if name.is_empty() || name == "." || name == ".." {
        name = "file".into();
    }
    let ext = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if REFUSED.contains(&ext.as_str()) {
        bail!("Skiff does not open a copy of a .{ext} file: opening it could run or install something on this Mac.");
    }
    if name.len() > 120 {
        let tail = if ext.is_empty() || ext.len() > 16 { String::new() } else { format!(".{ext}") };
        let mut cut = 120 - tail.len();
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name = format!("{}{tail}", &name[..cut]);
    }
    Ok(name)
}

/// Writes `bytes` as a new read-only copy named for `host_file`, under
/// `base`'s folder for this process. Its path, to hand to an app.
pub(crate) fn save_in(base: &Path, host_file: &str, bytes: &[u8]) -> Result<PathBuf> {
    use std::{io::Write, os::unix::fs::{DirBuilderExt, OpenOptionsExt}};
    let name = copy_name(host_file)?;
    private(base)?;
    let mine = base.join(std::process::id().to_string());
    match std::fs::DirBuilder::new().mode(0o700).create(&mine) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).context("make the copy folder"),
    }
    if !std::fs::symlink_metadata(&mine)?.is_dir() {
        bail!("{} is not a folder", mine.display());
    }
    prune(&mine, KEEP - 1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let folder = mine.join(format!("{stamp:032}-{n}"));
    std::fs::DirBuilder::new().mode(0o700).create(&folder).context("make the copy folder")?;
    let path = folder.join(&name);
    // A new file only: never through a link, never over a file.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .with_context(|| format!("write {}", path.display()))?;
    file.write_all(bytes)?;
    // Flagged while it is still writable, then made read-only.
    quarantine(&file);
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o400))?;
    file.sync_all()?;
    Ok(path)
}

/// [`save_in`] the account's folder.
pub(crate) fn save(host_file: &str, bytes: &[u8]) -> Result<PathBuf> {
    save_in(&base(), host_file, bytes)
}

/// Keeps the newest `keep` copy folders of `mine`.
fn prune(mine: &Path, keep: usize) {
    let Ok(read) = std::fs::read_dir(mine) else { return };
    let mut folders: Vec<_> = read.flatten().map(|e| e.path()).collect();
    if folders.len() <= keep {
        return;
    }
    // Named by time first: the name order is the age order.
    folders.sort();
    for old in &folders[..folders.len() - keep] {
        let _ = remove(old);
    }
}

/// Removes a copy folder. Its files are read-only, the folder is not.
fn remove(path: &Path) -> std::io::Result<()> {
    std::fs::remove_dir_all(path)
}

/// The app quits: this process's copies go.
pub(crate) fn cleanup_in(base: &Path) {
    let _ = remove(&base.join(std::process::id().to_string()));
}

pub(crate) fn cleanup() {
    cleanup_in(&base());
}

/// The app starts: copies of app processes that no longer run go. Only
/// folders named by a process id, inside a private folder of this account.
pub(crate) fn sweep_in(base: &Path) {
    if std::fs::symlink_metadata(base).map(|m| !m.is_dir()).unwrap_or(true) || private(base).is_err() {
        return;
    }
    let Ok(read) = std::fs::read_dir(base) else { return };
    for e in read.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<libc::pid_t>().ok()) else { continue };
        // SAFETY: kill(2) with signal 0 only checks that the process exists.
        let alive = pid > 0 && (unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM));
        if !alive && e.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = remove(&e.path());
        }
    }
}

pub(crate) fn sweep() {
    sweep_in(&base());
}

/// Marks the copy as downloaded, so macOS checks it before it runs anything.
#[cfg(target_os = "macos")]
fn quarantine(file: &std::fs::File) {
    use std::os::fd::AsRawFd;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let value = format!("0081;{secs:x};Skiff;");
    // SAFETY: fsetxattr(2) on an open fd, a NUL-terminated name and a buffer of the given length.
    unsafe {
        libc::fsetxattr(file.as_raw_fd(), c"com.apple.quarantine".as_ptr(), value.as_ptr().cast(), value.len(), 0, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn quarantine(_: &std::fs::File) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Removes a test's folder, also when an assertion fails.
    struct Gone(PathBuf);
    impl Drop for Gone {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("skiff-copies-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn names_keep_the_type_and_refuse_what_runs() {
        assert_eq!(copy_name("src/main.rs").unwrap(), "main.rs");
        assert_eq!(copy_name("a\nb\\c.md").unwrap(), "a_b_c.md");
        assert_eq!(copy_name("..").unwrap(), "file");
        assert_eq!(copy_name(".env").unwrap(), ".env");
        let long = copy_name(&format!("{}.png", "é".repeat(100))).unwrap();
        assert!(long.len() <= 120 && long.ends_with(".png"), "{long}");
        for bad in ["x.command", "Y.APP", "z.webloc", "run.sh", "a.desktop", "i.pkg"] {
            assert!(copy_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn copies_are_private_read_only_bounded_and_cleaned_up() {
        let base = scratch("save");
        let _gone = Gone(base.clone());
        let a = save_in(&base, "docs/readme.md", b"hello").unwrap();
        let b = save_in(&base, "other/readme.md", b"world").unwrap();
        assert_ne!(a, b, "two files with one name never meet");
        assert_eq!(a.file_name().unwrap(), "readme.md");
        assert_eq!(std::fs::read(&a).unwrap(), b"hello");
        assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o400);
        assert_eq!(std::fs::metadata(&base).unwrap().permissions().mode() & 0o777, 0o700);
        #[cfg(target_os = "macos")]
        assert!(std::process::Command::new("xattr").arg("-p").arg("com.apple.quarantine").arg(&a).output().unwrap().status.success());
        // Bounded: the oldest go.
        for i in 0..KEEP + 3 {
            save_in(&base, &format!("f{i}.txt"), b"x").unwrap();
        }
        let mine = base.join(std::process::id().to_string());
        assert_eq!(std::fs::read_dir(&mine).unwrap().count(), KEEP);
        assert!(!a.exists());
        // A dead process's copies go at start; a live one's stay.
        let dead = base.join("999999");
        std::fs::create_dir_all(dead.join("x")).unwrap();
        std::fs::create_dir_all(base.join("not-a-pid")).unwrap();
        sweep_in(&base);
        assert!(!dead.exists() && mine.exists() && base.join("not-a-pid").exists());
        cleanup_in(&base);
        assert!(!mine.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_shared_or_linked_folder_is_refused() {
        let base = scratch("open");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o777)).unwrap();
        let open = save_in(&base, "a.txt", b"x");
        std::fs::remove_dir_all(&base).unwrap();
        let target = scratch("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&target, &base).unwrap();
        let linked = save_in(&base, "a.txt", b"x");
        let written = std::fs::read_dir(&target).unwrap().count();
        std::fs::remove_file(&base).unwrap();
        std::fs::remove_dir_all(&target).unwrap();
        assert!(open.is_err() && linked.is_err());
        assert_eq!(written, 0);
    }
}
