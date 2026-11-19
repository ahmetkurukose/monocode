//! Code insight through a whole shell, where files open in panes: going to
//! a definition in another file and back, the Outline tab following the
//! pane in front, the language servers chip, the project's settings and a
//! project going away. The servers are scripted, except in the last test,
//! which drives the real basedpyright when `MONOCODE_TEST_PYTHON` names an
//! interpreter whose environment has it.
use super::{tests::window, *};
use crate::{
    code_editor::{self, Location, Severity},
    code_insight::tests::{
        self as insight, CHIP, OUTLINE_TAB, Rig, SETTINGS_PAGE, range as tests_range, range_of,
        server_for, uri, wait, wait_for,
    },
    code_shell::{
        chips::{self, ChipState, Tone},
        commands, context, panel_tabs, processes, settings,
    },
    file_editor::FileEditor,
};
use gpui::{Focusable, TestAppContext, VisualTestContext, WindowHandle};
use monocode_core::code::lsp::{self, ServerState};
use serde_json::json;
use std::{
    cell::RefCell,
    path::Path,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// A shell with the checkout at `root` among its projects.
fn start(root: &Path, cx: &mut TestAppContext) -> (WindowHandle<Shell>, VisualTestContext) {
    let (window, _, _) = window(cx);
    cx.update(|cx| cx.set_reduce_motion(true));
    let project = root.to_string_lossy().into_owned();
    window
        .update(cx, |shell, _, cx| {
            shell.workspace.add_project(&project);
            cx.notify();
        })
        .unwrap();
    let visual = VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    (window, visual)
}

/// Calls into the feature the way its own handlers do: with the window,
/// outside any update of the shell.
fn feature<R>(
    window: WindowHandle<Shell>,
    cx: &mut VisualTestContext,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    cx.update_window(window.into(), |_, window, cx| f(window, cx))
        .unwrap()
}

fn focused(cx: &mut VisualTestContext) -> Option<(String, Entity<FileEditor>)> {
    cx.update(|_, cx| {
        let file = context::focused_file(cx)?;
        Some((file.path, file.editor.upgrade()?))
    })
}

/// Opens a file in the pane in front and gives its editor the keyboard.
fn open_file(
    window: WindowHandle<Shell>,
    cx: &mut VisualTestContext,
    path: &Path,
) -> Entity<FileEditor> {
    let name = path.to_string_lossy().into_owned();
    assert!(feature(window, cx, |window, cx| {
        context::open_file(&name, None, window, cx)
    }));
    wait(cx, "the file to be in front", |cx| {
        focused(cx).is_some_and(|(path, _)| path == name)
    });
    let editor = focused(cx).unwrap().1;
    wait(cx, "the file to be read", |cx| {
        editor.read_with(cx, |editor, cx| !editor.input.read(cx).content.is_empty())
    });
    focus(window, cx, &editor);
    editor
}

fn focus(window: WindowHandle<Shell>, cx: &mut VisualTestContext, editor: &Entity<FileEditor>) {
    feature(window, cx, |window, cx| {
        let input = editor.read(cx).input.focus_handle(cx);
        window.focus(&input, cx);
    });
    cx.run_until_parked();
}

fn set_caret(cx: &mut VisualTestContext, editor: &Entity<FileEditor>, offset: usize) {
    editor.update(cx, |editor, cx| {
        (editor.input).update(cx, |input, cx| input.select_range(offset..offset, cx))
    });
    cx.run_until_parked();
}

/// Where the caret of the pane in front is.
fn place(cx: &mut VisualTestContext) -> Option<Location> {
    let (path, editor) = focused(cx)?;
    Some(editor.read_with(cx, |editor, cx| {
        let input = editor.input.read(cx);
        Location::of(Path::new(&path), &input.content, input.caret())
    }))
}

fn chip(cx: &mut VisualTestContext) -> Option<ChipState> {
    cx.update(|_, cx| chips::find(CHIP, cx).and_then(|chip| (chip.state)(cx)))
}

#[gpui::test]
fn go_to_definition_opens_the_other_file_where_the_server_points_and_back_returns(
    cx: &mut TestAppContext,
) {
    let rig = Rig::new(cx);
    let app_text = "from pkg.lib import helper\n\nvalue = helper()\n";
    let app = rig.write("app.py", app_text);
    let lib_text = "import os\n\n\ndef helper():\n    return os.sep  # é\n";
    let lib = rig.write("pkg/lib.py", lib_text);
    let (window, mut visual) = start(&rig.root, cx);
    let cx = &mut visual;
    let editor = open_file(window, cx, &app);
    wait(cx, "the document to be opened with the manager", |cx| {
        cx.update(|_, cx| insight::synced(&app, cx))
    });
    let pyright = rig.pyright();
    wait(cx, "the server to be ready", |_| {
        rig.state_of("basedpyright") == Some(ServerState::Ready)
    });
    // The server names the file with its links resolved, as servers do
    // (`/private/var` for `/var`); it must still open as the one file.
    let real = std::fs::canonicalize(&lib).unwrap();
    pyright.on("textDocument/definition", move |message, out| {
        out.result(
            message,
            json!([{ "uri": uri(&real), "range": range_of(lib_text, "helper") }]),
        );
    });
    let call = app_text.rfind("helper").unwrap() + 2;
    set_caret(cx, &editor, call);
    cx.simulate_keystrokes("f12");
    let lib_name = lib.to_string_lossy().into_owned();
    wait(cx, "the definition's file to be in front", |cx| {
        focused(cx).is_some_and(|(path, _)| path == lib_name)
    });
    wait(cx, "the caret to be on the definition", |cx| {
        place(cx).is_some_and(|at| (at.line, at.column) == (4, 5))
    });
    let asked = pyright.wait_for("textDocument/definition");
    assert_eq!(
        asked["params"]["position"],
        json!({ "line": 2, "character": 10 })
    );
    // The file it led to is served by the same server, not a new one.
    wait(cx, "the other file to be opened with the manager", |cx| {
        cx.update(|_, cx| insight::synced(&lib, cx))
    });
    wait(cx, "the server to be told of it", |_| {
        pyright.count("textDocument/didOpen") == 2
    });
    assert_eq!(rig.launcher.count(), 2);

    // Back returns to the call.
    let other = focused(cx).unwrap().1;
    focus(window, cx, &other);
    cx.simulate_keystrokes("ctrl--");
    let app_name = app.to_string_lossy().into_owned();
    wait(cx, "the first file to be in front again", |cx| {
        focused(cx).is_some_and(|(path, _)| path == app_name)
    });
    wait(cx, "the caret to be back on the call", |cx| {
        place(cx).is_some_and(|at| (at.line, at.column) == (3, 11))
    });

    // Many usages open in the right panel's Usages tab, and a row there
    // opens its place in a pane.
    let mut places = vec![
        json!({ "uri": uri(&app), "range": range_of(app_text, "helper") }),
        json!({ "uri": uri(&app), "range": tests_range((2, 8), (2, 14)) }),
        json!({ "uri": uri(&lib), "range": range_of(lib_text, "helper") }),
    ];
    let more = rig.write("more.py", &"helper()\n".repeat(5));
    for line in 0..5 {
        places.push(json!({ "uri": uri(&more), "range": tests_range((line, 0), (line, 6)) }));
    }
    pyright.on("textDocument/references", move |message, out| {
        out.result(message, json!(places))
    });
    let again = focused(cx).unwrap().1;
    focus(window, cx, &again);
    cx.simulate_keystrokes("shift-f12");
    wait(cx, "the usages tab", |cx| {
        cx.update(|_, cx| insight::usages_shown(cx).is_some())
    });
    let (symbol, count, lines) = cx.update(|_, cx| insight::usages_shown(cx).unwrap());
    assert_eq!((symbol.as_str(), count, lines.len()), ("helper", 8, 8));
    wait(cx, "the tab to be drawn in the right panel", |cx| {
        cx.debug_bounds("code-usages").is_some()
    });
    feature(window, cx, |window, cx| {
        insight::usages_open("more.py", 4, window, cx)
    });
    let more_name = more.to_string_lossy().into_owned();
    wait(cx, "the usage's file to be in front", |cx| {
        focused(cx).is_some_and(|(path, _)| path == more_name)
    });
    wait(cx, "the caret to be on the usage", |cx| {
        place(cx).is_some_and(|at| (at.line, at.column) == (4, 1))
    });
}

#[gpui::test]
fn the_outline_follows_the_pane_in_front_and_marks_the_carets_symbol(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let text = "class Store:\n    def load(self):\n        return 1\n\n    def save(self):\n        return 2\n\n\ndef main():\n    pass\n";
    let store = rig.write("store.py", text);
    let other = rig.write("other.py", "def only():\n    pass\n");
    let (window, mut visual) = start(&rig.root, cx);
    let cx = &mut visual;
    cx.update(|_, cx| {
        assert!(panel_tabs::is_shown(OUTLINE_TAB, cx));
        assert!(insight::outline_listed(cx).is_empty());
    });
    let editor = open_file(window, cx, &store);
    let names = |cx: &mut VisualTestContext| -> Vec<String> {
        cx.update(|_, cx| insight::outline_listed(cx))
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    };
    let current = |cx: &mut VisualTestContext| -> Vec<String> {
        cx.update(|_, cx| insight::outline_listed(cx))
            .into_iter()
            .filter(|(_, current)| *current)
            .map(|(name, _)| name)
            .collect()
    };
    wait(cx, "the file's outline", |cx| {
        names(cx) == ["Store", "load", "save", "main"]
    });
    // The symbol the caret is in is the marked one.
    set_caret(cx, &editor, text.find("return 2").unwrap());
    wait(cx, "the caret's symbol to be marked", |cx| {
        current(cx) == ["save"]
    });
    // The filter keeps matches and what encloses them.
    cx.update(|_, cx| insight::outline_filter("SA", cx));
    assert_eq!(names(cx), ["Store", "save"]);
    cx.update(|_, cx| insight::outline_filter("", cx));
    // A row jumps to its symbol, as a move Back returns from.
    feature(window, cx, |window, cx| {
        insight::outline_jump("main", window, cx)
    });
    cx.run_until_parked();
    assert_eq!(place(cx).map(|at| (at.line, at.column)), Some((9, 5)));
    assert_eq!(current(cx), ["main"]);
    cx.update(|_, cx| assert!(code_editor::hub(cx).read(cx).history().can_back()));

    // The tab is drawn in the right panel.
    assert!(feature(window, cx, |window, cx| {
        panel_tabs::reveal(OUTLINE_TAB, window, cx)
    }));
    cx.run_until_parked();
    assert!(cx.debug_bounds("code-outline").is_some());

    // Quick open's `@` lists the file's symbols, and what is typed is
    // also looked for in the checkout's other files of the language.
    let found: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let search = |cx: &mut VisualTestContext, typed: &str| {
        found.borrow_mut().clear();
        let (found, folder) = (found.clone(), rig.root.to_string_lossy().into_owned());
        let typed = typed.to_owned();
        cx.update(|_, cx| {
            let (mode, query) = commands::mode_for(&typed, cx).expect("the @ mode");
            let scope = context::scope(&folder, cx);
            let rows = (mode.search)(query, scope, cx);
            cx.spawn(async move |_| {
                let rows = rows.await;
                *found.borrow_mut() = (rows.iter())
                    .map(|row| (row.title.to_string(), row.detail.to_string()))
                    .collect();
            })
            .detach();
        });
    };
    search(cx, "@");
    wait(cx, "the file's symbols", |_| found.borrow().len() == 4);
    assert_eq!(found.borrow()[1].1, "method · store.py:2");
    search(cx, "@on");
    wait(cx, "symbols across the checkout", |_| {
        !found.borrow().is_empty()
    });
    assert_eq!(
        *found.borrow(),
        [("only".to_owned(), "function · other.py:1".to_owned())]
    );

    // Another file in front, another outline.
    open_file(window, cx, &other);
    wait(cx, "the other file's outline", |cx| names(cx) == ["only"]);
}

#[gpui::test]
fn the_chip_follows_the_servers_and_the_projects_settings_decide_what_runs(
    cx: &mut TestAppContext,
) {
    static FAIL_RUFF: AtomicBool = AtomicBool::new(false);
    FAIL_RUFF.store(false, Ordering::SeqCst);
    let rig = Rig::with(
        |launch| {
            if launch.program.ends_with("ruff") && FAIL_RUFF.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("it is not a program"));
            }
            Ok(server_for(launch))
        },
        cx,
    );
    let path = rig.write("app.py", "import os\n");
    let project = rig.root.to_string_lossy().into_owned();
    let (window, mut visual) = start(&rig.root, cx);
    let cx = &mut visual;
    assert_eq!(chip(cx), None, "no server is known yet");
    open_file(window, cx, &path);
    let shows = |cx: &mut VisualTestContext, tone: Tone, text: &str| {
        chip(cx).is_some_and(|chip| chip.tone == tone && chip.text.as_ref() == text)
    };
    wait(cx, "a quiet chip for two ready servers", |cx| {
        shows(cx, Tone::Quiet, "2 servers")
    });
    wait(cx, "ruff's diagnostic", |cx| {
        cx.update(|_, cx| insight::diagnostics(&path, cx).len() == 1)
    });

    // Reported work shows as activity.
    let pyright = rig.pyright();
    let progress = |kind: &str| {
        pyright.notify(
            "$/progress",
            json!({ "token": "t", "value": { "kind": kind, "title": "Indexing" } }),
        );
    };
    progress("begin");
    wait(cx, "a busy chip", |cx| {
        shows(cx, Tone::Busy, "basedpyright-langserver: indexing")
    });
    progress("end");
    wait(cx, "a quiet chip again", |cx| {
        shows(cx, Tone::Quiet, "2 servers")
    });
    // The list names each with its root, state and documents.
    let rows = cx.update(|_, cx| (chips::find(CHIP, cx).unwrap().items)(cx));
    assert_eq!(rows.len(), 2);
    assert!(rows[0].title.contains("basedpyright-langserver"));
    assert_eq!(rows[0].detail.as_deref(), Some("Ready · 1 file"));
    assert_eq!(rows[0].actions[1].1.as_ref(), "Stop");

    // An executable set in the project's settings is that or nothing.
    assert!(feature(window, cx, |window, cx| {
        settings::open(&project, SETTINGS_PAGE, window, cx)
    }));
    cx.run_until_parked();
    assert!(cx.debug_bounds("code-insight-settings").is_some());
    feature(window, cx, |window, cx| {
        insight::set_tool_path("ruff", "/nowhere/ruff", window, cx)
    });
    wait(cx, "a warning for the missing tool", |cx| {
        shows(cx, Tone::Warning, "ruff not installed")
    });
    cx.update(|_, cx| {
        let saved = settings::value(&project, SETTINGS_PAGE, cx).unwrap();
        assert_eq!(saved["toolPaths"]["ruff"], "/nowhere/ruff");
    });
    // What to install can be copied from the list.
    let rows = cx.update(|_, cx| (chips::find(CHIP, cx).unwrap().items)(cx));
    let note = (rows.iter())
        .find(|row| row.id.starts_with("copy:"))
        .expect("a row to copy the hint from");
    assert!(note.title.starts_with("Looked for ruff."), "{}", note.title);
    let id = note.id.clone();
    feature(window, cx, |window, cx| {
        (chips::find(CHIP, cx).unwrap().pick)(&id, window, cx)
    });
    let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert!(
        copied
            .unwrap()
            .starts_with("ruff was not found (looked for: ruff).")
    );

    // A server that cannot start is a warning too.
    FAIL_RUFF.store(true, Ordering::SeqCst);
    feature(window, cx, |window, cx| {
        insight::set_tool_path("ruff", "", window, cx)
    });
    wait(cx, "a warning for the failed server", |cx| {
        shows(cx, Tone::Warning, "ruff failed")
    });
    cx.update(|_, cx| assert_eq!(settings::value(&project, SETTINGS_PAGE, cx), None));

    // Switched off, the project has no document open and no server.
    feature(window, cx, |window, cx| {
        insight::set_disabled(true, window, cx)
    });
    wait(cx, "the project's servers to be gone", |cx| {
        !rig.manager.is_open(&path) && rig.manager.statuses().is_empty() && chip(cx).is_none()
    });
    assert!(rig.manager.project_settings(&rig.root).disabled);
    cx.update(|_, cx| assert!(insight::diagnostics(&path, cx).is_empty()));
    let launched = rig.launcher.count();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(rig.launcher.count(), launched);

    // Switched on again, its open files are served again.
    FAIL_RUFF.store(false, Ordering::SeqCst);
    feature(window, cx, |window, cx| {
        insight::set_disabled(false, window, cx)
    });
    wait(cx, "the servers to be back", |cx| {
        cx.update(|_, cx| insight::synced(&path, cx)) && shows(cx, Tone::Quiet, "2 servers")
    });

    // The project is removed: its documents are closed, its servers stopped.
    window
        .update(cx, |shell, _, cx| {
            shell.workspace.projects.retain(|known| *known != project);
            cx.notify();
        })
        .unwrap();
    wait(cx, "the removed project's servers to be gone", |_| {
        !rig.manager.is_open(&path) && rig.manager.statuses().is_empty()
    });
}

#[gpui::test]
fn a_leading_at_sign_still_finds_files_and_text_where_there_are_no_symbols(
    cx: &mut TestAppContext,
) {
    let rig = Rig::new(cx);
    let store = rig.write("store.py", "class Store:\n    pass\n");
    rig.write("web/@types/shapes.d.ts", "export type Shape = string;\n");
    let css = rig.write("web/site.css", "@media print {\n  body { margin: 0 }\n}\n");
    let (window, mut visual) = start(&rig.root, cx);
    let cx = &mut visual;
    // Quick open lists the files of the project in front.
    window
        .update(cx, |shell, _, _| {
            shell.workspace.project = rig.root.to_string_lossy().into_owned()
        })
        .unwrap();
    let ask = |cx: &mut VisualTestContext, typed: &str, content: bool| -> Vec<String> {
        let typed = typed.to_owned();
        window
            .update(cx, |shell, window, cx| {
                shell.open_quick_open(false, window, cx);
                if content {
                    let picker = shell.quick_open.as_ref().unwrap().picker().clone();
                    picker.update(cx, |picker, cx| picker.show_content(cx));
                }
                shell.set_quick_open_query(typed, cx);
            })
            .unwrap();
        let mut rows = Vec::new();
        wait(cx, "quick open's rows", |cx| {
            rows = window
                .update(cx, |shell, _, cx| {
                    let open = shell.quick_open.as_ref().unwrap();
                    open.picker().read(cx).row_labels()
                })
                .unwrap();
            !rows.is_empty()
        });
        window
            .update(cx, |shell, window, cx| shell.close_quick_open(window, cx))
            .unwrap();
        rows.into_iter().map(|(label, _)| label).collect()
    };
    // No symbols in the file in front: `@` is a character of the name
    // looked for.
    open_file(window, cx, &css);
    assert_eq!(ask(cx, "@types", false), ["shapes.d.ts"]);
    // A file with symbols in front: its symbols, and when none answers,
    // the files again.
    open_file(window, cx, &store);
    assert_eq!(ask(cx, "@sto", false), ["Store"]);
    assert_eq!(ask(cx, "@types", false), ["shapes.d.ts"]);
    // Inside the files the text is looked for as it was typed.
    assert_eq!(ask(cx, "@media", true), ["@media print {"]);
}

#[gpui::test]
fn another_language_has_no_server_until_its_project_switches_it_on(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.write(".venv/bin/rust-analyzer", "");
    let path = rig.write("main.rs", "fn main() {}\n");
    let project = rig.root.to_string_lossy().into_owned();
    let (window, mut visual) = start(&rig.root, cx);
    let cx = &mut visual;
    let editor = open_file(window, cx, &path);
    let provided = |cx: &mut VisualTestContext| {
        editor.read_with(cx, |editor, cx| {
            (editor.input.read(cx).editor_providers())
                .is_some_and(|providers| providers.completion.is_some())
        })
    };
    // Off, the file is what it was before there were servers: none is
    // started or looked for, by the editor or for an agent.
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(rig.launcher.count(), 0);
    assert!(!rig.manager.is_open(&path) && rig.manager.statuses().is_empty());
    assert_eq!(chip(cx), None);
    assert!(!provided(cx));
    let off = rig.manager.project_settings(&rig.root).disabled_servers;
    assert!(off.contains("rust-analyzer") && !off.contains("basedpyright"));

    assert!(feature(window, cx, |window, cx| {
        settings::open(&project, SETTINGS_PAGE, window, cx)
    }));
    cx.run_until_parked();
    feature(window, cx, |window, cx| {
        insight::set_language("rust-analyzer", true, window, cx)
    });
    wait(cx, "the file to be served", |cx| {
        cx.update(|_, cx| insight::synced(&path, cx)) && provided(cx) && chip(cx).is_some()
    });
    cx.update(|_, cx| {
        let saved = settings::value(&project, SETTINGS_PAGE, cx).unwrap();
        assert_eq!(saved["languages"], json!(["rust-analyzer"]));
    });

    feature(window, cx, |window, cx| {
        insight::set_language("rust-analyzer", false, window, cx)
    });
    wait(cx, "the server to be gone", |cx| {
        !rig.manager.is_open(&path) && rig.manager.statuses().is_empty() && !provided(cx)
    });
    cx.update(|_, cx| assert_eq!(settings::value(&project, SETTINGS_PAGE, cx), None));
}

#[gpui::test]
fn real_basedpyright_reports_a_type_error_and_finds_a_definition_in_another_file(
    cx: &mut TestAppContext,
) {
    let Some(python) = std::env::var_os("MONOCODE_TEST_PYTHON") else {
        return;
    };
    cx.executor().allow_parking();
    let venv = (Path::new(&python).parent())
        .and_then(Path::parent)
        .expect("the interpreter is in a virtual environment's bin");
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().canonicalize().unwrap().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"sample\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(venv, root.join(".venv")).unwrap();
    let lib = root.join("lib.py");
    std::fs::write(&lib, "def helper() -> str:\n    return \"x\"\n").unwrap();
    let app_text = "from lib import helper\n\nvalue: int = helper()\n";
    let app = root.join("app.py");
    std::fs::write(&app, app_text).unwrap();

    let manager = lsp::Manager::new(lsp::ManagerOptions::default());
    cx.update(|cx| {
        crate::input::bind(cx);
        crate::file_editor::bind(cx);
        crate::popover::bind(cx);
        crate::code_insight::init_with(manager.clone(), cx);
    });
    let (window, mut visual) = start(&root, cx);
    let cx = &mut visual;
    let editor = open_file(window, cx, &app);
    let patience = Duration::from_secs(90);
    wait_for(cx, patience, "basedpyright's type error", &mut |cx| {
        cx.update(|_, cx| {
            insight::diagnostics(&app, cx)
                .iter()
                .any(|(source, severity, marked, _)| {
                    source == "basedpyright" && *severity == Severity::Error && marked == "helper()"
                })
        })
    });
    assert!(chip(cx).is_some());
    // The servers are listed with the running work, and measured with
    // everything they started.
    let servers: Vec<u32> = (manager.statuses().iter())
        .filter_map(|status| status.pid)
        .collect();
    assert!(!servers.is_empty());
    assert!(servers.iter().all(|pid| processes::owns(*pid)));
    wait_for(cx, patience, "the servers' memory", &mut |cx| {
        let rows = cx.update(|_, cx| (chips::find(CHIP, cx).unwrap().items)(cx));
        (rows.iter()).any(|row| {
            row.detail
                .as_ref()
                .is_some_and(|detail| detail.ends_with(" MB"))
        })
    });

    set_caret(cx, &editor, app_text.rfind("helper").unwrap() + 1);
    cx.simulate_keystrokes("f12");
    let lib_name = lib.to_string_lossy().into_owned();
    wait_for(cx, patience, "the definition's file", &mut |cx| {
        focused(cx).is_some_and(|(path, _)| path == lib_name)
    });
    wait(cx, "the caret to be on the definition", |cx| {
        place(cx).is_some_and(|at| (at.line, at.column) == (1, 5))
    });
    manager.stop_all();
}
