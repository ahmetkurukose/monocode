//! Run and Debug from the code editor: the file, or the test or entry
//! point at the caret, through the project's run and debug support. What
//! runs is a built-in task of the file's language, started the way the
//! header's buttons and the gutter's markers start theirs.
use super::{DebugAtCaret, DebugFile, FileEditor, RunAtCaret, RunFile};
use crate::{
    code_editor::{
        self,
        symbols::{self, Symbol, SymbolKind},
    },
    code_shell::{
        context::{self, Checkout},
        launch, settings,
    },
    popover::{MenuAction, MenuItem},
};
use gpui::{App, Context, Window};
use monocode_core::code::run::{self, RunConfig, RunContext, Runnable, RunnableKind, TestRunner};
use std::path::Path;

/// What a Run or Debug row starts.
#[derive(Clone, Debug)]
struct Target {
    /// What the row calls it: Run '<label>'.
    label: String,
    config: RunConfig,
    context: RunContext,
    checkout: Checkout,
}

/// A shortcut as this platform writes it.
fn keys(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        mac
    } else {
        other
    }
}

/// The checkout a file belongs to: the one the shell knows, else the folder
/// its editor was opened from.
fn checkout_of(path: &Path, root: &Path, cx: &App) -> Option<Checkout> {
    context::checkout_for_path(&path.to_string_lossy(), cx).or_else(|| {
        let root = root.to_string_lossy();
        (!root.is_empty()).then(|| Checkout {
            project: root.to_string(),
            folder: root.into_owned(),
        })
    })
}

/// The file's name when the task runs that file, else the task's own name
/// (`cargo run`, `go test .`).
fn file_label(config: &RunConfig, path: &Path) -> String {
    let label = config.label();
    match path.file_name().map(|name| name.to_string_lossy()) {
        Some(name) if label.contains(name.as_ref()) => name.into_owned(),
        _ => label,
    }
}

/// Whether the attributes right above `start` mark a Rust test:
/// `#[test]`, `#[tokio::test]`, `#[gpui::test]` and the like.
fn rust_test_at(text: &str, start: usize) -> bool {
    let Some(before) = text.get(..start) else {
        return false;
    };
    (before.trim_end().lines().rev())
        .map(str::trim)
        .take_while(|line| line.starts_with("#[") || line.starts_with("//"))
        .filter_map(|line| line.strip_prefix("#["))
        .any(|attribute| {
            let path = attribute.split(['(', ']']).next().unwrap_or("");
            path.trim().ends_with("test")
        })
}

impl FileEditor {
    /// What the file's Run row starts, and what the caret's does when the
    /// caret is in a test of its own. Both `None` when the file cannot run.
    fn run_targets(&self, cx: &App) -> (Option<Target>, Option<Target>) {
        let doc = &self.extensions.doc;
        let Some(checkout) = checkout_of(&doc.path, &doc.root, cx) else {
            return (None, None);
        };
        let runner = settings::runs(&checkout.project, cx).test_runner;
        let base = RunContext::new(&checkout.folder, &checkout.project).with_file(&doc.path);
        let Some(config) = run::builtin_tasks(&base, runner).into_iter().next() else {
            return (None, None);
        };
        let file = Target {
            label: file_label(&config, &doc.path),
            config,
            context: base.clone(),
            checkout: checkout.clone(),
        };
        let caret =
            self.caret_task(base, runner, &file.config.id, cx)
                .map(|(label, config, context)| Target {
                    label,
                    config,
                    context,
                    checkout,
                });
        (Some(file), caret)
    }

    /// The test around the caret as a task other than the file's own.
    fn caret_task(
        &self,
        mut context: RunContext,
        runner: TestRunner,
        file_task: &str,
        cx: &App,
    ) -> Option<(String, RunConfig, RunContext)> {
        let input = self.input.read(cx);
        let (text, offset) = (input.content.as_ref(), input.caret());
        let known = self.outline();
        let parsed;
        let outline: &[Symbol] = if known.is_empty() {
            parsed = symbols::outline(&self.path, text);
            &parsed
        } else {
            &known
        };
        let chain = symbols::chain_at(outline, offset);
        let (line, column) = code_editor::position_at(text, offset);
        context.row = Some(line as u32 + 1);
        context.column = Some(column as u32 + 1);
        let function =
            |symbol: &&Symbol| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method);
        match self.extensions.doc.language? {
            "python" => {
                // As `$SYMBOL` names it: the classes, then the first function.
                let mut names = Vec::new();
                for symbol in &chain {
                    names.push(symbol.name.as_str());
                    if function(symbol) {
                        break;
                    }
                }
                // The innermost test: a helper inside a test class runs the class.
                (1..=names.len()).rev().find_map(|len| {
                    let symbol = names[..len].join("::");
                    let mut context = context.clone();
                    context.symbol = Some(symbol.clone());
                    let task = run::builtin_tasks(&context, runner).into_iter().next()?;
                    (task.id != file_task).then_some((symbol, task, context))
                })
            }
            language @ ("rust" | "go") => {
                let at = chain.iter().rposition(function)?;
                let found = chain[at];
                let kind = match language {
                    "rust" if rust_test_at(text, found.range.start) => RunnableKind::RustTest,
                    "go" if self.path.ends_with("_test.go")
                        && found.depth == 0
                        && found.name.starts_with("Test") =>
                    {
                        RunnableKind::GoTest
                    }
                    _ => return None,
                };
                let modules: Vec<&str> = (chain[..at].iter())
                    .filter(|symbol| symbol.kind == SymbolKind::Module)
                    .map(|symbol| symbol.name.as_str())
                    .collect();
                let runnable = Runnable {
                    kind,
                    name: found.name.clone(),
                    class_name: (!modules.is_empty()).then(|| modules.join("::")),
                    row: found.row as u32,
                };
                context.symbol = Some(found.name.clone());
                let task = run::runnable_tasks(&runnable, &context, runner)
                    .into_iter()
                    .next()?;
                (task.id != file_task).then(|| (found.name.clone(), task, context))
            }
            _ => None,
        }
    }

    /// Whether Debug can start this file: a debugger is installed, and it
    /// takes Python only.
    fn debuggable(&self, cx: &App) -> bool {
        launch::can_debug(cx) && self.extensions.doc.language == Some("python")
    }

    /// The code menu's Run and Debug rows; empty when the file cannot run.
    /// The caret's keys go on the caret's rows, or on the file's when the
    /// caret is in nothing of its own, since they then run the file.
    pub(super) fn run_menu_items(&self, cx: &App) -> Vec<MenuItem> {
        let (Some(file), caret) = self.run_targets(cx) else {
            return Vec::new();
        };
        let debug = self.debuggable(cx);
        let row = |id: &'static str,
                   verb: &str,
                   icon: &'static str,
                   label: &str,
                   key: Option<&'static str>|
         -> MenuItem {
            let action = MenuAction::new(id, format!("{verb} '{label}'")).icon(icon);
            match key {
                Some(key) => action.shortcut(key),
                None => action,
            }
            .into()
        };
        let (run_key, debug_key) = (keys("⌃⇧R", "Ctrl+Shift+R"), keys("⌃⇧D", "Ctrl+Shift+D"));
        let file_keys = caret.is_none();
        let mut items: Vec<MenuItem> = vec![row(
            "run:file",
            "Run",
            "play",
            &file.label,
            file_keys.then_some(run_key),
        )];
        if debug {
            items.push(row(
                "debug:file",
                "Debug",
                "bug",
                &file.label,
                file_keys.then_some(debug_key),
            ));
        }
        if let Some(caret) = caret {
            items.push(row("run:caret", "Run", "play", &caret.label, Some(run_key)));
            if debug {
                items.push(row(
                    "debug:caret",
                    "Debug",
                    "bug",
                    &caret.label,
                    Some(debug_key),
                ));
            }
        }
        items
    }

    /// Takes a picked row of `run_menu_items`; `false` when it is not one.
    pub(super) fn run_menu_pick(
        &mut self,
        id: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (debug, at_caret) = match id {
            "run:file" => (false, false),
            "debug:file" => (true, false),
            "run:caret" => (false, true),
            "debug:caret" => (true, true),
            _ => return false,
        };
        self.start_run(debug, at_caret, cx);
        true
    }

    /// Starts the file's task, or the caret's (the file's when the caret is
    /// in nothing of its own), as a run or under the debugger.
    fn start_run(&mut self, debug: bool, at_caret: bool, cx: &mut Context<Self>) {
        let (file, caret) = self.run_targets(cx);
        let target = match at_caret {
            true => caret.or(file),
            false => file,
        };
        let Some(Target {
            config,
            context,
            checkout,
            ..
        }) = target
        else {
            self.show_notice("Nothing is set up to run this file.".into(), cx);
            return;
        };
        if !debug {
            crate::run_ui::start(config, context, checkout, cx);
        } else if !launch::can_debug(cx) {
            self.show_notice("No debugger is installed.".into(), cx);
        } else if !self.debuggable(cx) {
            self.show_notice("Only Python programs can be debugged for now.".into(), cx);
        } else {
            crate::run_ui::debug(config, context, checkout, cx);
        }
    }

    pub(super) fn run_file(&mut self, _: &RunFile, _: &mut Window, cx: &mut Context<Self>) {
        self.start_run(false, false, cx);
    }
    pub(super) fn debug_file(&mut self, _: &DebugFile, _: &mut Window, cx: &mut Context<Self>) {
        self.start_run(true, false, cx);
    }
    pub(super) fn run_at_caret(&mut self, _: &RunAtCaret, _: &mut Window, cx: &mut Context<Self>) {
        self.start_run(false, true, cx);
    }
    pub(super) fn debug_at_caret(
        &mut self,
        _: &DebugAtCaret,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_run(true, true, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{code_shell, shell::Shell, workspace::Workspace};
    use gpui::{AppContext, Entity, TestAppContext, VisualTestContext, WindowHandle};
    use monocode_core::{
        code::{run::RunInfo, services::services, spawn::Spawn},
        store::Store,
    };
    use std::{
        cell::RefCell,
        path::PathBuf,
        rc::Rc,
        sync::{Arc, Mutex},
        time::Duration,
    };

    const TESTS: &str = "\
def test_top():
    assert True


class TestMath:
    def test_add(self):
        assert 1 + 1 == 2

    def helper(self):
        pass


def helper():
    pass
";

    /// A project with a stand-in interpreter (a script that says what it
    /// was asked to run), the shell showing it, and its files.
    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        window: WindowHandle<Shell>,
        visual: VisualTestContext,
    }

    impl Fixture {
        fn new(files: &[(&str, &str)], cx: &mut TestAppContext) -> Self {
            cx.executor().allow_parking();
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            for (name, text) in files {
                let path = root.join(name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }
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
            let project = root.to_string_lossy().into_owned();
            let store = Store::open(Path::new(":memory:")).unwrap();
            let session = store.create("claude", &project).unwrap();
            let mut state = Workspace {
                new_layout_boxy: false,
                ..Default::default()
            };
            state.add_project(&project);
            state.open(session.id);
            let saved = serde_json::to_string(&state).unwrap();
            let store = Arc::new(Mutex::new(store));
            let window = cx.update(|cx| {
                code_shell::register(cx);
                launch::clear_debugger(cx);
                cx.open_window(Default::default(), |_, cx| {
                    cx.new(|cx| Shell::new(store, vec![session], project.clone(), Some(saved), cx))
                })
                .unwrap()
            });
            cx.update(|cx| cx.set_reduce_motion(true));
            window
                .update(cx, |shell, _, _| shell.terminals_in_dock = true)
                .unwrap();
            cx.refresh().unwrap();
            let visual = VisualTestContext::from_window(window.into(), cx);
            visual.run_until_parked();
            Self {
                _dir: dir,
                root,
                window,
                visual,
            }
        }

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

        /// Opens a file of the project and returns its editor once it shows.
        fn open(&mut self, name: &str) -> Entity<FileEditor> {
            let path = self.root.join(name).to_string_lossy().into_owned();
            let opened = path.clone();
            let window = self.window.into();
            let ok = (self.visual)
                .update_window(window, |_, window, cx| {
                    context::open_file(&opened, None, window, cx)
                })
                .unwrap();
            assert!(ok);
            let mut editor = None;
            self.wait("the file to open", |cx| {
                editor = cx.update(|_, cx| {
                    let file = context::focused_file(cx).filter(|file| file.path == path)?;
                    let editor = file.editor.upgrade()?;
                    (!editor.read(cx).input.read(cx).content.is_empty()).then_some(editor)
                });
                editor.is_some()
            });
            editor.unwrap()
        }

        /// Puts the caret on the line of `needle`, past its indentation.
        fn caret_at(&mut self, editor: &Entity<FileEditor>, needle: &str) {
            self.visual.update(|_, cx| {
                let input = editor.read(cx).input.clone();
                input.update(cx, |input, cx| {
                    let at = input.content.find(needle).unwrap() + 4;
                    input.select_range(at..at, cx);
                });
            });
        }

        /// The Run and Debug rows as `(id, label, shortcut)`.
        fn rows(&mut self, editor: &Entity<FileEditor>) -> Vec<(String, String, Option<String>)> {
            self.visual.update(|_, cx| {
                (editor.read(cx).run_menu_items(cx).iter())
                    .filter_map(|item| match item {
                        MenuItem::Action(action) => Some((
                            action.id.to_string(),
                            action.label.to_string(),
                            action.shortcut.as_ref().map(ToString::to_string),
                        )),
                        _ => None,
                    })
                    .collect()
            })
        }

        fn pick(&mut self, editor: &Entity<FileEditor>, id: &str) -> bool {
            editor.update_in(&mut self.visual, |editor, window, cx| {
                editor.run_menu_pick(id, window, cx)
            })
        }

        fn runs(&self) -> Vec<RunInfo> {
            let mut runs: Vec<RunInfo> = (services().runs().list().into_iter())
                .filter(|info| info.checkout == self.root)
                .collect();
            runs.sort_by_key(|info| info.id);
            runs
        }

        /// Waits for the project's `n`th run and returns its arguments.
        fn run_args(&mut self, n: usize) -> Vec<String> {
            let root = self.root.clone();
            self.wait("the run to start", |_| {
                (services().runs().list().iter())
                    .filter(|info| info.checkout == root)
                    .count()
                    > n
            });
            self.runs().remove(n).spawn.args
        }
    }

    fn row(id: &str, label: &str, key: Option<&str>) -> (String, String, Option<String>) {
        (id.into(), label.into(), key.map(str::to_owned))
    }

    #[gpui::test]
    fn the_rows_offer_the_file_and_the_test_at_the_caret(cx: &mut TestAppContext) {
        let mut fx = Fixture::new(
            &[
                ("tests/test_math.py", TESTS),
                ("app.py", "print('hi')\n"),
                ("notes.txt", "nothing to run\n"),
            ],
            cx,
        );
        let (run, debug) = (keys("⌃⇧R", "Ctrl+Shift+R"), keys("⌃⇧D", "Ctrl+Shift+D"));
        let editor = fx.open("tests/test_math.py");
        fx.caret_at(&editor, "assert 1 + 1");
        assert_eq!(
            fx.rows(&editor),
            [
                row("run:file", "Run 'test_math.py'", None),
                row("run:caret", "Run 'TestMath::test_add'", Some(run)),
            ]
        );
        // A helper inside a test class runs the class.
        fx.caret_at(&editor, "    pass\n\n\ndef helper");
        assert_eq!(
            fx.rows(&editor)[1],
            row("run:caret", "Run 'TestMath'", Some(run))
        );
        // Outside any test the keys run the file, so its rows show them.
        let debugged: Rc<RefCell<Vec<Spawn>>> = Rc::default();
        let sink = debugged.clone();
        fx.visual.update(|_, cx| {
            launch::set_debugger(
                Rc::new(move |spawn, _, _, _| sink.borrow_mut().push(spawn)),
                cx,
            )
        });
        fx.caret_at(&editor, "def helper():\n    pass");
        assert_eq!(
            fx.rows(&editor),
            [
                row("run:file", "Run 'test_math.py'", Some(run)),
                row("debug:file", "Debug 'test_math.py'", Some(debug)),
            ]
        );
        fx.caret_at(&editor, "def test_top");
        assert_eq!(
            fx.rows(&editor),
            [
                row("run:file", "Run 'test_math.py'", None),
                row("debug:file", "Debug 'test_math.py'", None),
                row("run:caret", "Run 'test_top'", Some(run)),
                row("debug:caret", "Debug 'test_top'", Some(debug)),
            ]
        );
        // A plain script has only itself; a text file has nothing.
        let app = fx.open("app.py");
        assert_eq!(
            fx.rows(&app),
            [
                row("run:file", "Run 'app.py'", Some(run)),
                row("debug:file", "Debug 'app.py'", Some(debug)),
            ]
        );
        let notes = fx.open("notes.txt");
        assert!(fx.rows(&notes).is_empty());
        assert!(debugged.borrow().is_empty());
    }

    #[gpui::test]
    fn rust_tests_at_the_caret_run_alone_and_cannot_be_debugged(cx: &mut TestAppContext) {
        let lib = "\
fn helper() {}

#[cfg(test)]
mod tests {
    #[test]
    fn adds() {
        assert_eq!(1 + 1, 2);
    }
}
";
        let mut fx = Fixture::new(&[("src/lib.rs", lib)], cx);
        fx.visual
            .update(|_, cx| launch::set_debugger(Rc::new(|_, _, _, _| {}), cx));
        let editor = fx.open("src/lib.rs");
        fx.caret_at(&editor, "assert_eq");
        let rows = fx.rows(&editor);
        let labels: Vec<&str> = rows.iter().map(|(_, label, _)| label.as_str()).collect();
        assert_eq!(labels, ["Run 'cargo run'", "Run 'adds'"]);
        fx.caret_at(&editor, "fn helper");
        assert_eq!(fx.rows(&editor).len(), 1);
        fx.caret_at(&editor, "assert_eq");
        editor.update_in(&mut fx.visual, |editor, window, cx| {
            editor.debug_at_caret(&DebugAtCaret, window, cx)
        });
        fx.visual.update(|_, cx| {
            assert_eq!(
                editor.read(cx).error_text(),
                Some("Only Python programs can be debugged for now.")
            );
            let (_, caret) = editor.read(cx).run_targets(cx);
            let command = caret.unwrap().config.command;
            assert!(command.starts_with("cargo test ") && command.contains("tests::adds"));
        });
    }

    #[gpui::test]
    fn each_action_starts_its_run_or_debug_session(cx: &mut TestAppContext) {
        let mut fx = Fixture::new(
            &[("tests/test_math.py", TESTS), ("notes.txt", "text\n")],
            cx,
        );
        let editor = fx.open("tests/test_math.py");
        fx.caret_at(&editor, "assert 1 + 1");

        // Run at the caret runs that one test, in a run tab.
        editor.update_in(&mut fx.visual, |editor, window, cx| {
            editor.run_at_caret(&RunAtCaret, window, cx)
        });
        assert_eq!(
            fx.run_args(0),
            ["-m", "pytest", "tests/test_math.py::TestMath::test_add"]
        );
        // Run file runs all of the file's tests.
        editor.update_in(&mut fx.visual, |editor, window, cx| {
            editor.run_file(&RunFile, window, cx)
        });
        assert_eq!(fx.run_args(1), ["-m", "pytest", "tests/test_math.py"]);
        // The menu's rows do what the keys do.
        assert!(fx.pick(&editor, "run:caret"));
        assert_eq!(
            fx.run_args(2),
            ["-m", "pytest", "tests/test_math.py::TestMath::test_add"]
        );
        assert!(!fx.pick(&editor, "copy"));

        // Debug without a debugger says so; with one, the process goes to it.
        editor.update_in(&mut fx.visual, |editor, window, cx| {
            editor.debug_file(&DebugFile, window, cx)
        });
        fx.visual.update(|_, cx| {
            assert_eq!(
                editor.read(cx).error_text(),
                Some("No debugger is installed.")
            );
        });
        let debugged: Rc<RefCell<Vec<Spawn>>> = Rc::default();
        let sink = debugged.clone();
        fx.visual.update(|_, cx| {
            launch::set_debugger(
                Rc::new(move |spawn, _, _, _| sink.borrow_mut().push(spawn)),
                cx,
            )
        });
        editor.update_in(&mut fx.visual, |editor, window, cx| {
            editor.debug_at_caret(&DebugAtCaret, window, cx)
        });
        fx.wait("the debugger to get the test", |_| {
            debugged.borrow().len() == 1
        });
        assert_eq!(
            debugged.borrow()[0].args,
            ["-m", "pytest", "tests/test_math.py::TestMath::test_add"]
        );
        assert!(fx.pick(&editor, "debug:file"));
        fx.wait("the debugger to get the file", |_| {
            debugged.borrow().len() == 2
        });
        assert_eq!(
            debugged.borrow()[1].args,
            ["-m", "pytest", "tests/test_math.py"]
        );
        assert_eq!(fx.runs().len(), 3, "debugging opens no run tab");

        // A file with nothing to run says so.
        let notes = fx.open("notes.txt");
        notes.update_in(&mut fx.visual, |editor, window, cx| {
            editor.run_at_caret(&RunAtCaret, window, cx)
        });
        fx.visual.update(|_, cx| {
            assert_eq!(
                notes.read(cx).error_text(),
                Some("Nothing is set up to run this file.")
            );
        });
    }
}
