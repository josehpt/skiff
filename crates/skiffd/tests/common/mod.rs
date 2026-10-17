use std::{path::Path, process::Child, time::{Duration, Instant}};

/// Stop only this test's daemon through its private socket. Cleanup must
/// never send a process signal, including when a test panics.
pub fn stop(child: &mut Child, dir: &Path) -> bool {
    if child.try_wait().ok().flatten().is_some() {
        return true;
    }
    let socket = dir.join("skiffd.sock");
    // Drop can run inside a Tokio test runtime. A separate thread can use
    // its own runtime without nesting block_on calls.
    let _ = std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
            return;
        };
        runtime.block_on(async {
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Ok(client) = skiff_client::Client::connect(&socket).await {
                        if client.shutdown().await.is_ok() {
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }).await;
        });
    }).join();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                eprintln!("test daemon {} did not exit; kept {} for inspection", child.id(), dir.display());
                assert!(std::thread::panicking(), "test daemon cleanup failed");
                return false;
            }
        }
    }
}
