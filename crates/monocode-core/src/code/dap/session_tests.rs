//! The session model against a scripted adapter that behaves like debugpy:
//! the start handshake, the cache and its events, breakpoints both ways,
//! the terminal and child-session requests, and the state transitions.
use super::adapter::{adapter, derive, Console, LaunchOptions, DEBUGPY};
use super::breakpoints::{Breakpoint, BreakpointStatus, BreakpointStore, ExceptionFilters};
use super::fake::{fake, fake_listening, wait_until, Peer};
use super::session::{Refresh, Session, SessionEvent, Setup};
use super::types::{
    Actor, DapError, EvaluateContext, OutputCategory, Page, SessionState, StepKind,
};
use crate::code::spawn::Spawn;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const SOON: Duration = Duration::from_secs(5);
const APP: &str = "/p/app.py";

type Log = Arc<Mutex<Vec<Value>>>;

#[derive(Clone, Copy, Default)]
struct Plan {
    /// Stop at a breakpoint as soon as the debuggee runs.
    stop_at_start: bool,
    refuse_launch: bool,
    /// Announce a subprocess whose adapter listens here.
    child_port: Option<u16>,
    /// Hang up instead of answering `continue`.
    die_on_continue: bool,
}

/// An adapter with one thread in `/p/app.py`. The first `continue` runs
/// into an exception, the second to the end with exit code 3.
fn python_like(peer: &mut Peer, log: &Log, plan: Plan) {
    let mut launch = None;
    let mut line = 10;
    let mut continues = 0;
    let mut n = "1".to_string();
    let mut child_port = plan.child_port;
    while let Some(message) = peer.next() {
        log.lock().unwrap().push(message.clone());
        if message["type"] != json!("request") {
            continue;
        }
        let arguments = &message["arguments"];
        match message["command"].as_str().unwrap() {
            "initialize" => peer.respond(
                &message,
                json!({
                    "supportsConfigurationDoneRequest": true,
                    "supportsConditionalBreakpoints": true,
                    "supportsSetVariable": true,
                    "supportsTerminateRequest": true,
                    "supportsExceptionInfoRequest": true,
                    "exceptionBreakpointFilters": [
                        { "filter": "raised", "label": "Raised" },
                        { "filter": "uncaught", "label": "Uncaught" },
                    ],
                }),
            ),
            "launch" | "attach" => {
                if plan.refuse_launch {
                    peer.refuse(&message, "The file does not exist.");
                    continue;
                }
                if arguments["console"] == json!("integratedTerminal") {
                    peer.request(
                        "runInTerminal",
                        json!({
                            "kind": "integrated", "title": "Python Debug Console", "cwd": "/p",
                            "args": ["/p/.venv/bin/python", "launcher", "--", "app.py"],
                            "env": { "DEBUGPY_PORT": "5000", "GONE": null },
                        }),
                    );
                    // Like debugpy, wait for the launcher before going on.
                    let answer = peer.next().unwrap();
                    assert_eq!(answer["type"], json!("response"), "{answer}");
                    log.lock().unwrap().push(answer);
                }
                launch = Some(message.clone());
                peer.event("initialized", json!({}));
            }
            "setBreakpoints" => {
                let answers: Vec<Value> = arguments["breakpoints"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|breakpoint| match breakpoint["line"].as_u64().unwrap() {
                        // A line without code: moved to the next one.
                        13 => json!({ "id": 13, "verified": true, "line": 14 }),
                        99 => json!({ "id": 99, "verified": false, "message": "No code here." }),
                        line => json!({ "id": line, "verified": true, "line": line }),
                    })
                    .collect();
                peer.respond(&message, json!({ "breakpoints": answers }));
            }
            "setExceptionBreakpoints" => peer.respond(&message, json!({})),
            "configurationDone" => {
                peer.respond(&message, json!({}));
                peer.respond(launch.as_ref().unwrap(), json!({}));
                peer.event(
                    "output",
                    json!({ "category": "stdout", "output": "hello\n" }),
                );
                peer.event(
                    "output",
                    json!({ "category": "telemetry", "output": "ignored" }),
                );
                if plan.stop_at_start {
                    peer.event("stopped", json!({ "reason": "breakpoint", "threadId": 1 }));
                }
            }
            "threads" => {
                peer.respond(
                    &message,
                    json!({ "threads": [{ "id": 1, "name": "MainThread" }] }),
                );
                // The subprocess appears when the test asks for threads,
                // after it has subscribed.
                if let Some(port) = child_port.take() {
                    peer.request(
                        "startDebugging",
                        json!({ "request": "attach", "configuration": {
                            "name": "Subprocess 77", "subProcessId": 77,
                            "connect": { "host": "127.0.0.1", "port": port },
                        } }),
                    );
                }
            }
            "stackTrace" => peer.respond(
                &message,
                json!({ "stackFrames": [
                    { "id": 1000 + line, "name": "work", "line": line, "column": 1,
                      "source": { "path": APP } },
                    { "id": 1, "name": "<module>", "line": 30, "column": 1,
                      "source": { "path": APP } },
                    { "id": 2, "name": "run", "line": 5, "column": 1,
                      "source": { "path": "/lib/runpy.py", "presentationHint": "deemphasize" } },
                ] }),
            ),
            "scopes" => peer.respond(
                &message,
                json!({ "scopes": [
                    { "name": "Globals", "variablesReference": 11, "expensive": true },
                    { "name": "Locals", "variablesReference": 10, "expensive": false },
                ] }),
            ),
            "variables" => {
                let variables = match arguments["variablesReference"].as_i64().unwrap() {
                    10 => json!([
                        { "name": "n", "value": n, "type": "int", "variablesReference": 0 },
                        { "name": "items", "value": "[0, 1, 2, 3, 4]", "type": "list",
                          "variablesReference": 12, "indexedVariables": 5,
                          "evaluateName": "items" },
                    ]),
                    12 => (0..5)
                        .map(|index| {
                            json!({ "name": index.to_string(), "value": index.to_string(),
                                    "variablesReference": 0 })
                        })
                        .collect(),
                    _ => json!([]),
                };
                peer.respond(&message, json!({ "variables": variables }));
            }
            "evaluate" => {
                let result = format!("<{}>", arguments["expression"].as_str().unwrap());
                peer.respond(
                    &message,
                    json!({ "result": result, "type": "str", "variablesReference": 0 }),
                );
            }
            "setVariable" => {
                n = arguments["value"].as_str().unwrap().to_string();
                peer.respond(&message, json!({ "value": n, "type": "int" }));
            }
            "next" | "stepIn" | "stepOut" => {
                peer.respond(&message, json!({}));
                peer.event("continued", json!({ "threadId": 1 }));
                line += 1;
                peer.event("stopped", json!({ "reason": "step", "threadId": 1 }));
            }
            "continue" => {
                if plan.die_on_continue {
                    return;
                }
                peer.respond(&message, json!({ "allThreadsContinued": true }));
                continues += 1;
                if continues == 1 {
                    line = 20;
                    peer.event(
                        "stopped",
                        json!({ "reason": "exception", "threadId": 1, "text": "ValueError" }),
                    );
                } else {
                    peer.event("output", json!({ "category": "stderr", "output": "bye\n" }));
                    peer.event("exited", json!({ "exitCode": 3 }));
                    peer.event("terminated", json!({}));
                }
            }
            "exceptionInfo" => peer.respond(
                &message,
                json!({ "exceptionId": "ValueError", "description": "bad value" }),
            ),
            "pause" => {
                peer.respond(&message, json!({}));
                peer.event("stopped", json!({ "reason": "pause", "threadId": 1 }));
            }
            "terminate" => {
                peer.respond(&message, json!({}));
                peer.event("exited", json!({ "exitCode": 143 }));
                peer.event("terminated", json!({}));
            }
            "disconnect" => {
                peer.respond(&message, json!({}));
                return;
            }
            _ => peer.refuse(&message, "Unknown request."),
        }
    }
}

struct Started {
    session: Arc<Session>,
    log: Log,
    adapter: JoinHandle<()>,
}

impl Started {
    /// The requests named `command` that the adapter received, in order.
    fn received(&self, command: &str) -> Vec<Value> {
        let log = self.log.lock().unwrap();
        log.iter()
            .filter(|message| {
                message["command"] == json!(command) && message["type"] == json!("request")
            })
            .map(|message| message["arguments"].clone())
            .collect()
    }

    fn end(self) {
        drop(self.session);
        self.adapter.join().unwrap();
    }
}

fn spawn() -> Spawn {
    Spawn {
        label: "app".into(),
        command: "python".into(),
        args: vec!["app.py".into()],
        cwd: PathBuf::from("/p"),
        env: Vec::new(),
    }
}

fn setup(store: &BreakpointStore, console: Console) -> Setup {
    let options = LaunchOptions {
        console,
        ..LaunchOptions::default()
    };
    Setup {
        adapter: adapter(DEBUGPY).unwrap(),
        request: derive(&spawn(), &options).unwrap(),
        store: store.clone(),
        origin: Actor::Agent("agent-1".into()),
        driver: Actor::Agent("agent-1".into()),
        parent: None,
    }
}

fn start_with(plan: Plan, store: BreakpointStore, console: Console) -> Started {
    let log = Log::default();
    let script_log = log.clone();
    let (client, incoming, adapter) = fake(move |peer| python_like(peer, &script_log, plan));
    let session = Session::open(client, incoming, setup(&store, console)).unwrap();
    Started {
        session,
        log,
        adapter,
    }
}

fn start(plan: Plan) -> Started {
    start_with(plan, BreakpointStore::new(), Console::InternalConsole)
}

fn stopped(plan: Plan) -> Started {
    let started = start(Plan {
        stop_at_start: true,
        ..plan
    });
    let state = started.session.wait_for_stop(SOON).unwrap();
    assert!(state.stop().is_some(), "{state:?}");
    started
}

fn drain(events: &async_channel::Receiver<SessionEvent>) -> Vec<String> {
    std::iter::from_fn(|| events.try_recv().ok())
        .map(|event| match event {
            SessionEvent::StateChanged(SessionState::Starting) => "state starting".into(),
            SessionEvent::StateChanged(SessionState::Running) => "state running".into(),
            SessionEvent::StateChanged(SessionState::Stopped(_)) => "state stopped".into(),
            SessionEvent::StateChanged(SessionState::Terminated { .. }) => {
                "state terminated".into()
            }
            SessionEvent::Stopped(stop) => format!("stopped {}", stop.reason),
            SessionEvent::Terminated { exit_code } => format!("terminated {exit_code:?}"),
            SessionEvent::Threads => "threads".into(),
            SessionEvent::Stack { thread_id } => format!("stack {thread_id}"),
            SessionEvent::Scopes { frame_id } => format!("scopes {frame_id}"),
            SessionEvent::Variables { reference } => format!("variables {reference}"),
            SessionEvent::Output(output) => format!("output {}", output.text.trim()),
            SessionEvent::RunInTerminal(_) => "terminal".into(),
            SessionEvent::ChildStarted(_) => "child".into(),
            SessionEvent::DriverChanged(_) => "driver".into(),
            SessionEvent::Failed { action, message } => format!("failed {action}: {message}"),
        })
        .collect()
}

#[test]
fn a_session_starts_stops_steps_and_ends() {
    let started = stopped(Plan::default());
    let session = &started.session;

    // The handshake: capabilities announced, then launch as derived.
    let initialize = &started.received("initialize")[0];
    assert_eq!(initialize["supportsRunInTerminalRequest"], json!(true));
    assert_eq!(initialize["supportsStartDebuggingRequest"], json!(true));
    assert_eq!(initialize["linesStartAt1"], json!(true));
    let launch = &started.received("launch")[0];
    assert_eq!(launch["program"], json!("app.py"));
    assert_eq!(launch["console"], json!("internalConsole"));
    assert!(session.supports("supportsSetVariable"));
    assert!(!session.supports("supportsRestartRequest"));

    // Stopped, with the top frame's place known without asking.
    let stop = session.state().stop().cloned().unwrap();
    assert_eq!(stop.reason, "breakpoint");
    assert_eq!(stop.thread_id, Some(1));
    assert_eq!(stop.path.as_deref(), Some(Path::new(APP)));
    assert_eq!((stop.line, stop.frame_id), (Some(10), Some(1010)));
    assert_eq!(stop.exception, None);
    assert_eq!(session.cached_threads()[0].name, "MainThread");
    let frames = session.cached_stack(1).unwrap();
    assert_eq!(frames.len(), 3);
    assert!(frames[2].subtle && !frames[0].subtle);

    // Stack with locals: the cheap scope of the frames asked for.
    let locals = session.stack_with_locals(1).unwrap();
    assert_eq!(locals.len(), 3);
    assert_eq!(locals[0].frame.name, "work");
    let names: Vec<&str> = locals[0].locals.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["n", "items"]);
    assert_eq!(locals[0].locals[1].indexed_children, Some(5));
    assert!(locals[1].locals.is_empty());
    assert_eq!(session.cached_scopes(1010).unwrap().len(), 2);
    assert!(session.cached_variables(10, None).is_some());

    // Children are fetched when asked; this adapter cannot page, so the
    // page is cut here.
    let page = Some(Page { start: 1, count: 2 });
    let children = session.variables(12, page).unwrap();
    let values: Vec<&str> = children.iter().map(|v| v.value.as_str()).collect();
    assert_eq!(values, ["1", "2"]);
    assert_eq!(
        started.received("variables").last().unwrap(),
        &json!({ "variablesReference": 12 })
    );
    // A second read is served from the cache.
    let asked = started.received("variables").len();
    session.variables(12, page).unwrap();
    assert_eq!(started.received("variables").len(), asked);

    let hover = session
        .evaluate("n + 1", Some(1010), EvaluateContext::Watch)
        .unwrap();
    assert_eq!(
        (hover.result.as_str(), hover.type_name.as_deref()),
        ("<n + 1>", Some("str"))
    );
    assert_eq!(
        started.received("evaluate")[0],
        json!({ "expression": "n + 1", "context": "watch", "frameId": 1010 })
    );
    assert_eq!(
        session.evaluate("n", None, EvaluateContext::Hover),
        Err(DapError::Unsupported("evaluating on hover".into()))
    );

    // Setting a variable drops the cached values, which are then read anew.
    let events = session.subscribe();
    assert_eq!(session.set_variable(10, "n", "41").unwrap().result, "41");
    assert!(session.cached_variables(10, None).is_none());
    assert!(drain(&events).contains(&"variables 10".to_string()));
    assert_eq!(session.variables(10, None).unwrap()[0].value, "41");

    // A step: running, then stopped one line on, each announced in order.
    drain(&events);
    let state = session.step_and_wait(StepKind::Over, SOON).unwrap();
    assert_eq!(
        state.stop().map(|stop| (stop.reason.as_str(), stop.line)),
        Some(("step", Some(11)))
    );
    assert_eq!(started.received("next")[0], json!({ "threadId": 1 }));
    assert!(session.cached_scopes(1010).is_none());
    assert_eq!(
        drain(&events),
        [
            "state running",
            "threads",
            "stack 1",
            "state stopped",
            "stopped step"
        ]
    );

    // Continue into an exception, whose text comes with the stop.
    let state = session.continue_and_wait(SOON).unwrap();
    let stop = state.stop().unwrap();
    assert_eq!(stop.reason, "exception");
    assert_eq!(stop.exception.as_deref(), Some("ValueError: bad value"));
    assert_eq!(stop.line, Some(20));

    // Continue to the end: exit code and everything printed.
    let state = session.continue_and_wait(SOON).unwrap();
    assert_eq!(state, SessionState::Terminated { exit_code: Some(3) });
    assert_eq!(session.wait_for_exit(SOON), Ok(Some(3)));
    let output = session.output();
    let printed: Vec<(OutputCategory, &str)> = output
        .iter()
        .map(|o| (o.category, o.text.as_str()))
        .collect();
    assert_eq!(
        printed,
        [
            (OutputCategory::Stdout, "hello\n"),
            (OutputCategory::Stderr, "bye\n")
        ]
    );
    let seen = drain(&events);
    assert!(
        seen.ends_with(&[
            "state terminated".to_string(),
            "terminated Some(3)".to_string()
        ]),
        "{seen:?}"
    );
    assert!(seen.contains(&"output bye".to_string()));

    // Over is over.
    assert_eq!(
        session.continue_and_wait(SOON),
        Err(DapError::State("The program is not paused.".into()))
    );
    assert!(session.cached_stack(1).is_none());
    started.end();
}

#[test]
fn breakpoints_go_to_the_adapter_and_its_answers_come_back() {
    let store = BreakpointStore::new();
    let app = Path::new(APP);
    let other = Path::new("/p/other.py");
    store.set(app, Breakpoint::at(10).with_condition("n > 3"));
    store.toggle(app, 13);
    store.toggle(app, 99);
    let mut disabled = Breakpoint::at(50);
    disabled.enabled = false;
    disabled.log_message = Some("unused".into());
    store.set(app, disabled);
    store.set_exceptions(ExceptionFilters {
        raised: true,
        uncaught: true,
    });

    let started = start_with(
        Plan {
            stop_at_start: true,
            ..Plan::default()
        },
        store.clone(),
        Console::InternalConsole,
    );
    started.session.wait_for_stop(SOON).unwrap();

    // Sent before the debuggee ran: enabled ones only, with what the
    // adapter supports (it has conditions, not log points).
    let sent = started.received("setBreakpoints");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["source"], json!({ "path": APP, "name": "app.py" }));
    assert_eq!(
        sent[0]["breakpoints"],
        json!([{ "line": 10, "condition": "n > 3" }, { "line": 13 }, { "line": 99 }])
    );
    assert_eq!(
        started.received("setExceptionBreakpoints"),
        [json!({ "filters": ["raised", "uncaught"] })]
    );
    let order: Vec<String> = started
        .log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|message| message["command"].as_str().map(str::to_string))
        .take(5)
        .collect();
    assert_eq!(
        order,
        [
            "initialize",
            "launch",
            "setBreakpoints",
            "setExceptionBreakpoints",
            "configurationDone"
        ]
    );

    // The adapter's verdicts, per breakpoint.
    let status = |line: u32| {
        let list = store.in_file(app);
        list.iter().find(|b| b.line == line).unwrap().status.clone()
    };
    assert_eq!(status(10), BreakpointStatus::Verified { line: 10 });
    assert_eq!(status(13), BreakpointStatus::Verified { line: 14 });
    assert_eq!(
        status(99),
        BreakpointStatus::Rejected {
            message: Some("No code here.".into())
        }
    );
    assert_eq!(status(50), BreakpointStatus::Idle);

    // Edits while the session runs reach the adapter, file by file.
    store.toggle(other, 7);
    wait_until("the new file's breakpoints", || {
        started.received("setBreakpoints").len() == 2
    });
    assert_eq!(
        started.received("setBreakpoints")[1]["source"]["path"],
        json!("/p/other.py")
    );
    wait_until("its answer", || {
        store.in_file(other)[0].status == BreakpointStatus::Verified { line: 7 }
    });
    store.toggle(other, 7);
    wait_until("the removal", || {
        started.received("setBreakpoints").len() == 3
    });
    assert_eq!(
        started.received("setBreakpoints")[2]["breakpoints"],
        json!([])
    );
    store.lines_changed(app, 1, 0, 2);
    wait_until("the moved lines", || {
        started.received("setBreakpoints").len() == 4
    });
    assert_eq!(
        started.received("setBreakpoints")[3]["lines"],
        json!([12, 15, 101])
    );
    store.set_exceptions(ExceptionFilters {
        raised: false,
        uncaught: false,
    });
    wait_until("the filters", || {
        started.received("setExceptionBreakpoints").len() == 2
    });
    assert_eq!(
        started.received("setExceptionBreakpoints")[1],
        json!({ "filters": [] })
    );

    // The adapter changing its mind later is reflected too.
    wait_until("the answers", || {
        status(12) == BreakpointStatus::Verified { line: 12 }
    });

    // When the session ends, nothing is verified any more.
    started.session.terminate();
    assert_eq!(started.session.wait_for_exit(SOON), Ok(Some(143)));
    wait_until("the forgotten answers", || {
        status(12) == BreakpointStatus::Idle
    });
    started.end();
}

#[test]
fn the_debuggee_is_started_in_a_terminal_when_asked() {
    let started = start_with(
        Plan {
            stop_at_start: true,
            ..Plan::default()
        },
        BreakpointStore::new(),
        Console::IntegratedTerminal,
    );
    let session = &started.session;
    wait_until("the terminal request", || {
        session.pending_terminal().is_some()
    });
    assert_eq!(session.state(), SessionState::Starting);
    let terminal = session.pending_terminal().unwrap();
    assert_eq!(
        terminal.spawn,
        Spawn {
            label: "Python Debug Console".into(),
            command: "/p/.venv/bin/python".into(),
            args: vec!["launcher".into(), "--".into(), "app.py".into()],
            cwd: PathBuf::from("/p"),
            env: vec![("DEBUGPY_PORT".into(), "5000".into())],
        }
    );
    terminal.started(Some(4242));
    // Answered once; a second answer goes nowhere.
    terminal.failed("too late");
    assert!(session.pending_terminal().is_none());
    assert!(session.wait_for_stop(SOON).unwrap().stop().is_some());
    let log = started.log.lock().unwrap().clone();
    let answers: Vec<&Value> = log
        .iter()
        .filter(|m| m["type"] == json!("response"))
        .collect();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0]["command"], json!("runInTerminal"));
    assert_eq!(answers[0]["body"], json!({ "processId": 4242 }));
    started.end();
}

#[test]
fn a_subprocess_becomes_a_child_session() {
    let child_log = Log::default();
    let script_log = child_log.clone();
    let (port, child_adapter) = fake_listening(move |peer| {
        let plan = Plan {
            stop_at_start: true,
            ..Plan::default()
        };
        python_like(peer, &script_log, plan);
    });
    let started = start(Plan {
        child_port: Some(port),
        ..Plan::default()
    });
    let parent = &started.session;
    let events = parent.subscribe();
    parent.threads().unwrap();
    wait_until("the child session", || parent.children().len() == 1);
    let child = parent.children().remove(0);
    assert_eq!(child.parent(), Some(parent.id()));
    assert_eq!(child.label(), "Subprocess 77");
    assert_eq!(child.origin(), &Actor::Agent("agent-1".into()));
    assert!(child.wait_for_stop(SOON).unwrap().stop().is_some());
    wait_until("the announcement", || {
        drain(&events).contains(&"child".to_string())
    });

    // The child attached with the configuration the parent's adapter gave.
    let attach = child_log
        .lock()
        .unwrap()
        .iter()
        .find(|message| message["command"] == json!("attach"))
        .cloned()
        .unwrap();
    assert_eq!(attach["arguments"]["subProcessId"], json!(77));
    assert_eq!(attach["arguments"]["request"], json!("attach"));
    // And the parent's adapter got its answer.
    let answered = |log: &Log| {
        log.lock().unwrap().iter().any(|message| {
            message["type"] == json!("response") && message["command"] == json!("startDebugging")
        })
    };
    assert!(answered(&started.log));

    // Taking over the parent takes its children along.
    assert_eq!(child.driver(), Actor::Agent("agent-1".into()));
    parent.take_over(Actor::User);
    assert_eq!(
        (parent.driver(), child.driver()),
        (Actor::User, Actor::User)
    );
    assert_eq!(parent.origin(), &Actor::Agent("agent-1".into()));
    assert!(drain(&events).contains(&"driver".to_string()));

    // The parent owns the child: ending one ends the other.
    parent.close();
    assert!(child.state().is_terminated());
    assert!(parent.children().is_empty());
    drop(child);
    child_adapter.join().unwrap();
    started.end();
}

#[test]
fn control_calls_run_in_the_background_and_report_failures() {
    let started = start(Plan::default());
    let session = &started.session;
    let events = session.subscribe();
    wait_until("running", || session.state() == SessionState::Running);

    // Nothing to continue: told as an event, and as an error to who waits.
    session.resume(None);
    wait_until("the failure", || {
        drain(&events).contains(&"failed continue: The program is not paused.".to_string())
    });
    assert_eq!(
        session.wait_for_stop(Duration::from_millis(30)),
        Err(DapError::StillRunning)
    );

    // Pause needs a thread; the first refresh supplies it.
    session.refresh(Refresh::Threads);
    wait_until("threads", || !session.cached_threads().is_empty());
    session.pause(None);
    let state = session.wait_for_stop(SOON).unwrap();
    assert_eq!(state.stop().map(|stop| stop.reason.as_str()), Some("pause"));

    session.refresh(Refresh::Scopes(1010));
    session.refresh(Refresh::Variables(10, None));
    wait_until("the refreshed lists", || {
        session.cached_variables(10, None).is_some()
    });
    let seen = drain(&events);
    assert!(seen.contains(&"scopes 1010".to_string()), "{seen:?}");
    assert!(seen.contains(&"variables 10".to_string()), "{seen:?}");

    session.step(StepKind::In, None);
    wait_until("the step", || started.received("stepIn").len() == 1);
    wait_until("the stop after it", || {
        session
            .state()
            .stop()
            .is_some_and(|stop| stop.reason == "step")
    });

    session.disconnect();
    assert_eq!(session.wait_for_exit(SOON), Ok(None));
    assert_eq!(
        started.received("disconnect"),
        [json!({ "terminateDebuggee": false })]
    );
    started.end();
}

#[test]
fn a_refused_launch_ends_the_session_with_the_reason() {
    let started = start(Plan {
        refuse_launch: true,
        ..Plan::default()
    });
    let session = &started.session;
    assert_eq!(session.wait_for_exit(SOON), Ok(None));
    assert_eq!(
        session.failure().as_deref(),
        Some("The file does not exist.")
    );
    assert_eq!(
        session.wait_for_stop(SOON),
        Ok(SessionState::Terminated { exit_code: None })
    );
    started.end();
}

#[test]
fn an_adapter_that_dies_ends_the_session() {
    let started = stopped(Plan {
        die_on_continue: true,
        ..Plan::default()
    });
    let session = &started.session;
    assert_eq!(session.continue_and_wait(SOON), Err(DapError::Closed));
    assert_eq!(session.wait_for_exit(SOON), Ok(None));
    assert_eq!(session.threads(), Err(DapError::Closed));
    started.end();
}

#[test]
fn an_adapter_that_never_answers_is_a_start_error() {
    let (client, incoming, adapter) = fake(|peer| {
        peer.expect("initialize");
    });
    let result = Session::open(
        client,
        incoming,
        setup(&BreakpointStore::new(), Console::InternalConsole),
    );
    assert!(matches!(result, Err(DapError::Start(_))));
    adapter.join().unwrap();
}
