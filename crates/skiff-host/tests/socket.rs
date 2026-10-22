//! Review M4 P1: startup once took any entry at the service path for a stale
//! socket and deleted it, a regular file included. Now only a stale socket
//! of this account is removed; anything else is refused and kept. Cleanup
//! removes only the socket this service bound. Private fixtures only.
#![cfg(unix)]

use std::{
    os::unix::fs::{symlink, FileTypeExt},
    path::{Path, PathBuf},
    process::Command,
};

mod common;
use common::{Guard};

fn is_socket(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_socket())
}

#[tokio::test]
async fn only_a_stale_socket_of_this_account_is_replaced_and_only_its_own_is_removed() {
    let dir = PathBuf::from("/tmp").join(format!("hs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = Guard(dir.clone());
    let path = dir.join("skiffd-host.sock");

    // A regular file: refused, its bytes kept.
    std::fs::write(&path, "keep me").unwrap();
    let e = skiff_host::bind(&path).await.err().unwrap();
    assert!(e.to_string().contains("is not a socket"), "{e:#}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
    std::fs::remove_file(&path).unwrap();

    // A symlink, even to a stale socket: refused, link and target kept.
    let target = dir.join("elsewhere.sock");
    drop(std::os::unix::net::UnixListener::bind(&target).unwrap());
    symlink(&target, &path).unwrap();
    assert!(skiff_host::bind(&path).await.is_err());
    assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
    assert!(is_socket(&target));
    std::fs::remove_file(&path).unwrap();

    // A folder: refused and kept.
    std::fs::create_dir(&path).unwrap();
    assert!(skiff_host::bind(&path).await.is_err());
    assert!(path.is_dir());
    std::fs::remove_dir(&path).unwrap();

    // A live socket: another service; it keeps its socket.
    let live = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let e = skiff_host::bind(&path).await.err().unwrap();
    assert!(e.to_string().contains("already running"), "{e:#}");
    drop(live);

    // The same socket now stale (its listener gone): replaced.
    assert!(is_socket(&path));
    let (listener, socket) = skiff_host::bind(&path).await.unwrap();
    assert!(std::os::unix::net::UnixStream::connect(&path).is_ok());

    // Release after another service took the path: that socket stays.
    drop(listener);
    std::fs::remove_file(&path).unwrap();
    let newer = std::os::unix::net::UnixListener::bind(&path).unwrap();
    socket.release();
    assert!(is_socket(&path), "release removed another owner's socket");
    assert!(std::os::unix::net::UnixStream::connect(&path).is_ok());
    drop(newer);
    std::fs::remove_file(&path).unwrap();

    // Release of its own socket removes it.
    let (listener, socket) = skiff_host::bind(&path).await.unwrap();
    drop(listener);
    socket.release();
    assert!(std::fs::symlink_metadata(&path).is_err());
}

/// The binary, as the app starts it: a file at its path stays, and it exits
/// with an error instead of serving there.
#[test]
fn the_service_refuses_to_start_over_a_regular_file() {
    let dir = PathBuf::from("/tmp").join(format!("hb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = Guard(dir.clone());
    let session = dir.join("skiffd.sock");
    let path = skiff_core::socket::data_socket(&session);
    std::fs::write(&path, "not a socket").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_skiff-host"))
        .arg("--session-socket")
        .arg(&session)
        .env_remove("SKIFF_CONFIG")
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // A regression would serve there: stop it rather than wait forever.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = child.kill();
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not a socket"), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not a socket");
}
