//! M5: a worktree's files and a project's background through skiff-host,
//! on a private socket and private state (never the live socket, config or
//! Host id), reached as the app reaches it: over this Mac's socket and over
//! a fake `ssh` forward. Both routes must answer the same bytes as the core
//! code; nothing leaves a known worktree, follows a link or blocks.
#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use skiff_client::host::{
    data,
    local::HostSelection,
    ssh::{SshOptions, SshTarget},
};
use skiff_core::files;
use skiff_host_api::{Capability, EntryKind, Error, FileContent, Hello, ReadPurpose};
use tokio::{net::UnixListener, task::JoinHandle};

struct Guard(PathBuf);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "init"]);
}

fn start(socket: &Path, hello: Hello) -> JoinHandle<()> {
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        let _ = skiff_host::serve(listener, Arc::new(hello)).await;
    })
}

/// A fake ssh: binds the -L path and pipes each connection to the remote
/// path, which here is a socket on this machine.
fn fake_ssh(dir: &Path) -> Option<PathBuf> {
    if !Path::new("/usr/bin/python3").is_file() {
        eprintln!("skipped the SSH route: no /usr/bin/python3");
        return None;
    }
    let script = r#"#!/usr/bin/python3
import socket,sys,threading
spec=sys.argv[sys.argv.index('-L')+1]
local,remote=spec.split(':',1)
s=socket.socket(socket.AF_UNIX); s.bind(local); s.listen()
def pipe(a,b):
 try:
  while True:
   d=a.recv(65536)
   if not d: break
   b.sendall(d)
 except OSError: pass
 for x in (a,b):
  try: x.shutdown(socket.SHUT_RDWR)
  except OSError: pass
while True:
 c,_=s.accept()
 r=socket.socket(socket.AF_UNIX)
 try: r.connect(remote)
 except OSError:
  c.close(); continue
 threading.Thread(target=pipe,args=(c,r),daemon=True).start()
 threading.Thread(target=pipe,args=(r,c),daemon=True).start()
"#;
    let ssh = dir.join("fake-ssh");
    std::fs::write(&ssh, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    Some(ssh)
}

#[tokio::test(flavor = "multi_thread")]
async fn files_and_backgrounds_come_from_the_host_bounded_and_inside_its_worktrees() {
    let dir = PathBuf::from("/tmp").join(format!("shf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = Guard(dir.clone());
    let socket = dir.join("skiffd.sock");
    skiff_core::state::set_socket(socket.clone());
    assert!(skiff_core::state::private_dir().is_some(), "never the live state");

    let app = dir.join("app");
    repo(&app);
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(app.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(app.join(".gitignore"), "target/\n").unwrap();
    std::fs::create_dir_all(app.join("target")).unwrap();
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[7u8; 64]].concat();
    std::fs::write(app.join("logo.png"), &png).unwrap();
    std::fs::write(app.join("blob.bin"), [0u8, 1, 2]).unwrap();
    let mut big = png.clone();
    big.resize(files::BYTES_LIMIT as usize, 1);
    std::fs::write(app.join("big.png"), &big).unwrap();
    std::fs::File::create(app.join("huge.bin")).unwrap().set_len(files::BYTES_LIMIT + 1).unwrap();
    std::fs::write(app.join("long.txt"), "y".repeat(99) + "\n").unwrap();
    let long = "z".repeat(999) + "\n";
    std::fs::write(app.join("long.txt"), long.repeat(files::TEXT_LIMIT / 1000 + 50)).unwrap();
    std::fs::write(app.join("bg.txt"), "not an image").unwrap();
    // Outside every worktree: a secret, a closed project, links to both.
    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret"), "secret").unwrap();
    std::os::unix::fs::symlink(outside.join("secret"), app.join("link")).unwrap();
    std::os::unix::fs::symlink(&outside, app.join("dirlink")).unwrap();
    assert!(Command::new("mkfifo").arg(app.join("pipe")).status().unwrap().success());
    let closed = dir.join("closed");
    repo(&closed);
    let plain = dir.join("plain");
    repo(&plain);
    std::fs::write(plain.join("bg.jpg"), [0xffu8, 0xd8, 0xff, 0xe0, 1, 2]).unwrap();
    // Backgrounds that are a link, or go through one, to an image outside;
    // and an absolute regular image, named without a link.
    std::fs::write(outside.join("bg.png"), &png).unwrap();
    let linkbg = dir.join("linkbg");
    repo(&linkbg);
    std::os::unix::fs::symlink(outside.join("bg.png"), linkbg.join("bg-link.png")).unwrap();
    let dirbg = dir.join("dirbg");
    repo(&dirbg);
    std::os::unix::fs::symlink(&outside, dirbg.join("art")).unwrap();
    let absbg = dir.join("absbg");
    repo(&absbg);
    let absolute = outside.canonicalize().unwrap().join("bg.png");

    let config = skiff_core::config::config_path();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        format!(
            "[[project]]\nname = \"app\"\npath = \"{}\"\nbackground = \"bg.txt\"\n\n\
             [[project]]\nname = \"plain\"\npath = \"{}\"\nbackground = \"bg.jpg\"\n\n\
             [[project]]\nname = \"closed\"\npath = \"{}\"\nclosed = true\n\n\
             [[project]]\nname = \"linkbg\"\npath = \"{}\"\nbackground = \"bg-link.png\"\n\n\
             [[project]]\nname = \"dirbg\"\npath = \"{}\"\nbackground = \"art/bg.png\"\n\n\
             [[project]]\nname = \"absbg\"\npath = \"{}\"\nbackground = \"{}\"\n",
            app.display(),
            plain.display(),
            closed.display(),
            linkbg.display(),
            dirbg.display(),
            absbg.display(),
            absolute.display()
        ),
    )
    .unwrap();

    let hello = skiff_host::hello().unwrap();
    for c in [Capability::files(), Capability::project_assets()] {
        assert!(hello.capabilities.contains(&c), "{c:?}");
    }
    let host = hello.host.host_id.clone();
    let _service = start(&skiff_core::socket::data_socket(&socket), hello);
    let local = HostSelection::new(socket.clone(), dir.join("bootstrap-id")).connect_data(&host).await.unwrap();
    let l = local.connection();
    let remote = match fake_ssh(&dir) {
        Some(ssh) => Some(
            HostSelection::ssh(
                SshTarget { destination: "devbox".into(), socket: socket.to_string_lossy().into_owned() },
                SshOptions { program: ssh, config: None },
                dir.join("fwd"),
                Some(host.clone()),
            )
            .unwrap()
            .connect_data(&host)
            .await
            .unwrap(),
        ),
        None => None,
    };
    let r = remote.as_ref().map(|p| p.connection()).unwrap_or_else(|| l.clone());
    let wt = app.to_string_lossy().into_owned();
    let root = skiff_core::git::Root::open(&app).unwrap();

    // Listing: the core's own answer, on both routes. Links and the FIFO are
    // listed by type, ignored folders are not.
    for rel in ["", "src"] {
        let core = skiff_host::host_listing(files::list_in(&root, &app, rel).unwrap());
        assert_eq!(data::file_list(&l, &wt, rel).await.unwrap(), core, "{rel}");
        assert_eq!(data::file_list(&r, &wt, rel).await.unwrap(), core, "{rel}");
    }
    let top = data::file_list(&r, &wt, "").await.unwrap();
    let kind = |n: &str| top.entries.iter().find(|e| e.name == n).map(|e| e.kind);
    assert_eq!((kind("src"), kind("link"), kind("dirlink"), kind("pipe"), kind("target")), (Some(EntryKind::Dir), Some(EntryKind::Link), Some(EntryKind::Link), Some(EntryKind::Other), None));

    // Reads: the core's bytes, on both routes, typed by content.
    for (rel, purpose, content) in [
        ("src/main.rs", ReadPurpose::Preview, FileContent::Text),
        ("logo.png", ReadPurpose::Preview, FileContent::Image),
        ("blob.bin", ReadPurpose::Preview, FileContent::Binary),
        ("link", ReadPurpose::Preview, FileContent::Link),
        ("long.txt", ReadPurpose::Preview, FileContent::Text),
        ("blob.bin", ReadPurpose::Copy, FileContent::Binary),
        ("big.png", ReadPurpose::Copy, FileContent::Image),
    ] {
        let p = if purpose == ReadPurpose::Copy { files::Purpose::Copy } else { files::Purpose::Preview };
        let core = skiff_host::host_file(files::read_in(&root, &app, rel, p).unwrap());
        assert_eq!(core.content, content, "{rel}");
        assert_eq!(data::file_read(&l, &wt, rel, purpose).await.unwrap(), core, "{rel}");
        assert_eq!(data::file_read(&r, &wt, rel, purpose).await.unwrap(), core, "{rel}");
    }
    let long = data::file_read(&r, &wt, "long.txt", ReadPurpose::Preview).await.unwrap();
    assert!(long.truncated && long.text.unwrap().len() <= files::TEXT_LIMIT);
    // A 5 MiB image fits one frame, as a preview and as a copy.
    let shown = data::file_read(&r, &wt, "big.png", ReadPurpose::Preview).await.unwrap();
    assert_eq!(shown.bytes.as_deref(), Some(big.as_slice()));
    let link = data::file_read(&r, &wt, "link", ReadPurpose::Preview).await.unwrap();
    assert_eq!((link.bytes, link.text), (None, None), "a link is never followed");

    // Typed refusals; none reads outside, follows a link or blocks.
    let started = Instant::now();
    for rel in ["../outside/secret", "/etc/hosts", "dirlink/secret", "pipe", "src", "missing", ""] {
        let got = data::file_read(&r, &wt, rel, ReadPurpose::Preview).await;
        assert!(matches!(got, Err(Error::NotFound { .. })), "{rel}: {got:?}");
    }
    for rel in ["dirlink", "../outside", "/", "logo.png", "nope"] {
        let got = data::file_list(&r, &wt, rel).await;
        assert!(matches!(got, Err(Error::NotFound { .. })), "{rel}: {got:?}");
    }
    assert!(started.elapsed() < Duration::from_secs(10), "a refusal waited");
    for rel in ["huge.bin", "link"] {
        let got = data::file_read(&r, &wt, rel, ReadPurpose::Copy).await;
        assert!(matches!(got, Err(Error::Invalid { .. })), "{rel}: {got:?}");
    }
    // Only open projects' worktrees: never a closed one, a parent or `/`.
    for w in [closed.to_string_lossy().into_owned(), dir.to_string_lossy().into_owned(), "/".into(), "app".into()] {
        assert!(matches!(data::file_list(&r, &w, "").await, Err(Error::NotFound { .. })), "{w}");
        assert!(matches!(data::file_read(&r, &w, "a.txt", ReadPurpose::Copy).await, Err(Error::NotFound { .. })), "{w}");
    }

    // Backgrounds: bytes the Host checked, never a path.
    let bg = data::project_background(&r, "plain").await.unwrap();
    assert_eq!((bg.mime.as_str(), bg.bytes.as_slice()), ("image/jpeg", [0xffu8, 0xd8, 0xff, 0xe0, 1, 2].as_slice()));
    assert_eq!(data::project_background(&l, "plain").await.unwrap(), bg);
    assert!(matches!(data::project_background(&r, "app").await, Err(Error::Invalid { .. })), "a text file named as the background");
    assert!(matches!(data::project_background(&r, "closed").await, Err(Error::NotFound { .. })), "no background");
    assert!(matches!(data::project_background(&r, "ghost").await, Err(Error::NotFound { .. })));
    // Never through a link: the entry itself, or a folder on the way. The
    // preview says the same entry is a link.
    assert_eq!(data::file_read(&r, &linkbg.to_string_lossy(), "bg-link.png", ReadPurpose::Preview).await.unwrap().content, FileContent::Link);
    for c in [&l, &r] {
        assert!(matches!(data::project_background(c, "linkbg").await, Err(Error::Invalid { .. })), "a link as the background");
        assert!(matches!(data::project_background(c, "dirbg").await, Err(Error::NotFound { .. })), "a background through a folder link");
        let abs = data::project_background(c, "absbg").await.unwrap();
        assert_eq!((abs.mime.as_str(), abs.bytes.as_slice()), ("image/png", png.as_slice()), "an absolute regular image");
    }
}
