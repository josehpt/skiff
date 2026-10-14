use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use crate::connection::{ensure_client, App};

/// The page's latency trace is on: output streams carry skiffd's echo times.
static TRACING: AtomicBool = AtomicBool::new(false);

pub(crate) fn tracing_on() -> bool {
    TRACING.load(Ordering::Relaxed)
}

/// Turns the echo times on or off. Returns the builds, as
/// `app debug, skiffd release`, so a trace can compare the two.
#[tauri::command]
pub(crate) async fn trace_set(app: State<'_, App>, on: bool) -> Result<String, String> {
    TRACING.store(on, Ordering::Relaxed);
    let build = |debug: bool| if debug { "debug" } else { "release" };
    let daemon = match ensure_client(&app).await {
        Ok((c, _)) => c.daemon_info().await.ok().and_then(|i| i.debug).map_or("unknown", build),
        Err(_) => "unknown",
    };
    Ok(format!("app {}, skiffd {daemon}", build(cfg!(debug_assertions))))
}

/// System load for the latency trace. `cpu` and `io`: the share of the last
/// 10 s that some task waited for that resource (Linux pressure stall
/// information, `some avg10`). `load`: the 1-minute load average.
#[derive(Serialize, Default)]
pub(crate) struct Load {
    cpu: Option<f32>,
    io: Option<f32>,
    load: Option<f32>,
}

#[tauri::command]
pub(crate) fn system_load() -> Load {
    let mut load = Load::default();
    #[cfg(target_os = "linux")]
    {
        let avg10 = |file: &str| {
            let text = std::fs::read_to_string(format!("/proc/pressure/{file}")).ok()?;
            let some = text.lines().find(|l| l.starts_with("some "))?;
            some.split_whitespace().find_map(|f| f.strip_prefix("avg10="))?.parse().ok()
        };
        load.cpu = avg10("cpu");
        load.io = avg10("io");
    }
    #[cfg(unix)]
    {
        let mut avg = [0f64; 3];
        // SAFETY: getloadavg writes at most 3 values into `avg`.
        if unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) } >= 1 {
            load.load = Some(avg[0] as f32);
        }
    }
    load
}

/// Where the keystroke latency trace goes: `SKIFF_TRACE=<file>`, or
/// latency.jsonl in the app's log folder.
fn trace_file(handle: &AppHandle) -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("SKIFF_TRACE") {
        return Ok(PathBuf::from(p));
    }
    let dir = handle.path().app_log_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("latency.jsonl"))
}

/// The trace file, and whether SKIFF_TRACE asks for tracing from launch.
#[tauri::command]
pub(crate) fn trace_info(handle: AppHandle) -> Result<(String, bool), String> {
    let path = trace_file(&handle)?;
    Ok((path.display().to_string(), std::env::var_os("SKIFF_TRACE").is_some()))
}

/// Appends JSON lines from the page's latency trace.
#[tauri::command]
pub(crate) fn trace_write(handle: AppHandle, lines: Vec<String>) -> Result<(), String> {
    use std::io::Write;
    let path = trace_file(&handle)?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    for line in lines {
        writeln!(f, "{line}").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Memory in bytes: this app with its WebKit children, and skiffd without the
/// sessions it runs. Linux reports proportional set size, so shared libraries
/// count once; macOS reports resident set size.
#[tauri::command]
pub(crate) fn memory_usage() -> Option<(u64, u64)> {
    #[cfg(target_os = "linux")]
    {
        let procs: Vec<(u32, u32, String)> = std::fs::read_dir("/proc")
            .ok()?
            .filter_map(|e| {
                let pid: u32 = e.ok()?.file_name().to_str()?.parse().ok()?;
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                // `pid (comm) state ppid ...`; comm may hold spaces and parens.
                let (head, tail) = stat.rsplit_once(')')?;
                let comm = head.split_once('(')?.1.to_string();
                let ppid = tail.split_whitespace().nth(1)?.parse().ok()?;
                Some((pid, ppid, comm))
            })
            .collect();
        let me = std::process::id();
        let app = std::iter::once(me)
            .chain(procs.iter().filter(|p| p.1 == me).map(|p| p.0))
            .map(pss)
            .sum();
        let daemon = procs.iter().filter(|p| p.2 == "skiffd").map(|p| pss(p.0)).sum();
        Some((app, daemon))
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("ps")
            .args(["-axo", "pid=,ppid=,rss=,comm="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let procs: Vec<(u32, u32, u64, String)> = String::from_utf8(output.stdout)
            .ok()?
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid = fields.next()?.parse().ok()?;
                let ppid = fields.next()?.parse().ok()?;
                let rss_kb: u64 = fields.next()?.parse().ok()?;
                let command = fields.next()?.to_string();
                Some((pid, ppid, rss_kb * 1024, command))
            })
            .collect();
        let me = std::process::id();
        let app = procs
            .iter()
            .filter(|p| p.0 == me || p.1 == me)
            .map(|p| p.2)
            .sum();
        let daemon = procs
            .iter()
            .filter(|p| std::path::Path::new(&p.3).file_name().is_some_and(|name| name == "skiffd"))
            .map(|p| p.2)
            .sum();
        Some((app, daemon))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    None
}

#[cfg(target_os = "linux")]
fn pss(pid: u32) -> u64 {
    let kb = |file: &str, key: &str| {
        let text = std::fs::read_to_string(format!("/proc/{pid}/{file}")).ok()?;
        let line = text.lines().find(|l| l.starts_with(key))?;
        line[key.len()..].trim().trim_end_matches("kB").trim().parse::<u64>().ok()
    };
    kb("smaps_rollup", "Pss:").or_else(|| kb("status", "VmRSS:")).unwrap_or(0) * 1024
}
