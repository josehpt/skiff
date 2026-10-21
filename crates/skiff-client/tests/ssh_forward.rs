//! The M2 review's fixtures, kept as regressions: a fake `ssh` (a Python
//! Unix-socket server, never OpenSSH, never a network connection) that binds
//! only the `-L` path it is given and answers `ping` with the Host id in a
//! private file. Skips when this machine has no python3.

use std::path::{Path, PathBuf};

use skiff_client::host::{
    local::HostSelection,
    ssh::{SshForward, SshOptions, SshTarget},
};
use skiff_host_api::{Error, HostId};

struct Fixture {
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    fn new(name: &str) -> Option<Self> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            eprintln!("skipped: no /usr/bin/python3");
            return None;
        }
        // Short: the forward's socket path must fit in sun_path.
        let dir = PathBuf::from("/tmp").join(format!("sf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let id = dir.join("remote-id");
        let script = format!(
            "#!/usr/bin/python3\n\
             import socket,sys,json,threading\n\
             spec=sys.argv[sys.argv.index('-L')+1]\n\
             s=socket.socket(socket.AF_UNIX); s.bind(spec.split(':')[0]); s.listen()\n\
             def serve(c):\n\
             \x20with c, c.makefile('rb') as f:\n\
             \x20 for line in f:\n\
             \x20  req=json.loads(line)\n\
             \x20  if req.get('cmd')=='ping':\n\
             \x20   hid=open('{id}').read()\n\
             \x20   data={{'id':req['id'],'type':'pong','version':'1.6.6','protocol':27,'identity':{{'host_id':hid,'instance_id':'run','os':'fixture','account':'fixture'}}}}\n\
             \x20   c.sendall((json.dumps(data)+'\\n').encode())\n\
             while True:\n\
             \x20c,_=s.accept();threading.Thread(target=serve,args=(c,),daemon=True).start()\n",
            id = id.display()
        );
        let ssh = dir.join("fake-ssh");
        std::fs::write(&ssh, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        Some(Self { dir })
    }

    fn report(&self, host_id: &str) {
        std::fs::write(self.dir.join("remote-id"), host_id).unwrap();
    }

    fn options(&self) -> SshOptions {
        SshOptions {
            program: self.dir.join("fake-ssh"),
            config: None,
        }
    }

    fn target(&self) -> SshTarget {
        SshTarget {
            destination: "fixture".into(),
            socket: "/fixture.sock".into(),
        }
    }

    fn selection(&self, pin: Option<&str>) -> HostSelection {
        let pin = pin.map(|p| HostId::try_from(p.to_string()).unwrap());
        HostSelection::ssh(self.target(), self.options(), self.dir.join("fwd"), pin).unwrap()
    }
}

async fn activate(selection: &HostSelection) -> anyhow::Result<()> {
    let mut c = selection.connect().await?;
    c.daemon_info().await?;
    c.activate().await.map(drop)
}

fn changed(r: anyhow::Result<()>) -> bool {
    matches!(
        r.err().as_ref().and_then(|e| e.downcast_ref()),
        Some(Error::IdentityChanged { .. })
    )
}

/// Review P1: accept A; the same endpoint then reports B. A reconnect refused
/// B, and so must a selection rebuilt with the stored pin, as after picking
/// another Host and back, or a restart.
#[tokio::test]
async fn a_changed_identity_stays_refused_after_reselection() {
    let Some(f) = Fixture::new("pin") else { return };
    let first = f.selection(None);
    f.report("A");
    activate(&first).await.unwrap();
    // Activation pins nothing: the desktop pins only what it stored in
    // hosts.toml, so a failed store leaves nothing to skip it on a retry.
    assert_eq!(first.pinned(), None, "activation pinned before the store");
    let pin = HostId::try_from("A".to_string()).unwrap();
    first.repin(pin.clone());
    f.report("B");
    assert!(changed(activate(&first).await), "same selection accepted B");
    drop(first);
    let reselected = f.selection(Some(pin.as_str()));
    assert!(
        changed(activate(&reselected).await),
        "reselected endpoint accepted B"
    );
    drop(reselected);
    // Only an accepted new identity (the person's step) connects to B.
    let accepted = f.selection(Some("B"));
    activate(&accepted).await.unwrap();
    // The store held another id than the one first accepted here (another
    // window won): after a repin this selection refuses what it accepted.
    accepted.repin(HostId::try_from("A".to_string()).unwrap());
    assert!(changed(activate(&accepted).await));
}

/// Review P2: two windows or app runs open the same destination. Each forward
/// has its own socket, and dropping one leaves the other's in place.
#[tokio::test]
async fn two_forwards_to_one_destination_never_share_a_socket() {
    let Some(f) = Fixture::new("own") else { return };
    let parent = f.dir.join("fwd");
    let first = SshForward::open(&f.target(), &f.options(), &parent)
        .await
        .unwrap();
    let second = SshForward::open(&f.target(), &f.options(), &parent)
        .await
        .unwrap();
    assert_ne!(first.socket(), second.socket());
    let kept = second.socket().to_path_buf();
    drop(first);
    assert!(
        kept.exists(),
        "dropping the first forward removed the second's socket"
    );
    drop(second);
    assert!(!kept.exists() && !kept.parent().unwrap().exists());
    // The shared parent stays for other owners.
    assert!(parent.is_dir());
}
