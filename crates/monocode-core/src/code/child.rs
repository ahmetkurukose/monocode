//! Child processes that lead a process group of their own: runs, language
//! servers and debug adapters. A launcher's real work is often done by its
//! children (node under a pip script, a test runner's workers), so ending
//! one means ending the group, also after the leader itself has gone.
//!
//! The rule that makes that safe: the leader is never reaped before its
//! group is signalled. While it is unreaped, even as a zombie, its id is
//! still its own and so is the group's; once reaped, the id may pass to
//! any new process. So `exited` looks without reaping, and `kill_group`
//! signals first and reaps last.
use std::process::{Child, Command};

/// Makes `command` the leader of a new process group.
pub(crate) fn lead_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let _ = command;
}

/// Whether the child has ended. On Unix it is left unreaped, so its group
/// can still be signalled; `kill_group` or `Child::wait` reaps it.
pub(crate) fn exited(child: &mut Child) -> bool {
    #[cfg(unix)]
    {
        let id = child.id() as libc::id_t;
        // Zeroed first: with nothing to report the call leaves it alone.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        // An error means the child is not ours to wait for any more.
        result != 0 || info.si_signo == libc::SIGCHLD
    }
    #[cfg(not(unix))]
    {
        !matches!(child.try_wait(), Ok(None))
    }
}

/// Sends `signal` to the group `pid` leads.
#[cfg(unix)]
pub(crate) fn signal_group(pid: u32, signal: i32) {
    if let Ok(group) = libc::pid_t::try_from(pid) {
        // Never 0 or 1: those would be this app's own group, or everything.
        if group > 1 {
            unsafe {
                libc::killpg(group, signal);
            }
        }
    }
}

/// Ends the child and everything in its group, then reaps it. The group is
/// signalled whether or not the leader still runs: what it started may have
/// outlived it.
pub(crate) fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    signal_group(child.id(), libc::SIGKILL);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    fn group_alive(pid: u32) -> bool {
        unsafe { libc::kill(-(pid as i32), 0) == 0 }
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn an_exit_is_seen_without_losing_the_status() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 0.2; exit 7"]);
        lead_group(&mut command);
        let mut child = command.spawn().unwrap();
        assert!(!exited(&mut child));
        wait_for("the exit", || exited(&mut child));
        // Seen, seen again, and still there to be collected.
        assert!(exited(&mut child));
        assert_eq!(child.wait().unwrap().code(), Some(7));
    }

    #[test]
    fn workers_left_by_a_leader_that_exited_are_killed_with_the_group() {
        // The launcher starts a worker and leaves, as a pip script that
        // runs node does when it is asked to exit.
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 60 & exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        lead_group(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        wait_for("the leader's exit", || exited(&mut child));
        assert!(group_alive(pid), "the worker outlives its launcher");
        kill_group(&mut child);
        wait_for("the group to go", || !group_alive(pid));
    }
}
