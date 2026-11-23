//! `monocode-pty-exec`, the small executable that starts terminal children
//! on macOS (`process::pty`), gives the child the PTY as its controlling
//! terminal like the app's own trampoline does.
#![cfg(target_os = "macos")]

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd};
use std::time::{Duration, Instant};

#[test]
fn pty_spawn_through_the_exec_helper_gets_a_controlling_terminal() {
    // Only this test runs in this binary, so setting the variable races nothing.
    std::env::set_var("MONOCODE_PTY_EXEC", env!("CARGO_BIN_EXE_monocode-pty-exec"));
    let size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let (master, slave) = monocode_core::process::pty::openpty(&size).expect("a PTY");
    let script = "tty; echo \"tpgid:$(ps -o tpgid= -p $$ | tr -d ' ')\"; exit 3";
    let env: Vec<(OsString, OsString)> = vec![("PATH".into(), "/usr/bin:/bin".into())];
    let pid = monocode_core::process::pty::spawn(
        slave.as_fd(),
        OsStr::new("/bin/sh"),
        &["-c".into(), script.into()],
        &env,
        None,
    )
    .expect("spawned");
    drop(slave);
    let mut master = std::fs::File::from(master);
    let mut out = String::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = [0u8; 4096];
    // Blocking reads end with EIO once the child closed the user side.
    while Instant::now() < deadline {
        match master.read(&mut buf) {
            Ok(n) if n > 0 => out.push_str(&String::from_utf8_lossy(&buf[..n])),
            _ => break,
        }
    }
    let mut status = 0;
    // SAFETY: our own child.
    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
    assert!(libc::WIFEXITED(status), "{out}");
    assert_eq!(libc::WEXITSTATUS(status), 3, "{out}");
    assert!(out.contains("/dev/ttys"), "{out}");
    assert!(out.contains(&format!("tpgid:{pid}")), "{out}");
    let _ = master.as_raw_fd();
}
