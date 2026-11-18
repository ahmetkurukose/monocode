//! Run through a real shell: a temporary project with a stand-in interpreter
//! (a shell script that says what it was asked to run), so the whole path is
//! exercised without Python: the dock tab, the registry, the last run, the
//! header's menu, the gutter's markers, the settings page, the chip and
//! quick open. One test, gated on `MONOCODE_TEST_PYTHON`, runs real pytest.
use super::{
    chip, gutter,
    header::{self, PAGE},
    output,
    settings_page::{Field, RunsPage},
    start, targets,
};
use crate::{
    code_editor::{self, DocumentInfo, GutterSlot, HeaderClick, HeaderProvider},
    code_shell::{
        self, chips,
        commands::{self, Scope},
        context::{self, Checkout},
        launch, settings,
    },
    popover::MenuItem,
    shell::Shell,
    workspace::Workspace,
};
use gpui::{App, AppContext, Modifiers, TestAppContext, VisualTestContext, Window, WindowHandle};
use monocode_core::{
    code::{
        run::{RunConfig, RunId, RunInfo, RunState, StartedBy, TestRunner},
        services::services,
        spawn::Spawn,
        toolchain::Toolchain,
    },
    store::Store,
    terminal_setup::{TerminalSetup, Venv},
};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

/// A project on disk, the shell showing it, and the session in it.
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    project: String,
    window: WindowHandle<Shell>,
    visual: VisualTestContext,
    session: i64,
}

impl Fixture {
    /// Writes `files` into a new project whose `.venv` holds a stand-in
    /// interpreter, and opens it.
    fn new(files: &[(&str, &str)], cx: &mut TestAppContext) -> Self {
        Self::with(files, None, cx)
    }

    /// As `new`; with `setup`, the project has that terminal setup and no
    /// stand-in interpreter.
    fn with(files: &[(&str, &str)], setup: Option<TerminalSetup>, cx: &mut TestAppContext) -> Self {
        // Runs are real processes that wake the UI from their own threads.
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for (name, text) in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        if setup.is_none() {
            let bin = root.join(".venv/bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("activate"), "").unwrap();
            let python = bin.join("python");
            std::fs::write(&python, "#!/bin/sh\necho \"python $*\"\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let project = root.to_string_lossy().into_owned();

        let store = Store::open(Path::new(":memory:")).unwrap();
        let session = store.create("claude", &project).unwrap();
        let id = session.id;
        let mut state = Workspace {
            new_layout_boxy: false,
            ..Default::default()
        };
        state.add_project(&project);
        state.open(id);
        if let Some(setup) = setup {
            state.project_terminal_setups.insert(project.clone(), setup);
        }
        let saved = serde_json::to_string(&state).unwrap();
        let store = Arc::new(Mutex::new(store));
        let window = cx.update(|cx| {
            code_shell::register(cx);
            // These tests install a debugger themselves where one matters.
            launch::clear_debugger(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| Shell::new(store, vec![session], project.clone(), Some(saved), cx))
            })
            .unwrap()
        });
        cx.update(|cx| cx.set_reduce_motion(true));
        window
            .update(cx, |shell, _, _| {
                shell.workspace.preferences.set_terminal_placement("bottom")
            })
            .unwrap();
        cx.refresh().unwrap();
        let visual = VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        Self {
            _dir: dir,
            root,
            project,
            window,
            visual,
            session: id,
        }
    }

    fn checkout(&self) -> Checkout {
        Checkout {
            project: self.project.clone(),
            folder: self.project.clone(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn doc(&self, name: &str) -> DocumentInfo {
        DocumentInfo::new(self.path(name), &self.root)
    }

    /// Calls into the features the way a click handler does: with the
    /// window, outside any update of the shell.
    fn feature<R>(&mut self, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        (self.visual)
            .update_window(self.window.into(), |_, window, cx| f(window, cx))
            .unwrap()
    }

    /// Lets real time pass until `done`.
    fn wait(&mut self, what: &str, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
        for _ in 0..1500 {
            self.visual.run_until_parked();
            if done(&mut self.visual) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    /// Opens a file of the project and waits for it to be the focused one.
    fn open(&mut self, name: &str) {
        let path = self.path(name).to_string_lossy().into_owned();
        let opened = path.clone();
        assert!(self.feature(move |window, cx| context::open_file(&opened, None, window, cx)));
        self.wait("the file to open", |cx| {
            cx.update(|_, cx| {
                context::focused_file(cx).is_some_and(|file| {
                    file.path == path
                        && (file.editor.upgrade()).is_some_and(|editor| {
                            !editor.read(cx).input.read(cx).content.is_empty()
                        })
                })
            })
        });
    }

    /// The registry's runs of this project, oldest first.
    fn runs(&self) -> Vec<RunInfo> {
        let mut runs: Vec<RunInfo> = (services().runs().list().into_iter())
            .filter(|info| info.checkout == self.root)
            .collect();
        runs.sort_by_key(|info| info.id);
        runs
    }

    fn wait_for_end(&mut self, id: RunId) -> RunInfo {
        self.wait("the run to end", |_| {
            (services().runs().get(id)).is_some_and(|info| !info.state.is_running())
        });
        services().runs().get(id).unwrap()
    }

    /// Waits for a start to report and returns what it reported.
    fn outcome(&mut self, outcome: start::Outcome) -> Result<RunId, String> {
        self.wait("the start to report", |_| !outcome.is_empty());
        outcome.try_recv().unwrap()
    }

    fn tabs(&mut self) -> usize {
        let project = self.project.clone();
        self.window
            .update(&mut self.visual, |shell, _, _| {
                (shell.workspace.docks.get(&project)).map_or(0, |dock| dock.terminals.len())
            })
            .unwrap()
    }

    fn output_of(&self, id: RunId) -> String {
        services().runs().output(id, 0).unwrap().text
    }
}

fn user(name: &str, command: &str) -> RunConfig {
    RunConfig {
        name: name.into(),
        command: command.into(),
        ..RunConfig::default()
    }
}

/// The actions of a menu as `(id, label, checked)`.
fn actions(items: &[MenuItem]) -> Vec<(String, String, bool)> {
    (items.iter())
        .filter_map(|item| match item {
            MenuItem::Action(action) => Some((
                action.id.to_string(),
                action.label.to_string(),
                action.checked,
            )),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn running_a_file_saves_it_and_gives_it_a_tab_a_registry_entry_and_the_last_run(
    cx: &mut TestAppContext,
) {
    let mut fx = Fixture::new(&[("hello.py", "print('hi')\n")], cx);
    fx.open("hello.py");
    let checkout = fx.checkout();
    let path = fx.path("hello.py");

    // The file has unsaved edits when it is run.
    fx.visual.update(|_, cx| {
        let editor = context::focused_file(cx).unwrap().editor.upgrade().unwrap();
        let input = editor.read(cx).input.clone();
        input.update(cx, |input, cx| {
            input.set_text("print('edited')\n".into(), cx)
        });
    });
    fx.visual.run_until_parked();
    let (task, context) = fx.visual.update(|_, cx| {
        let context = targets::editor_context(&path, &checkout, None, cx);
        let choices = targets::choices(&checkout, &context, cx);
        assert!(choices.user.is_empty());
        let runs = settings::runs(&checkout.project, cx);
        (choices.default(&runs).cloned().unwrap(), context)
    });
    assert_eq!(task.name, "Run hello.py");
    assert_eq!(task.command, "$PYTHON hello.py");

    let outcome = (fx.visual).update(|_, cx| start::start(task.clone(), context, checkout, cx));
    let id = fx.outcome(outcome).expect("the run starts");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "print('edited')\n",
        "the file was saved before it ran"
    );
    let info = fx.wait_for_end(id);
    assert_eq!(info.state, RunState::Exited(Some(0)));
    assert_eq!(info.started_by, StartedBy::User);
    assert!(!info.headless);
    assert_eq!(info.label, "Run hello.py");
    assert!(info.spawn.command.ends_with(".venv/bin/python"), "{info:?}");
    assert_eq!(info.spawn.args, ["hello.py"]);
    assert_eq!(fx.output_of(id).trim(), "python hello.py");
    assert_eq!(fx.tabs(), 1);
    fx.visual.update(|_, cx| {
        assert!(start::tab_of(id, cx).is_some_and(|tab| tab.is_open()));
        assert_eq!(
            settings::runs(&fx.project, cx).last_used(),
            Some(&task),
            "the task is the project's last run"
        );
        assert!(start::has_last(cx));
    });
    let project = fx.project.clone();
    fx.window
        .update(&mut fx.visual, |shell, _, _| {
            let saved = &shell.workspace.project_runs[&project];
            assert_eq!(saved.last_used.as_ref(), Some(&task));
            assert!(shell.workspace.saved_json().contains("builtin:python:file"));
        })
        .unwrap();

    // Running it again uses the same tab, and is a new run in the registry.
    assert!(fx.visual.update(|_, cx| start::rerun_last(cx)));
    fx.wait("the second run", |_| fx_runs(&info.checkout).len() == 2);
    let second = fx.runs()[1].id;
    assert_eq!(fx.wait_for_end(second).state, RunState::Exited(Some(0)));
    assert_eq!(fx.output_of(second).trim(), "python hello.py");
    assert_eq!(fx.tabs(), 1);
    fx.visual.update(|_, cx| {
        assert!(start::tab_of(second, cx).is_some());
        assert!(start::tab_of(id, cx).is_none(), "the tab moved on");
    });
}

fn fx_runs(checkout: &Path) -> Vec<RunInfo> {
    (services().runs().list().into_iter())
        .filter(|info| info.checkout == checkout)
        .collect()
}

#[gpui::test]
fn a_configuration_that_cannot_be_resolved_says_why(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    let checkout = fx.checkout();
    let context = targets::bare_context(&checkout);
    let outcome = fx.visual.update(|_, cx| {
        let config = user("Grep", "rg $SELECTED_TEXT");
        start::start(config, context.clone(), checkout.clone(), cx)
    });
    let sentence = fx.outcome(outcome).unwrap_err();
    assert_eq!(
        sentence,
        "\u{201c}Grep\u{201d} cannot run. No text is selected."
    );
    fx.visual.update(|_, cx| {
        assert_eq!(start::last_notice(cx), Some(sentence.clone()));
    });
    let outcome = fx
        .visual
        .update(|_, cx| start::start(user("Open quote", "echo 'x"), context, checkout, cx));
    let sentence = fx.outcome(outcome).unwrap_err();
    assert!(sentence.ends_with("The command has a quote that is never closed."));
    assert!(fx.runs().is_empty());
    assert_eq!(fx.tabs(), 0);
}

#[gpui::test]
fn a_stop_asked_of_the_registry_stops_the_tab_and_a_new_start_reuses_it(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    let checkout = fx.checkout();
    let context = targets::bare_context(&checkout);
    let config = user("Serve", "sh -c 'echo up; sleep 30'");
    let outcome = fx
        .visual
        .update(|_, cx| start::start(config.clone(), context.clone(), checkout.clone(), cx));
    let first = fx.outcome(outcome).unwrap();
    fx.wait("the server to print", |_| {
        (services().runs().output(first, 0)).is_some_and(|output| output.text.contains("up"))
    });
    let info = services().runs().get(first).unwrap();
    assert!(info.state.is_running());
    let pid = info.pid.expect("the tab told the registry its process");
    assert!(code_shell::processes::owns(pid));

    // Started again while it runs: the same tab, the old process ended.
    let outcome = fx
        .visual
        .update(|_, cx| start::start(config.clone(), context.clone(), checkout.clone(), cx));
    let second = fx.outcome(outcome).unwrap();
    assert_ne!(first, second);
    assert_eq!(fx.wait_for_end(first).state, RunState::Stopped);
    fx.wait("the restarted server to print", |_| {
        (services().runs().output(second, 0)).is_some_and(|output| output.text.contains("up"))
    });
    assert_eq!(fx.tabs(), 1);
    let restarted = services().runs().get(second).unwrap();
    assert!(restarted.state.is_running());
    assert_ne!(restarted.pid, Some(pid));

    // An agent, or the footer, asks the registry; the tab's process ends.
    assert!(services().runs().stop(second));
    let stopped = fx.wait_for_end(second);
    assert_eq!(stopped.state, RunState::Stopped);
    fx.visual.update(|_, cx| {
        let tab = start::tab_of(second, cx).expect("the tab stays");
        assert!(tab.is_open());
    });
    fx.wait("the process to leave the list", |_| {
        !restarted.pid.is_some_and(code_shell::processes::owns)
    });
}

const TESTS: &str = "\
import unittest


def test_top():
    assert True


class TestMath:
    def test_add(self):
        assert 1 + 1 == 2

    def helper(self):
        pass

    class TestNested:
        def test_inner(self):
            pass


class Legacy(unittest.TestCase):
    def testOld(self):
        pass


def helper():
    pass


if __name__ == \"__main__\":
    unittest.main()
";

fn row(text: &str, needle: &str) -> usize {
    text[..text.find(needle).unwrap()].matches('\n').count()
}

#[gpui::test]
fn python_tests_get_run_markers_and_a_click_runs_that_one_test(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[("tests/test_math.py", TESTS), ("app.py", TESTS)], cx);
    let root = fx.root.clone();
    fx.open("tests/test_math.py");
    let path = fx.path("tests/test_math.py");
    let hub = fx.visual.update(|_, cx| code_editor::hub(cx));
    fx.wait("the markers", |cx| {
        !hub.read_with(cx, |hub, _| {
            hub.gutter_marks(&path, GutterSlot::Run).is_empty()
        })
    });
    let marks: Vec<(usize, String)> = hub.read_with(&fx.visual, |hub, _| {
        (hub.gutter_marks(&path, GutterSlot::Run).iter())
            .map(|mark| (mark.row, mark.tooltip.to_string()))
            .collect()
    });
    let expected = [
        ("def test_top", "Run test_top"),
        ("class TestMath", "Run the tests in TestMath"),
        ("def test_add", "Run TestMath::test_add"),
        ("class TestNested", "Run the tests in TestNested"),
        ("def test_inner", "Run TestMath::TestNested::test_inner"),
        ("class Legacy", "Run the tests in Legacy"),
        ("def testOld", "Run Legacy::testOld"),
        ("if __name__", "Run test_math.py"),
    ]
    .map(|(needle, tip)| (row(TESTS, needle), tip.to_owned()));
    assert_eq!(marks, expected);

    // The click on a test's marker runs that test alone.
    let add = row(TESTS, "def test_add");
    fx.visual.run_until_parked();
    let marker = fx.visual.update(|_, cx| {
        let editor = context::focused_file(cx).unwrap().editor.upgrade().unwrap();
        (editor.read(cx).input.read(cx)).gutter_slot_center(GutterSlot::Run, add)
    });
    let marker = marker.expect("the run column is drawn");
    fx.visual.simulate_click(marker, Modifiers::default());
    fx.wait("the test to start", |_| !fx_runs(&root).is_empty());
    let info = fx.runs().remove(0);
    assert!(info.spawn.command.ends_with(".venv/bin/python"));
    assert_eq!(
        info.spawn.args,
        ["-m", "pytest", "tests/test_math.py::TestMath::test_add"]
    );
    assert_eq!(info.spawn.cwd, fx.root);
    assert_eq!(info.label, "pytest TestMath::test_add");
    let ended = fx.wait_for_end(info.id);
    assert_eq!(ended.state, RunState::Exited(Some(0)));
    assert_eq!(
        fx.output_of(info.id).trim(),
        "python -m pytest tests/test_math.py::TestMath::test_add"
    );

    // The menu lists the marker's tasks, and each under the debugger once
    // one is installed.
    let doc = fx.doc("tests/test_math.py");
    let class = row(TESTS, "class TestMath");
    let debugged: Rc<RefCell<Vec<Spawn>>> = Rc::default();
    fx.visual.update(|_, cx| {
        let marks = super::test_marks(cx);
        let (tasks, context, _) = gutter::marker_tasks(&marks, &doc, class, cx).unwrap();
        assert_eq!(context.row, Some(class as u32 + 1));
        assert_eq!(context.symbol.as_deref(), Some("TestMath"));
        assert_eq!(
            actions(&gutter::marker_menu(&tasks, cx)),
            [("run:0".into(), "pytest TestMath".into(), false)]
        );
        let sink = debugged.clone();
        launch::set_debugger(
            Rc::new(move |spawn, _, _, _| sink.borrow_mut().push(spawn)),
            cx,
        );
        assert_eq!(
            actions(&gutter::marker_menu(&tasks, cx)),
            [
                ("run:0".into(), "pytest TestMath".into(), false),
                ("debug:0".into(), "Debug pytest TestMath".into(), false)
            ]
        );
        // The main guard runs the file, and as a module where it is one.
        let main = row(TESTS, "if __name__");
        let (tasks, _, _) = gutter::marker_tasks(&marks, &doc, main, cx).unwrap();
        assert_eq!(tasks[0].command, "$PYTHON tests/test_math.py");
        // Debug resolves the same way and hands the process over.
        let (tasks, context, checkout) = gutter::marker_tasks(&marks, &doc, class, cx).unwrap();
        start::debug(tasks[0].clone(), context, checkout, cx);
    });
    fx.wait("the debugger to be handed the test", |_| {
        !debugged.borrow().is_empty()
    });
    assert_eq!(
        debugged.borrow()[0].args,
        ["-m", "pytest", "tests/test_math.py::TestMath"]
    );
    assert_eq!(fx.runs().len(), 1, "a debug start is not a run");

    // A file the test runners do not collect only has its main guard.
    fx.open("app.py");
    let app = fx.path("app.py");
    fx.wait("the other file's marker", |cx| {
        !hub.read_with(cx, |hub, _| {
            hub.gutter_marks(&app, GutterSlot::Run).is_empty()
        })
    });
    let marks: Vec<String> = hub.read_with(&fx.visual, |hub, _| {
        (hub.gutter_marks(&app, GutterSlot::Run).iter())
            .map(|mark| mark.tooltip.to_string())
            .collect()
    });
    assert_eq!(marks, ["Run app.py"]);
}

#[gpui::test]
fn debug_chosen_in_a_markers_menu_starts_without_the_editor_being_read_twice(
    cx: &mut TestAppContext,
) {
    // The menu's click runs inside an update of the file's editor; a start
    // that reads that editor at once (to save it) used to panic there.
    let mut fx = Fixture::new(&[("tests/test_math.py", TESTS)], cx);
    fx.open("tests/test_math.py");
    let path = fx.path("tests/test_math.py");
    let hub = fx.visual.update(|_, cx| code_editor::hub(cx));
    fx.wait("the markers", |cx| {
        !hub.read_with(cx, |hub, _| {
            hub.gutter_marks(&path, GutterSlot::Run).is_empty()
        })
    });
    let debugged: Rc<RefCell<Vec<Spawn>>> = Rc::default();
    fx.visual.update(|_, cx| {
        let sink = debugged.clone();
        launch::set_debugger(
            Rc::new(move |spawn, _, _, _| sink.borrow_mut().push(spawn)),
            cx,
        );
    });
    fx.visual.run_until_parked();
    let add = row(TESTS, "def test_add");
    let marker = fx.visual.update(|_, cx| {
        let editor = context::focused_file(cx).unwrap().editor.upgrade().unwrap();
        (editor.read(cx).input.read(cx)).gutter_slot_center(GutterSlot::Run, add)
    });
    let marker = marker.expect("the run column is drawn");
    fx.visual
        .simulate_mouse_down(marker, gpui::MouseButton::Right, Modifiers::default());
    fx.visual
        .simulate_mouse_up(marker, gpui::MouseButton::Right, Modifiers::default());
    fx.visual.run_until_parked();
    let row = fx
        .visual
        .debug_bounds("menu-action-debug:0")
        .expect("the menu offers Debug");
    fx.visual.simulate_click(row.center(), Modifiers::default());
    fx.wait("the debugger to be handed the test", |_| {
        !debugged.borrow().is_empty()
    });
}

#[gpui::test]
fn the_header_offers_what_can_run_in_the_file_and_its_button_runs_the_default(
    cx: &mut TestAppContext,
) {
    let files = [
        ("pkg/__init__.py", ""),
        ("pkg/tool.py", "print('tool')\n"),
        ("script.py", "print('script')\n"),
        ("notes.txt", "nothing to run\n"),
    ];
    let mut fx = Fixture::new(&files, cx);
    let root = fx.root.clone();
    fx.open("pkg/tool.py");
    let (inside, outside, notes) = (
        fx.doc("pkg/tool.py"),
        fx.doc("script.py"),
        fx.doc("notes.txt"),
    );
    // The buttons the header is given for a file, by id.
    let buttons = |fx: &mut Fixture, doc: &DocumentInfo| -> Vec<String> {
        fx.visual.update(|_, cx| {
            (super::test_header(cx).actions(doc, cx).iter())
                .map(|action| action.id.to_string())
                .collect()
        })
    };
    assert_eq!(buttons(&mut fx, &inside), ["run-file"], "no debugger yet");
    fx.visual.update(|_, cx| {
        assert_eq!(
            actions(&header::menu(&inside, None, cx)),
            [
                (
                    "run:builtin:python:file:pkg/tool.py".into(),
                    "Run tool.py".into(),
                    true
                ),
                (
                    "run:builtin:python:module:pkg.tool".into(),
                    "Run module pkg.tool".into(),
                    false
                ),
                ("edit".into(), "Edit configurations\u{2026}".into(), false),
            ]
        );
        assert_eq!(
            actions(&header::menu(&outside, None, cx)),
            [
                (
                    "run:builtin:python:file:script.py".into(),
                    "Run script.py".into(),
                    true
                ),
                ("edit".into(), "Edit configurations\u{2026}".into(), false),
            ]
        );
        assert_eq!(actions(&header::menu(&notes, None, cx)).len(), 1);
    });

    // A file of no language has no button until the project has something
    // of its own to run; a configuration that needs a selection is left out.
    fx.open("notes.txt");
    assert!(buttons(&mut fx, &notes).is_empty());
    let project = fx.project.clone();
    let serve = fx.feature(|window, cx| {
        let mut runs = settings::runs(&project, cx);
        let serve = runs.add(user("Serve", "sh -c 'echo serving $FILE_NAME'"));
        runs.add(user("Grep", "rg $SELECTED_TEXT"));
        settings::save_runs(&project, runs, window, cx);
        serve
    });
    fx.visual.run_until_parked();
    assert_eq!(buttons(&mut fx, &notes), ["run-file"]);
    fx.visual.update(|_, cx| {
        assert_eq!(
            actions(&header::menu(&notes, None, cx)),
            [
                (format!("run:{serve}"), "Serve".into(), true),
                ("edit".into(), "Edit configurations\u{2026}".into(), false),
            ]
        );
        let listed = actions(&header::menu(&inside, None, cx));
        assert_eq!(listed[0].1, "Serve");
        assert!(
            !listed[0].2 && listed[1].2,
            "the file's own task is the default"
        );
    });

    // The button runs the default: here the project's only configuration.
    let click = HeaderClick {
        doc: Rc::new(notes.clone()),
        position: gpui::point(gpui::px(0.), gpui::px(0.)),
        offset: 0,
        row: 0,
    };
    fx.feature(|window, cx| {
        let run = super::test_header(cx).actions(&notes, cx).remove(0);
        (run.on_click)(&click, window, cx)
    });
    fx.wait("the configuration to run", |_| !fx_runs(&root).is_empty());
    let info = fx.runs().remove(0);
    assert_eq!(info.label, "Serve");
    fx.wait_for_end(info.id);
    assert_eq!(fx.output_of(info.id).trim(), "serving notes.txt");

    // Having been run, it is what the button repeats in a Python file too.
    fx.visual.update(|_, cx| {
        let listed = actions(&header::menu(&inside, None, cx));
        assert!(listed[0].2 && !listed[1].2, "{listed:?}");
    });
    // With a debugger installed the Debug button joins it.
    fx.visual.update(|_, cx| {
        launch::set_debugger(Rc::new(|_, _, _, _| {}), cx);
        code_shell::refresh(cx);
    });
    fx.open("script.py");
    assert_eq!(buttons(&mut fx, &outside), ["run-file", "debug-file"]);

    // The key runs the focused file's default as the button does.
    fx.visual.simulate_keystrokes(header::RUN_KEY);
    fx.wait("the key's run", |_| fx_runs(&root).len() == 2);
    assert_eq!(fx.runs()[1].label, "Serve");
}

#[gpui::test]
fn the_configurations_page_edits_the_projects_list_in_the_workspace(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[("hello.py", "print('hi')\n")], cx);
    let project = fx.project.clone();
    let saved = |fx: &mut Fixture| {
        let project = fx.project.clone();
        fx.window
            .update(&mut fx.visual, |shell, _, _| {
                shell.workspace.project_runs.get(&project).cloned()
            })
            .unwrap()
    };
    // The page is registered, and opens in the project dialog.
    let opened = project.clone();
    assert!(fx.feature(move |window, cx| settings::open(&opened, PAGE, window, cx)));
    fx.visual.run_until_parked();
    assert!(fx.visual.debug_bounds("run-configurations-page").is_some());

    let page = {
        let project = project.clone();
        fx.feature(move |_, cx| cx.new(|cx| RunsPage::new(project, None, cx)))
    };
    assert!(page.read_with(&fx.visual, |page, _| page.runs().is_empty()));

    // A new configuration is not saved until it has a command.
    let edit =
        |fx: &mut Fixture,
         f: Box<dyn FnOnce(&mut RunsPage, &mut Window, &mut gpui::Context<RunsPage>)>| {
            let page = page.clone();
            fx.feature(move |window, cx| page.update(cx, |page, cx| f(page, window, cx)));
            fx.visual.run_until_parked();
        };
    edit(&mut fx, Box::new(|page, window, cx| page.add(window, cx)));
    assert_eq!(saved(&mut fx), None);
    edit(
        &mut fx,
        Box::new(|page, window, cx| {
            assert_eq!(page.selected().unwrap().name, "New configuration");
            page.set_field(Field::Name, "Echo", cx);
            page.set_field(Field::Command, "echo $FILE_NAME", cx);
            page.set_field(Field::Arguments, "--loud", cx);
            page.set_field(Field::Folder, "sub", cx);
            page.set_field(Field::Environment, "MODE=dev", cx);
            // Without a focused file the preview says what is missing.
            assert_eq!(page.preview(cx), Some(Err("No file is focused.".into())));
            page.save(window, cx);
        }),
    );
    let runs = saved(&mut fx).expect("the project has an entry now");
    assert_eq!(runs.configs.len(), 1);
    let echo = runs.configs[0].clone();
    assert_eq!(
        (
            echo.name.as_str(),
            echo.command.as_str(),
            echo.args.as_str()
        ),
        ("Echo", "echo $FILE_NAME", "--loud")
    );
    assert_eq!((echo.cwd.as_str(), echo.env.as_str()), ("sub", "MODE=dev"));
    fx.visual.update(|_, cx| {
        assert_eq!(settings::runs(&project, cx).configs, [echo.clone()]);
    });

    // With a file focused the preview is the command line it comes to.
    fx.open("hello.py");
    page.read_with(&fx.visual, |page, cx| {
        assert_eq!(page.preview(cx), Some(Ok("echo hello.py --loud".into())));
    });

    // Duplicate, move, remove and the test runner are saved as they happen.
    edit(
        &mut fx,
        Box::new(|page, window, cx| page.duplicate(window, cx)),
    );
    let names = |runs: &monocode_core::code::run::ProjectRuns| -> Vec<String> {
        runs.configs
            .iter()
            .map(|config| config.name.clone())
            .collect()
    };
    assert_eq!(names(&saved(&mut fx).unwrap()), ["Echo", "Echo copy"]);
    edit(
        &mut fx,
        Box::new(|page, window, cx| page.shift(false, window, cx)),
    );
    assert_eq!(names(&saved(&mut fx).unwrap()), ["Echo copy", "Echo"]);
    edit(
        &mut fx,
        Box::new(|page, window, cx| {
            page.remove(window, cx);
            assert_eq!(page.selected().unwrap().name, "Echo");
            page.set_runner(TestRunner::Unittest, window, cx);
        }),
    );
    let runs = saved(&mut fx).unwrap();
    assert_eq!(names(&runs), ["Echo"]);
    assert_eq!(runs.configs[0].id, echo.id, "ids survive the edits");
    assert_eq!(runs.test_runner, TestRunner::Unittest);

    // What is saved is what a restart restores, and what the agents' tools
    // are answered with.
    let json = (fx.window)
        .update(&mut fx.visual, |shell, _, _| shell.workspace.saved_json())
        .unwrap();
    let restored = Workspace::restore(Some(&json), &[], &project);
    assert_eq!(restored.project_runs[&project], runs);

    // Removing the last one and going back to pytest leaves no entry.
    edit(
        &mut fx,
        Box::new(|page, window, cx| {
            page.remove(window, cx);
            page.set_runner(TestRunner::Pytest, window, cx);
        }),
    );
    assert_eq!(saved(&mut fx), None);
}

#[gpui::test]
fn the_chip_counts_what_is_running_and_lists_agents_runs_with_their_output(
    cx: &mut TestAppContext,
) {
    let mut fx = Fixture::new(&[], cx);
    let root = fx.root.clone();
    let chip_state = |fx: &mut Fixture| {
        fx.visual.update(|_, cx| {
            let chip = chips::find(chip::CHIP, cx).expect("the chip is registered");
            ((chip.state)(cx), (chip.items)(cx))
        })
    };
    let (state, items) = chip_state(&mut fx);
    assert!(state.is_none() && items.is_empty());
    assert!(fx.visual.debug_bounds(chip::CHIP).is_none());

    // An agent starts a server through its tools: no tab, only the registry.
    let toolchain = Toolchain::for_folder(&TerminalSetup::default(), &fx.root, &fx.root);
    let spawn = Spawn {
        label: "dev server".into(),
        command: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "echo Local: http://localhost:5173/; sleep 30".into(),
        ],
        cwd: fx.root.clone(),
        env: Vec::new(),
    };
    let id = (services().runs())
        .start_headless(spawn, &fx.root, StartedBy::Agent(fx.session), &toolchain)
        .unwrap();
    fx.wait("the address to be seen", |_| {
        (services().runs().get(id)).is_some_and(|info| info.url.is_some())
    });
    fx.visual.run_until_parked();
    let (state, items) = chip_state(&mut fx);
    let state = state.expect("something is running");
    assert_eq!(state.text, "1 running");
    assert!(
        fx.visual.debug_bounds(chip::CHIP).is_some(),
        "the chip is drawn"
    );
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "dev server");
    let detail = items[0].detail.clone().unwrap();
    assert!(detail.starts_with("Running \u{00b7} "), "{detail}");
    assert!(detail.ends_with("New Claude Code session"), "{detail}");
    let verbs: Vec<String> = (items[0].actions.iter())
        .map(|(id, label)| format!("{} = {label}", id.split(':').next().unwrap()))
        .collect();
    assert_eq!(
        verbs,
        [
            "show = Show output",
            "open = Open http://localhost:5173/ in the browser",
            "stop = Stop",
            "rerun = Run again",
        ]
    );
    let pid = services().runs().get(id).unwrap().pid.unwrap();
    assert!(code_shell::processes::owns(pid), "listed as running work");

    // Its row opens a tab that follows the registry's output.
    let info = services().runs().get(id).unwrap();
    fx.feature(|window, cx| chip::pick(&format!("show:{}", id.0), window, cx));
    fx.visual.run_until_parked();
    assert!(fx.visual.update(|_, cx| output::open_view(&info, cx)));
    assert_eq!(fx.tabs(), 1);
    assert!(fx.visual.debug_bounds("run-output").is_some());
    let view = fx.feature(|_, cx| output::view(id, cx));
    assert_eq!(
        view.read_with(&fx.visual, |view, _| view.text()),
        "Local: http://localhost:5173/\n"
    );

    // Stopped from the list, the chip stays, quiet, so that the row stays
    // reachable.
    fx.feature(|window, cx| chip::pick(&format!("stop:{}", id.0), window, cx));
    assert_eq!(fx.wait_for_end(id).state, RunState::Stopped);
    fx.visual.run_until_parked();
    let (state, items) = chip_state(&mut fx);
    let state = state.expect("a finished run keeps the chip");
    assert_eq!(state.text, "1 run");
    assert_eq!(state.tone, chips::Tone::Quiet);
    assert!(fx.visual.debug_bounds(chip::CHIP).is_some());
    assert!(items[0].detail.clone().unwrap().starts_with("Stopped"));
    let last = items[0].actions.last().unwrap().1.clone();
    assert_eq!(last, "Remove from this list");

    // Run again from the list: now the user's, in a tab of its own.
    fx.feature(|window, cx| chip::pick(&format!("rerun:{}", id.0), window, cx));
    fx.wait("the rerun", |_| fx_runs(&root).len() == 2);
    let again = fx.runs()[1].clone();
    assert_eq!(again.started_by, StartedBy::User);
    assert!(!again.headless);
    fx.wait("the rerun's address", |_| {
        (services().runs().get(again.id)).is_some_and(|info| info.url.is_some())
    });
    fx.visual.run_until_parked();
    let (state, items) = chip_state(&mut fx);
    assert_eq!(state.unwrap().text, "1 running");
    assert_eq!(items.len(), 2);
    assert!(items[0].detail.clone().unwrap().ends_with("you"));
    assert_eq!(items[0].actions[0].1, "Show its tab");
    services().runs().stop(again.id);
    fx.wait_for_end(again.id);

    fx.feature(|window, cx| chip::pick(&format!("forget:{}", id.0), window, cx));
    assert!(services().runs().get(id).is_none());
}

#[gpui::test]
fn quick_open_runs_what_is_offered_reruns_the_last_and_stops_what_runs(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[("hello.py", "print('hi')\n")], cx);
    let root = fx.root.clone();
    let project = fx.project.clone();
    fx.feature(|window, cx| {
        let mut runs = settings::runs(&project, cx);
        runs.add(user("Serve", "sh -c 'echo up; sleep 30'"));
        runs.add(user("Current file", "echo $FILE_NAME"));
        settings::save_runs(&project, runs, window, cx);
    });
    let scope = Scope {
        project: fx.project.clone(),
        folder: fx.project.clone(),
    };
    let titles = |fx: &mut Fixture| -> Vec<(String, String)> {
        fx.visual.update(|_, cx| {
            (commands::commands(&scope, cx).iter())
                .map(|command| (command.title.to_string(), command.subtitle.to_string()))
                // The debug feature adds its own commands beside these.
                .filter(|(title, _)| title.starts_with("Run:"))
                .collect()
        })
    };
    let pick = |fx: &mut Fixture, title: &str| {
        let command = fx.visual.update(|_, cx| {
            (commands::commands(&scope, cx).into_iter())
                .find(|command| command.title == title)
                .unwrap_or_else(|| panic!("no command {title}"))
        });
        fx.feature(|window, cx| (command.run)(window, cx));
    };
    let pair = |title: &str, subtitle: &str| (title.to_owned(), subtitle.to_owned());
    // Without a focused file only what needs none is offered.
    assert_eq!(
        titles(&mut fx),
        [
            pair("Run: Serve", "Run configuration"),
            pair("Run: Edit configurations", "Project settings"),
        ]
    );
    fx.open("hello.py");
    assert_eq!(
        titles(&mut fx),
        [
            pair("Run: Serve", "Run configuration"),
            pair("Run: Current file", "Run configuration"),
            pair("Run: Run hello.py", "This file"),
            pair("Run: Edit configurations", "Project settings"),
        ]
    );

    pick(&mut fx, "Run: Current file");
    fx.wait("the picked run", |_| fx_runs(&root).len() == 1);
    let first = fx.runs()[0].id;
    fx.wait_for_end(first);
    assert_eq!(fx.output_of(first).trim(), "hello.py");
    let rerun = pair("Run: Rerun last", &header::shown_key(header::RERUN_KEY));
    assert!(titles(&mut fx).contains(&rerun));
    pick(&mut fx, "Run: Rerun last");
    fx.wait("the rerun", |_| fx_runs(&root).len() == 2);
    let second = fx.runs()[1].id;
    fx.wait_for_end(second);
    assert_eq!(fx.tabs(), 1, "the rerun reused the tab");

    pick(&mut fx, "Run: Serve");
    fx.wait("the server", |_| fx_runs(&root).len() == 3);
    let server = fx.runs()[2].id;
    assert!(titles(&mut fx).contains(&pair("Run: Stop Serve", "Running")));
    pick(&mut fx, "Run: Stop Serve");
    assert_eq!(fx.wait_for_end(server).state, RunState::Stopped);
    assert!(
        !titles(&mut fx)
            .iter()
            .any(|(title, _)| title.starts_with("Run: Stop"))
    );

    pick(&mut fx, "Run: Edit configurations");
    fx.visual.run_until_parked();
    assert!(fx.visual.debug_bounds("run-configurations-page").is_some());
}

#[gpui::test]
fn an_agents_run_gets_a_tab_that_says_who_started_it(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    let (checkout, session) = (fx.checkout(), fx.session);
    let outcome = fx.visual.update(|_, cx| {
        let context = targets::bare_context(&checkout);
        start::start_for_agent(
            session,
            user("Check", "echo checked"),
            context,
            checkout,
            cx,
        )
    });
    let id = fx.outcome(outcome).unwrap();
    let info = fx.wait_for_end(id);
    assert_eq!(info.started_by, StartedBy::Agent(session));
    assert!(!info.headless);
    assert_eq!(fx.output_of(id).trim(), "checked");
    assert_eq!(fx.tabs(), 1);
    fx.visual.run_until_parked();
    assert!(fx.visual.debug_bounds("dock-run-agent").is_some());
    fx.visual.update(|_, cx| {
        // An agent's run is not what the user's run button repeats.
        assert!(settings::runs(&fx.project, cx).last_used().is_none());
        assert!(!start::has_last(cx));
    });
}

#[gpui::test]
fn a_real_pytest_test_runs_from_its_marker(cx: &mut TestAppContext) {
    let Ok(python) = std::env::var("MONOCODE_TEST_PYTHON") else {
        return;
    };
    let venv = Path::new(&python).parent().and_then(Path::parent).unwrap();
    let tests =
        "def test_ok():\n    assert 1 + 1 == 2\n\n\ndef test_bad():\n    assert 1 + 1 == 3\n";
    // The project's terminal setup names the real environment.
    let setup = TerminalSetup {
        venv: Venv::Path(venv.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let mut fx = Fixture::with(&[("test_sums.py", tests)], Some(setup), cx);
    let root = fx.root.clone();
    fx.open("test_sums.py");
    let path = fx.path("test_sums.py");
    let hub = fx.visual.update(|_, cx| code_editor::hub(cx));
    fx.wait("the markers", |cx| {
        hub.read_with(cx, |hub, _| {
            hub.gutter_marks(&path, GutterSlot::Run).len() == 2
        })
    });
    fx.visual.run_until_parked();
    let click = |fx: &mut Fixture, row: usize| {
        let marker = fx.visual.update(|_, cx| {
            let editor = context::focused_file(cx).unwrap().editor.upgrade().unwrap();
            (editor.read(cx).input.read(cx)).gutter_slot_center(GutterSlot::Run, row)
        });
        fx.visual
            .simulate_click(marker.unwrap(), Modifiers::default());
    };
    click(&mut fx, row(tests, "def test_ok"));
    fx.wait("the passing test to start", |_| fx_runs(&root).len() == 1);
    let passing = fx.runs()[0].clone();
    assert_eq!(Path::new(&passing.spawn.command), Path::new(&python));
    let ended = fx.wait_for_end(passing.id);
    let output = fx.output_of(passing.id);
    assert_eq!(ended.state, RunState::Exited(Some(0)), "{output}");
    assert!(output.contains("1 passed"), "{output}");

    click(&mut fx, row(tests, "def test_bad"));
    fx.wait("the failing test to start", |_| fx_runs(&root).len() == 2);
    let failing = fx.runs()[1].id;
    let ended = fx.wait_for_end(failing);
    let output = fx.output_of(failing);
    assert_eq!(ended.state, RunState::Exited(Some(1)), "{output}");
    assert!(output.contains("1 failed"), "{output}");
    assert!(output.contains("assert 1 + 1 == 3"), "{output}");
    assert_eq!(fx.tabs(), 2, "each test has its own tab");
}
