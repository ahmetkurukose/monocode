//! Starts a terminal's child on macOS: `process::pty` spawns this small
//! executable (no `fork`) as a session leader with the PTY on fd 0; it takes
//! the PTY as the controlling terminal and execs the program in place. Far
//! cheaper than loading the app's own image for the same step.
//!
//! A C `main`: no Rust runtime, which would install signal handlers first.
#![cfg_attr(target_os = "macos", no_main)]

#[cfg(target_os = "macos")]
#[path = "../process/pty_exec.rs"]
mod pty_exec;

#[cfg(target_os = "macos")]
/// # Safety
/// Called by the C runtime only, with its argv and envp.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn main(
    argc: std::os::raw::c_int,
    argv: *const *const std::os::raw::c_char,
    envp: *const *const std::os::raw::c_char,
) -> std::os::raw::c_int {
    // SAFETY: the C runtime passes valid argv and envp.
    unsafe { pty_exec::run(argc, argv, envp) };
    const USAGE: &[u8] = b"monocode-pty-exec: started by Monocode's terminal only\n";
    // SAFETY: a plain write of a valid buffer.
    unsafe { libc::write(2, USAGE.as_ptr().cast(), USAGE.len()) };
    2
}

#[cfg(not(target_os = "macos"))]
fn main() {}
