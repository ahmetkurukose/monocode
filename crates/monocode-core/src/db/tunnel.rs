//! Tunnels: the proxy (AlloyDB, Cloud SQL), `kubectl port-forward` or custom
//! command that makes a remote server reachable on a data source's local port.
//!
//! Ported from dbports (`internal/tunnel`). Processes are started detached,
//! in their own session, so they outlive Monocode like they outlive the
//! dbports TUI; what is running is learned from `lsof`, not from handles we
//! keep. Everything is keyed by port: several data sources (other databases,
//! other users) may share one tunnel.
//!
//! Files under `dir`: `logs/<port>.log` (appended, the proxy's output),
//! `pids/<port>.pid` (what we spawned and when), `locks/<port>.lock` (held
//! while [`ensure`] starts a tunnel, so the UI and agents never race).

use super::source::{Tunnel, TunnelKind};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A process listening on a local TCP port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub pid: u32,
    /// The command name as `lsof` reports it: truncated (9 bytes on macOS).
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelState {
    Stopped,
    Starting,
    Running {
        pid: u32,
    },
    /// Another program holds the port.
    Busy {
        pid: u32,
        command: String,
    },
    /// We started it and it is gone (or never opened the port): why, if the
    /// log says.
    Failed(String),
}

/// How long a spawned process may take to open its port before it counts as
/// failed rather than starting.
const STARTING_GRACE: Duration = Duration::from_secs(30);
/// A lock older than this was left by a caller that died.
const STALE_LOCK: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(200);
/// `stop` escalates to SIGKILL after this.
const TERM_GRACE: Duration = Duration::from_secs(3);
/// `log_tail` reads at most this much: logs are appended forever.
const TAIL_BYTES: u64 = 256 << 10;
/// Starts each run in the log; only lines after the last one are this run's.
const RUN_MARKER: &str = "===== ";

// ---------------------------------------------------------------- listeners

/// One `lsof -nP -iTCP -sTCP:LISTEN -F pcn` call: port -> listener. Empty
/// map on error.
pub fn listeners() -> HashMap<u16, Listener> {
    lsof(&["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pcn"])
}

/// The listener on one port: much cheaper than [`listeners`] when polling.
fn listener_on(port: u16) -> Option<Listener> {
    let spec = format!("-iTCP:{port}");
    lsof(&["-nP", &spec, "-sTCP:LISTEN", "-F", "pcn"]).remove(&port)
}

fn lsof(args: &[&str]) -> HashMap<u16, Listener> {
    // Finder-launched apps get launchd's PATH, which may lack /usr/sbin.
    let program = ["/usr/sbin/lsof", "/usr/bin/lsof"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .unwrap_or("lsof");
    // lsof exits 1 when nothing listens; the output is what counts.
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|out| parse_lsof(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or_default()
}

/// Parses `lsof -F pcn` output: `p<pid>`, `c<command>`, then one `n<addr>`
/// per socket (`n*:5432`, `n127.0.0.1:5432`, `n[::1]:5432`). The first
/// process seen on a port wins.
fn parse_lsof(text: &str) -> HashMap<u16, Listener> {
    let mut found = HashMap::new();
    let (mut pid, mut command) = (0u32, String::new());
    for line in text.lines() {
        let Some(field) = line.chars().next() else {
            continue;
        };
        let value = &line[field.len_utf8()..];
        match field {
            'p' => {
                pid = value.parse().unwrap_or(0);
                command.clear();
            }
            'c' => command = value.to_owned(),
            'n' => {
                let Some(port) = value
                    .rsplit_once(':')
                    .and_then(|(_, port)| port.parse::<u16>().ok())
                else {
                    continue;
                };
                found.entry(port).or_insert_with(|| Listener {
                    pid,
                    command: command.clone(),
                });
            }
            _ => {}
        }
    }
    found
}

// -------------------------------------------------------------------- state

/// The process names a kind's listener has. Empty for Custom: any listener
/// is taken to be it.
pub fn expected_program(kind: TunnelKind) -> &'static [&'static str] {
    match kind {
        TunnelKind::AlloyDb => &["alloydb-auth-proxy"],
        // v2 and the older v1 binary.
        TunnelKind::CloudSql => &["cloud-sql-proxy", "cloud_sql_proxy"],
        TunnelKind::Kubectl => &["kubectl"],
        TunnelKind::Custom => &[],
    }
}

/// Whether `command`, as lsof reports it, is one of the kind's programs.
/// lsof truncates names, so either may be a prefix of the other.
fn is_expected(kind: TunnelKind, command: &str) -> bool {
    let expected = expected_program(kind);
    if expected.is_empty() {
        return true;
    }
    let command = command.trim();
    !command.is_empty()
        && expected
            .iter()
            .any(|name| name.starts_with(command) || command.starts_with(name))
}

pub fn state(
    tunnel: &Tunnel,
    port: u16,
    listeners: &HashMap<u16, Listener>,
    dir: &Path,
) -> TunnelState {
    if let Some(listener) = listeners.get(&port) {
        return classify_listener(tunnel.kind, listener);
    }
    let Some(spawn) = read_pid_file(dir, port) else {
        return TunnelState::Stopped;
    };
    let alive = is_alive(spawn.pid);
    if alive && spawn.age() < STARTING_GRACE {
        return TunnelState::Starting;
    }
    let reason = failure_reason(tunnel.kind, dir, port);
    if alive {
        let waited = STARTING_GRACE.as_secs();
        return TunnelState::Failed(match reason {
            Some(reason) => format!("Port {port} is still not open after {waited}s: {reason}"),
            None => format!("Port {port} is still not open after {waited}s."),
        });
    }
    TunnelState::Failed(reason.unwrap_or_else(|| "The tunnel exited.".into()))
}

fn classify_listener(kind: TunnelKind, listener: &Listener) -> TunnelState {
    if is_expected(kind, &listener.command) {
        TunnelState::Running { pid: listener.pid }
    } else {
        TunnelState::Busy {
            pid: listener.pid,
            command: listener.command.clone(),
        }
    }
}

// ------------------------------------------------------------------ command

/// Program + args (no shell, except Custom → `("sh", ["-c", command])`).
pub fn command_line(tunnel: &Tunnel, port: u16) -> Result<(String, Vec<String>), String> {
    let instance = tunnel.instance.trim();
    if tunnel.kind != TunnelKind::Custom && instance.is_empty() {
        return Err(match tunnel.kind {
            TunnelKind::AlloyDb => "The AlloyDB instance is empty.",
            TunnelKind::CloudSql => "The Cloud SQL instance connection name is empty.",
            _ => "The kubectl target (svc/name, pod/name) is empty.",
        }
        .into());
    }
    let extra = tunnel.args.iter().cloned();
    let line = match tunnel.kind {
        TunnelKind::AlloyDb => {
            let mut args = vec![instance.to_owned(), format!("--port={port}")];
            if tunnel.public_ip {
                args.push("--public-ip".into());
            }
            (
                "alloydb-auth-proxy".into(),
                args.into_iter().chain(extra).collect(),
            )
        }
        TunnelKind::CloudSql => {
            let args = vec![instance.to_owned(), format!("--port={port}")];
            (
                "cloud-sql-proxy".into(),
                args.into_iter().chain(extra).collect(),
            )
        }
        TunnelKind::Kubectl => {
            let mut args = Vec::new();
            if !tunnel.context.trim().is_empty() {
                args.extend(["--context".into(), tunnel.context.trim().to_owned()]);
            }
            if !tunnel.namespace.trim().is_empty() {
                args.extend(["-n".into(), tunnel.namespace.trim().to_owned()]);
            }
            let remote = tunnel.remote_port.unwrap_or(port);
            args.extend([
                "port-forward".into(),
                instance.to_owned(),
                format!("{port}:{remote}"),
            ]);
            ("kubectl".into(), args.into_iter().chain(extra).collect())
        }
        TunnelKind::Custom => {
            let command = tunnel.command.trim();
            if command.is_empty() {
                return Err("The tunnel command is empty.".into());
            }
            ("sh".into(), vec!["-c".into(), command.to_owned()])
        }
    };
    Ok(line)
}

/// Human-readable one-liner of [`command_line`] for display and copy.
pub fn display_command(tunnel: &Tunnel, port: u16) -> String {
    if tunnel.kind == TunnelKind::Custom {
        return tunnel.command.trim().to_owned();
    }
    match command_line(tunnel, port) {
        Ok((program, args)) => std::iter::once(&program)
            .chain(&args)
            .map(|word| {
                shlex::try_quote(word)
                    .map(|quoted| quoted.into_owned())
                    .unwrap_or_else(|_| word.clone())
            })
            .collect::<Vec<_>>()
            .join(" "),
        Err(error) => error,
    }
}

/// Where proxies are often installed but GUI apps do not look.
fn fallback_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = crate::scm::shell::dirs_home() {
        let home = PathBuf::from(home);
        for sub in [".local/bin", "go/bin", "google-cloud-sdk/bin"] {
            dirs.push(home.join(sub));
        }
    }
    for dir in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/opt/homebrew/share/google-cloud-sdk/bin",
        "/usr/local/share/google-cloud-sdk/bin",
    ] {
        dirs.push(dir.into());
    }
    dirs
}

/// The login shell's PATH plus [`fallback_dirs`]. Also the child's PATH:
/// kubectl runs `gke-gcloud-auth-plugin` from it.
fn search_path() -> std::ffi::OsString {
    let gui = crate::scm::shell::gui_search_path();
    let paths = std::env::split_paths(&gui).chain(fallback_dirs());
    std::env::join_paths(paths).unwrap_or_else(|_| gui.into())
}

fn find_binary(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    if name.contains('/') {
        return is_executable(Path::new(name)).then(|| name.into());
    }
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// The binary for `program`. Cloud SQL: `cloud-sql-proxy`, else the v1-named
/// `cloud_sql_proxy` (some installs ship v2 under that name; both take
/// `--port=N`).
fn resolve_program(program: &str, path: &std::ffi::OsStr) -> Result<PathBuf, String> {
    if let Some(found) = find_binary(program, path) {
        return Ok(found);
    }
    if program == "cloud-sql-proxy" {
        return find_binary("cloud_sql_proxy", path).ok_or_else(|| {
            "Neither cloud-sql-proxy nor cloud_sql_proxy is installed or on PATH. Install it with `gcloud components install cloud-sql-proxy`, then open the tunnel again.".into()
        });
    }
    Err(install_hint(program))
}

fn install_hint(program: &str) -> String {
    let how = match program {
        "alloydb-auth-proxy" => "Install it with `gcloud components install alloydb-auth-proxy`",
        "cloud-sql-proxy" | "cloud_sql_proxy" => {
            "Install it with `gcloud components install cloud-sql-proxy`"
        }
        "kubectl" => {
            "Install it with `gcloud components install kubectl` or `brew install kubectl`"
        }
        "gke-gcloud-auth-plugin" => {
            "Install it with `gcloud components install gke-gcloud-auth-plugin`"
        }
        _ => "Install it",
    };
    format!("{program} is not installed or not on PATH. {how}, then open the tunnel again.")
}

// -------------------------------------------------------------- start, stop

const UNSUPPORTED: &str = "Tunnels are only supported on macOS and Linux.";

/// Spawns the tunnel detached and returns its pid. The process gets its own
/// session (no controlling terminal, not in our process group), so it keeps
/// running when Monocode quits. May block a few seconds the first time, while
/// the login shell's PATH is read.
pub fn start(tunnel: &Tunnel, port: u16, dir: &Path) -> Result<u32, String> {
    #[cfg(unix)]
    {
        spawn_detached(tunnel, port, dir)
    }
    #[cfg(not(unix))]
    {
        let _ = (tunnel, port, dir);
        Err(UNSUPPORTED.into())
    }
}

#[cfg(unix)]
fn spawn_detached(tunnel: &Tunnel, port: u16, dir: &Path) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let (program, args) = command_line(tunnel, port)?;
    let path = search_path();
    let binary = if program == "sh" {
        PathBuf::from("/bin/sh")
    } else {
        resolve_program(&program, &path)?
    };
    for sub in ["logs", "pids"] {
        std::fs::create_dir_all(dir.join(sub)).map_err(|e| e.to_string())?;
    }
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(dir, port))
        .map_err(|e| format!("Could not open the tunnel log: {e}"))?;
    let _ = writeln!(
        log,
        "\n{RUN_MARKER}{}  start: {} =====",
        chrono::Local::now().to_rfc3339(),
        display_command(tunnel, port)
    );
    let stderr = log.try_clone().map_err(|e| e.to_string())?;

    let mut command = Command::new(&binary);
    command.args(&args);
    crate::scm::shell::apply_gui_env(&mut command);
    command
        .env("PATH", &path)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(stderr);
    // SAFETY: setsid is async-signal-safe; nothing else runs between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            install_hint(&program)
        } else {
            format!("Could not start {program}: {e}")
        }
    })?;
    let pid = child.id();
    write_pid_file(dir, port, pid);
    // Reap it if it exits while we run: a zombie still answers kill(pid, 0)
    // and would look alive. The thread ends with the process, or with us.
    std::thread::Builder::new()
        .name(format!("tunnel-{port}"))
        .spawn(move || {
            let _ = child.wait();
        })
        .map_err(|e| e.to_string())?;
    Ok(pid)
}

/// SIGTERMs the pid listening on `port`, and the process group we spawned;
/// SIGKILL after 3s for whatever ignores it. Ok if nothing listens.
pub fn stop(port: u16, dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let listener = listener_on(port).map(|listener| listener.pid);
        let spawned = read_pid_file(dir, port)
            .map(|spawn| spawn.pid)
            .filter(|pid| is_alive(*pid));
        let _ = std::fs::remove_file(pid_path(dir, port));
        let mut targets: Vec<u32> = listener.into_iter().chain(spawned).collect();
        targets.dedup();
        for &pid in &targets {
            signal(pid, libc::SIGTERM, Some(pid) == spawned)?;
        }
        let deadline = std::time::Instant::now() + TERM_GRACE;
        while targets.iter().any(|pid| is_alive(*pid)) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        for &pid in targets.iter().filter(|pid| is_alive(**pid)) {
            let _ = signal(pid, libc::SIGKILL, Some(pid) == spawned);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (port, dir);
        Err(UNSUPPORTED.into())
    }
}

/// Signals `pid`, or its whole process group when we made it the leader of
/// one: `sh -c` and whatever it started go together.
#[cfg(unix)]
fn signal(pid: u32, sig: libc::c_int, group: bool) -> Result<(), String> {
    let pid = pid as libc::pid_t;
    // SAFETY: plain syscalls on a pid.
    let sent = unsafe { (group && libc::kill(-pid, sig) == 0) || libc::kill(pid, sig) == 0 };
    if sent || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!(
            "Could not stop process {pid}: {}",
            std::io::Error::last_os_error()
        ))
    }
}

fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid == 0 {
            return false;
        }
        // SAFETY: signal 0 only checks that the pid exists.
        let found = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
        found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

// ------------------------------------------------------------------- ensure

/// Blocks until the tunnel's port listens with the expected program, starting
/// it if needed. Serialized per port across processes by a lock file, so the
/// UI and agent calls never start the same tunnel twice.
pub fn ensure(tunnel: &Tunnel, port: u16, dir: &Path, timeout: Duration) -> Result<(), String> {
    if !cfg!(unix) {
        return Err(UNSUPPORTED.into());
    }
    let deadline = std::time::Instant::now() + timeout;
    if let Some(done) = check_listening(tunnel.kind, port) {
        return done;
    }
    let lock = acquire_lock(dir, port, deadline)?;
    // Someone may have started it while we waited for the lock.
    if let Some(done) = check_listening(tunnel.kind, port) {
        return done;
    }
    // A start() from the UI that is still coming up: wait for it instead.
    let pid = match read_pid_file(dir, port) {
        Some(spawn) if is_alive(spawn.pid) && spawn.age() < STARTING_GRACE => spawn.pid,
        _ => start(tunnel, port, dir)?,
    };
    loop {
        if let Some(done) = check_listening(tunnel.kind, port) {
            return done;
        }
        if !is_alive(pid) {
            // Its last words may still be on their way to the log.
            std::thread::sleep(Duration::from_millis(50));
            return Err(match failure_reason(tunnel.kind, dir, port) {
                Some(reason) => reason,
                None => format!(
                    "The tunnel exited before opening port {port}. See {}",
                    log_path(dir, port).display()
                ),
            });
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "The tunnel did not open port {port} within {}s. See {}",
                timeout.as_secs(),
                log_path(dir, port).display()
            ));
        }
        lock.refresh();
        std::thread::sleep(POLL);
    }
}

/// `Some` when the question is settled: our program listens, or another does.
fn check_listening(kind: TunnelKind, port: u16) -> Option<Result<(), String>> {
    let listener = listener_on(port)?;
    Some(match classify_listener(kind, &listener) {
        TunnelState::Busy { pid, command } => {
            Err(format!("Port {port} is used by {command} (pid {pid})"))
        }
        _ => Ok(()),
    })
}

/// An exclusive lock file, removed on drop.
struct PortLock {
    path: PathBuf,
    file: std::fs::File,
}

impl PortLock {
    /// Keeps a long wait from looking stale to other callers.
    fn refresh(&self) {
        let _ = self.file.set_modified(SystemTime::now());
    }
}

impl Drop for PortLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn lock_path(dir: &Path, port: u16) -> PathBuf {
    dir.join("locks").join(format!("{port}.lock"))
}

/// One attempt: `None` if someone else holds a fresh lock. A lock untouched
/// for [`STALE_LOCK`] belonged to a caller that died, and is broken.
fn try_lock(dir: &Path, port: u16) -> Result<Option<PortLock>, String> {
    let path = lock_path(dir, port);
    std::fs::create_dir_all(dir.join("locks")).map_err(|e| e.to_string())?;
    for _ in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let _ = write!(file, "{}", std::process::id());
                return Ok(Some(PortLock { path, file }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|at| at.elapsed().ok())
                    .is_some_and(|age| age > STALE_LOCK);
                if !stale {
                    return Ok(None);
                }
                let _ = std::fs::remove_file(&path);
            }
            Err(e) => return Err(format!("Could not lock the tunnel: {e}")),
        }
    }
    Ok(None)
}

fn acquire_lock(dir: &Path, port: u16, deadline: std::time::Instant) -> Result<PortLock, String> {
    loop {
        if let Some(lock) = try_lock(dir, port)? {
            return Ok(lock);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "Another start of the tunnel on port {port} is still in progress."
            ));
        }
        std::thread::sleep(POLL);
    }
}

// ----------------------------------------------------------------- pid file

struct Spawn {
    pid: u32,
    at: SystemTime,
}

impl Spawn {
    fn age(&self) -> Duration {
        self.at.elapsed().unwrap_or_default()
    }
}

fn pid_path(dir: &Path, port: u16) -> PathBuf {
    dir.join("pids").join(format!("{port}.pid"))
}

/// `<pid> <unix seconds>`.
fn write_pid_file(dir: &Path, port: u16, pid: u32) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = std::fs::write(pid_path(dir, port), format!("{pid} {secs}\n"));
}

fn read_pid_file(dir: &Path, port: u16) -> Option<Spawn> {
    let text = std::fs::read_to_string(pid_path(dir, port)).ok()?;
    let mut words = text.split_whitespace();
    let pid = words.next()?.parse().ok()?;
    let secs = words.next().and_then(|secs| secs.parse().ok()).unwrap_or(0);
    Some(Spawn {
        pid,
        at: UNIX_EPOCH + Duration::from_secs(secs),
    })
}

// ---------------------------------------------------------------------- log

pub fn log_path(dir: &Path, port: u16) -> PathBuf {
    dir.join("logs").join(format!("{port}.log"))
}

/// Last `n` non-empty lines of the log, oldest first.
pub fn log_tail(dir: &Path, port: u16, n: usize) -> Vec<String> {
    let text = read_tail(&log_path(dir, port), TAIL_BYTES);
    let mut lines: Vec<String> = text
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(n)
        .map(str::to_owned)
        .collect();
    lines.reverse();
    lines
}

/// At most `max` trailing bytes, from a line boundary.
fn read_tail(path: &Path, max: u64) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = size.saturating_sub(max);
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        return text
            .split_once('\n')
            .map(|(_, rest)| rest.to_owned())
            .unwrap_or_default();
    }
    text
}

/// Why the last run failed, friendly-ized: the last line that names a known
/// error, else the last that looks like one, else the last line of the run.
fn failure_reason(kind: TunnelKind, dir: &Path, port: u16) -> Option<String> {
    let tail = log_tail(dir, port, 40);
    let run_start = tail
        .iter()
        .rposition(|line| line.starts_with(RUN_MARKER))
        .map_or(0, |at| at + 1);
    let run = &tail[run_start..];
    if let Some(explained) = run
        .iter()
        .rev()
        .find_map(|line| explain_failure_for(Some(kind), line))
    {
        return Some(explained);
    }
    let looks_bad = |line: &&String| {
        let lower = line.to_lowercase();
        [
            "error",
            "fail",
            "denied",
            "refused",
            "not found",
            "unable",
            "cannot",
        ]
        .iter()
        .any(|word| lower.contains(word))
    };
    run.iter()
        .rev()
        .find(looks_bad)
        .or_else(|| run.last())
        .map(|line| strip_timestamp(line).to_owned())
}

/// Proxies prefix lines with `2024/05/01 12:00:00 `; that is noise in a toast.
fn strip_timestamp(line: &str) -> &str {
    let line = line.trim();
    let mut words = line.splitn(3, ' ');
    match (words.next(), words.next(), words.next()) {
        (Some(date), Some(time), Some(rest))
            if date.len() == 10
                && date
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '/' || c == '-')
                && time.starts_with(|c: char| c.is_ascii_digit())
                && time.contains(':') =>
        {
            rest.trim()
        }
        _ => line,
    }
}

/// Maps known proxy and kubectl errors to an actionable message; `None` if
/// the line is not one of them. Case-insensitive; credentials are checked
/// before everything else, application default credentials first.
pub fn explain_failure(log_line: &str) -> Option<String> {
    explain_failure_for(None, log_line)
}

const ADC_HINT: &str = "Google application default credentials are missing or expired. Run `gcloud auth application-default login`, then open the tunnel again.";
const GCLOUD_LOGIN_HINT: &str =
    "Your gcloud login expired. Run `gcloud auth login`, then open the tunnel again.";
const KUBECTL_LOGIN_HINT: &str = "kubectl is not logged in to the cluster. Run `gcloud auth login` (or switch to a kubectl context that is), then open the tunnel again.";

/// [`explain_failure`], worded for the tunnel's kind when it is known: a
/// kubectl "must be logged in" is about the cluster, not gcloud alone.
fn explain_failure_for(kind: Option<TunnelKind>, log_line: &str) -> Option<String> {
    let line = strip_timestamp(log_line);
    let lower = line.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));

    if has(&[
        "application default credentials",
        "default credentials",
        "application-default",
        "could not find default credentials",
        "failed to create default credentials",
    ]) {
        return Some(ADC_HINT.into());
    }
    if has(&[
        "must be logged in",
        "provide credentials",
        "unauthorized",
        "reauthentication",
        "gcloud auth login",
        "invalid_grant",
        "token has been expired or revoked",
        "invalid authentication credentials",
        "refresh token",
        "oauth2: cannot fetch token",
    ]) {
        let kubectl = kind == Some(TunnelKind::Kubectl)
            && has(&["must be logged in", "unauthorized", "provide credentials"]);
        return Some(
            if kubectl {
                KUBECTL_LOGIN_HINT
            } else {
                GCLOUD_LOGIN_HINT
            }
            .into(),
        );
    }
    if has(&["address already in use"]) {
        return Some(
            "The local port is already in use by another program. Stop it, or give the data source another port."
                .into(),
        );
    }
    if has(&[
        "executable file not found",
        "no such file or directory",
        "command not found",
    ]) {
        if let Some(program) = missing_program(line) {
            return Some(install_hint(&program));
        }
    }
    if has(&["connection refused", "i/o timeout"]) {
        return Some(line.to_owned());
    }
    None
}

/// The program a "not found" line is about: `exec: "name": executable file
/// not found`, `sh: name: command not found`, `sh: line 1: name: command not
/// found`, `fork/exec /path/name: no such file or directory`.
fn missing_program(line: &str) -> Option<String> {
    if let Some((_, rest)) = line.split_once('"') {
        if let Some((quoted, _)) = rest.split_once('"') {
            return Some(base_name(quoted));
        }
    }
    let parts: Vec<&str> = line.split(": ").map(str::trim).collect();
    let error_at = parts.iter().position(|part| {
        let lower = part.to_lowercase();
        lower.contains("not found") || lower.contains("no such file")
    })?;
    let candidate = parts[..error_at]
        .iter()
        .rev()
        .map(|part| part.trim_start_matches("fork/exec "))
        .find(|part| !part.is_empty() && !part.contains(' '))?;
    (!["sh", "bash", "zsh"].contains(&candidate)).then(|| base_name(candidate))
}

fn base_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tunnel(kind: TunnelKind) -> Tunnel {
        Tunnel {
            kind,
            instance: "projects/p/locations/l/clusters/c/instances/i".into(),
            ..Tunnel::default()
        }
    }

    fn args(line: (String, Vec<String>)) -> String {
        std::iter::once(line.0)
            .chain(line.1)
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn command_lines_for_each_kind() {
        let mut alloy = tunnel(TunnelKind::AlloyDb);
        alloy.public_ip = true;
        alloy.args = vec!["--auto-iam-authn".into()];
        assert_eq!(
            args(command_line(&alloy, 1234).unwrap()),
            "alloydb-auth-proxy projects/p/locations/l/clusters/c/instances/i --port=1234 --public-ip --auto-iam-authn"
        );

        let mut sql = tunnel(TunnelKind::CloudSql);
        sql.instance = "proj:europe-west4:db".into();
        assert_eq!(
            args(command_line(&sql, 5433).unwrap()),
            "cloud-sql-proxy proj:europe-west4:db --port=5433"
        );

        let mut kube = tunnel(TunnelKind::Kubectl);
        kube.instance = "svc/postgres".into();
        assert_eq!(
            args(command_line(&kube, 6432).unwrap()),
            "kubectl port-forward svc/postgres 6432:6432",
            "the remote port defaults to the local one"
        );
        kube.context = "gke_prod".into();
        kube.namespace = "db".into();
        kube.remote_port = Some(5432);
        kube.args = vec!["--address=127.0.0.1".into()];
        assert_eq!(
            args(command_line(&kube, 6432).unwrap()),
            "kubectl --context gke_prod -n db port-forward svc/postgres 6432:5432 --address=127.0.0.1"
        );

        let mut custom = tunnel(TunnelKind::Custom);
        custom.command = "ssh -N -L 6000:10.0.0.5:5432 bastion".into();
        assert_eq!(
            command_line(&custom, 6000).unwrap(),
            ("sh".into(), vec!["-c".into(), custom.command.clone()])
        );
        assert_eq!(display_command(&custom, 6000), custom.command);
    }

    #[test]
    fn empty_instance_or_command_is_an_error() {
        for kind in TunnelKind::ALL {
            let empty = Tunnel {
                kind,
                ..Tunnel::default()
            };
            assert!(command_line(&empty, 1).is_err(), "{kind:?}");
        }
    }

    #[test]
    fn display_command_quotes_what_needs_it() {
        let mut kube = tunnel(TunnelKind::Kubectl);
        kube.instance = "svc/pg".into();
        kube.context = "my context".into();
        assert_eq!(
            display_command(&kube, 1),
            "kubectl --context 'my context' port-forward svc/pg 1:1"
        );
    }

    #[test]
    fn lsof_output_is_parsed_per_port() {
        let text = "p101\ncalloydb-a\nf10\nn127.0.0.1:1234\np202\nckubectl\nf7\nn127.0.0.1:6432\nf8\nn[::1]:6432\np303\ncpostgres\nf5\nn*:5432\nf6\nn[::]:5432\np404\ncother\nn*:6432\nnbogus\n";
        let found = parse_lsof(text);
        assert_eq!(found.len(), 3);
        assert_eq!(
            found[&1234],
            Listener {
                pid: 101,
                command: "alloydb-a".into()
            }
        );
        assert_eq!(found[&6432].pid, 202, "the first process on a port wins");
        assert_eq!(found[&5432].command, "postgres");
        assert!(parse_lsof("").is_empty());
    }

    #[test]
    fn state_matches_truncated_names_and_reports_busy() {
        let dir = tempfile::tempdir().unwrap();
        let listening = |command: &str| {
            HashMap::from([(
                1234u16,
                Listener {
                    pid: 9,
                    command: command.into(),
                },
            )])
        };
        let alloy = tunnel(TunnelKind::AlloyDb);
        assert_eq!(
            state(&alloy, 1234, &listening("alloydb-a"), dir.path()),
            TunnelState::Running { pid: 9 }
        );
        assert_eq!(
            state(&alloy, 1234, &listening("postgres"), dir.path()),
            TunnelState::Busy {
                pid: 9,
                command: "postgres".into()
            }
        );
        let sql = tunnel(TunnelKind::CloudSql);
        for name in ["cloud-sql", "cloud_sql", "cloud-sql-proxy"] {
            assert_eq!(
                state(&sql, 1234, &listening(name), dir.path()),
                TunnelState::Running { pid: 9 },
                "{name}"
            );
        }
        let kube = tunnel(TunnelKind::Kubectl);
        assert!(matches!(
            state(&kube, 1234, &listening("alloydb-a"), dir.path()),
            TunnelState::Busy { .. }
        ));
        let custom = tunnel(TunnelKind::Custom);
        assert_eq!(
            state(&custom, 1234, &listening("ssh"), dir.path()),
            TunnelState::Running { pid: 9 }
        );
        assert_eq!(
            state(&alloy, 1234, &HashMap::new(), dir.path()),
            TunnelState::Stopped
        );
    }

    #[test]
    fn state_reports_starting_and_failed_from_the_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("pids")).unwrap();
        std::fs::create_dir_all(dir.path().join("logs")).unwrap();
        let alloy = tunnel(TunnelKind::AlloyDb);
        // Our own pid: alive, and just spawned.
        write_pid_file(dir.path(), 1, std::process::id());
        assert_eq!(
            state(&alloy, 1, &HashMap::new(), dir.path()),
            TunnelState::Starting
        );
        // A pid that cannot exist: it died. The log says why.
        std::fs::write(pid_path(dir.path(), 1), "2147483646 0\n").unwrap();
        std::fs::write(
            log_path(dir.path(), 1),
            "old run error\n===== now  start: x =====\n2024/05/01 10:00:00 Authorizing\n2024/05/01 10:00:01 failed to create default credentials: boom\n",
        )
        .unwrap();
        assert_eq!(
            state(&alloy, 1, &HashMap::new(), dir.path()),
            TunnelState::Failed(explain_failure("could not find default credentials").unwrap())
        );
    }

    #[test]
    fn known_failures_are_explained() {
        let says = |line: &str| explain_failure(line).unwrap_or_default();
        for line in [
            "2024/05/01 10:00:00 could not find default credentials. See https://…",
            "failed to create default credentials: x",
            "Run gcloud auth application-default login",
            "Could not load the Application Default Credentials",
            // ADC wins over the login hints when a line names both.
            "oauth2: cannot fetch token: Application Default Credentials expired",
        ] {
            assert_eq!(says(line), ADC_HINT, "{line}");
        }
        for line in [
            "oauth2: cannot fetch token: 400 Bad Request",
            "Response: {\"error\": \"invalid_grant\"}",
            "Token has been expired or revoked.",
            "Reauthentication failed. cannot prompt during non-interactive execution.",
            "Request had invalid authentication credentials.",
            "error: You must be logged in to the server (Unauthorized)",
        ] {
            assert_eq!(says(line), GCLOUD_LOGIN_HINT, "{line}");
        }
        assert!(
            explain_failure("listen tcp 127.0.0.1:1234: bind: address already in use")
                .unwrap()
                .contains("already in use")
        );
        for line in [
            "error: You must be logged in to the server (Unauthorized)",
            "UNAUTHORIZED",
            "the server has asked for the client to provide credentials",
        ] {
            assert_eq!(
                explain_failure_for(Some(TunnelKind::Kubectl), line).as_deref(),
                Some(KUBECTL_LOGIN_HINT),
                "{line}"
            );
        }
        let missing = |line: &str| explain_failure(line).unwrap();
        assert!(
            missing("exec: \"gke-gcloud-auth-plugin\": executable file not found in $PATH")
                .starts_with("gke-gcloud-auth-plugin is not installed")
        );
        assert!(missing("sh: line 1: cloud-sql-proxy: command not found")
            .starts_with("cloud-sql-proxy is not installed"));
        assert!(missing("sh: kubectl: command not found").starts_with("kubectl is not installed"));
        assert!(
            missing("fork/exec /opt/bin/alloydb-auth-proxy: no such file or directory")
                .starts_with("alloydb-auth-proxy is not installed")
        );
        assert_eq!(
            explain_failure(
                "2024/05/01 10:00:00 dial tcp 10.0.0.5:5432: connect: connection refused"
            )
            .as_deref(),
            Some("dial tcp 10.0.0.5:5432: connect: connection refused"),
            "passed through, without the timestamp"
        );
        assert_eq!(explain_failure("Listening on 127.0.0.1:1234"), None);
    }

    #[test]
    fn log_tail_skips_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("logs")).unwrap();
        std::fs::write(log_path(dir.path(), 7), "a\n\nb\n  \nc\n\n").unwrap();
        assert_eq!(log_tail(dir.path(), 7, 2), vec!["b", "c"]);
        assert_eq!(log_tail(dir.path(), 7, 10), vec!["a", "b", "c"]);
        assert!(log_tail(dir.path(), 8, 3).is_empty());
    }

    #[test]
    fn the_lock_is_exclusive_and_stale_locks_break() {
        let dir = tempfile::tempdir().unwrap();
        let first = try_lock(dir.path(), 5432).unwrap().expect("free");
        assert!(try_lock(dir.path(), 5432).unwrap().is_none(), "held");
        assert!(try_lock(dir.path(), 5433).unwrap().is_some(), "per port");
        drop(first);
        let again = try_lock(dir.path(), 5432)
            .unwrap()
            .expect("released on drop");
        // A holder that died two minutes ago.
        let old = SystemTime::now() - Duration::from_secs(120);
        again.file.set_modified(old).unwrap();
        std::mem::forget(again);
        assert!(
            try_lock(dir.path(), 5432).unwrap().is_some(),
            "stale lock broken"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cloud_sql_falls_back_to_the_underscore_binary() {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempfile::tempdir().unwrap();
        let path = std::ffi::OsString::from(bin.path());
        let err = resolve_program("cloud-sql-proxy", &path).unwrap_err();
        assert!(
            err.contains("cloud-sql-proxy") && err.contains("cloud_sql_proxy"),
            "{err}"
        );
        let old = bin.path().join("cloud_sql_proxy");
        std::fs::write(&old, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_program("cloud-sql-proxy", &path).unwrap(), old);
        let new = bin.path().join("cloud-sql-proxy");
        std::fs::write(&new, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            resolve_program("cloud-sql-proxy", &path).unwrap(),
            new,
            "v2 name first"
        );
        assert!(resolve_program("kubectl", &path)
            .unwrap_err()
            .starts_with("kubectl is not installed"));
    }

    #[cfg(unix)]
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[cfg(unix)]
    #[test]
    fn a_custom_tunnel_starts_runs_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let port = free_port();
        let mut custom = tunnel(TunnelKind::Custom);
        custom.command = format!("exec nc -lk 127.0.0.1 {port}");
        ensure(&custom, port, dir.path(), Duration::from_secs(10)).unwrap();
        let running = state(&custom, port, &listeners(), dir.path());
        assert!(
            matches!(running, TunnelState::Running { .. }),
            "{running:?}"
        );
        // Already up: no second process.
        ensure(&custom, port, dir.path(), Duration::from_secs(5)).unwrap();
        // Busy for a kind that expects another program.
        let err = ensure(
            &tunnel(TunnelKind::AlloyDb),
            port,
            dir.path(),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(
            err.starts_with(&format!("Port {port} is used by nc")),
            "{err}"
        );
        stop(port, dir.path()).unwrap();
        assert_eq!(
            state(&custom, port, &listeners(), dir.path()),
            TunnelState::Stopped
        );
        stop(port, dir.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_tunnel_that_dies_reports_its_last_words() {
        let dir = tempfile::tempdir().unwrap();
        let port = free_port();
        let mut custom = tunnel(TunnelKind::Custom);
        custom.command =
            "echo starting; echo 'proxy: the instance went sideways' >&2; exit 3".into();
        let err = ensure(&custom, port, dir.path(), Duration::from_secs(10)).unwrap_err();
        assert!(err.contains("proxy: the instance went sideways"), "{err}");
        assert_eq!(
            state(&custom, port, &HashMap::new(), dir.path()),
            TunnelState::Failed("proxy: the instance went sideways".into())
        );
        // And with a known error, the friendly version.
        custom.command = "echo 'could not find default credentials' >&2; exit 1".into();
        let err = ensure(&custom, port, dir.path(), Duration::from_secs(10)).unwrap_err();
        assert!(
            err.contains("gcloud auth application-default login"),
            "{err}"
        );
    }
}
