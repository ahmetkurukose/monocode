//! Child processes the code features start: language servers, debug adapters
//! and runs. Registered here they are listed with "the running work Monocode
//! owns" (`resource_pressure.rs`) with their memory, and can be stopped from
//! that list. The registry is process-wide and may be used from any thread.
use monocode_core::resources::{self, OwnedProcess};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessKind {
    Run,
    LanguageServer,
    DebugAdapter,
}
impl ProcessKind {
    /// The heading its processes are listed under.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Run => "Runs",
            Self::LanguageServer => "Language servers",
            Self::DebugAdapter => "Debugging",
        }
    }
    /// The list groups rows by session id; each kind takes one no session has.
    fn group(self) -> i64 {
        i64::MIN + self as i64
    }
}

/// The heading for a group id of the running work list, when it is one of
/// the code features' rather than a session's.
pub(crate) fn group_title(group: i64) -> Option<&'static str> {
    [
        ProcessKind::Run,
        ProcessKind::LanguageServer,
        ProcessKind::DebugAdapter,
    ]
    .into_iter()
    .find(|kind| kind.group() == group)
    .map(ProcessKind::title)
}

type Stop = Arc<dyn Fn() + Send + Sync>;

struct Owned {
    pid: u32,
    /// The process's birth time, so a reused pid is never listed or signalled.
    identity: String,
    label: String,
    kind: ProcessKind,
    stop: Option<Stop>,
}

static OWNED: Mutex<Vec<Owned>> = Mutex::new(Vec::new());

fn owned() -> std::sync::MutexGuard<'static, Vec<Owned>> {
    OWNED.lock().unwrap_or_else(|error| error.into_inner())
}

fn identity(pid: u32) -> String {
    resources::identity(pid).unwrap_or_else(|| format!("code:{pid}"))
}

fn alive(process: &Owned) -> bool {
    #[cfg(unix)]
    let running = unsafe { libc::kill(process.pid as i32, 0) } == 0;
    #[cfg(not(unix))]
    let running = true;
    running && identity(process.pid) == process.identity
}

/// Keeps a process listed for as long as it is held; dropping it takes the
/// process off the list (it does not stop it).
#[must_use = "the process is listed only while this is held"]
pub(crate) struct Registration(u32);
impl Drop for Registration {
    fn drop(&mut self) {
        unregister(self.0);
    }
}

/// Lists `pid` as running work under `kind`, named `label`. Stopping it from
/// the list terminates it, then kills it if it is still there two seconds on.
pub(crate) fn register(pid: u32, label: impl Into<String>, kind: ProcessKind) -> Registration {
    add(pid, label.into(), kind, None)
}

/// As `register`, with the feature's own way to stop the process (a server's
/// shutdown request, a debug session's disconnect). `stop` is called on a
/// background thread; the process is still killed if it outlives it.
pub(crate) fn register_with_stop(
    pid: u32,
    label: impl Into<String>,
    kind: ProcessKind,
    stop: impl Fn() + Send + Sync + 'static,
) -> Registration {
    add(pid, label.into(), kind, Some(Arc::new(stop)))
}

fn add(pid: u32, label: String, kind: ProcessKind, stop: Option<Stop>) -> Registration {
    let mut owned = owned();
    owned.retain(|process| process.pid != pid);
    owned.push(Owned {
        pid,
        identity: identity(pid),
        label,
        kind,
        stop,
    });
    Registration(pid)
}

pub(crate) fn unregister(pid: u32) {
    owned().retain(|process| process.pid != pid);
}

/// Opens the running work dialog, where these processes are listed with
/// their memory and a Stop each.
pub(crate) fn show_list(window: &mut gpui::Window, cx: &mut gpui::App) {
    super::with_shell(window, cx, |shell, window, cx| {
        shell.open_running_work(window, cx)
    });
}

/// Whether `pid` is one of the registered processes.
pub(crate) fn owns(pid: u32) -> bool {
    owned().iter().any(|process| process.pid == pid)
}

/// `pid -> (parent, cpu percent, resident bytes)` for every process.
fn snapshot() -> HashMap<u32, (u32, f64, u64)> {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .env("LC_ALL", "C")
        .args(["-axww", "-o", "pid=,ppid=,%cpu=,rss="])
        .output()
    else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            let cpu = fields.next()?.parse().ok()?;
            let memory = fields.next()?.parse::<u64>().ok()?.saturating_mul(1024);
            Some((pid, (parent, cpu, memory)))
        })
        .collect()
}

/// The registered processes that are still running, each with the CPU and
/// memory of everything it started. Ones that have gone are forgotten.
/// Blocking: it reads the process table.
pub(crate) fn rows() -> Vec<OwnedProcess> {
    owned().retain(alive);
    if owned().is_empty() {
        return Vec::new();
    }
    let processes = snapshot();
    let usage = |root: u32| {
        let (mut cpu, mut memory) = (0., 0u64);
        let mut family = vec![root];
        let mut next = 0;
        while let Some(&pid) = family.get(next) {
            next += 1;
            if let Some((_, own_cpu, own_memory)) = processes.get(&pid) {
                cpu += own_cpu;
                memory += own_memory;
            }
            for (child, (parent, _, _)) in &processes {
                if *parent == pid && !family.contains(child) {
                    family.push(*child);
                }
            }
        }
        (cpu, memory)
    };
    owned()
        .iter()
        .map(|process| {
            let (cpu_percent, memory_bytes) = usage(process.pid);
            OwnedProcess {
                pid: process.pid,
                identity: process.identity.clone(),
                session: process.kind.group(),
                command: process.label.clone(),
                cpu_percent,
                memory_bytes,
                agent: false,
            }
        })
        .collect()
}

/// Stops a listed process: the feature's own stop if it gave one, else a
/// terminate signal to it and the group it leads; either way it is killed if
/// it is still running two seconds later. `identity` is the row's, so a
/// process that has since exited is not mistaken for a newer one.
pub(crate) fn stop(pid: u32, identity: &str) -> Result<(), String> {
    let stop = {
        let owned = owned();
        let process = (owned.iter())
            .find(|process| process.pid == pid && process.identity == identity)
            .ok_or("This process has exited or changed; refresh the list")?;
        if !alive(process) {
            return Err("This process has exited or changed; refresh the list".into());
        }
        process.stop.clone()
    };
    match stop {
        Some(stop) => stop(),
        None => signal(pid, false),
    }
    let stamp = identity.to_owned();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        if self::identity(pid) == stamp {
            signal(pid, true);
        }
    });
    Ok(())
}

#[cfg(unix)]
fn signal(pid: u32, kill: bool) {
    let signal = if kill { libc::SIGKILL } else { libc::SIGTERM };
    unsafe {
        // A run leads its own group; a server started on pipes does not, and
        // signalling its group would reach the app itself.
        if libc::getpgid(pid as i32) == pid as i32 {
            libc::kill(-(pid as i32), signal);
        }
        libc::kill(pid as i32, signal);
    }
}
#[cfg(not(unix))]
fn signal(_pid: u32, _kill: bool) {}
