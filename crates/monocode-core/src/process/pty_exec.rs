//! The last step of starting a terminal's child without `fork` (see
//! `process::pty`): in a process `posix_spawn` made a session leader with the
//! PTY on fd 0, take the PTY as the controlling terminal and `execve` the
//! program in place.
//!
//! Shared by the small `monocode-pty-exec` executable (which `process::pty`
//! starts when it sits beside the app) and the app's own image-initializer
//! trampoline (the fallback). Only plain system calls: neither has the Rust
//! runtime set up, and this file needs nothing but `libc`.
use std::os::raw::{c_char, c_int};

/// argv[1] of the exec step; argv[2] is the program, argv[3..] its argv.
pub const SENTINEL: &[u8] = b"--monocode-pty-exec";

/// Darwin's `NSIG`: signals are 1 to 31.
const NSIG: c_int = 32;

/// Does nothing unless `argv` is `[_, SENTINEL, program, argv0, ...]`;
/// otherwise takes the terminal and becomes `program`, never returning.
///
/// # Safety
/// `argv` holds `argc` valid C strings; `envp` is a NULL-terminated array of
/// them (what dyld or the C runtime passes).
pub unsafe fn run(argc: c_int, argv: *const *const c_char, envp: *const *const c_char) {
    // SAFETY: the caller's contract.
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
