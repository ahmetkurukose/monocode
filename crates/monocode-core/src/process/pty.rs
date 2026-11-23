//! Terminal children on macOS, started without `fork`.
//!
//! A terminal's child needs its own session with the PTY as its controlling
//! terminal: without one, Ctrl-C reaches nobody, a resize sends no SIGWINCH
//! and the shell runs without job control. The usual way is a `pre_exec`
//! closure (`setsid`, `TIOCSCTTY`), which makes std fork, and a forked child
//! of this Chromium-hosting process can hang before `exec` (see the module
//! comment of `process`).
//!
//! `posix_spawn` can start the session (`POSIX_SPAWN_SETSID`) but runs its
//! file actions before it, so no file action can make the PTY controlling
//! (a `dup2` never does, and an `open` happens while the child is still in
//! this process's session; with either, `tcgetpgrp` fails in the child). So
//! the child is this executable in a trampoline mode: [`TRAMPOLINE`] runs as
//! an image initializer, before `main`, takes the PTY on fd 0 as its
//! controlling terminal and `execve`s the program in place. Nothing forks:
//! posix_spawn, then one more exec in the same process, so the pid callers
//! get is the program's. (node-pty's `spawn-helper` does the same with a
//! separate executable.)
use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// argv[1] of the trampoline; argv[2] is the program, argv[3..] its argv.
const SENTINEL: &[u8] = b"--monocode-pty-exec";

/// Darwin's `POSIX_SPAWN_SETSID` (sys/spawn.h); libc does not export it.
const POSIX_SPAWN_SETSID: c_int = 0x0400;
/// Darwin's `NSIG`: signals are 1 to 31.
const NSIG: c_int = 32;

extern "C" {
    // macOS 10.15+; the libc version in use does not declare it.
    fn posix_spawn_file_actions_addchdir_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        path: *const c_char,
    ) -> c_int;
}

/// A new PTY sized `size`: its controller side and its user side, both
/// close-on-exec, and neither this process's controlling terminal (opened
/// with `O_NOCTTY`: an app that leads a session without one, as launchd
/// starts it, would otherwise take the user side, and then the child could
/// not).
pub fn openpty(size: &libc::winsize) -> io::Result<(OwnedFd, OwnedFd)> {
    // SAFETY: plain calls on the fds opened here, owned right after; the
    // name buffer is the 128 bytes TIOCPTYGNAME writes.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        if master < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = OwnedFd::from_raw_fd(master);
        let fd = master.as_raw_fd();
        if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == -1
            || libc::grantpt(fd) == -1
            || libc::unlockpt(fd) == -1
        {
            return Err(io::Error::last_os_error());
        }
        // ptsname's buffer is shared by every thread; this is its ioctl.
        let mut name = [0 as c_char; 128];
        if libc::ioctl(fd, libc::TIOCPTYGNAME as _, name.as_mut_ptr()) == -1 {
            return Err(io::Error::last_os_error());
        }
        let slave = libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        );
        if slave < 0 {
            return Err(io::Error::last_os_error());
        }
        let slave = OwnedFd::from_raw_fd(slave);
        if libc::ioctl(fd, libc::TIOCSWINSZ, size as *const libc::winsize) == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok((master, slave))
    }
}

/// Start `program` with `args` on the PTY whose user side is `tty`: fds 0, 1
/// and 2 are the PTY and no other descriptor is inherited, in a session of
/// its own with the PTY as controlling terminal, with an empty signal mask
/// and default dispositions, in `cwd` (ignored unless it is a directory),
/// with exactly `env`. A bare `program` is looked up on `env`'s PATH. Returns
/// the child's pid; the caller reaps it.
pub fn spawn(
    tty: BorrowedFd<'_>,
    program: &OsStr,
    args: &[OsString],
    env: &[(OsString, OsString)],
    cwd: Option<&Path>,
) -> io::Result<u32> {
    let path = env
        .iter()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let cwd = cwd.filter(|dir| dir.is_dir());
    let resolved = super::program_on_path(program, &path);
    // A relative path (`./gradlew`) names a file in the child's directory,
    // as it did when the child changed directory before its exec.
    let resolved = match cwd {
        Some(dir) if resolved.is_relative() && program.as_bytes().contains(&b'/') => {
            dir.join(resolved)
        }
        _ => resolved,
    };
    // The trampoline could only report a missing program inside the
    // terminal; fail here like a direct spawn would.
    if !super::is_executable(&resolved) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} was not found", resolved.display()),
        ));
    }
    // A user side on 0, 1 or 2 would be replaced by its own first dup2;
    // a duplicate is above them (and close-on-exec, like every fd here).
    let tty = tty.try_clone_to_owned()?;
    let trampoline = trampoline_executable();
    let mut argv = Vec::with_capacity(args.len() + 4);
    if let Some(exe) = &trampoline {
        argv.push(c_string(exe.as_os_str())?);
        argv.push(CString::new(SENTINEL).expect("no NUL"));
        argv.push(c_string(resolved.as_os_str())?);
    }
    argv.push(c_string(program)?);
    for arg in args {
        argv.push(c_string(arg)?);
    }
    let envp = env
        .iter()
        .map(|(key, value)| {
            let mut pair = key.clone();
            pair.push("=");
            pair.push(value);
            c_string(&pair)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let cwd = cwd.map(|dir| c_string(dir.as_os_str())).transpose()?;
    let image = match &trampoline {
        Some(exe) => c_string(exe.as_os_str())?,
        None => c_string(resolved.as_os_str())?,
    };
    let mut argv_ptrs: Vec<*mut c_char> = argv.iter().map(|a| a.as_ptr() as *mut _).collect();
    argv_ptrs.push(std::ptr::null_mut());
    let mut envp_ptrs: Vec<*mut c_char> = envp.iter().map(|e| e.as_ptr() as *mut _).collect();
    envp_ptrs.push(std::ptr::null_mut());
    // SAFETY: every pointer handed to posix_spawn outlives the call, and the
    // attribute and file-action objects are destroyed on every path.
    unsafe {
        let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
        check(libc::posix_spawnattr_init(&mut attr))?;
        let mut actions: libc::posix_spawn_file_actions_t = std::mem::zeroed();
        if let Err(error) = check(libc::posix_spawn_file_actions_init(&mut actions)) {
            libc::posix_spawnattr_destroy(&mut attr);
            return Err(error);
        }
        let result = (|| {
            let flags = POSIX_SPAWN_SETSID
                | libc::POSIX_SPAWN_CLOEXEC_DEFAULT
                | libc::POSIX_SPAWN_SETSIGMASK
                | libc::POSIX_SPAWN_SETSIGDEF;
            check(libc::posix_spawnattr_setflags(&mut attr, flags as _))?;
            let mut empty = std::mem::zeroed();
            libc::sigemptyset(&mut empty);
            check(libc::posix_spawnattr_setsigmask(&mut attr, &empty))?;
            let mut all = std::mem::zeroed();
            libc::sigfillset(&mut all);
            check(libc::posix_spawnattr_setsigdefault(&mut attr, &all))?;
            for fd in 0..3 {
                check(libc::posix_spawn_file_actions_adddup2(
                    &mut actions,
                    tty.as_raw_fd(),
                    fd,
                ))?;
            }
            if let Some(cwd) = &cwd {
                check(posix_spawn_file_actions_addchdir_np(
                    &mut actions,
                    cwd.as_ptr(),
                ))?;
            }
            let mut pid = 0;
            check(libc::posix_spawn(
                &mut pid,
                image.as_ptr(),
                &actions,
                &attr,
                argv_ptrs.as_ptr(),
                envp_ptrs.as_ptr(),
            ))?;
            Ok(pid as u32)
        })();
        libc::posix_spawn_file_actions_destroy(&mut actions);
        libc::posix_spawnattr_destroy(&mut attr);
        result
    }
}

/// This executable, whose image initializer is the trampoline. None (the
/// program then starts without a controlling terminal) if it is gone.
fn trampoline_executable() -> Option<PathBuf> {
    // The initializer lives in this object file; using it keeps the linker
    // from leaving it out of a binary that only calls `spawn`.
    std::hint::black_box(&TRAMPOLINE);
    // Absolute: the child changes directory before it execs.
    match std::env::current_exe().and_then(std::fs::canonicalize) {
        Ok(exe) if super::is_executable(&exe) => Some(exe),
        other => {
            eprintln!("pty: no trampoline ({other:?}); the child has no controlling terminal");
            None
        }
    }
}

fn c_string(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} contains a NUL byte", value.to_string_lossy()),
        )
    })
}

fn check(code: c_int) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code))
    }
}

/// dyld calls the initializers in `__mod_init_func` with argc, argv and
/// envp before `main`, in the app and in test binaries alike. The section
/// type `mod_init_funcs` is what makes dyld call them (std's own
/// initializer is declared the same way).
#[used]
#[link_section = "__DATA,__mod_init_func,mod_init_funcs"]
static TRAMPOLINE: extern "C" fn(c_int, *const *const c_char, *const *const c_char) = trampoline;

/// Runs before `main` in every process of this executable; does nothing
/// unless started by [`spawn`]. Only plain syscalls: the Rust runtime is not
/// set up yet.
extern "C" fn trampoline(argc: c_int, argv: *const *const c_char, envp: *const *const c_char) {
    // SAFETY: dyld passes argc valid argv entries and a NULL-terminated envp.
    unsafe {
        if argc < 4 || std::ffi::CStr::from_ptr(*argv.add(1)).to_bytes() != SENTINEL {
            return;
        }
        // posix_spawn made this process a session leader with the PTY on
        // fd 0; opening no terminal since, it can take it as controlling.
        if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
            report(b"monocode: the terminal is not the controlling one: ");
        }
        // Dispositions and mask were reset by posix_spawn; this image's own
        // initializers may have changed them.
        for signal in 1..NSIG {
            if signal != libc::SIGKILL && signal != libc::SIGSTOP {
                libc::signal(signal, libc::SIG_DFL);
            }
        }
        let mut empty = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
        // Only 0, 1 and 2 came in; whatever the initializers opened stays here.
        let max = libc::getdtablesize().min(4096);
        for fd in 3..max {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let program = *argv.add(2);
        libc::execve(program, argv.add(3), envp);
        report(b"monocode: could not start the program: ");
        libc::_exit(127);
    }
}

/// `message` and the last OS error on stderr, which is the terminal.
unsafe fn report(message: &[u8]) {
    // SAFETY: plain writes of valid buffers.
    unsafe {
        let error = std::ffi::CStr::from_ptr(libc::strerror(*libc::__error()));
        libc::write(2, message.as_ptr().cast(), message.len());
        libc::write(2, error.as_ptr().cast(), error.to_bytes().len());
        libc::write(2, b"\r\n".as_ptr().cast(), 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::AsFd;
    use std::time::{Duration, Instant};

    /// A PTY's controller side and, until a child is started, its user side.
    struct Pty {
        master: std::fs::File,
        slave: Option<OwnedFd>,
        out: String,
    }

    fn open() -> Pty {
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let (master, slave) = openpty(&size).expect("a PTY");
        // SAFETY: flags of our own fd.
        unsafe {
            let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Pty {
            master: std::fs::File::from(master),
            slave: Some(slave),
            out: String::new(),
        }
    }

    fn env(extra: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
            .filter(|(key, _)| key != "ENV" && key != "BASH_ENV" && key != "PS1")
            .collect();
        env.push(("TERM".into(), "xterm-256color".into()));
        for (key, value) in extra {
            env.retain(|(k, _)| k != key);
            env.push(((*key).into(), (*value).into()));
        }
        env
    }

    impl Pty {
        /// Start a child and close this side's copy of the user side, as
        /// the terminal does, so the controller reads EOF once it exits.
        fn spawn(&mut self, program: &str, args: &[&str], extra: &[(&str, &str)]) -> u32 {
            let args: Vec<OsString> = args.iter().map(OsString::from).collect();
            let slave = self.slave.take().expect("one child per PTY");
            spawn(
                slave.as_fd(),
                OsStr::new(program),
                &args,
                &env(extra),
                Some(Path::new("/private/tmp")),
            )
            .expect("spawned")
        }
        fn write(&mut self, bytes: &[u8]) {
            self.master.write_all(bytes).unwrap();
        }
        /// Reads until `needle` was output, or until the child hung up when
        /// `needle` is None. Whether it was seen.
        fn read_until(&mut self, needle: Option<&str>) -> bool {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut buf = [0u8; 4096];
            let seen = |out: &str| needle.is_some_and(|n| out.contains(n));
            while Instant::now() < deadline && !seen(&self.out) {
                match self.master.read(&mut buf) {
                    Ok(n) if n > 0 => self.out.push_str(&String::from_utf8_lossy(&buf[..n])),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    // 0 or EIO: the user side is closed everywhere.
                    _ => return needle.is_none() || seen(&self.out),
                }
            }
            seen(&self.out)
        }
        /// Reads until `needle` was output after the first `mark` bytes.
        fn read_after(&mut self, mark: usize, needle: &str) -> bool {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.out[mark..].contains(needle) {
                if Instant::now() > deadline {
                    return false;
                }
                self.pump();
                std::thread::sleep(Duration::from_millis(10));
            }
            true
        }
        /// Takes in what output is there, without waiting.
        fn pump(&mut self) {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = self.master.read(&mut buf) {
                self.out.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }
        fn foreground(&self) -> libc::pid_t {
            // SAFETY: our own fd.
            unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) }
        }
    }

    /// The child's exit code; None if a signal ended it, or if it is still
    /// running after 10 seconds (it is killed then).
    fn wait(pid: u32) -> Option<c_int> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut status = 0;
        // SAFETY: our own child; WNOHANG never blocks.
        while unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) } == 0 {
            if Instant::now() > deadline {
                // SAFETY: our own, unreaped child.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                    libc::waitpid(pid as libc::pid_t, &mut status, 0);
                }
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status))
    }

    #[test]
    fn pty_spawn_round_trip_tty_cwd_env_and_exit_status() {
        let mut pty = open();
        let script = "tty; pwd; echo \"env:$MONO_PTY_TEST\"; \
                      echo \"tpgid:$(ps -o tpgid= -p $$ | tr -d ' ')\"; \
                      read line; echo \"got:$line\"; exit 7";
        let pid = pty.spawn("/bin/sh", &["-c", script], &[("MONO_PTY_TEST", "yes")]);
        // It leads a session and a process group of its own, as callers
        // that signal the group rely on.
        // SAFETY: plain queries about our own child, not yet reaped.
        unsafe {
            assert_eq!(libc::getsid(pid as libc::pid_t), pid as libc::pid_t);
            assert_eq!(libc::getpgid(pid as libc::pid_t), pid as libc::pid_t);
        }
        assert!(pty.read_until(Some("tpgid:")), "{}", pty.out);
        pty.write(b"hello\n");
        assert!(pty.read_until(Some("got:hello")), "{}", pty.out);
        assert_eq!(wait(pid), Some(7), "{}", pty.out);
        let out = &pty.out;
        assert!(out.contains("/dev/ttys"), "tty names a device: {out}");
        assert!(out.contains("/private/tmp"), "{out}");
        assert!(out.contains("env:yes"), "{out}");
        // The child's group holds the foreground of its controlling terminal.
        assert!(out.contains(&format!("tpgid:{pid}")), "{out}");
    }

    #[test]
    fn pty_spawn_a_bare_name_is_found_on_the_childs_path_and_a_missing_one_fails() {
        let pty = open();
        let missing = spawn(
            pty.slave.as_ref().unwrap().as_fd(),
            OsStr::new("no-such-program-monocode"),
            &[],
            &env(&[]),
            None,
        );
        assert_eq!(missing.unwrap_err().kind(), io::ErrorKind::NotFound);
        let mut pty = open();
        let pid = pty.spawn("echo", &["found"], &[("PATH", "/bin")]);
        assert!(pty.read_until(Some("found")), "{}", pty.out);
        assert_eq!(wait(pid), Some(0));
    }

    #[test]
    fn pty_spawn_a_relative_program_is_found_in_the_childs_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("monocode-pty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("run.sh");
        std::fs::write(&script, "#!/bin/sh\necho relative-ok\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut pty = open();
        let slave = pty.slave.take().unwrap();
        let pid = spawn(
            slave.as_fd(),
            OsStr::new("./run.sh"),
            &[],
            &env(&[]),
            Some(dir.as_path()),
        )
        .expect("spawned");
        drop(slave);
        assert!(pty.read_until(Some("relative-ok")), "{}", pty.out);
        assert_eq!(wait(pid), Some(0), "{}", pty.out);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pty_spawn_only_the_terminal_is_inherited() {
        let mut pty = open();
        // A descriptor without close-on-exec, as a careless library leaves.
        // SAFETY: duplicating our own fd.
        let leaked = unsafe { libc::fcntl(pty.master.as_raw_fd(), libc::F_DUPFD, 200) };
        assert!(leaked >= 200);
        let pid = pty.spawn("/bin/ls", &["/dev/fd"], &[]);
        assert!(pty.read_until(None));
        wait(pid);
        // SAFETY: the duplicate made above.
        unsafe { libc::close(leaked) };
        let fds: Vec<u32> = pty
            .out
            .split_whitespace()
            .filter_map(|fd| fd.parse().ok())
            .collect();
        assert!(fds.starts_with(&[0, 1, 2]), "{}", pty.out);
        // Then only the directory handles ls opens itself.
        assert!(fds.iter().all(|fd| *fd < 8), "{}", pty.out);
    }

    #[test]
    fn pty_spawn_signals_start_unblocked_even_from_a_masking_thread() {
        // A GUI executor's thread may block signals; the child must not.
        let (pid, mut pty) = std::thread::spawn(|| {
            // SAFETY: this thread's own mask.
            unsafe {
                let mut set = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGINT);
                libc::sigaddset(&mut set, libc::SIGWINCH);
                libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
            }
            let mut pty = open();
            // The test harness ignores SIGPIPE; `yes` must die of it, not
            // see EPIPE, so it is back to its default.
            let script = "echo \"mask:$(ps -o sigmask= -p $$ | tr -d ' ')\"; \
                          /usr/bin/yes | /usr/bin/head -1 >/dev/null; \
                          echo \"pipe:${PIPESTATUS[0]}\"";
            let pid = pty.spawn("/bin/bash", &["-c", script], &[]);
            (pid, pty)
        })
        .join()
        .unwrap();
        assert!(pty.read_until(None));
        wait(pid);
        assert!(pty.out.contains("mask:0\r\n"), "{}", pty.out);
        assert!(pty.out.contains("pipe:141"), "{}", pty.out);
    }

    #[test]
    fn pty_spawn_ctrl_c_interrupts_the_foreground_job_of_an_interactive_shell() {
        let mut pty = open();
        let pid = pty.spawn("/bin/zsh", &["-f", "-i"], &[("PS1", "prompt> ")]);
        assert!(pty.read_until(Some("prompt> ")), "{}", pty.out);
        pty.write(b"/bin/sleep 30\r");
        // Job control: sleep runs in a group of its own, in the foreground.
        let deadline = Instant::now() + Duration::from_secs(10);
        while pty.foreground() == pid as i32 || pty.foreground() <= 0 {
            assert!(
                Instant::now() < deadline,
                "no foreground job: {:?}",
                pty.out
            );
            pty.pump();
            std::thread::sleep(Duration::from_millis(20));
        }
        // The terminal discards typeahead on an interrupt, and zle on a new
        // prompt: type each line once the shell prompts for it.
        let mark = pty.out.len();
        pty.write(b"\x03");
        assert!(pty.read_after(mark, "prompt> "), "{}", pty.out);
        let mark = pty.out.len();
        pty.write(b"echo \"after:$?\"\r");
        assert!(pty.read_after(mark, "after:130"), "{}", pty.out);
        assert!(pty.read_after(mark, "prompt> "), "{}", pty.out);
        assert!(!pty.out.contains("job control"), "{}", pty.out);
        assert_eq!(pty.foreground(), pid as i32);
        let mark = pty.out.len();
        pty.write(b"exit 3\r");
        let ok = pty.read_after(mark, "exit 3");
        assert!(ok, "{:?}", pty.out);
        assert_eq!(wait(pid), Some(3), "{}", pty.out);
    }

    #[test]
    fn pty_spawn_a_signal_to_the_group_ends_the_child() {
        let mut pty = open();
        let pid = pty.spawn("/bin/sh", &["-c", "echo ready; /bin/sleep 30"], &[]);
        assert!(pty.read_until(Some("ready")), "{}", pty.out);
        // SAFETY: our own child's group.
        assert_eq!(
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGTERM) },
            0
        );
        // `wait` reports a signal as None, and also kills a child still
        // there after 10 seconds; the time tells the two apart.
        let started = Instant::now();
        assert_eq!(wait(pid), None);
        assert!(started.elapsed() < Duration::from_secs(5));
        // The sleep was in the group too: nothing holds the terminal open.
        assert!(pty.read_until(None), "{}", pty.out);
    }

    #[test]
    fn pty_spawn_a_resize_sends_sigwinch() {
        let mut pty = open();
        let script = "trap 'echo \"winch:$(stty size)\"; exit 0' WINCH; \
                      echo ready; while :; do /bin/sleep 0.05; done";
        let pid = pty.spawn("/bin/bash", &["-c", script], &[]);
        assert!(pty.read_until(Some("ready")), "{}", pty.out);
        let size = libc::winsize {
            ws_row: 33,
            ws_col: 101,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: our own fd and a valid winsize.
        unsafe { libc::ioctl(pty.master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        assert!(pty.read_until(Some("winch:33 101")), "{}", pty.out);
        assert_eq!(wait(pid), Some(0), "{}", pty.out);
    }
}
