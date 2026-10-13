//! Puts the bundled `skiff` CLI on the user's PATH, so the CLI ships and
//! updates with the app. The .deb and .rpm install it to /usr/bin, so they
//! need nothing here. The AppImage and the macOS app keep it inside the
//! bundle: the app copies or links it out on every start.
//!
//! The Linux dev build (`pnpm tauri dev`) builds the CLI beside the app
//! binary and links ~/.local/bin/skiff to it on every start, so the `skiff`
//! on PATH is the one built from this tree. It replaces only a link or a
//! skiff CLI there.

use std::path::{Path, PathBuf};

/// The CLI next to the app binary, if this build ships one.
fn bundled() -> Option<PathBuf> {
    let cli = std::env::current_exe().ok()?.with_file_name("skiff");
    cli.is_file().then_some(cli)
}

/// Runs at app start, off the main thread. Never prompts.
pub fn sync() {
    if let Err(e) = imp::sync() {
        eprintln!("skiff CLI: {e:#}");
    }
}

/// The palette and menu command. On macOS it asks for an admin password
/// when /usr/local/bin is not writable. Returns where the CLI is.
#[tauri::command]
pub async fn install_cli() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| imp::install().map(|p| p.display().to_string()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

/// A `skiff` that a Skiff bundle placed: a link into a Skiff.app, or a copy
/// that answers `--version` as the skiff CLI. Anything else belongs to the
/// user and stays.
#[allow(dead_code)]
fn ours(dest: &Path) -> bool {
    if let Ok(target) = std::fs::read_link(dest) {
        return target.to_string_lossy().ends_with(".app/Contents/MacOS/skiff");
    }
    std::process::Command::new(dest)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).starts_with("skiff "))
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{bundled, ours};
    use anyhow::{bail, Context};
    use std::path::{Path, PathBuf};

    /// The AppImage's mount point changes on every start, so it copies the
    /// CLI to ~/.local/bin, which most distros put on PATH. A dev build
    /// links it there instead, so it follows each rebuild.
    pub fn sync() -> anyhow::Result<()> {
        if std::env::var_os("APPIMAGE").is_none() {
            if cfg!(debug_assertions) {
                let src = bundled().context("this dev build has no skiff CLI beside the app")?;
                let home = std::env::var_os("HOME").context("HOME is not set")?;
                dev_link(&src, &PathBuf::from(home).join(".local/bin"))?;
            }
            return Ok(());
        }
        install().map(|_| ())
    }

    /// Links `dir/skiff` to `src`. Replaces only a link or a skiff CLI: a
    /// file of the user's stays.
    pub(super) fn dev_link(src: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
        let dest = dir.join("skiff");
        if std::fs::read_link(&dest).is_ok_and(|t| t == src) {
            return Ok(dest);
        }
        if let Ok(meta) = dest.symlink_metadata() {
            if !meta.file_type().is_symlink() && !ours(&dest) {
                bail!("{} is not the skiff CLI. Left as it is", dest.display());
            }
        }
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        // Link beside it and rename, so `skiff` never goes missing.
        let tmp = dir.join(".skiff.link");
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(src, &tmp).with_context(|| format!("link {}", tmp.display()))?;
        std::fs::rename(&tmp, &dest).with_context(|| format!("replace {}", dest.display()))?;
        Ok(dest)
    }

    pub fn install() -> anyhow::Result<PathBuf> {
        if std::env::var_os("APPIMAGE").is_none() {
            if let Some(cli) = bundled() {
                return Ok(cli);
            }
            bail!("this build has no skiff CLI");
        }
        let src = bundled().context("the AppImage has no skiff CLI")?;
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        let dir = PathBuf::from(home).join(".local/bin");
        let dest = dir.join("skiff");
        let new = std::fs::read(&src).with_context(|| format!("read {}", src.display()))?;
        if dest.symlink_metadata().is_ok() {
            if std::fs::read(&dest).is_ok_and(|old| old == new) {
                return Ok(dest);
            }
            if !ours(&dest) {
                bail!("{} is not the skiff CLI. Remove it, then try again", dest.display());
            }
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        // Write beside it and rename, so a running `skiff` never sees half a file.
        let tmp = dir.join(".skiff.new");
        std::fs::write(&tmp, &new).with_context(|| format!("write {}", tmp.display()))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, &dest).with_context(|| format!("replace {}", dest.display()))?;
        Ok(dest)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{bundled, ours};
    use anyhow::{bail, Context};
    use std::path::{Path, PathBuf};

    /// /usr/local/bin is on the default macOS PATH (/etc/paths).
    /// /opt/homebrew/bin is on PATH for Homebrew users on Apple silicon.
    const DIRS: [&str; 2] = ["/usr/local/bin", "/opt/homebrew/bin"];

    fn source() -> anyhow::Result<PathBuf> {
        let src = bundled().context("this build has no skiff CLI")?;
        let s = src.to_string_lossy();
        // A link into the disk image or a translocated copy breaks later.
        if s.starts_with("/Volumes/") || s.contains("/AppTranslocation/") {
            bail!("move Skiff to Applications first, then try again");
        }
        Ok(src)
    }

    fn links_to(dest: &Path, src: &Path) -> bool {
        std::fs::read_link(dest).is_ok_and(|t| t == src)
    }

    /// Links where it can without a password. Fixes a link that points at
    /// an old place of the app. Never prompts.
    pub fn sync() -> anyhow::Result<()> {
        let Ok(src) = source() else { return Ok(()) };
        if DIRS.iter().any(|d| links_to(&Path::new(d).join("skiff"), &src)) {
            return Ok(());
        }
        for dir in DIRS {
            if try_link(Path::new(dir), &src).is_ok() {
                return Ok(());
            }
        }
        Ok(())
    }

    fn try_link(dir: &Path, src: &Path) -> anyhow::Result<PathBuf> {
        let dest = dir.join("skiff");
        if dest.symlink_metadata().is_ok() {
            if !ours(&dest) {
                bail!("{} is not the skiff CLI", dest.display());
            }
            std::fs::remove_file(&dest)?;
        }
        std::os::unix::fs::symlink(src, &dest)?;
        Ok(dest)
    }

    pub fn install() -> anyhow::Result<PathBuf> {
        let src = source()?;
        for dir in DIRS {
            let dest = Path::new(dir).join("skiff");
            if links_to(&dest, &src) {
                return Ok(dest);
            }
        }
        for dir in DIRS {
            if let Ok(dest) = try_link(Path::new(dir), &src) {
                return Ok(dest);
            }
        }
        let dest = Path::new(DIRS[0]).join("skiff");
        if dest.symlink_metadata().is_ok() && !ours(&dest) {
            bail!("{} is not the skiff CLI. Remove it, then try again", dest.display());
        }
        let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', r"'\''"));
        let sh = format!("mkdir -p {} && ln -sf {} {}", DIRS[0], quote(&src), quote(&dest));
        let script = format!(
            "do shell script \"{}\" with prompt \"Skiff wants to add the skiff command to {}.\" with administrator privileges",
            sh.replace('\\', "\\\\").replace('"', "\\\""),
            DIRS[0],
        );
        let out = std::process::Command::new("osascript").arg("-e").arg(&script).output()?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("-128") {
                bail!("cancelled");
            }
            bail!("osascript: {}", err.trim());
        }
        Ok(dest)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use std::path::PathBuf;
    pub fn sync() -> anyhow::Result<()> {
        Ok(())
    }
    pub fn install() -> anyhow::Result<PathBuf> {
        anyhow::bail!("not supported on this platform")
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::imp::dev_link;

    #[test]
    fn dev_link_replaces_a_link_and_keeps_a_file_of_the_users() {
        let tmp = std::env::temp_dir().join(format!("skiff-cli-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let bin = tmp.join("home/.local/bin");
        let src = tmp.join("target/debug/skiff");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "").unwrap();
        // No file yet: the link is made, folder and all.
        let dest = dev_link(&src, &bin).unwrap();
        assert_eq!(std::fs::read_link(&dest).unwrap(), src);
        // A link elsewhere is replaced.
        std::fs::remove_file(&dest).unwrap();
        std::os::unix::fs::symlink("/nowhere/skiff", &dest).unwrap();
        dev_link(&src, &bin).unwrap();
        assert_eq!(std::fs::read_link(&dest).unwrap(), src);
        // A file that is not the skiff CLI stays.
        std::fs::remove_file(&dest).unwrap();
        std::fs::write(&dest, "#!/bin/sh\necho mine\n").unwrap();
        assert!(dev_link(&src, &bin).is_err());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "#!/bin/sh\necho mine\n");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
