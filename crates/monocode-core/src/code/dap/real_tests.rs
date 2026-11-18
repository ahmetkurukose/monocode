//! The debugger against the real debugpy. These need an interpreter with
//! debugpy and pytest installed, named by `MONOCODE_TEST_PYTHON`; without
//! the variable they pass without doing anything.
use super::adapter::{Console, LaunchOptions};
use super::breakpoints::{Breakpoint, BreakpointStatus, BreakpointStore, ExceptionFilters};
use super::fake::wait_until;
use super::manager::Manager;
use super::session::Session;
use super::types::{Actor, EvaluateContext, OutputCategory, SessionState, StepKind, Stop};
use crate::code::spawn::Spawn;
use crate::code::toolchain::Toolchain;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const PATIENCE: Duration = Duration::from_secs(30);

struct Project {
    _folder: tempfile::TempDir,
    root: PathBuf,
    toolchain: Toolchain,
    manager: Manager,
}

/// A scratch project with these files, or `None` when the real tools are
/// not to be used.
fn project(files: &[(&str, &str)]) -> Option<Project> {
    let python = PathBuf::from(std::env::var_os("MONOCODE_TEST_PYTHON")?);
    let folder = tempfile::tempdir().unwrap();
    // The debugger reports resolved paths; breakpoints must be set on them.
    let root = folder.path().canonicalize().unwrap();
    for (name, text) in files {
        std::fs::write(root.join(name), text).unwrap();
    }
    let venv = python
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf();
    let search_path = std::env::var("PATH").unwrap_or_default();
    let toolchain = Toolchain {
        venv: Some(venv),
        environment: vec![("PATH".into(), search_path.clone())],
        search_path,
    };
    Some(Project {
        _folder: folder,
        root,
        toolchain,
        manager: Manager::new(BreakpointStore::new()),
    })
}

impl Project {
    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn spawn(&self, command: &str, args: &[&str]) -> Spawn {
        Spawn {
            label: "test".into(),
            command: command.into(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            cwd: self.root.clone(),
            env: vec![("MONOCODE_DEBUG_TEST".into(), "yes".into())],
        }
    }

    fn debug(&self, command: &str, args: &[&str]) -> Arc<Session> {
        let options = LaunchOptions {
            console: Console::InternalConsole,
            ..LaunchOptions::default()
        };
        self.manager
            .start(
                &self.spawn(command, args),
                &self.toolchain,
                &options,
                Actor::User,
            )
            .unwrap()
    }
}

fn stop_of(session: &Session) -> Stop {
    match session.wait_for_stop(PATIENCE).unwrap() {
        SessionState::Stopped(stop) => stop,
        other => panic!("not stopped: {other:?}\n{}", printed(session)),
    }
}

fn printed(session: &Session) -> String {
    session
        .output()
        .iter()
        .filter(|output| output.category != OutputCategory::Console)
        .map(|output| output.text.as_str())
        .collect()
}

const APP: &str = "\
import os
import sys


def work(n):
    total = n * 2
    items = list(range(total))
    print('total', total, len(items))
    return total


result = work(21)
print('done', result, os.environ.get('MONOCODE_DEBUG_TEST'))
sys.exit(4)
";

#[test]
fn a_script_is_launched_stopped_inspected_stepped_and_run_to_its_end() {
    let Some(project) = project(&[("app.py", APP)]) else {
        return;
    };
    let app = project.path("app.py");
    let store = project.manager.breakpoints();
    store.toggle(&app, 7);
    let session = project.debug("python", &["app.py"]);
    assert_eq!(project.manager.sessions().len(), 1);
    assert!(project.manager.get(session.id()).is_some());

    let stop = stop_of(&session);
    assert_eq!(stop.reason, "breakpoint");
    assert_eq!(
        (stop.path.as_deref(), stop.line),
        (Some(app.as_path()), Some(7))
    );
    assert_eq!(
        store.in_file(&app)[0].status,
        BreakpointStatus::Verified { line: 7 }
    );

    // The stack and the locals of its top frame.
    let stack = session.stack_with_locals(1).unwrap();
    let names: Vec<&str> = stack
        .iter()
        .map(|entry| entry.frame.name.as_str())
        .collect();
    assert_eq!(names, ["work", "<module>"]);
    let local = |name: &str| {
        let found = stack[0]
            .locals
            .iter()
            .find(|variable| variable.name == name);
        found.map(|variable| variable.value.clone())
    };
    assert_eq!(local("n").as_deref(), Some("21"));
    assert_eq!(local("total").as_deref(), Some("42"));
    assert_eq!(local("items"), None);

    let frame = stop.frame_id;
    let sum = session
        .evaluate("n + total", frame, EvaluateContext::Watch)
        .unwrap();
    assert_eq!(
        (sum.result.as_str(), sum.type_name.as_deref()),
        ("63", Some("int"))
    );
    assert!(session
        .evaluate("n +", frame, EvaluateContext::Watch)
        .is_err());
    assert_eq!(
        session
            .evaluate("total", frame, EvaluateContext::Hover)
            .unwrap()
            .result,
        "42"
    );

    // One line on, the list exists and its elements can be paged through.
    let stop = session.step_and_wait(StepKind::Over, PATIENCE).unwrap();
    let stop = stop.stop().cloned().unwrap();
    assert_eq!((stop.reason.as_str(), stop.line), ("step", Some(8)));
    let stack = session.stack_with_locals(1).unwrap();
    let items = stack[0]
        .locals
        .iter()
        .find(|variable| variable.name == "items")
        .unwrap();
    assert_ne!(items.variables_reference, 0);
    let page = super::types::Page {
        start: 40,
        count: 2,
    };
    let elements = session
        .variables(items.variables_reference, Some(page))
        .unwrap();
    assert!(!elements.is_empty() && elements.len() <= 2, "{elements:?}");

    // A changed variable is what the program goes on with.
    let scopes = session.scopes(stop.frame_id.unwrap()).unwrap();
    let locals = scopes.iter().find(|scope| !scope.expensive).unwrap();
    assert!(session.supports("supportsSetVariable"));
    let changed = session
        .set_variable(locals.variables_reference, "total", "50")
        .unwrap();
    assert_eq!(changed.result, "50");

    // debugpy counts `sys.exit` with a non-zero code as an uncaught
    // exception and stops on it while that filter is on.
    let state = session.continue_and_wait(PATIENCE).unwrap();
    let stop = state.stop().cloned().unwrap();
    assert_eq!((stop.reason.as_str(), stop.line), ("exception", Some(14)));
    assert_eq!(stop.exception.as_deref(), Some("SystemExit: 4"));
    let state = session.continue_and_wait(PATIENCE).unwrap();
    assert_eq!(
        state,
        SessionState::Terminated { exit_code: Some(4) },
        "{}",
        printed(&session)
    );
    wait_until("the output", || printed(&session).contains("done"));
    let text = printed(&session);
    assert!(text.contains("total 50 42"), "{text}");
    assert!(text.contains("done 50 yes"), "{text}");
    // Nothing stays verified once the session is over.
    wait_until("idle breakpoints", || {
        store.in_file(&app)[0].status == BreakpointStatus::Idle
    });
}

const LOOP: &str = "\
squares = []
for i in range(6):
    squares.append(i * i)
print(squares)
";

#[test]
fn a_conditional_breakpoint_stops_only_when_its_condition_holds() {
    let Some(project) = project(&[("loop.py", LOOP)]) else {
        return;
    };
    let file = project.path("loop.py");
    let store = project.manager.breakpoints();
    store.set(&file, Breakpoint::at(3).with_condition("i == 4"));
    let session = project.debug("python", &["loop.py"]);
    let stop = stop_of(&session);
    assert_eq!(stop.line, Some(3));
    let value = session
        .evaluate("i", stop.frame_id, EvaluateContext::Repl)
        .unwrap();
    assert_eq!(value.result, "4");

    // A breakpoint added while stopped is honoured; a removed one is not.
    store.remove(&file, 3);
    store.toggle(&file, 4);
    wait_until("the new breakpoint", || {
        store.in_file(&file).first().map(|b| b.status.clone())
            == Some(BreakpointStatus::Verified { line: 4 })
    });
    let state = session.continue_and_wait(PATIENCE).unwrap();
    assert_eq!(state.stop().and_then(|stop| stop.line), Some(4));
    let state = session.continue_and_wait(PATIENCE).unwrap();
    assert_eq!(state, SessionState::Terminated { exit_code: Some(0) });
}

const RAISES: &str = "\
def parse(text):
    return int(text)


parse('not a number')
";

#[test]
fn an_uncaught_exception_stops_the_program_where_it_was_raised() {
    let Some(project) = project(&[("raises.py", RAISES)]) else {
        return;
    };
    let session = project.debug("python", &["raises.py"]);
    let stop = stop_of(&session);
    assert_eq!(stop.reason, "exception");
    assert_eq!(stop.line, Some(2));
    let text = stop.exception.unwrap();
    assert!(
        text.contains("ValueError") && text.contains("not a number"),
        "{text}"
    );
    let state = session.continue_and_wait(PATIENCE).unwrap();
    assert_eq!(state, SessionState::Terminated { exit_code: Some(1) });

    // With the filter off, the same program runs to its failure.
    project
        .manager
        .breakpoints()
        .set_exceptions(ExceptionFilters {
            raised: false,
            uncaught: false,
        });
    let session = project.debug("python", &["raises.py"]);
    let state = session.wait_for_stop(PATIENCE).unwrap();
    assert_eq!(state, SessionState::Terminated { exit_code: Some(1) });
    wait_until("the traceback", || printed(&session).contains("ValueError"));
}

const TEST: &str = "\
def add(a, b):
    return a + b


def test_add():
    value = add(2, 3)
    assert value == 5
";

#[test]
fn a_pytest_run_stops_inside_the_test() {
    let Some(project) = project(&[("test_sample.py", TEST)]) else {
        return;
    };
    let file = project.path("test_sample.py");
    project.manager.breakpoints().toggle(&file, 7);
    let session = project.debug(
        "pytest",
        &["test_sample.py", "-q", "-p", "no:cacheprovider"],
    );
    assert_eq!(session.request().arguments["module"], "pytest");
    let stop = stop_of(&session);
    assert_eq!(
        (stop.path.as_deref(), stop.line),
        (Some(file.as_path()), Some(7))
    );
    let stack = session.stack_with_locals(1).unwrap();
    assert_eq!(stack[0].frame.name, "test_add");
    assert_eq!(stack[0].locals[0].name, "value");
    assert_eq!(stack[0].locals[0].value, "5");
    let state = session.continue_and_wait(PATIENCE).unwrap();
    assert_eq!(
        state,
        SessionState::Terminated { exit_code: Some(0) },
        "{}",
        printed(&session)
    );
    wait_until("pytest's summary", || {
        printed(&session).contains("1 passed")
    });
}

const PARENT: &str = "\
import subprocess
import sys

code = subprocess.run([sys.executable, 'child.py', 'seven']).returncode
print('child exited with', code)
sys.exit(code)
";

const CHILD: &str = "\
import sys

word = sys.argv[1]
print('child got', word)
sys.exit(7)
";

#[test]
fn a_subprocess_is_debugged_as_a_child_session() {
    let Some(project) = project(&[("parent.py", PARENT), ("child.py", CHILD)]) else {
        return;
    };
    let file = project.path("child.py");
    project.manager.breakpoints().toggle(&file, 4);
    // Exit codes are what this test follows, not stops on `sys.exit`.
    project
        .manager
        .breakpoints()
        .set_exceptions(ExceptionFilters {
            raised: false,
            uncaught: false,
        });
    let parent = project.debug("python", &["parent.py"]);
    let events = parent.subscribe();
    wait_until("the child session", || !parent.children().is_empty());
    let child = parent.children().remove(0);
    assert_eq!(child.parent(), Some(parent.id()));
    assert!(project.manager.get(child.id()).is_some());
    assert_eq!(project.manager.sessions().len(), 1);
    let announced = std::iter::from_fn(|| events.try_recv().ok())
        .any(|event| matches!(event, super::SessionEvent::ChildStarted(id) if id == child.id()));
    assert!(announced);

    let stop = stop_of(&child);
    assert_eq!(
        (stop.path.as_deref(), stop.line),
        (Some(file.as_path()), Some(4))
    );
    let word = child
        .evaluate("word", stop.frame_id, EvaluateContext::Watch)
        .unwrap();
    assert_eq!(word.result, "'seven'");
    // The parent waits for its child meanwhile.
    assert_eq!(parent.state(), SessionState::Running);

    let state = child.continue_and_wait(PATIENCE).unwrap();
    assert!(state.is_terminated(), "{state:?}");
    assert_eq!(parent.wait_for_exit(PATIENCE), Ok(Some(7)));
    wait_until("the parent's output", || {
        printed(&parent).contains("child exited with 7")
    });
}

const WAITS: &str = "\
import os
import time

pid = os.getpid()
print('ready', pid, flush=True)
while True:
    time.sleep(0.05)
";

/// `debugpy.adapter` stays up after its debuggee is gone, waiting for a
/// client with nothing more to ask: ending the session has to end it.
#[cfg(unix)]
#[test]
fn the_adapter_goes_when_the_program_ends() {
    let Some(project) = project(&[("quick.py", "print('done')\n")]) else {
        return;
    };
    let session = project.debug("python", &["quick.py"]);
    let adapter = session.adapter_process_id().expect("a process of its own");
    assert!(alive(adapter as i32));
    assert_eq!(session.wait_for_exit(PATIENCE), Ok(Some(0)));
    wait_until("the adapter to leave", || !alive(adapter as i32));
}

#[cfg(unix)]
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(unix)]
#[test]
fn sessions_are_paused_restarted_and_killed_with_the_manager() {
    let Some(project) = project(&[("waits.py", WAITS)]) else {
        return;
    };
    let session = project.debug("python", &["waits.py"]);
    wait_until("the program", || printed(&session).contains("ready"));
    assert_eq!(session.state(), SessionState::Running);

    // Pause a running program, look around, and let it go on.
    session.threads().unwrap();
    session.pause(None);
    let stop = stop_of(&session);
    assert_eq!(stop.reason, "pause");
    let stack = session.stack_with_locals(8).unwrap();
    let module = stack
        .iter()
        .find(|entry| entry.frame.name == "<module>")
        .unwrap();
    let pid = module
        .locals
        .iter()
        .find(|variable| variable.name == "pid")
        .unwrap();
    let first: i32 = pid.value.parse().unwrap();
    assert!(alive(first));
    session.resume(None);
    wait_until("running again", || session.state() == SessionState::Running);

    // A restart ends that process and starts the request anew, in place.
    session.take_over(Actor::Agent("agent-7".into()));
    let restarted = project.manager.restart(session.id()).unwrap();
    assert!(session.state().is_terminated());
    wait_until("the first process to die", || !alive(first));
    assert_ne!(restarted.id(), session.id());
    assert_eq!(restarted.driver(), Actor::Agent("agent-7".into()));
    assert_eq!(restarted.origin(), &Actor::User);
    assert_eq!(project.manager.sessions().len(), 1);
    assert!(project.manager.get(session.id()).is_none());
    wait_until("the program again", || {
        printed(&restarted).contains("ready")
    });
    let text = printed(&restarted);
    let second: i32 = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(alive(second) && second != first);

    // Dropping the manager leaves nothing running.
    let Project { manager, .. } = project;
    drop(manager);
    assert!(restarted.state().is_terminated());
    wait_until("the second process to die", || !alive(second));
}

#[cfg(unix)]
#[test]
fn the_debuggee_runs_in_a_terminal_the_caller_starts() {
    let Some(project) = project(&[("app.py", APP)]) else {
        return;
    };
    let app = project.path("app.py");
    project.manager.breakpoints().toggle(&app, 8);
    project
        .manager
        .breakpoints()
        .set_exceptions(ExceptionFilters {
            raised: false,
            uncaught: false,
        });
    let options = LaunchOptions::default();
    assert_eq!(options.console, Console::IntegratedTerminal);
    let session = project
        .manager
        .start(
            &project.spawn("python", &["app.py"]),
            &project.toolchain,
            &options,
            Actor::User,
        )
        .unwrap();
    wait_until("the terminal request", || {
        session.pending_terminal().is_some()
    });
    let terminal = session.pending_terminal().unwrap();

    // What a terminal pane does: run the command in the checkout's
    // environment plus the request's, and say which process it is.
    let spawn = &terminal.spawn;
    assert_eq!(spawn.cwd, project.root);
    let mut command = std::process::Command::new(&spawn.command);
    command
        .args(&spawn.args)
        .current_dir(&spawn.cwd)
        .envs(project.toolchain.environment.iter().map(|(k, v)| (k, v)))
        .envs(spawn.env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped());
    let process = command.spawn().unwrap();
    terminal.started(Some(process.id()));

    let stop = stop_of(&session);
    assert_eq!(
        (stop.path.as_deref(), stop.line),
        (Some(app.as_path()), Some(8))
    );
    let state = session.continue_and_wait(PATIENCE).unwrap();
    assert!(state.is_terminated(), "{state:?}");
    // The output went to the terminal, not through the debugger.
    let output = process.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(4));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("done 42 yes"), "{text}");
    assert!(!printed(&session).contains("done"));
}
