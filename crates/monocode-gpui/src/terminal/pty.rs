//! alacritty_terminal's Unix PTY, with its child started without `fork`.
//!
//! alacritty spawns through std's `Command` with a `pre_exec` closure, which
//! makes std fork; a forked child of this Chromium-hosting app can hang
//! before `exec` (see `monocode_core::process`). This is the same PTY for
//! alacritty's event loop, its child started by
//! `monocode_core::process::pty::spawn`, which gives it its own session with
//! the PTY as controlling terminal.
use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event, PollMode, Poller};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::Arc;

/// alacritty's poll keys for the PTY and for its child's events.
const PTY_READ_WRITE_TOKEN: usize = 0;
const PTY_CHILD_EVENT_TOKEN: usize = 1;

/// The child, by pid (as `std::process::Child::id`).
pub(crate) struct PtyChild {
    pid: u32,
    status: Option<ExitStatus>,
}
impl PtyChild {
    pub(crate) fn id(&self) -> u32 {
        self.pid
    }
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_none() {
            let mut status = 0;
            // SAFETY: our own child; WNOHANG never blocks.
            match unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, libc::WNOHANG) } {
                0 => {}
                -1 => return Err(io::Error::last_os_error()),
                _ => self.status = Some(ExitStatus::from_raw(status)),
            }
        }
        Ok(self.status)
    }
    fn wait(&mut self) {
        if self.status.is_none() {
            let mut status = 0;
            // SAFETY: our own child.
            if unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, 0) } > 0 {
                self.status = Some(ExitStatus::from_raw(status));
            }
        }
    }
}

pub(crate) struct Pty {
    child: PtyChild,
    file: File,
    signals: UnixStream,
    sig_id: signal_hook::SigId,
}
impl Pty {
    pub(crate) fn child(&self) -> &PtyChild {
        &self.child
    }
    pub(crate) fn file(&self) -> &File {
        &self.file
    }
}

/// alacritty's `tty::new` for `program` and `args`: the environment, UTF-8
/// input mode, non-blocking controller and SIGCHLD wakeups it sets up.
pub(crate) fn new(
    program: &str,
    args: &[String],
    env: &HashMap<String, String>,
    working_directory: &Path,
    window_size: WindowSize,
    window_id: u64,
) -> io::Result<Pty> {
    let (master, slave) = openpty(window_size)?;
    // SAFETY: a termios read and write on our own controller fd.
    unsafe {
        let mut termios = std::mem::zeroed();
        if libc::tcgetattr(master.as_raw_fd(), &mut termios) == 0 {
            termios.c_iflag |= libc::IUTF8;
            libc::tcsetattr(master.as_raw_fd(), libc::TCSANOW, &termios);
        }
    }
    let window_id = window_id.to_string();
    let mut vars: HashMap<OsString, OsString> = std::env::vars_os().collect();
    let (user, home) = user_and_home();
    vars.insert("ALACRITTY_WINDOW_ID".into(), window_id.clone().into());
    vars.insert("USER".into(), user.into());
    vars.insert("HOME".into(), home.into());
    vars.insert("WINDOWID".into(), window_id.into());
    for (key, value) in env {
        vars.insert(key.into(), value.into());
    }
    vars.remove(OsStr::new("XDG_ACTIVATION_TOKEN"));
    vars.remove(OsStr::new("DESKTOP_STARTUP_ID"));
    let vars: Vec<(OsString, OsString)> = vars.into_iter().collect();
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();

    let (signals, sig_id) = {
        let (sender, recv) = UnixStream::pair()?;
        let sig_id = signal_hook::low_level::pipe::register(libc::SIGCHLD, sender)?;
        recv.set_nonblocking(true)?;
        (recv, sig_id)
    };
    let spawned = monocode_core::process::pty::spawn(
        slave.as_fd(),
        OsStr::new(program),
        &args,
        &vars,
        Some(working_directory),
    );
    // Only the child keeps the user side.
    drop(slave);
    match spawned {
        Ok(pid) => {
            // SAFETY: flags of our own controller fd.
            unsafe {
                let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL, 0);
                libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
            Ok(Pty {
                child: PtyChild { pid, status: None },
                file: File::from(master),
                signals,
                sig_id,
            })
        }
        Err(err) => {
            signal_hook::low_level::unregister(sig_id);
            Err(io::Error::new(
                err.kind(),
                format!("Failed to spawn command '{program}': {err}"),
            ))
        }
    }
}

/// Both sides of a new PTY, close-on-exec (the child gets the user side
/// through its file actions only), neither this app's controlling terminal.
fn openpty(window_size: WindowSize) -> io::Result<(OwnedFd, OwnedFd)> {
    monocode_core::process::pty::openpty(&to_winsize(window_size))
}

/// USER and HOME from the environment, else from the password database.
fn user_and_home() -> (String, String) {
    let user = std::env::var("USER").ok();
    let home = std::env::var("HOME").ok();
    if let (Some(user), Some(home)) = (&user, &home) {
        return (user.clone(), home.clone());
    }
    let mut buf = vec![0 as libc::c_char; 4096];
    // SAFETY: getpwuid_r writes into `entry` and `buf`, both outliving the
    // reads of the strings it points into.
    let (name, dir) = unsafe {
        let mut entry: libc::passwd = std::mem::zeroed();
        let mut found = std::ptr::null_mut();
        libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buf.as_mut_ptr(),
            buf.len(),
            &mut found,
        );
        if found.is_null() {
            (String::new(), String::new())
        } else {
            let text =
                |p: *const libc::c_char| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
            (text(entry.pw_name), text(entry.pw_dir))
        }
    };
    (user.unwrap_or(name), home.unwrap_or(dir))
}

fn to_winsize(window_size: WindowSize) -> libc::winsize {
    let ws_row = window_size.num_lines as libc::c_ushort;
    let ws_col = window_size.num_cols as libc::c_ushort;
    libc::winsize {
        ws_row,
        ws_col,
        ws_xpixel: ws_col * window_size.cell_width as libc::c_ushort,
        ws_ypixel: ws_row * window_size.cell_height as libc::c_ushort,
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // The hangup a closing terminal sends, then reap. Not once reaped:
        // the pid may be another process's by now.
        if self.child.status.is_none() {
            // SAFETY: a signal to our own, unreaped child.
            unsafe { libc::kill(self.child.pid as libc::pid_t, libc::SIGHUP) };
        }
        signal_hook::low_level::unregister(self.sig_id);
        self.child.wait();
    }
}

impl EventedReadWrite for Pty {
    type Reader = File;
    type Writer = File;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        mut interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        interest.key = PTY_READ_WRITE_TOKEN;
        // SAFETY: both sources live in `self`, which the caller keeps
        // registered no longer than it lives.
        unsafe {
            poll.add_with_mode(&self.file, interest, mode)?;
            poll.add_with_mode(
                &self.signals,
                Event::readable(PTY_CHILD_EVENT_TOKEN),
                PollMode::Level,
            )
        }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        mut interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        interest.key = PTY_READ_WRITE_TOKEN;
        poll.modify_with_mode(&self.file, interest, mode)?;
        poll.modify_with_mode(
            &self.signals,
            Event::readable(PTY_CHILD_EVENT_TOKEN),
            PollMode::Level,
        )
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        poll.delete(&self.file)?;
        poll.delete(&self.signals)
    }

    fn reader(&mut self) -> &mut File {
        &mut self.file
    }

    fn writer(&mut self) -> &mut File {
        &mut self.file
    }
}

impl EventedPty for Pty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        // Only after a SIGCHLD.
        let mut buf = [0u8; 1];
        if let Err(err) = self.signals.read(&mut buf) {
            if err.kind() != io::ErrorKind::WouldBlock {
                eprintln!("pty: reading the signal pipe failed: {err}");
            }
            return None;
        }
        match self.child.try_wait() {
            Err(err) => {
                eprintln!("pty: checking the child failed: {err}");
                None
            }
            Ok(None) => None,
            Ok(status) => Some(ChildEvent::Exited(status)),
        }
    }
}

impl OnResize for Pty {
    fn on_resize(&mut self, window_size: WindowSize) {
        let size = to_winsize(window_size);
        // SAFETY: our own controller fd and a valid winsize.
        if unsafe { libc::ioctl(self.file.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
            eprintln!("pty: TIOCSWINSZ failed: {}", io::Error::last_os_error());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const SIZE: WindowSize = WindowSize {
        num_lines: 24,
        num_cols: 80,
        cell_width: 8,
        cell_height: 16,
    };

    fn sh(script: &str, env: &HashMap<String, String>) -> Pty {
        new(
            "/bin/sh",
            &["-c".to_owned(), script.to_owned()],
            env,
            Path::new("/private/tmp"),
            SIZE,
            42,
        )
        .expect("spawned")
    }

    /// Output until `needle` was seen, or until the child hung up when
    /// `needle` is None, for at most 10 seconds.
    fn read(pty: &mut Pty, needle: Option<&str>) -> String {
        let mut out = String::new();
        let mut buf = [0u8; 4096];
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !needle.is_some_and(|n| out.contains(n)) {
            match pty.reader().read(&mut buf) {
                Ok(n) if n > 0 => out.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                // EOF or EIO: the child has hung up.
                _ => break,
            }
        }
        out
    }

    #[test]
    fn pty_spawn_terminal_child_gets_its_pty_environment_and_exit_status() {
        let env = HashMap::from([("MONO_PTY_TEST".to_owned(), "yes".to_owned())]);
        let script = "tty; pwd; echo \"env:$MONO_PTY_TEST window:$WINDOWID\"; \
                      echo \"size:$(stty size)\"; exit 5";
        let mut pty = sh(script, &env);
        let out = read(&mut pty, None);
        assert!(out.contains("/dev/ttys"), "{out}");
        assert!(out.contains("/private/tmp"), "{out}");
        assert!(out.contains("env:yes window:42"), "{out}");
        assert!(out.contains("size:24 80"), "{out}");
        // alacritty's event loop learns of the exit through the SIGCHLD pipe.
        let deadline = Instant::now() + Duration::from_secs(10);
        let event = loop {
            if let Some(event) = pty.next_child_event() {
                break event;
            }
            assert!(Instant::now() < deadline, "no child event");
            std::thread::sleep(Duration::from_millis(10));
        };
        let ChildEvent::Exited(status) = event;
        assert_eq!(status.and_then(|s| s.code()), Some(5));
    }

    #[test]
    fn pty_spawn_terminal_resize_reaches_the_child() {
        let script = "trap 'echo \"winch:$(stty size)\"; exit 0' WINCH; \
                      echo ready; while :; do /bin/sleep 0.05; done";
        let mut pty = sh(script, &HashMap::new());
        assert!(read(&mut pty, Some("ready")).contains("ready"));
        pty.on_resize(WindowSize {
            num_lines: 30,
            num_cols: 90,
            ..SIZE
        });
        let out = read(&mut pty, Some("winch:30 90"));
        assert!(out.contains("winch:30 90"), "{out}");
    }

    #[test]
    fn pty_spawn_closing_the_terminal_hangs_up_on_the_child() {
        let mut pty = sh("echo ready; /bin/sleep 30", &HashMap::new());
        assert!(read(&mut pty, Some("ready")).contains("ready"));
        let pid = pty.child().id() as libc::pid_t;
        let started = Instant::now();
        // SIGHUP, then reaped.
        drop(pty);
        assert!(started.elapsed() < Duration::from_secs(5));
        // SAFETY: probing a pid; it was reaped, so nothing of ours is hit.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "the shell outlived its terminal");
    }
}
