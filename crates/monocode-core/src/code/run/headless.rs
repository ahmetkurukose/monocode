//! Runs with no terminal, for callers that have none (the agents' MCP
//! tools): the registry starts the process itself in a process group of
//! its own, reads its output into the run's tail and reports its exit. A
//! stop ends the whole group, so a test runner's children go with it.
use super::output::Decoder;
use super::registry::{Registry, RunId, StartedBy};
use crate::code::child::{exited, lead_group};
use crate::code::spawn::Spawn;
use crate::code::toolchain::Toolchain;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How often a run's watcher looks for its exit and for a stop request.
const POLL: Duration = Duration::from_millis(15);
/// How long a process asked to stop may take before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(2);
/// How long output is waited for after the exit, in case a child of the
/// process still holds the pipe open.
const DRAIN: Duration = Duration::from_millis(500);

// The process leads its own group, so a signal to it reaches its children.
#[cfg(unix)]
use crate::code::child::signal_group;

/// Asks the run's process group to end.
fn interrupt(pid: u32) {
    #[cfg(unix)]
    signal_group(pid, libc::SIGTERM);
    // There is no polite request for a console tree on Windows.
    #[cfg(not(unix))]
    kill(pid);
}

/// Ends the run's process group now.
pub(super) fn kill(pid: u32) {
    #[cfg(unix)]
    signal_group(pid, libc::SIGKILL);
    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill");
        command.args(["/PID", &pid.to_string(), "/T", "/F"]);
        crate::process::configure_child(&mut command);
        let _ = command.status();
    }
    #[cfg(not(any(unix, windows)))]
    let _ = pid;
}

fn read_into(
    mut pipe: impl Read + Send + 'static,
    registry: Registry,
    id: RunId,
    done: mpsc::Sender<()>,
) {
    std::thread::spawn(move || {
        let mut decoder = Decoder::default();
        let mut buffer = [0u8; 8192];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => registry.append_output(id, &decoder.push(&buffer[..read])),
            }
        }
        let _ = done.send(());
    });
}

/// Waits for the process, honouring a stop request on the way. Only this
/// thread reaps the child, and only after the group has been killed, so
/// the group it signals is always still its own.
fn watch(mut child: Child, registry: Registry, id: RunId, readers: mpsc::Receiver<()>) {
    std::thread::spawn(move || {
        let pid = child.id();
        let mut asked: Option<Instant> = None;
        let mut killed = false;
        while !exited(&mut child) {
            if registry.stop_requested(id) {
                match asked {
                    None => {
                        interrupt(pid);
                        asked = Some(Instant::now());
                    }
                    Some(since) if !killed && since.elapsed() >= STOP_GRACE => {
                        kill(pid);
                        killed = true;
                    }
                    Some(_) => {}
                }
            }
            std::thread::sleep(POLL);
        }
        // What the process left behind in its group goes with it, however
        // it ended: a run that is over holds no processes, and nothing
        // could stop them afterwards. The leader is not reaped yet, so its
        // exit status is untouched by this.
        kill(pid);
        let status = child.wait().ok();
        let deadline = Instant::now() + DRAIN;
        for _ in 0..2 {
            let left = deadline.saturating_duration_since(Instant::now());
            if readers.recv_timeout(left).is_err() {
                break;
            }
        }
        registry.finish(id, status.and_then(|status| status.code()));
    });
}

impl Registry {
    /// Starts `spawn` without a terminal and follows it like any other
    /// run: output (stdout and stderr together) goes to the run's tail,
    /// the exit is reported, and `stop` ends its process group. The
    /// process gets this app's environment, then `toolchain.environment`,
    /// then the spawn's own variables, and no standard input.
    ///
    /// Returns once the process is started; the error is a sentence for
    /// the caller to show when it could not be.
    pub fn start_headless(
        &self,
        spawn: Spawn,
        checkout: &Path,
        started_by: StartedBy,
        toolchain: &Toolchain,
    ) -> Result<RunId, String> {
        // A bare name is looked up where the checkout's tools are, not on
        // this app's own path.
        let bare = !spawn.command.contains(['/', '\\']);
        let program = match bare {
            true => toolchain.tool(None, &spawn.command),
            false => None,
        };
        let mut command = match &program {
            Some(program) => Command::new(program),
            None => Command::new(&spawn.command),
        };
        command
            .args(&spawn.args)
            .current_dir(&spawn.cwd)
            // Output through a pipe is otherwise held back until the
            // process ends, and a reader would see nothing of a server.
            .env("PYTHONUNBUFFERED", "1")
            .envs(
                toolchain
                    .environment
                    .iter()
                    .map(|(key, value)| (key, value)),
            )
            .envs(spawn.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The user's own program: not throttled like background work.
        crate::process::configure_user_child(&mut command);
        lead_group(&mut command);
        let mut child = command.spawn().map_err(|error| {
            if !spawn.cwd.is_dir() {
                format!("The folder {} does not exist.", spawn.cwd.display())
            } else if error.kind() == std::io::ErrorKind::NotFound {
                format!("The command {} was not found.", spawn.command)
            } else {
                format!("{} could not be started: {error}.", spawn.command)
            }
        })?;
        let id = self.insert(spawn, checkout, started_by, true, Some(child.id()));
        let (done, readers) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            read_into(stdout, self.clone(), id, done.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            read_into(stderr, self.clone(), id, done);
        }
        watch(child, self.clone(), id, readers);
        Ok(id)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::registry::{RunEvent, RunInfo, RunState};
    use super::*;

    fn shell(script: &str, cwd: &Path) -> Spawn {
        Spawn {
            label: "script".into(),
            command: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: cwd.to_path_buf(),
            env: vec![("FROM_RUN".into(), "run".into())],
        }
    }

    fn wait_until(registry: &Registry, id: RunId, done: impl Fn(&RunInfo) -> bool) -> RunInfo {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let info = registry.get(id).expect("the run is registered");
            if done(&info) {
                return info;
            }
            assert!(Instant::now() < deadline, "timed out: {info:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn finished(registry: &Registry, id: RunId) -> RunInfo {
        wait_until(registry, id, |info| !info.state.is_running())
    }

    #[test]
    fn output_and_the_exit_code_are_captured() {
        let folder = tempfile::tempdir().unwrap();
        let registry = Registry::new();
        let events = registry.subscribe();
        let toolchain = Toolchain {
            environment: vec![
                ("FROM_SETUP".into(), "setup".into()),
                ("FROM_RUN".into(), "overridden".into()),
            ],
            ..Toolchain::default()
        };
        let script = "echo \"out $FROM_SETUP $FROM_RUN\"; pwd; echo err >&2; \
                      read line || echo no-input; exit 3";
        let id = registry
            .start_headless(
                shell(script, folder.path()),
                folder.path(),
                StartedBy::Agent(5),
                &toolchain,
            )
            .unwrap();
        let started = registry.get(id).unwrap();
        assert!(started.headless && started.pid.is_some());
        assert_eq!(started.started_by, StartedBy::Agent(5));

        let info = finished(&registry, id);
        assert_eq!(info.state, RunState::Exited(Some(3)));
        let output = registry.output(id, 0).unwrap().text;
        assert!(output.contains("out setup run\n"), "{output}");
        assert!(output.contains("err\n"), "{output}");
        assert!(output.contains("no-input\n"), "{output}");
        let cwd = folder.path().canonicalize().unwrap();
        assert!(output.contains(&*cwd.to_string_lossy()), "{output}");

        let heard: Vec<RunEvent> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert_eq!(heard.first(), Some(&RunEvent::Started(id)));
        assert_eq!(heard.last(), Some(&RunEvent::Changed(id)));
        assert!(heard.contains(&RunEvent::Output(id)));
        assert!(!registry.stop(id));
    }

    #[test]
    fn a_stop_ends_the_whole_process_group() {
        let folder = tempfile::tempdir().unwrap();
        let registry = Registry::new();
        // The shell's child must go with it.
        let script = "echo started; sleep 60 & wait";
        let id = registry
            .start_headless(
                shell(script, folder.path()),
                folder.path(),
                StartedBy::User,
                &Toolchain::default(),
            )
            .unwrap();
        let pid = registry.get(id).unwrap().pid.unwrap();
        wait_until(&registry, id, |_| {
            registry
                .output(id, 0)
                .is_some_and(|output| output.text.contains("started"))
        });
        assert_eq!(unsafe { libc::kill(-(pid as i32), 0) }, 0);
        let asked = Instant::now();
        assert!(registry.stop(id));
        assert!(registry.stop(id), "asking twice is harmless");
        let info = finished(&registry, id);
        assert_eq!(info.state, RunState::Stopped);
        assert!(info.stop_requested);
        assert!(asked.elapsed() < STOP_GRACE, "ended on the first request");
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { libc::kill(-(pid as i32), 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            unsafe { libc::kill(-(pid as i32), 0) },
            -1,
            "the group is gone"
        );
    }

    #[test]
    fn what_a_finished_run_left_running_is_killed() {
        let folder = tempfile::tempdir().unwrap();
        let registry = Registry::new();
        // The shell exits at once and leaves a child behind in its group.
        let script = "sleep 60 & echo started; exit 4";
        let id = registry
            .start_headless(
                shell(script, folder.path()),
                folder.path(),
                StartedBy::User,
                &Toolchain::default(),
            )
            .unwrap();
        let pid = registry.get(id).unwrap().pid.unwrap();
        let ended = Instant::now();
        let info = finished(&registry, id);
        // Its own exit code, not the kill's, and without waiting out the
        // child that held the output open.
        assert_eq!(info.state, RunState::Exited(Some(4)));
        assert!(ended.elapsed() < Duration::from_secs(10));
        assert!(registry.output(id, 0).unwrap().text.contains("started"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { libc::kill(-(pid as i32), 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            unsafe { libc::kill(-(pid as i32), 0) },
            -1,
            "the group is gone"
        );
    }

    /// A run is the user's program: it starts at this app's own priority,
    /// not at the one background work is given.
    #[test]
    fn a_run_is_not_niced() {
        let folder = tempfile::tempdir().unwrap();
        let registry = Registry::new();
        let id = registry
            .start_headless(
                shell("ps -o nice= -p $$", folder.path()),
                folder.path(),
                StartedBy::User,
                &Toolchain::default(),
            )
            .unwrap();
        assert_eq!(finished(&registry, id).state, RunState::Exited(Some(0)));
        let ours = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
        let output = registry.output(id, 0).unwrap().text;
        assert_eq!(output.trim().parse::<i32>().ok(), Some(ours), "{output}");
    }

    #[test]
    fn a_process_that_ignores_the_request_is_killed() {
        let folder = tempfile::tempdir().unwrap();
        let registry = Registry::new();
        let script = "trap '' TERM; echo started; while :; do sleep 0.05; done";
        let id = registry
            .start_headless(
                shell(script, folder.path()),
                folder.path(),
                StartedBy::User,
                &Toolchain::default(),
            )
            .unwrap();
        wait_until(&registry, id, |_| {
            registry
                .output(id, 0)
                .is_some_and(|output| output.text.contains("started"))
        });
        let asked = Instant::now();
        assert!(registry.stop(id));
        assert_eq!(finished(&registry, id).state, RunState::Stopped);
        assert!(asked.elapsed() >= STOP_GRACE);
    }

    #[test]
    fn shutdown_kills_what_is_running() {
        let folder = tempfile::tempdir().unwrap();
        let registry = Registry::new();
        let id = registry
            .start_headless(
                shell("sleep 60", folder.path()),
                folder.path(),
                StartedBy::User,
                &Toolchain::default(),
            )
            .unwrap();
        registry.shutdown();
        // Nobody asked for a stop: the run reports a death by signal.
        assert_eq!(finished(&registry, id).state, RunState::Exited(None));
    }

    #[test]
    fn a_bare_command_is_found_on_the_checkouts_path() {
        let folder = tempfile::tempdir().unwrap();
        let tool = folder.path().join("bin/greet");
        std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
        std::fs::write(&tool, "#!/bin/sh\necho \"hello $1\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let toolchain = Toolchain {
            search_path: folder.path().join("bin").to_string_lossy().into_owned(),
            ..Toolchain::default()
        };
        let registry = Registry::new();
        let spawn = Spawn {
            label: "greet".into(),
            command: "greet".into(),
            args: vec!["you there".into()],
            cwd: folder.path().to_path_buf(),
            env: Vec::new(),
        };
        let id = registry
            .start_headless(spawn, folder.path(), StartedBy::User, &toolchain)
            .unwrap();
        assert_eq!(finished(&registry, id).state, RunState::Exited(Some(0)));
        assert_eq!(registry.output(id, 0).unwrap().text, "hello you there\n");
    }

    #[test]
    fn what_cannot_start_is_said_plainly_and_not_registered() {
        let folder = tempfile::tempdir().unwrap();
        let registry = Registry::new();
        let mut absent = shell("true", folder.path());
        absent.command = "/no/such/program".into();
        let error = registry
            .start_headless(
                absent,
                folder.path(),
                StartedBy::User,
                &Toolchain::default(),
            )
            .unwrap_err();
        assert_eq!(error, "The command /no/such/program was not found.");
        let nowhere = shell("true", &folder.path().join("missing"));
        let error = registry
            .start_headless(
                nowhere,
                folder.path(),
                StartedBy::User,
                &Toolchain::default(),
            )
            .unwrap_err();
        assert!(
            error.starts_with("The folder ") && error.ends_with(" does not exist."),
            "{error}"
        );
        assert!(registry.list().is_empty());
    }

    /// The whole path with real tools: a marker in a test file becomes a
    /// task, the task a process, and pytest's verdict is the exit code.
    #[test]
    fn a_real_pytest_single_test_runs_headlessly() {
        use super::super::{
            resolve, runnable_tasks, RunContext, Runnable, RunnableKind, TestRunner,
        };
        let Some(python) = std::env::var_os("MONOCODE_TEST_PYTHON") else {
            return;
        };
        let python = std::path::PathBuf::from(python);
        let venv = python
            .parent()
            .and_then(Path::parent)
            .expect("a venv interpreter");
        let toolchain = Toolchain {
            venv: Some(venv.to_path_buf()),
            ..Toolchain::default()
        };
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().canonicalize().unwrap().join("my project");
        let file = checkout.join("tests/test_orders.py");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            "import unittest\n\n\nclass TestOrders(unittest.TestCase):\n    def test_post(self):\n        print('posting')\n        assert 1 + 1 == 2\n\n    def test_broken(self):\n        assert 1 + 1 == 3\n",
        )
        .unwrap();
        let context = RunContext::new(&checkout, &checkout).with_file(&file);
        let registry = Registry::new();
        let run = |name: &str, runner: TestRunner| {
            let marker = Runnable {
                kind: RunnableKind::PythonTestFunction,
                name: name.into(),
                class_name: Some("TestOrders".into()),
                row: 4,
            };
            let task = runnable_tasks(&marker, &context, runner).remove(0);
            let spawn = resolve(&task, &context, &toolchain).unwrap();
            assert_eq!(spawn.command, python.to_string_lossy());
            let id = registry
                .start_headless(spawn, &checkout, StartedBy::Agent(1), &toolchain)
                .unwrap();
            let info = finished(&registry, id);
            (info.state, registry.output(id, 0).unwrap().text)
        };
        let (state, output) = run("test_post", TestRunner::Pytest);
        assert_eq!(state, RunState::Exited(Some(0)), "{output}");
        assert!(output.contains("1 passed"), "{output}");
        let (state, output) = run("test_broken", TestRunner::Pytest);
        assert_eq!(state, RunState::Exited(Some(1)), "{output}");
        assert!(output.contains("1 failed"), "{output}");
        assert!(
            output.contains("test_orders.py::TestOrders::test_broken"),
            "{output}"
        );
        let (state, output) = run("test_post", TestRunner::Unittest);
        assert_eq!(state, RunState::Exited(Some(0)), "{output}");
        assert!(output.contains("Ran 1 test"), "{output}");
        let (state, output) = run("test_broken", TestRunner::Unittest);
        assert_eq!(state, RunState::Exited(Some(1)), "{output}");
        assert!(output.contains("FAILED"), "{output}");
    }
}
