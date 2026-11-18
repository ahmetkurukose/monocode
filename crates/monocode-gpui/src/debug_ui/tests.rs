//! The debug interface against the core's scripted adapter, through a real
//! shell and real file editors: the gutter and the store, persistence, a
//! session from its first stop to its end with what each step shows, the
//! debuggee's terminal, an agent's session, and the hover wrapper.
use super::{
    breakpoints,
    hover::DebugHover,
    model::{Control, Model, StackRow, Tone, VarKind},
    sessions,
};
use crate::{
    code_editor::{
        self, Breakpoint as MarkState, DocumentInfo, GutterMarkKind, GutterSlot, Hover,
        HoverProvider, LineDecoration, LineStyle, Position,
    },
    code_shell::{context, dock, launch, rail, settings},
    file_editor::FileEditor,
    git_status::ProjectGit,
    popover::MenuItem,
    shell::Shell,
};
use gpui::{
    App, AppContext, Bounds, Entity, Focusable, Modifiers, MouseButton, Task, TestAppContext,
    VisualTestContext, WindowBounds, WindowHandle, WindowOptions, point, px, size,
};
use monocode_core::code::{
    dap::{
        Actor, Breakpoint, DebugRequest, ExceptionFilters, RequestKind, Session, SessionState,
        adapter::DEBUGPY, fake::Peer,
    },
    services::services,
    spawn::Spawn,
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};

const PROJECT: &str = "/work/monocode";
const SOURCE: &str = "import os\n\n\ndef work(n, items):\n    total = 0\n    for item in items:\n        total += item\n    count = n\n    limit = n + 1\n    return total + n\n\nwork(1, [])\n";

/// What the scripted adapter does besides answering.
#[derive(Clone, Default)]
struct Plan {
    /// The file its one thread is in.
    app: PathBuf,
    /// Stop on line 10 as soon as the debuggee runs.
    stop_at_start: bool,
    /// Make that first stop the `SystemExit: 3` debugpy reports for `sys.exit(3)`.
    system_exit: bool,
    /// What its request for a terminal was answered with.
    terminal: Arc<Mutex<Option<Value>>>,
}

/// An adapter that behaves like debugpy with one thread in `plan.app`. A
/// step moves one line down; `continue` runs to the end.
fn python_like(peer: &mut Peer, plan: Plan) {
    let mut launch = None;
    let mut line = 10;
    let mut n = "1".to_owned();
    let app = plan.app.to_string_lossy().into_owned();
    while let Some(message) = peer.next() {
        if message["type"] != json!("request") {
            continue;
        }
        let arguments = &message["arguments"];
        match message["command"].as_str().unwrap_or_default() {
            "initialize" => peer.respond(
                &message,
                json!({
                    "supportsConfigurationDoneRequest": true,
                    "supportsConditionalBreakpoints": true,
                    "supportsSetVariable": true,
                    "supportsEvaluateForHovers": true,
                    "supportsTerminateRequest": true,
                    "supportsExceptionInfoRequest": true,
                }),
            ),
            "launch" => {
                if arguments["console"] == json!("integratedTerminal") {
                    peer.request(
                        "runInTerminal",
                        json!({
                            "kind": "integrated", "title": "Debuggee",
                            "cwd": plan.app.parent().unwrap(),
                            "args": ["/bin/sh", "-c", "sleep 30"], "env": { "DEBUGGEE": "1" },
                        }),
                    );
                    *plan.terminal.lock().unwrap() = peer.next();
                }
                launch = Some(message.clone());
                peer.event("initialized", json!({}));
            }
            "setBreakpoints" => {
                let answers: Vec<Value> = (arguments["breakpoints"].as_array().unwrap().iter())
                    .map(|breakpoint| json!({ "verified": true, "line": breakpoint["line"] }))
                    .collect();
                peer.respond(&message, json!({ "breakpoints": answers }));
            }
            "configurationDone" => {
                peer.respond(&message, json!({}));
                peer.respond(launch.as_ref().unwrap(), json!({}));
                peer.event(
                    "output",
                    json!({ "category": "stdout", "output": "hello\n" }),
                );
                if plan.system_exit {
                    peer.event(
                        "stopped",
                        json!({ "reason": "exception", "threadId": 1, "text": "SystemExit" }),
                    );
                } else if plan.stop_at_start {
                    peer.event("stopped", json!({ "reason": "breakpoint", "threadId": 1 }));
                }
            }
            "threads" => peer.respond(
                &message,
                json!({ "threads": [{ "id": 1, "name": "MainThread" }] }),
            ),
            "stackTrace" => peer.respond(
                &message,
                json!({ "stackFrames": [
                    { "id": 1000 + line, "name": "work", "line": line, "column": 1,
                      "source": { "path": app } },
                    { "id": 1, "name": "<module>", "line": 12, "column": 1,
                      "source": { "path": app } },
                    { "id": 2, "name": "run", "line": 5, "column": 1,
                      "source": { "path": "/lib/runpy.py", "presentationHint": "deemphasize" } },
                ] }),
            ),
            "scopes" => peer.respond(
                &message,
                json!({ "scopes": [
                    { "name": "Locals", "variablesReference": 10, "expensive": false },
                    { "name": "Globals", "variablesReference": 11, "expensive": true },
                ] }),
            ),
            "variables" => {
                let variables = match arguments["variablesReference"].as_i64().unwrap() {
                    10 => json!([
                        { "name": "n", "value": n, "type": "int", "variablesReference": 0 },
                        { "name": "items", "value": "[0, 1, 2, ...]", "type": "list",
                          "variablesReference": 12, "indexedVariables": 250 },
                    ]),
                    12 => (0..250)
                        .map(|index| {
                            json!({ "name": index.to_string(), "value": index.to_string(),
                                    "variablesReference": 0 })
                        })
                        .collect(),
                    11 => json!([{ "name": "os", "value": "<module 'os'>",
                                   "variablesReference": 0 }]),
                    _ => json!([]),
                };
                peer.respond(&message, json!({ "variables": variables }));
            }
            "exceptionInfo" => peer.respond(
                &message,
                json!({ "exceptionId": "SystemExit", "description": "3" }),
            ),
            "evaluate" => {
                let expression = arguments["expression"].as_str().unwrap();
                let result = match expression {
                    "n" => n.clone(),
                    other => format!("<{other}>"),
                };
                peer.respond(
                    &message,
                    json!({ "result": result, "type": "int", "variablesReference": 0 }),
                );
            }
            "setVariable" => {
                n = arguments["value"].as_str().unwrap().to_owned();
                peer.respond(&message, json!({ "value": n }));
            }
            "next" | "stepIn" | "stepOut" => {
                peer.respond(&message, json!({}));
                line += 1;
                peer.event("stopped", json!({ "reason": "step", "threadId": 1 }));
            }
            "pause" => {
                peer.respond(&message, json!({}));
                peer.event("stopped", json!({ "reason": "pause", "threadId": 1 }));
            }
            "continue" => {
                peer.respond(&message, json!({}));
                let code = if plan.system_exit { 3 } else { 0 };
                peer.event("exited", json!({ "exitCode": code }));
                peer.event("terminated", json!({}));
            }
            "terminate" => {
                peer.respond(&message, json!({}));
                peer.event("terminated", json!({}));
            }
            "disconnect" => {
                peer.respond(&message, json!({}));
                break;
            }
            _ => peer.respond(&message, json!({})),
        }
    }
}

/// Lets real time pass until `done`: the adapter answers on its own thread.
fn wait(
    cx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    for _ in 0..1000 {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

/// Runs `f` outside the window's own update, as a task of the feature runs.
fn app<R>(cx: &mut VisualTestContext, f: impl FnOnce(&mut App) -> R) -> R {
    let cx: &mut TestAppContext = cx;
    cx.update(f)
}

fn bind(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::input::bind(cx);
        crate::file_editor::bind(cx);
        crate::popover::bind(cx);
        super::init(cx);
    });
}

/// A shell with the debug interface installed, and the agent session of
/// its project in front.
fn shell(cx: &mut TestAppContext) -> (WindowHandle<Shell>, VisualTestContext, i64) {
    bind(cx);
    let (window, _, session) = Shell::test_window(cx);
    cx.update(|cx| cx.set_reduce_motion(true));
    let visual = VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    (window, visual, session)
}

fn app_file() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.py");
    std::fs::write(&path, SOURCE).unwrap();
    (dir, path)
}

fn start(plan: Plan, origin: Actor, console: &str) -> (Arc<Session>, JoinHandle<()>) {
    let mut arguments = serde_json::Map::new();
    arguments.insert("console".into(), json!(console));
    // Told apart from the sessions of other tests, which share the manager.
    arguments.insert("program".into(), json!(plan.app));
    let request = DebugRequest {
        adapter: DEBUGPY,
        kind: RequestKind::Launch,
        label: "app.py".into(),
        cwd: PROJECT.into(),
        arguments,
    };
    services()
        .dap()
        .start_scripted(request, origin, move |peer| python_like(peer, plan))
        .unwrap()
}

/// Ends the session for the manager the tests share. The script's thread
/// is not joined: it reads until the last holder of the session lets go,
/// and the model shown in the tab is one of them.
fn finish(session: &Arc<Session>, script: JoinHandle<()>) {
    services().dap().remove(session.id());
    drop(script);
}

fn model_of(session: &Arc<Session>, cx: &mut VisualTestContext) -> Option<Entity<Model>> {
    cx.update(|_, cx| {
        let models = sessions::debugger(cx)?.read(cx).models();
        (models.into_iter()).find(|model| model.read(cx).root().id() == session.id())
    })
}

fn stopped_on(session: &Arc<Session>, line: u32) -> bool {
    session.state().stop().and_then(|stop| stop.line) == Some(line)
}

fn decorations(path: &Path, cx: &mut VisualTestContext) -> Vec<LineDecoration> {
    cx.update(|_, cx| code_editor::hub(cx).read(cx).line_decorations(path))
}

fn mark_rows(path: &Path, cx: &mut App) -> Vec<(usize, GutterMarkKind)> {
    let hub = code_editor::hub(cx);
    let marks = hub.read(cx).gutter_marks(path, GutterSlot::Breakpoint);
    marks.iter().map(|mark| (mark.row, mark.kind)).collect()
}

fn lines(path: &Path) -> Vec<u32> {
    let breakpoints = breakpoints::store().in_file(path);
    breakpoints
        .iter()
        .map(|breakpoint| breakpoint.line)
        .collect()
}

#[gpui::test]
fn the_gutter_edits_the_store_and_an_edit_above_moves_the_breakpoint(cx: &mut TestAppContext) {
    bind(cx);
    let (dir, path) = app_file();
    let (file, root) = (
        path.to_string_lossy().into_owned(),
        dir.path().to_string_lossy().into_owned(),
    );
    let window = cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(700.), px(400.)),
                ))),
                ..Default::default()
            },
            |window, cx| {
                let git = cx.new(|cx| ProjectGit::new(root, cx));
                let view = cx.new(|cx| FileEditor::new(file, git, cx));
                window.focus(&view.focus_handle(cx), cx);
                view
            },
        )
        .unwrap()
    });
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    let center = |row: usize, cx: &mut VisualTestContext| {
        window
            .update(cx, |view, _, cx| {
                (view.input.read(cx)).gutter_slot_center(GutterSlot::Breakpoint, row)
            })
            .unwrap()
    };
    // A Python file has the breakpoint column once it has loaded.
    wait(visual, "the file to load", |cx| center(4, cx).is_some());

    let fifth = center(4, visual).unwrap();
    visual.simulate_click(fifth, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(lines(&path), [5]);
    let enabled = GutterMarkKind::Breakpoint(MarkState::Enabled);
    visual.update(|_, cx| assert_eq!(mark_rows(&path, cx), [(4, enabled)]));

    // A breakpoint set elsewhere (an agent's, over MCP) is drawn as well.
    breakpoints::store().set(&path, Breakpoint::at(9).with_condition("n > 1"));
    visual.run_until_parked();
    let conditional = GutterMarkKind::Breakpoint(MarkState::Conditional);
    visual.update(|_, cx| assert_eq!(mark_rows(&path, cx), [(4, enabled), (8, conditional)]));
    window
        .update(visual, |view, _, cx| {
            let input = view.input.read(cx);
            assert_eq!(input.gutter_marks(GutterSlot::Breakpoint).len(), 2);
        })
        .unwrap();

    // Two lines typed above both: they stay on their statements.
    window
        .update(visual, |view, _, cx| {
            view.input.update(cx, |input, cx| {
                input.replace_bytes(0..0, "# one\n# two\n", 12, cx)
            })
        })
        .unwrap();
    visual.run_until_parked();
    assert_eq!(lines(&path), [7, 11]);
    assert_eq!(
        breakpoints::store().in_file(&path)[1].condition.as_deref(),
        Some("n > 1")
    );
    visual.update(|_, cx| assert_eq!(mark_rows(&path, cx), [(6, enabled), (10, conditional)]));
    // The store's echo of that move does not redraw older rows over an
    // edit made meanwhile.
    window
        .update(visual, |view, _, cx| {
            view.input
                .update(cx, |input, cx| input.replace_bytes(0..6, "", 0, cx));
            let input = view.input.read(cx);
            assert_eq!(input.gutter_marks(GutterSlot::Breakpoint)[0].row, 5);
        })
        .unwrap();
    visual.run_until_parked();
    assert_eq!(lines(&path), [6, 10]);
    // A breakpoint an agent added whose change is not drawn yet survives a
    // line typed above, and the ones that moved keep their conditions.
    breakpoints::store().set(&path, Breakpoint::at(20));
    window
        .update(visual, |view, _, cx| {
            view.input
                .update(cx, |input, cx| input.replace_bytes(0..0, "# new\n", 6, cx))
        })
        .unwrap();
    visual.run_until_parked();
    let kept = lines(&path);
    assert!(kept.contains(&7) && kept.contains(&11), "{kept:?}");
    assert!(kept.iter().any(|line| *line >= 20), "{kept:?}");
    assert_eq!(kept.len(), 3);
    assert_eq!(
        breakpoints::store().in_file(&path)[1].condition.as_deref(),
        Some("n > 1")
    );
    visual.update(|_, cx| assert_eq!(mark_rows(&path, cx).len(), 3));
    window
        .update(visual, |view, _, cx| {
            view.input
                .update(cx, |input, cx| input.replace_bytes(0..6, "", 0, cx))
        })
        .unwrap();
    visual.run_until_parked();
    for line in lines(&path).into_iter().filter(|line| *line > 12) {
        breakpoints::store().remove(&path, line);
    }
    visual.run_until_parked();
    assert_eq!(lines(&path), [6, 10]);

    // A right press opens the breakpoint's form; a left press removes it.
    let first = center(5, visual).unwrap();
    visual.simulate_mouse_down(first, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(first, MouseButton::Right, Modifiers::default());
    visual.run_until_parked();
    window
        .update(visual, |view, _, cx| {
            assert!(view.input.read(cx).gutter_menu_open())
        })
        .unwrap();
    assert!(visual.debug_bounds("breakpoint-form").is_some());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    window
        .update(visual, |view, _, cx| {
            assert!(!view.input.read(cx).gutter_menu_open())
        })
        .unwrap();
    // What that menu holds: the form's texts and the rows act on the store.
    visual.update(|window, cx| {
        let menu = breakpoints::menu(&path, 6, cx);
        let MenuItem::Custom(form, _) = &menu.items[0] else {
            panic!("the form leads the menu");
        };
        let form = form.clone().downcast::<breakpoints::Form>().unwrap();
        form.update(cx, |form, cx| form.set_condition("x > 2", cx));
        (menu.on_pick)("toggle", window, cx);
    });
    visual.run_until_parked();
    let edited = breakpoints::store().in_file(&path)[0].clone();
    assert_eq!(edited.condition.as_deref(), Some("x > 2"));
    assert!(!edited.enabled);
    let disabled = GutterMarkKind::Breakpoint(MarkState::Disabled);
    visual.update(|_, cx| assert_eq!(mark_rows(&path, cx)[0], (5, disabled)));
    let first = center(5, visual).unwrap();
    visual.simulate_click(first, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(lines(&path), [10]);
    breakpoints::store().clear(Some(&path));
}

#[gpui::test]
fn breakpoints_are_saved_with_the_project_and_loaded_again(cx: &mut TestAppContext) {
    let (_window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    let path = PathBuf::from(format!("{PROJECT}/saved_{}.py", std::process::id()));
    let mut detailed = Breakpoint::at(4).with_condition("n == 2");
    detailed.enabled = false;
    breakpoints::store().set(&path, detailed);
    breakpoints::store().toggle(&path, 9);
    visual.run_until_parked();
    app(visual, breakpoints::persist_now);
    let saved = visual
        .update(|_, cx| settings::value(PROJECT, breakpoints::PAGE, cx))
        .expect("the project's breakpoints are saved");
    assert_eq!(
        saved["files"][path.to_string_lossy().as_ref()],
        json!([
            { "line": 4, "enabled": false, "condition": "n == 2" },
            { "line": 9, "enabled": true },
        ])
    );

    // A restart: the store is empty until the project's value is loaded.
    breakpoints::store().clear(Some(&path));
    assert!(lines(&path).is_empty());
    visual.update(|_, cx| breakpoints::restore(PROJECT, cx));
    assert_eq!(lines(&path), [4, 9]);
    assert!(!breakpoints::store().in_file(&path)[0].enabled);

    // Removing the last one forgets the saved value's entry for the file.
    breakpoints::store().clear(Some(&path));
    visual.run_until_parked();
    app(visual, breakpoints::persist_now);
    let saved = visual.update(|_, cx| settings::value(PROJECT, breakpoints::PAGE, cx));
    let listed =
        saved.and_then(|saved| saved["files"].get(path.to_string_lossy().as_ref()).cloned());
    assert_eq!(listed, None);
}

struct Docs;
impl HoverProvider for Docs {
    fn hover(&self, _: Position, _: &mut App) -> Task<Option<Hover>> {
        Task::ready(Some(Hover {
            range: None,
            markdown: "What the language server says.".into(),
        }))
    }
}

fn hover_at(needle: &str, path: &Path, cx: &mut VisualTestContext) -> Option<Hover> {
    let provider = DebugHover {
        inner: Some(Rc::new(Docs)),
    };
    let at = Position {
        doc: Rc::new(DocumentInfo::new(path, PROJECT)),
        text: SOURCE.into(),
        offset: SOURCE.find(needle).unwrap() + needle.len() - 1,
        version: 1,
    };
    let answer: Rc<RefCell<Option<Option<Hover>>>> = Rc::default();
    let slot = answer.clone();
    cx.update(|_, cx| {
        let asked = provider.hover(at, cx);
        cx.spawn(async move |_| *slot.borrow_mut() = Some(asked.await))
            .detach();
    });
    wait(cx, "the hover", |_| answer.borrow().is_some());
    answer.take().flatten()
}

#[gpui::test]
fn a_session_is_shown_from_its_first_stop_to_its_end(cx: &mut TestAppContext) {
    let (_window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    let (_dir, path) = app_file();
    let plan = Plan {
        app: path.clone(),
        stop_at_start: true,
        ..Plan::default()
    };
    let (session, script) = start(plan, Actor::User, "internalConsole");
    wait(visual, "the first stop", |_| stopped_on(&session, 10));
    wait(visual, "the session's tab", |cx| {
        model_of(&session, cx)
            .is_some_and(|model| model.read_with(cx, |model, _| model.tab.is_some()))
    });
    let model = model_of(&session, visual).unwrap();

    // The tab is in front, titled with the program and its state.
    wait(visual, "the tab to be drawn", |cx| {
        cx.debug_bounds("debug-tab").is_some()
    });
    assert!(visual.debug_bounds("debug-controls").is_some());
    assert!(visual.debug_bounds("debug-continue").is_some());
    model.read_with(visual, |model, _| {
        assert_eq!(model.title(), "app.py · paused")
    });

    // The line it stopped on carries the arrow, and its file was opened.
    let execution = LineDecoration {
        row: 9,
        style: LineStyle::Execution,
        arrow: true,
    };
    assert_eq!(decorations(&path, visual), [execution]);
    wait(visual, "the file to open at the stop", |cx| {
        cx.update(|_, cx| code_editor::hub(cx).read(cx).document(&path).is_some())
    });
    let focused = visual.update(|_, cx| context::focused_file(cx));
    assert_eq!(
        focused.map(|file| PathBuf::from(file.path)),
        Some(path.clone())
    );

    // The stack, with the library frame dimmed and the top frame selected.
    model.read_with(visual, |model, _| {
        let frames: Vec<(String, bool, bool)> = (model.stack_rows.iter())
            .filter_map(|row| match row {
                StackRow::Frame {
                    frame,
                    selected,
                    dim,
                    ..
                } => Some((frame.name.clone(), *selected, *dim)),
                _ => None,
            })
            .collect();
        assert_eq!(frames.len(), 3);
        assert_eq!((frames[0].0.as_str(), frames[0].1), ("work", true));
        assert_eq!(frames[2], ("run".to_owned(), false, true));
    });

    // The variables of the frame: the cheap scope open, the costly one closed.
    let names = |model: &Model| -> Vec<(usize, String, String)> {
        (model.var_rows.iter())
            .map(|row| (row.depth, row.name.to_string(), row.value.to_string()))
            .collect()
    };
    wait(visual, "the locals", |cx| {
        model.read_with(cx, |model, _| model.var_rows.len() == 4)
    });
    model.read_with(visual, |model, _| {
        assert_eq!(
            names(model),
            [
                (0, "Locals".to_owned(), String::new()),
                (1, "n".to_owned(), "1".to_owned()),
                (1, "items".to_owned(), "[0, 1, 2, ...]".to_owned()),
                (0, "Globals".to_owned(), String::new()),
            ]
        );
        assert!(model.var_rows[2].expandable && !model.var_rows[3].expanded);
    });
    // The values are written after the lines that name them.
    wait(visual, "the inline values", |cx| {
        cx.update(|_, cx| !code_editor::hub(cx).read(cx).inline_notes(&path).is_empty())
    });
    visual.update(|_, cx| {
        let notes = code_editor::hub(cx).read(cx).inline_notes(&path);
        assert_eq!(notes[0].row, 3);
        assert!(
            notes[0].text.starts_with("n = 1, items = "),
            "{}",
            notes[0].text
        );
        assert!(notes.iter().all(|note| note.row <= 9));
    });

    // A large list opens a page at a time.
    let items = model.read_with(visual, |model, _| model.var_rows[2].path.clone());
    model.update(visual, |model, cx| model.toggle(&items, cx));
    wait(visual, "the first page", |cx| {
        model.read_with(cx, |model, _| model.var_rows.len() == 105)
    });
    model.read_with(visual, |model, _| {
        assert_eq!(model.var_rows[3].name.as_ref(), "0");
        assert_eq!(model.var_rows[102].name.as_ref(), "99");
        assert_eq!(model.var_rows[103].kind, VarKind::More);
        assert_eq!(model.var_rows[103].name.as_ref(), "Show more (100 of 250)");
    });
    model.update(visual, |model, cx| model.show_more(&items, cx));
    wait(visual, "the second page", |cx| {
        model.read_with(cx, |model, _| model.var_rows.len() == 205)
    });
    model.update(visual, |model, cx| model.toggle(&items, cx));

    // The console has what the program printed, and evaluates in the frame.
    model.update(visual, |model, cx| model.evaluate("n + 1".into(), cx));
    wait(visual, "the evaluation", |cx| {
        model.read_with(cx, |model, _| {
            model
                .console
                .last()
                .is_some_and(|line| line.tone == Tone::Result)
        })
    });
    model.read_with(visual, |model, _| {
        let shown: Vec<(Tone, &str)> = (model.console.iter())
            .map(|line| (line.tone, line.text.as_ref()))
            .collect();
        assert_eq!(
            shown,
            [
                (Tone::Stdout, "hello"),
                (Tone::Input, "n + 1"),
                (Tone::Result, "<n + 1>")
            ]
        );
        assert_eq!(model.history, ["n + 1"]);
    });

    // Changing a variable shows its new value.
    model.update(visual, |model, cx| {
        model.set_variable(10, "n".into(), "5".into(), cx)
    });
    wait(visual, "the new value", |cx| {
        model.read_with(cx, |model, _| {
            names(model).get(1).is_some_and(|row| row.2 == "5")
        })
    });

    // While paused, a hover leads with the value and keeps the server's text.
    let hover = hover_at("return total + n", &path, visual).unwrap();
    assert!(
        hover.markdown.starts_with("```python\nn = 5\n```"),
        "{}",
        hover.markdown
    );
    assert!(hover.markdown.ends_with("What the language server says."));
    assert_eq!(hover.range.map(|range| &SOURCE[range]), Some("n"));
    // A keyword has no value: the wrapped provider answers alone.
    let plain = hover_at("return", &path, visual).unwrap();
    assert_eq!(plain.markdown, "What the language server says.");

    // A step moves the arrow.
    assert!(visual.update(|_, cx| sessions::act(Control::StepOver, cx)));
    wait(visual, "the step", |_| stopped_on(&session, 11));
    wait(visual, "the arrow to move", |cx| {
        decorations(&path, cx)
            == [LineDecoration {
                row: 10,
                ..execution
            }]
    });
    // Selecting the caller marks its line as the frame being read.
    model.update(visual, |model, cx| {
        model.select_frame(session.id(), 1, 1, cx)
    });
    assert_eq!(
        decorations(&path, visual),
        [
            LineDecoration {
                row: 10,
                ..execution
            },
            LineDecoration {
                row: 11,
                style: LineStyle::Frame,
                arrow: true
            },
        ]
    );

    // F5 continues; the program runs to its end and nothing stays marked.
    visual.simulate_keystrokes("f5");
    wait(visual, "the end", |_| session.state().is_terminated());
    wait(visual, "the marks to go", |cx| {
        decorations(&path, cx).is_empty()
    });
    visual.update(|_, cx| {
        assert!(code_editor::hub(cx).read(cx).inline_notes(&path).is_empty());
        assert!(!sessions::act(Control::StepOver, cx), "nothing to step");
    });
    model.read_with(visual, |model, _| {
        assert_eq!(model.title(), "app.py · ended");
        assert!(model.stack_rows.is_empty() && model.var_rows.is_empty());
        let last = model.console.last().unwrap();
        assert_eq!(last.text.as_ref(), "The program ended with exit code 0.");
    });
    assert_eq!(
        hover_at("return total + n", &path, visual)
            .unwrap()
            .markdown,
        "What the language server says."
    );
    finish(&session, script);
}

#[gpui::test]
fn the_debuggee_is_started_in_a_terminal_tab_and_ends_with_the_debug_tab(cx: &mut TestAppContext) {
    let (_window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    let (_dir, path) = app_file();
    let plan = Plan {
        app: path,
        ..Plan::default()
    };
    let answered = plan.terminal.clone();
    let (session, script) = start(plan, Actor::User, "integratedTerminal");
    wait(visual, "the terminal's answer", |_| {
        answered.lock().unwrap().is_some()
    });
    let answer = answered.lock().unwrap().clone().unwrap();
    assert_eq!(answer["success"], json!(true), "{answer}");
    let pid = answer["body"]["processId"].as_u64().expect("a process id");
    wait(visual, "the debuggee to run", |_| {
        session.state() == SessionState::Running
    });
    let model = model_of(&session, visual).unwrap();
    let run = model
        .read_with(visual, |model, _| model.terminal())
        .unwrap();
    run.read_with(visual, |run, _| {
        assert!(run.state().running());
        assert_eq!(run.pid().map(u64::from), Some(pid));
    });

    // Closing the debug tab ends the session and the program in the terminal.
    wait(visual, "the tab", |cx| {
        model.read_with(cx, |model, _| model.tab.is_some())
    });
    let tab = model
        .read_with(visual, |model, _| model.tab.clone())
        .unwrap();
    visual.update(|window, cx| tab.close(window, cx));
    wait(visual, "the debuggee to stop", |cx| {
        !run.read_with(cx, |run, _| run.state().running())
    });
    wait(visual, "the session to end", |_| {
        session.state().is_terminated()
    });
    assert!(model_of(&session, visual).is_none());
    drop(script);
}

#[gpui::test]
fn a_program_that_is_not_python_gets_a_plain_notice(cx: &mut TestAppContext) {
    let (_window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    let checkout = context::Checkout {
        project: PROJECT.into(),
        folder: PROJECT.into(),
    };
    let spawn = Spawn {
        label: "dev server".into(),
        command: "node".into(),
        args: vec!["server.js".into()],
        ..Spawn::default()
    };
    // The run feature's Debug action reaches the launcher installed here.
    assert!(visual.update(|window, cx| launch::debug(spawn, checkout, window, cx)));
    visual.run_until_parked();
    assert!(visual.debug_bounds("debug-start-notice").is_some());
    assert!(visual.debug_bounds("debug-copy-command").is_none());
    let tab = visual.update(|_, cx| dock::find_tab(PROJECT, "debug-notice", cx));
    assert!(tab.is_some());
}

#[gpui::test]
fn an_agents_session_is_shown_quietly_until_the_user_takes_over(cx: &mut TestAppContext) {
    let (_window, mut visual, agent) = shell(cx);
    let visual = &mut visual;
    let (_dir, path) = app_file();
    let plan = Plan {
        app: path.clone(),
        ..Plan::default()
    };
    let (session, script) = start(plan, Actor::Agent(agent.to_string()), "internalConsole");
    wait(visual, "the agent's tab", |cx| {
        model_of(&session, cx)
            .is_some_and(|model| model.read_with(cx, |model, _| model.tab.is_some()))
    });
    let model = model_of(&session, visual).unwrap();
    wait(visual, "the program to run", |_| {
        session.state() == SessionState::Running
    });
    visual.run_until_parked();
    // The tab is there, but the dock was not raised for it.
    assert!(visual.debug_bounds("debug-tab").is_none());
    let label = visual
        .update(|_, cx| rail::session_label(agent, cx))
        .unwrap();
    model.read_with(visual, |model, _| {
        assert_eq!(model.driven_by(), Some(label.clone()));
        assert!(!model.user_drives());
    });
    wait(visual, "the rail status", |cx| {
        cx.update(|_, cx| rail::status(agent, cx).as_deref() == Some("debugging"))
    });

    // The agent pauses it, as its tools would.
    session.threads().unwrap();
    session.pause(None);
    wait(visual, "the pause", |_| stopped_on(&session, 10));
    wait(visual, "the rail to say where", |cx| {
        cx.update(|_, cx| rail::status(agent, cx).as_deref() == Some("paused at app.py:10"))
    });
    model.read_with(visual, |model, _| {
        // The user's step controls are off; Stop is not.
        for control in [
            Control::Continue,
            Control::StepOver,
            Control::StepInto,
            Control::Restart,
        ] {
            assert!(!model.can(control), "{control:?}");
        }
        assert!(model.can(Control::Stop));
    });
    visual.update(|_, cx| assert!(!sessions::act(Control::StepOver, cx)));
    // Its project is in front, so the stop brought the tab up with the
    // Take over button in the dock's header.
    wait(visual, "the tab to show", |cx| {
        cx.debug_bounds("debug-take-over").is_some()
    });
    assert!(visual.debug_bounds("debug-driver").is_some());

    model.update(visual, |model, cx| model.take_over(cx));
    visual.run_until_parked();
    assert_eq!(session.driver(), Actor::User);
    model.read_with(visual, |model, _| {
        assert!(model.user_drives() && model.driven_by().is_none());
        assert!(model.can(Control::StepOver));
    });
    visual.update(|_, cx| assert_eq!(rail::status(agent, cx), None));
    assert!(visual.debug_bounds("debug-take-over").is_none());
    assert!(visual.update(|_, cx| sessions::act(Control::StepOver, cx)));
    wait(visual, "the user's step", |_| stopped_on(&session, 11));
    finish(&session, script);
}

#[gpui::test]
fn a_stop_on_system_exit_is_the_program_ending_unless_raised_is_on(cx: &mut TestAppContext) {
    let (_window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    let (_dir, path) = app_file();
    let plan = Plan {
        app: path,
        system_exit: true,
        ..Plan::default()
    };
    let (session, script) = start(plan.clone(), Actor::User, "internalConsole");
    // Nobody presses anything: the stop is passed over.
    wait(visual, "the program to end", |_| {
        session.state().is_terminated()
    });
    assert_eq!(
        session.state(),
        SessionState::Terminated { exit_code: Some(3) }
    );
    let model = model_of(&session, visual).unwrap();
    model.read_with(visual, |model, _| assert_eq!(model.notice, None));
    finish(&session, script);

    // With "raised" on, the user asked to see every exception.
    breakpoints::store().set_exceptions(ExceptionFilters {
        raised: true,
        uncaught: true,
    });
    let (session, script) = start(plan, Actor::User, "internalConsole");
    wait(visual, "the stop", |_| session.state().stop().is_some());
    visual.run_until_parked();
    std::thread::sleep(Duration::from_millis(50));
    visual.run_until_parked();
    assert!(session.state().stop().is_some(), "it stays paused");
    // The same program again takes the tab its last run left.
    let again = model_of(&session, visual).unwrap();
    assert_eq!(again.entity_id(), model.entity_id());
    again.read_with(visual, |model, _| {
        assert_eq!(model.notice.as_deref(), Some("SystemExit: 3"));
    });
    wait(visual, "the notice to be drawn", |cx| {
        cx.debug_bounds("debug-notice").is_some()
    });
    breakpoints::store().set_exceptions(ExceptionFilters::default());
    finish(&session, script);
}

#[gpui::test]
fn a_new_start_begins_with_an_empty_console_and_a_restart_goes_on_under_a_divider(
    cx: &mut TestAppContext,
) {
    let (_window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    let (_dir, path) = app_file();
    let plan = Plan {
        app: path,
        system_exit: true,
        ..Plan::default()
    };
    let ended = |model: &Entity<Model>, cx: &mut VisualTestContext| -> Vec<(Tone, String)> {
        model.read_with(cx, |model, _| {
            (model.console.iter())
                .map(|line| (line.tone, line.text.to_string()))
                .collect()
        })
    };
    let end = "The program ended with exit code 3.".to_string();
    // What one run of the scripted program prints.
    let run = vec![
        (Tone::Stdout, "hello".to_string()),
        (Tone::Info, end.clone()),
    ];

    let (first, script) = start(plan.clone(), Actor::User, "internalConsole");
    wait(visual, "the first run to end", |_| {
        first.state().is_terminated()
    });
    let model = model_of(&first, visual).unwrap();
    wait(visual, "its last line", |cx| {
        ended(&model, cx)
            .last()
            .is_some_and(|(_, text)| *text == end)
    });
    finish(&first, script);

    // Started again from the button: the same tab, an empty console.
    let (second, script) = start(plan.clone(), Actor::User, "internalConsole");
    wait(visual, "the second run to end", |_| {
        second.state().is_terminated()
    });
    wait(visual, "its last line", |cx| {
        model_of(&second, cx)
            .is_some_and(|again| again.entity_id() == model.entity_id() && ended(&again, cx) == run)
    });
    finish(&second, script);

    // A restart: what was printed stays, with a divider before the new run.
    app(visual, |cx| {
        let debugger = sessions::debugger(cx).unwrap();
        debugger.update(cx, |debugger, _| debugger.expect_restart(second.id()));
    });
    let (third, script) = start(plan, Actor::User, "internalConsole");
    wait(visual, "the restarted run's end", |cx| {
        ended(&model, cx).len() == 5
            && ended(&model, cx)
                .last()
                .is_some_and(|(_, text)| *text == end)
    });
    assert_eq!(
        ended(&model, visual),
        [
            run.clone(),
            vec![(Tone::Divider, "Restarted".to_string())],
            run
        ]
        .concat()
    );
    finish(&third, script);
}
