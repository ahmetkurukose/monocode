//! Code insight against scripted language servers, in a file pane: what the
//! servers are told, how their diagnostics land on the text, and what the
//! editor's completion, hover, signature and usages show of their answers.
//! The harness here is also what the tests through a whole shell
//! (`shell_tests.rs`) are built on.
use super::{outline, settings, usages};
use crate::{
    code_editor::{self, Severity},
    code_shell::panel_tabs,
    file_editor::{self, FileEditor},
    git_status::ProjectGit,
    input,
};
use gpui::{
    App, AppContext, Bounds, Entity, EntityInputHandler, Focusable, TestAppContext,
    VisualTestContext, WindowBounds, WindowHandle, WindowOptions, point, px, size,
};
use monocode_core::code::lsp::{
    self, Manager, ManagerOptions, Registry,
    client::{Launch, path_to_uri},
    fake::{FakeLauncher, FakeServer},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

/// What the scripted basedpyright announces.
fn insight() -> Value {
    json!({
        "textDocumentSync": 2,
        "hoverProvider": true,
        "definitionProvider": true,
        "referencesProvider": true,
        "documentSymbolProvider": true,
        "completionProvider": { "triggerCharacters": ["."], "resolveProvider": true },
        "signatureHelpProvider": { "triggerCharacters": ["(", ","] },
    })
}

/// A scripted server for a launch: basedpyright answers questions and
/// pushes its diagnostics, ruff is asked for its own and always has one
/// about the first line's import.
pub(crate) fn server_for(launch: &Launch) -> FakeServer {
    if !launch.program.ends_with("ruff") {
        return FakeServer::new(insight());
    }
    let ruff = FakeServer::new(json!({
        "textDocumentSync": { "openClose": true, "change": 1 },
        "diagnosticProvider": { "identifier": "Ruff", "interFileDependencies": false },
    }));
    ruff.on("textDocument/diagnostic", |message, out| {
        out.result(
            message,
            json!({ "kind": "full", "items": [{
                "range": range((0, 7), (0, 9)), "severity": 2, "code": "F401",
                "source": "Ruff", "message": "`os` imported but unused",
            }] }),
        );
    });
    ruff
}

pub(crate) fn range(start: (u32, u32), end: (u32, u32)) -> Value {
    json!({
        "start": { "line": start.0, "character": start.1 },
        "end": { "line": end.0, "character": end.1 },
    })
}

/// The protocol range of `needle` in `text`, in UTF-16 columns.
pub(crate) fn range_of(text: &str, needle: &str) -> Value {
    let at = text.find(needle).expect("the needle is in the text");
    let (line, start) = code_editor::position_at(text, at);
    let (_, end) = code_editor::position_at(text, at + needle.len());
    range((line as u32, start as u32), (line as u32, end as u32))
}

pub(crate) fn uri(path: &Path) -> String {
    path_to_uri(path)
}

/// A checkout on disk whose virtual environment holds the two Python
/// tools, and the feature installed over a manager that starts scripted
/// servers instead of processes.
pub(crate) struct Rig {
    pub(crate) manager: Manager,
    pub(crate) launcher: Arc<FakeLauncher>,
    /// The checkout, as the editors spell it.
    pub(crate) root: PathBuf,
    _folder: tempfile::TempDir,
}

impl Rig {
    pub(crate) fn new(cx: &mut TestAppContext) -> Self {
        Self::with(|launch| Ok(server_for(launch)), cx)
    }

    pub(crate) fn with(
        factory: impl Fn(&Launch) -> std::io::Result<FakeServer> + Send + Sync + 'static,
        cx: &mut TestAppContext,
    ) -> Self {
        // The servers answer from their own threads.
        cx.executor().allow_parking();
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("repo");
        for file in [
            ".venv/bin/activate",
            ".venv/bin/basedpyright-langserver",
            ".venv/bin/ruff",
            "pyproject.toml",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let launcher = FakeLauncher::new(factory);
        let manager = Manager::with_parts(
            ManagerOptions::default(),
            Registry::builtin(),
            launcher.clone(),
            Arc::new(Instant::now),
        );
        cx.update(|cx| {
            input::bind(cx);
            file_editor::bind(cx);
            crate::popover::bind(cx);
            super::init_with(manager.clone(), cx);
        });
        Self {
            manager,
            launcher,
            root,
            _folder: folder,
        }
    }

    pub(crate) fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    pub(crate) fn pyright(&self) -> FakeServer {
        self.launcher.named("basedpyright-langserver")
    }

    pub(crate) fn state_of(&self, server: &str) -> Option<lsp::ServerState> {
        (self.manager.statuses().into_iter())
            .find(|status| status.server == server)
            .map(|status| status.state)
    }
}

/// Lets real and editor time pass until `done`: the servers answer from
/// their threads, and the editor waits on its own timers before asking.
pub(crate) fn wait(
    cx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    wait_for(cx, Duration::from_secs(10), what, &mut done);
}

pub(crate) fn wait_for(
    cx: &mut VisualTestContext,
    patience: Duration,
    what: &str,
    done: &mut dyn FnMut(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + patience;
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(3));
        cx.executor().advance_clock(Duration::from_millis(25));
    }
}

/// Whether the document is open with the manager on the editor's behalf.
pub(crate) fn synced(path: &Path, cx: &App) -> bool {
    super::sync(cx).is_some_and(|sync| sync.read(cx).is_open(path))
}

/// The diagnostics in the hub as (source, severity, marked text, message).
pub(crate) fn diagnostics(path: &Path, cx: &mut App) -> Vec<(String, Severity, String, String)> {
    let hub = code_editor::hub(cx);
    let hub = hub.read(cx);
    let text = hub
        .document(path)
        .map(|open| open.text.clone())
        .unwrap_or_default();
    (hub.diagnostics(path, None).into_iter())
        .map(|item| {
            (
                item.source.to_string(),
                item.severity,
                text.get(item.range.clone()).unwrap_or("?").to_owned(),
                item.message.to_string(),
            )
        })
        .collect()
}

/// The Usages tab's view, when a result is shown there.
pub(crate) fn usages_shown(cx: &App) -> Option<(String, usize, Vec<String>)> {
    Some(usages::shown(cx)?.read(cx).listed())
}

/// The Outline tab's rows, each marked when the caret is in it.
pub(crate) fn outline_listed(cx: &App) -> Vec<(String, bool)> {
    outline::shown(cx).map_or_else(Vec::new, |view| view.read(cx).listed())
}

pub(crate) fn outline_filter(text: &str, cx: &mut App) {
    if let Some(view) = outline::shown(cx) {
        view.update(cx, |view, cx| view.filter_by(text, cx));
    }
}

pub(crate) fn outline_jump(name: &str, window: &mut gpui::Window, cx: &mut App) {
    if let Some(view) = outline::shown(cx) {
        view.update(cx, |view, cx| view.jump_to_named(name, window, cx));
    }
}

/// Changes the project's settings through the page the project menu
/// opened last, as its controls do.
fn with_settings_page(
    window: &mut gpui::Window,
    cx: &mut App,
    change: impl FnOnce(&mut settings::Page, &mut gpui::Window, &mut gpui::Context<settings::Page>),
) {
    let page = settings::last_page(cx).expect("the settings page was opened");
    page.update(cx, |page, cx| change(page, window, cx));
}

pub(crate) fn set_tool_path(server: &str, path: &str, window: &mut gpui::Window, cx: &mut App) {
    with_settings_page(window, cx, |page, window, cx| {
        page.set_tool_path(server, path, window, cx)
    });
}

pub(crate) fn set_language(server: &str, on: bool, window: &mut gpui::Window, cx: &mut App) {
    with_settings_page(window, cx, |page, window, cx| {
        page.set_language(server, on, window, cx)
    });
}

pub(crate) fn set_disabled(off: bool, window: &mut gpui::Window, cx: &mut App) {
    with_settings_page(window, cx, |page, window, cx| {
        page.set_disabled(off, window, cx)
    });
}

/// Opens the row of the Usages tab that lists `line` of the file `name`.
pub(crate) fn usages_open(name: &str, line: usize, window: &mut gpui::Window, cx: &mut App) {
    let view = usages::shown(cx).expect("the usages tab is shown");
    view.update(cx, |view, cx| {
        let wanted = format!("{name}:{line}: ");
        let at = (view.listed().2.iter())
            .position(|listed| listed.starts_with(&wanted))
            .expect("the usage is listed");
        let row = view.usage_rows()[at];
        view.open(row, window, cx);
    });
}

pub(crate) const SETTINGS_PAGE: &str = settings::PAGE;
pub(crate) const CHIP: &str = super::chip::CHIP;
pub(crate) const USAGES_TAB: &str = usages::TAB;
pub(crate) const OUTLINE_TAB: &str = outline::TAB;

fn open(path: &Path, root: &Path, cx: &mut TestAppContext) -> WindowHandle<FileEditor> {
    let file = path.to_string_lossy().into_owned();
    let root = root.to_string_lossy().into_owned();
    cx.update(|cx| {
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
                window.focus(&view.read(cx).input.focus_handle(cx), cx);
                view
            },
        )
        .unwrap()
    })
}

fn input_of(window: WindowHandle<FileEditor>, cx: &mut VisualTestContext) -> Entity<input::Input> {
    window.update(cx, |view, _, _| view.input.clone()).unwrap()
}

/// Puts the caret at `offset` and types `text` there.
fn type_at(
    window: WindowHandle<FileEditor>,
    offset: usize,
    text: &str,
    cx: &mut VisualTestContext,
) {
    window
        .update(cx, |view, window, cx| {
            view.input.update(cx, |input, cx| {
                input.select_range(offset..offset, cx);
                input.replace_text_in_range(None, text, window, cx);
            })
        })
        .unwrap();
}

#[gpui::test]
fn diagnostics_land_per_server_on_the_text_they_are_about_and_go_with_the_file(
    cx: &mut TestAppContext,
) {
    let rig = Rig::new(cx);
    let text = "import os\nlabel = 'é🙂'; value = missing\n";
    let path = rig.write("app.py", text);
    let window = open(&path, &rig.root, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    wait(cx, "the document to be opened with the manager", |cx| {
        cx.update(|_, cx| synced(&path, cx))
    });
    let pyright = rig.pyright();
    let opened = pyright.wait_for("textDocument/didOpen");
    assert_eq!(opened["params"]["textDocument"]["text"], text);
    assert_eq!(opened["params"]["textDocument"]["uri"], uri(&path));
    // The interpreter the servers are given is the checkout's own.
    let launched = rig.launcher.launched.lock().unwrap()[0].0.clone();
    assert!(launched.program.starts_with(rig.root.join(".venv/bin")));

    pyright.notify(
        "textDocument/publishDiagnostics",
        json!({ "uri": uri(&path), "version": 1, "diagnostics": [{
            "range": range_of(text, "missing"), "severity": 1,
            "code": "reportUndefinedVariable", "source": "basedpyright",
            "message": "\"missing\" is not defined",
        }] }),
    );
    wait(cx, "both servers' diagnostics", |cx| {
        cx.update(|_, cx| diagnostics(&path, cx).len() == 2)
    });
    // Each under its own server, on the bytes the server meant in UTF-16.
    assert_eq!(
        cx.update(|_, cx| diagnostics(&path, cx)),
        [
            (
                "ruff".to_owned(),
                Severity::Warning,
                "os".to_owned(),
                "`os` imported but unused (F401)".to_owned()
            ),
            (
                "basedpyright".to_owned(),
                Severity::Error,
                "missing".to_owned(),
                "\"missing\" is not defined (reportUndefinedVariable)".to_owned()
            ),
        ]
    );
    let shown = |cx: &mut VisualTestContext| -> Vec<String> {
        window
            .update(cx, |view, _, cx| {
                let input = view.input.read(cx);
                (input.external_diagnostics().iter())
                    .map(|item| input.content[item.range.clone()].to_owned())
                    .collect()
            })
            .unwrap()
    };
    assert_eq!(shown(cx), ["os", "missing"]);

    // Typing moves them at once, before any server has heard of it.
    type_at(window, 0, "# header\n", cx);
    cx.run_until_parked();
    assert_eq!(shown(cx), ["os", "missing"]);
    let changed = "# header\nimport os\nlabel = 'é🙂'; value = missing\n";
    wait(cx, "the change to reach the server", |_| {
        pyright.count("textDocument/didChange") == 1
    });
    let told = pyright.wait_for("textDocument/didChange");
    assert_eq!(told["params"]["contentChanges"][0]["text"], changed);
    // A late report on the text before the edit is not laid over this one;
    // the report on the new text is.
    pyright.notify(
        "textDocument/publishDiagnostics",
        json!({ "uri": uri(&path), "version": 1, "diagnostics": [{
            "range": range((0, 0), (0, 6)), "severity": 1, "message": "stale",
        }] }),
    );
    pyright.notify(
        "textDocument/publishDiagnostics",
        json!({ "uri": uri(&path), "version": 2, "diagnostics": [{
            "range": range_of(changed, "label"), "severity": 2, "message": "unused",
        }] }),
    );
    wait(cx, "the report on the new text", |cx| {
        cx.update(|_, cx| {
            diagnostics(&path, cx)
                .iter()
                .any(|(source, _, marked, _)| source == "basedpyright" && marked == "label")
        })
    });
    let now = cx.update(|_, cx| diagnostics(&path, cx));
    assert!(!now.iter().any(|(.., message)| message == "stale"));
    assert!(shown(cx).contains(&"label".to_owned()));
    assert!(!shown(cx).contains(&"missing".to_owned()));

    // Closing the pane closes the document and clears what it showed.
    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    wait(cx, "the document to be closed", |_| {
        !rig.manager.is_open(&path)
    });
    // The server hears of it on its own thread, a moment after the manager
    // forgets the document.
    pyright.wait_for("textDocument/didClose");
    assert_eq!(pyright.count("textDocument/didClose"), 1);
    cx.cx
        .update(|cx| assert!(diagnostics(&path, cx).is_empty()));
}

#[gpui::test]
fn completion_applies_its_import_resolves_documentation_and_stays_up_while_typing(
    cx: &mut TestAppContext,
) {
    let rig = Rig::new(cx);
    let text = "p = Pa\n";
    let path = rig.write("app.py", text);
    let window = open(&path, &rig.root, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    wait(cx, "the document to be opened with the manager", |cx| {
        cx.update(|_, cx| synced(&path, cx))
    });
    let pyright = rig.pyright();
    pyright.wait_for("textDocument/didOpen");
    pyright.on("textDocument/completion", |message, out| {
        let position = &message["params"]["position"];
        let typed = range((0, 4), (0, position["character"].as_u64().unwrap() as u32));
        out.result(
            message,
            json!({ "isIncomplete": false, "items": [
                { "label": "partition", "kind": 2, "sortText": "03" },
                { "label": "Parser", "kind": 7, "sortText": "02" },
                {
                    "label": "Path", "kind": 7, "detail": "pathlib", "sortText": "01",
                    "textEdit": { "range": typed, "newText": "Path" },
                    "additionalTextEdits": [{
                        "range": range((0, 0), (0, 0)),
                        "newText": "from pathlib import Path\n",
                    }],
                    "data": 7,
                },
                { "label": "zip", "kind": 3 },
            ] }),
        );
    });
    pyright.on("completionItem/resolve", |message, out| {
        let mut item = message["params"].clone();
        item["documentation"] = json!({ "kind": "markdown", "value": "Filesystem paths." });
        out.result(message, item);
    });
    let input = input_of(window, cx);
    let labels =
        |cx: &mut VisualTestContext| input.read_with(cx, |input, _| input.completion_labels());
    let caret = text.find("Pa").unwrap() + 2;
    input.update(cx, |input, cx| {
        input.select_range(caret..caret, cx);
        input.schedule_completion(true, cx);
    });
    wait(cx, "the server's completions", |cx| !labels(cx).is_empty());
    // Narrowed to what is typed, in the server's order.
    assert_eq!(labels(cx), ["Path", "Parser", "partition"]);
    let asked = pyright.wait_for("textDocument/completion");
    assert_eq!(
        asked["params"]["position"],
        json!({ "line": 0, "character": 6 })
    );
    assert_eq!(asked["params"]["context"]["triggerKind"], 1);
    // The documentation is asked for when the item is selected.
    wait(cx, "the documentation", |cx| {
        input.read_with(cx, |input, _| {
            input
                .completion_documentation_text()
                .is_some_and(|text| text.contains("Filesystem paths."))
        })
    });
    assert_eq!(
        pyright.wait_for("completionItem/resolve")["params"]["data"],
        7
    );

    // One more letter: the list stays, narrowed, while the server is asked
    // again, instead of closing until it answers.
    type_at(window, caret, "t", cx);
    cx.run_until_parked();
    assert_eq!(pyright.count("textDocument/completion"), 1);
    assert_eq!(labels(cx), ["Path", "partition"]);
    wait(cx, "the second answer", |_| {
        pyright.count("textDocument/completion") == 2
    });
    wait(cx, "the list for the new text", |cx| {
        labels(cx) == ["Path", "partition"]
    });
    // Accepting the item adds its import with it.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        input.read_with(cx, |input, _| input.content.to_string()),
        "from pathlib import Path\np = Path\n"
    );
}

#[gpui::test]
fn hover_signature_help_and_usages_show_what_the_server_answers(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let text = "def add(a: int, b: int) -> int:\n    return a + b\n\ntotal = add(1\n";
    let path = rig.write("app.py", text);
    let other_text = "from app import add\nadd(1, 2)\nadd(3, 4); add(5, 6)\n";
    let other = rig.write("other.py", other_text);
    let window = open(&path, &rig.root, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    wait(cx, "the document to be opened with the manager", |cx| {
        cx.update(|_, cx| synced(&path, cx))
    });
    let pyright = rig.pyright();
    pyright.wait_for("textDocument/didOpen");
    wait(cx, "the server to be ready", |_| {
        rig.state_of("basedpyright") == Some(lsp::ServerState::Ready)
    });
    let input = input_of(window, cx);

    // Hover, asked for at the caret.
    let total = range_of(text, "total");
    pyright.on("textDocument/hover", move |message, out| {
        out.result(
            message,
            json!({
                "contents": { "kind": "markdown", "value": "```python\n(variable) total: int\n```" },
                "range": total,
            }),
        );
    });
    let at = text.find("total").unwrap() + 2;
    input.update(cx, |input, cx| input.select_range(at..at, cx));
    cx.simulate_keystrokes("f1");
    wait(cx, "the hover", |cx| {
        input.read_with(cx, |input, _| input.hover_text().is_some())
    });
    let (hovered, shown) = input.read_with(cx, |input, _| input.hover_text().unwrap());
    assert_eq!(&text[hovered], "total");
    assert!(shown.contains("(variable) total: int"), "{shown}");
    let asked = pyright.wait_for("textDocument/hover");
    assert_eq!(
        asked["params"]["position"],
        json!({ "line": 3, "character": 2 })
    );

    // Signature help on the server's trigger character.
    pyright.on("textDocument/signatureHelp", |message, out| {
        out.result(
            message,
            json!({
                "signatures": [{
                    "label": "add(a: int, b: int) -> int",
                    "parameters": [{ "label": [4, 10] }, { "label": [12, 18] }],
                    "activeParameter": 1,
                }],
                "activeSignature": 0,
            }),
        );
    });
    let end = text.find("add(1").unwrap() + "add(1".len();
    type_at(window, end, ",", cx);
    wait(cx, "the signature", |cx| {
        input.read_with(cx, |input, _| input.signature_shown().is_some())
    });
    let signature = input.read_with(cx, |input, _| input.signature_shown().cloned().unwrap());
    assert_eq!(signature.active, Some(1));
    assert_eq!(&signature.label[signature.parameters[1].clone()], "b: int");
    cx.simulate_keystrokes("escape");

    // A handful of usages stays in the editor's own list.
    let text = input.read_with(cx, |input, _| input.content.to_string());
    let few = json!([
        { "uri": uri(&path), "range": range_of(&text, "add") },
        { "uri": uri(&other), "range": range((1, 0), (1, 3)) },
    ]);
    pyright.on("textDocument/references", move |message, out| {
        out.result(message, few.clone())
    });
    let at = text.find("add").unwrap();
    input.update(cx, |input, cx| input.select_range(at..at, cx));
    cx.simulate_keystrokes("shift-f12");
    wait(cx, "the editor's list", |cx| {
        input.read_with(cx, |input, _| input.picker_targets().is_some())
    });
    cx.update(|_, cx| {
        assert!(usages_shown(cx).is_none());
        assert!(!panel_tabs::is_shown(USAGES_TAB, cx));
    });
    cx.simulate_keystrokes("escape");

    // More than that opens in the Usages tab, grouped by file.
    let mut many = vec![
        json!({ "uri": uri(&path), "range": range_of(&text, "add") }),
        json!({ "uri": uri(&path), "range": range((3, 8), (3, 11)) }),
        json!({ "uri": uri(&other), "range": range((0, 16), (0, 19)) }),
        json!({ "uri": uri(&other), "range": range((1, 0), (1, 3)) }),
        json!({ "uri": uri(&other), "range": range((2, 0), (2, 3)) }),
        json!({ "uri": uri(&other), "range": range((2, 11), (2, 14)) }),
    ];
    for line in 3..5 {
        many.push(json!({ "uri": uri(&other), "range": range((line, 0), (line, 3)) }));
    }
    std::fs::write(&other, format!("{other_text}add(7, 8)\nadd(9, 0)\n")).unwrap();
    pyright.on("textDocument/references", move |message, out| {
        out.result(message, json!(many))
    });
    cx.simulate_keystrokes("shift-f12");
    wait(cx, "the usages tab", |cx| {
        cx.update(|_, cx| usages_shown(cx).is_some())
    });
    let (symbol, count, lines) = cx.update(|_, cx| usages_shown(cx).unwrap());
    assert_eq!((symbol.as_str(), count), ("add", 8));
    assert_eq!(
        lines,
        [
            "app.py:1: def add(a: int, b: int) -> int:",
            "app.py:4: total = add(1,",
            "other.py:1: from app import add",
            "other.py:2: add(1, 2)",
            "other.py:3: add(3, 4); add(5, 6)",
            "other.py:3: add(3, 4); add(5, 6)",
            "other.py:4: add(7, 8)",
            "other.py:5: add(9, 0)",
        ]
    );
    assert!(input.read_with(cx, |input, _| input.picker_targets().is_none()));
    cx.update(|_, cx| assert!(panel_tabs::is_shown(USAGES_TAB, cx)));

    // The tab draws, and opening a row is a jump Back can return from.
    let view = cx.update(|_, cx| usages::shown(cx).unwrap());
    let tab = cx
        .cx
        .update(|cx| cx.open_window(WindowOptions::default(), |_, _| view.clone()))
        .unwrap();
    let panel = &mut VisualTestContext::from_window(tab.into(), &cx.cx);
    panel.run_until_parked();
    assert!(panel.debug_bounds("code-usages").is_some());
    tab.update(panel, |usages, window, cx| {
        let rows = usages.usage_rows();
        usages.open(rows[3], window, cx);
    })
    .unwrap();
    panel.update(|_, cx| {
        let hub = code_editor::hub(cx);
        assert!(hub.read(cx).history().can_back());
        let back = hub.update(cx, |hub, cx| hub.back(None, cx)).unwrap();
        assert_eq!((back.path.as_path(), back.line), (path.as_path(), 1));
    });
}

/// basedpyright as `server_for` scripts it, offering rename and code actions.
fn refactoring_server(launch: &Launch) -> std::io::Result<FakeServer> {
    if launch.program.ends_with("ruff") {
        return Ok(server_for(launch));
    }
    let mut caps = insight();
    caps["renameProvider"] = json!({ "prepareProvider": true });
    caps["codeActionProvider"] = json!({ "resolveProvider": true });
    Ok(FakeServer::new(caps))
}

fn editor_error(window: WindowHandle<FileEditor>, cx: &mut VisualTestContext) -> Option<String> {
    window
        .read_with(cx, |view, _| view.error_text().map(str::to_owned))
        .unwrap()
}

#[gpui::test]
fn rename_changes_the_open_buffer_in_one_undo_step_and_closed_files_on_disk(
    cx: &mut TestAppContext,
) {
    let rig = Rig::with(refactoring_server, cx);
    let text = "def add(a, b):\n    return a + b\n\ntotal = add(1, 2)\n";
    let path = rig.write("app.py", text);
    let other_text = "from app import add\nadd(3, 4)\n";
    let other = rig.write("other.py", other_text);
    let window = open(&path, &rig.root, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    wait(cx, "the server to be ready", |cx| {
        cx.update(|_, cx| synced(&path, cx))
            && rig.state_of("basedpyright") == Some(lsp::ServerState::Ready)
    });
    let pyright = rig.pyright();
    let input = input_of(window, cx);

    // Nothing to rename: the editor says so.
    pyright.on("textDocument/prepareRename", |message, out| {
        out.result(message, Value::Null)
    });
    let blank = text.find("\n\n").unwrap() + 1;
    input.update(cx, |input, cx| input.select_range(blank..blank, cx));
    cx.simulate_keystrokes("shift-f6");
    wait(cx, "the notice", |cx| editor_error(window, cx).is_some());
    assert_eq!(
        editor_error(window, cx).as_deref(),
        Some("There is nothing to rename here")
    );

    let call = range_of(&text[blank..], "add");
    let call = json!({
        "start": { "line": 3, "character": call["start"]["character"] },
        "end": { "line": 3, "character": call["end"]["character"] },
    });
    pyright.on("textDocument/prepareRename", move |message, out| {
        out.result(message, call.clone())
    });
    let (here, there) = (uri(&path), uri(&other));
    pyright.on("textDocument/rename", move |message, out| {
        let name = message["params"]["newName"].clone();
        out.result(
            message,
            json!({ "changes": {
                here.clone(): [
                    { "range": range((0, 4), (0, 7)), "newText": name },
                    { "range": range((3, 8), (3, 11)), "newText": name },
                ],
                there.clone(): [
                    { "range": range((0, 16), (0, 19)), "newText": name },
                    { "range": range((1, 0), (1, 3)), "newText": name },
                ],
            } }),
        );
    });
    let at = text.rfind("add").unwrap() + 1;
    input.update(cx, |input, cx| input.select_range(at..at, cx));
    cx.simulate_keystrokes("shift-f6");
    wait(cx, "the rename field", |cx| {
        input.read_with(cx, |input, _| input.rename_open().is_some())
    });
    let field = input.read_with(cx, |input, _| input.rename_open().unwrap());
    assert_eq!(
        field.read_with(cx, |field, _| field.content.to_string()),
        "add"
    );
    assert!(
        cx.debug_bounds("editor-rename").is_some(),
        "the field is drawn"
    );
    field.update(cx, |field, cx| field.set_text("sum".into(), cx));
    cx.simulate_keystrokes("enter");
    wait(cx, "the renamed text", |cx| {
        input.read_with(cx, |input, _| input.content.contains("def sum"))
            && std::fs::read_to_string(&other).unwrap() != other_text
    });
    let renamed = "def sum(a, b):\n    return a + b\n\ntotal = sum(1, 2)\n";
    assert_eq!(
        input.read_with(cx, |input, _| input.content.to_string()),
        renamed
    );
    assert_eq!(
        std::fs::read_to_string(&other).unwrap(),
        "from app import sum\nsum(3, 4)\n"
    );
    assert!(input.read_with(cx, |input, _| input.rename_open().is_none()));
    // The caret stays on the name it was in.
    let caret = input.read_with(cx, |input, _| input.selection_range());
    assert_eq!(&renamed[caret.start - 1..caret.start + 2], "sum");
    // The open file's part is one step of undo.
    cx.simulate_keystrokes("cmd-z");
    assert_eq!(
        input.read_with(cx, |input, _| input.content.to_string()),
        text
    );
}

#[gpui::test]
fn context_actions_list_fixes_first_apply_edits_and_resolve_the_rest(cx: &mut TestAppContext) {
    let rig = Rig::with(refactoring_server, cx);
    let text = "value = sqrt(4)\n";
    let path = rig.write("app.py", text);
    let window = open(&path, &rig.root, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    wait(cx, "the server to be ready", |cx| {
        cx.update(|_, cx| synced(&path, cx))
            && rig.state_of("basedpyright") == Some(lsp::ServerState::Ready)
    });
    let pyright = rig.pyright();
    let input = input_of(window, cx);
    let here = uri(&path);
    pyright.on("textDocument/codeAction", move |message, out| {
        out.result(
            message,
            json!([
                { "title": "Extract variable", "kind": "refactor.extract", "data": 1 },
                { "title": "Run the linter", "command": "lint.run" },
                { "title": "Import sqrt from math", "kind": "quickfix", "isPreferred": true,
                  "edit": { "changes": { here.clone(): [
                      { "range": range((0, 0), (0, 0)), "newText": "from math import sqrt\n" },
                  ] } } },
            ]),
        );
    });
    let at = text.find("sqrt").unwrap();
    input.update(cx, |input, cx| input.select_range(at..at, cx));
    cx.simulate_keystrokes("alt-enter");
    wait(cx, "the actions", |cx| {
        input.read_with(cx, |input, _| input.actions_open())
    });
    let asked = pyright.wait_for("textDocument/codeAction");
    assert_eq!(
        asked["params"]["range"],
        json!({ "start": { "line": 0, "character": 8 }, "end": { "line": 0, "character": 8 } })
    );
    // The fix leads; the action that only runs a command comes last.
    let fix = cx.debug_bounds("menu-action-0").expect("the fix is listed");
    assert!(cx.debug_bounds("menu-action-2").is_some());
    cx.simulate_click(fix.center(), gpui::Modifiers::default());
    wait(cx, "the fix", |cx| {
        input.read_with(cx, |input, _| input.content.starts_with("from math"))
    });
    assert!(!input.read_with(cx, |input, _| input.actions_open()));

    // An action without its edit is resolved first.
    let text = input.read_with(cx, |input, _| input.content.to_string());
    let here = uri(&path);
    pyright.on("codeAction/resolve", move |message, out| {
        let mut action = message["params"].clone();
        action["edit"] = json!({ "changes": { here.clone(): [
            { "range": range((1, 8), (1, 15)), "newText": "root" },
        ] } });
        out.result(message, action);
    });
    let at = text.find("sqrt(4)").unwrap();
    input.update(cx, |input, cx| input.select_range(at..at + 7, cx));
    cx.simulate_keystrokes("alt-enter");
    wait(cx, "the actions", |cx| {
        input.read_with(cx, |input, _| input.actions_open())
    });
    let extract = cx.debug_bounds("menu-action-1").expect("the refactoring");
    cx.simulate_click(extract.center(), gpui::Modifiers::default());
    wait(cx, "the resolved edit", |cx| {
        input.read_with(cx, |input, _| input.content.contains("value = root"))
    });
    assert_eq!(
        pyright.wait_for("codeAction/resolve")["params"]["data"],
        json!(1)
    );

    // A command runs on the server, which asks for its edit while it does.
    let here = uri(&path);
    pyright.on("workspace/executeCommand", move |message, out| {
        out.send(
            &json!({ "jsonrpc": "2.0", "id": 77, "method": "workspace/applyEdit",
            "params": { "edit": { "changes": { here.clone(): [
                { "range": range((0, 0), (0, 0)), "newText": "# linted\n" } ] } } } }),
        );
        out.result(message, Value::Null);
    });
    cx.simulate_keystrokes("alt-enter");
    wait(cx, "the actions", |cx| {
        input.read_with(cx, |input, _| input.actions_open())
    });
    let command = cx.debug_bounds("menu-action-2").expect("the command");
    cx.simulate_click(command.center(), gpui::Modifiers::default());
    wait(cx, "the command's edit", |cx| {
        input.read_with(cx, |input, _| input.content.starts_with("# linted\n"))
    });
    assert_eq!(
        pyright.wait_for("workspace/executeCommand")["params"]["command"],
        json!("lint.run")
    );
}

#[gpui::test]
fn go_to_type_definition_jumps_implementations_list_and_missing_ones_say_so(
    cx: &mut TestAppContext,
) {
    let rig = Rig::with(
        |launch| {
            if launch.program.ends_with("ruff") {
                return Ok(server_for(launch));
            }
            let mut caps = insight();
            caps["typeDefinitionProvider"] = json!(true);
            caps["implementationProvider"] = json!(true);
            Ok(FakeServer::new(caps))
        },
        cx,
    );
    let text = "class Shape: pass\nclass Box(Shape): pass\nclass Dot(Shape): pass\nitem = Box()\n";
    let path = rig.write("app.py", text);
    let window = open(&path, &rig.root, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    wait(cx, "the server to be ready", |cx| {
        cx.update(|_, cx| synced(&path, cx))
            && rig.state_of("basedpyright") == Some(lsp::ServerState::Ready)
    });
    let pyright = rig.pyright();
    let input = input_of(window, cx);
    let here = uri(&path);
    let boxed = range_of(text, "Box");
    pyright.on("textDocument/typeDefinition", move |message, out| {
        out.result(message, json!({ "uri": here, "range": boxed }))
    });
    let here = uri(&path);
    pyright.on("textDocument/implementation", move |message, out| {
        out.result(
            message,
            json!([{ "uri": here, "range": range((1, 6), (1, 9)) },
                   { "uri": here, "range": range((2, 6), (2, 9)) }]),
        )
    });

    // One type definition: the caret goes there.
    let item = text.find("item").unwrap();
    input.update(cx, |input, cx| input.select_range(item..item, cx));
    window
        .update(cx, |_, window, cx| {
            window.dispatch_action(Box::new(input::GoToTypeDefinition), cx)
        })
        .unwrap();
    let target = text.find("Box").unwrap();
    wait(cx, "the jump", |cx| {
        input.read_with(cx, |input, _| input.selection_range() == (target..target))
    });

    // Two implementations: listed in the editor.
    let shape = text.find("Shape").unwrap();
    input.update(cx, |input, cx| input.select_range(shape..shape, cx));
    cx.simulate_keystrokes("cmd-f12");
    wait(cx, "the list", |cx| {
        input.read_with(cx, |input, _| input.picker_targets().is_some())
    });
    let lines: Vec<usize> = input.read_with(cx, |input, _| {
        input
            .picker_targets()
            .unwrap()
            .iter()
            .map(|t| t.location.line)
            .collect()
    });
    assert_eq!(lines, [2, 3]);
    cx.simulate_keystrokes("escape");

    // A server without declarations: the editor says so.
    window
        .update(cx, |_, window, cx| {
            window.dispatch_action(Box::new(input::GoToDeclaration), cx)
        })
        .unwrap();
    wait(cx, "the notice", |cx| editor_error(window, cx).is_some());
    assert_eq!(
        editor_error(window, cx).as_deref(),
        Some("The language server does not support this.")
    );
}

/// Asks for the code actions of `kind`, as the code menu's Refactor rows do.
fn actions_of_kind(window: WindowHandle<FileEditor>, kind: &str, cx: &mut VisualTestContext) {
    let action = input::CodeActionsOfKind(kind.to_string().into());
    window
        .update(cx, |_, window, cx| {
            window.dispatch_action(Box::new(action), cx)
        })
        .unwrap();
}

/// Opens `path` once its server is ready, with the server and the editor.
fn open_ready(
    rig: &Rig,
    path: &Path,
    cx: &mut TestAppContext,
) -> (
    WindowHandle<FileEditor>,
    VisualTestContext,
    FakeServer,
    Entity<input::Input>,
) {
    let window = open(path, &rig.root, cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    wait(&mut cx, "the server to be ready", |cx| {
        cx.update(|_, cx| synced(path, cx))
            && rig.state_of("basedpyright") == Some(lsp::ServerState::Ready)
    });
    let input = input_of(window, &mut cx);
    (window, cx, rig.pyright(), input)
}

#[gpui::test]
fn actions_of_a_kind_take_the_only_one_list_several_and_say_when_there_are_none(
    cx: &mut TestAppContext,
) {
    let rig = Rig::with(refactoring_server, cx);
    let text = "value = sqrt(4)\n";
    let path = rig.write("app.py", text);
    let (window, mut cx, pyright, input) = open_ready(&rig, &path, cx);
    let cx = &mut cx;
    let here = uri(&path);
    pyright.on("textDocument/codeAction", move |message, out| {
        let only = message["params"]["context"]["only"][0]
            .as_str()
            .unwrap_or("");
        let found = match only {
            // The server ignores `only` and sends an inline too; the only
            // extraction that can be taken is.
            "refactor.extract" => json!([
                { "title": "Extract variable", "kind": "refactor.extract",
                  "edit": { "changes": { here.clone(): [
                      { "range": range((0, 8), (0, 15)), "newText": "root" } ] } } },
                { "title": "Extract method", "kind": "refactor.extract.function",
                  "disabled": { "reason": "Select whole statements" } },
                { "title": "Inline", "kind": "refactor.inline" },
            ]),
            "refactor.rewrite" => json!([
                { "title": "Use f-string", "kind": "refactor.rewrite" },
                { "title": "Flip", "kind": "refactor.rewrite.flip" },
            ]),
            _ => json!([{ "title": "Fix", "kind": "quickfix" }]),
        };
        out.result(message, found);
    });
    let at = text.find("sqrt").unwrap();
    input.update(cx, |input, cx| input.select_range(at..at + 7, cx));
    actions_of_kind(window, "refactor.extract", cx);
    wait(cx, "the extraction", |cx| {
        input.read_with(cx, |input, _| input.content.as_ref() == "value = root\n")
    });
    let asked = pyright.wait_for("textDocument/codeAction");
    assert_eq!(
        asked["params"]["context"]["only"],
        json!(["refactor.extract"])
    );
    assert_eq!(
        asked["params"]["range"],
        json!({ "start": { "line": 0, "character": 8 }, "end": { "line": 0, "character": 15 } })
    );
    assert!(!input.read_with(cx, |input, _| input.actions_open()));

    // Two: listed as ⌥↩ lists them.
    actions_of_kind(window, "refactor.rewrite", cx);
    wait(cx, "the list", |cx| {
        input.read_with(cx, |input, _| input.actions_open())
    });
    assert!(cx.debug_bounds("menu-action-1").is_some());
    assert!(cx.debug_bounds("menu-action-2").is_none());
    cx.simulate_keystrokes("escape");

    // None: said.
    actions_of_kind(window, "source.organizeImports", cx);
    wait(cx, "the notice", |cx| editor_error(window, cx).is_some());
    assert_eq!(
        editor_error(window, cx).as_deref(),
        Some("No organize imports actions here")
    );
    assert!(!input.read_with(cx, |input, _| input.actions_open()));
}

#[gpui::test]
fn an_actions_command_runs_after_its_edit_on_the_text_the_edit_left(cx: &mut TestAppContext) {
    let rig = Rig::with(refactoring_server, cx);
    let text = "value = 1\n";
    let path = rig.write("app.py", text);
    let (window, mut cx, pyright, input) = open_ready(&rig, &path, cx);
    let cx = &mut cx;
    let here = uri(&path);
    pyright.on("textDocument/codeAction", move |message, out| {
        out.result(
            message,
            json!([{ "title": "Add a header and name it", "kind": "refactor.rewrite",
                     "edit": { "changes": { here.clone(): [
                         { "range": range((0, 0), (0, 0)), "newText": "ğ = 0\n" } ] } },
                     "command": { "title": "Name", "command": "name.it" } }]),
        );
    });
    let (here, server) = (uri(&path), pyright.clone());
    pyright.on("workspace/executeCommand", move |message, out| {
        // The server has the edited text by now; its edit is counted on it.
        let changes = server.received("textDocument/didChange");
        let seen = changes.last().map(|change| {
            change["params"]["contentChanges"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string()
        });
        let wanted = seen.as_deref() == Some("ğ = 0\nvalue = 1\n");
        out.send(
            &json!({ "jsonrpc": "2.0", "id": 930, "method": "workspace/applyEdit",
            "params": { "edit": { "changes": { here.clone(): [
                { "range": range((0, 1), (0, 1)),
                  "newText": if wanted { "x" } else { "stale" } } ] } } } }),
        );
        out.result(message, Value::Null);
    });
    actions_of_kind(window, "refactor.rewrite", cx);
    wait(cx, "the command's edit", |cx| {
        input.read_with(cx, |input, _| input.content.contains('x'))
            || editor_error(window, cx).is_some()
    });
    assert_eq!(editor_error(window, cx), None);
    assert_eq!(
        input.read_with(cx, |input, _| input.content.to_string()),
        "ğx = 0\nvalue = 1\n"
    );
    assert_eq!(
        pyright.wait_for("workspace/executeCommand")["params"]["command"],
        json!("name.it")
    );
}

#[gpui::test]
fn a_command_editing_a_file_in_rounds_applies_them_in_turn_as_one_undo_step(
    cx: &mut TestAppContext,
) {
    let rig = Rig::with(refactoring_server, cx);
    let text = "import sys\nimport os\n";
    let path = rig.write("app.py", text);
    let other = rig.write("other.py", "b = 2\n");
    let (window, mut cx, pyright, input) = open_ready(&rig, &path, cx);
    let cx = &mut cx;
    pyright.on("textDocument/codeAction", |message, out| {
        out.result(
            message,
            json!([{ "title": "Organize imports", "kind": "source.organizeImports",
                     "command": { "title": "Organize", "command": "organize" } }]),
        );
    });
    let (here, there) = (uri(&path), uri(&other));
    pyright.on("workspace/executeCommand", move |message, out| {
        let round = |id: i64, uri: &str, edits: Value| {
            json!({ "jsonrpc": "2.0", "id": id, "method": "workspace/applyEdit",
                    "params": { "edit": { "changes": { uri: edits } } } })
        };
        out.send(&round(
            940,
            &here,
            json!([
            { "range": range((0, 0), (2, 0)), "newText": "import os\nimport sys\n" } ]),
        ));
        // On the text the first round left.
        out.send(&round(
            941,
            &here,
            json!([
            { "range": range((2, 0), (2, 0)), "newText": "\nş = 'ğ'\n" } ]),
        ));
        out.send(&round(
            942,
            &here,
            json!([
            { "range": range((3, 5), (3, 6)), "newText": "g" } ]),
        ));
        out.send(&round(
            943,
            &there,
            json!([
            { "range": range((0, 0), (0, 1)), "newText": "c" } ]),
        ));
        out.send(&round(
            944,
            &there,
            json!([
            { "range": range((0, 1), (0, 1)), "newText": "c" } ]),
        ));
        out.result(message, Value::Null);
    });
    actions_of_kind(window, "source.organizeImports", cx);
    let organized = "import os\nimport sys\n\nş = 'g'\n";
    wait(cx, "the rounds", |cx| {
        input.read_with(cx, |input, _| input.content.as_ref() == organized)
            && std::fs::read_to_string(&other).unwrap() == "cc = 2\n"
    });
    assert_eq!(editor_error(window, cx), None);
    // All of the open file's rounds are one step of undo.
    cx.simulate_keystrokes("cmd-z");
    assert_eq!(
        input.read_with(cx, |input, _| input.content.to_string()),
        text
    );
}

#[gpui::test]
fn changes_that_create_rename_and_delete_files_are_carried_out_in_order(cx: &mut TestAppContext) {
    let rig = Rig::with(refactoring_server, cx);
    let text = "from util import helper\nhelper()\n";
    let path = rig.write("app.py", text);
    let util = rig.write("util.py", "def helper(): pass\n");
    let gone = rig.write("old/gone.py", "x = 1\n");
    let (window, mut cx, pyright, input) = open_ready(&rig, &path, cx);
    let cx = &mut cx;
    let moved = watch_moves(cx);
    let (made, tools) = (rig.root.join("pkg/new.py"), rig.root.join("tools.py"));
    let (here, util_uri, made_uri, tools_uri) = (uri(&path), uri(&util), uri(&made), uri(&tools));
    let old = uri(&rig.root.join("old"));
    pyright.on("textDocument/codeAction", move |message, out| {
        out.result(
            message,
            json!([{ "title": "Move helper", "kind": "refactor.move",
                     "edit": { "documentChanges": [
                { "kind": "create", "uri": made_uri },
                { "textDocument": { "uri": made_uri, "version": null }, "edits": [
                    { "range": range((0, 0), (0, 0)), "newText": "# ğ\n" } ] } ,
                { "textDocument": { "uri": made_uri, "version": null }, "edits": [
                    { "range": range((0, 3), (0, 3)), "newText": "!" } ] },
                { "kind": "rename", "oldUri": util_uri, "newUri": tools_uri },
                { "textDocument": { "uri": tools_uri, "version": null }, "edits": [
                    { "range": range((0, 4), (0, 10)), "newText": "tool" } ] },
                { "textDocument": { "uri": here, "version": 1 }, "edits": [
                    { "range": range((0, 5), (0, 9)), "newText": "tools" },
                    { "range": range((0, 17), (0, 23)), "newText": "tool" },
                    { "range": range((1, 0), (1, 6)), "newText": "tool" } ] },
                { "kind": "delete", "uri": old, "options": { "recursive": true } },
                { "kind": "delete", "uri": uri(&rig_missing()),
                  "options": { "ignoreIfNotExists": true } },
            ] } }]),
        );
    });
    actions_of_kind(window, "refactor.move", cx);
    wait(cx, "the files", |cx| {
        input.read_with(cx, |input, _| input.content.starts_with("from tools"))
            && !gone.exists()
            && !moved.lock().unwrap().is_empty()
    });
    assert_eq!(editor_error(window, cx), None);
    assert_eq!(
        input.read_with(cx, |input, _| input.content.to_string()),
        "from tools import tool\ntool()\n"
    );
    assert_eq!(std::fs::read_to_string(&made).unwrap(), "# ğ!\n");
    assert!(!util.exists());
    assert_eq!(
        std::fs::read_to_string(&tools).unwrap(),
        "def tool(): pass\n"
    );
    assert!(!rig.root.join("old").exists());
    assert_eq!(
        *moved.lock().unwrap(),
        [
            code_editor::refactor::FileMoved::Renamed {
                from: util.clone(),
                to: tools.clone(),
            },
            code_editor::refactor::FileMoved::Deleted(rig.root.join("old")),
        ]
    );

    // A step that cannot be taken refuses the whole change.
    let (here, tools_uri) = (uri(&path), uri(&tools));
    pyright.on("textDocument/codeAction", move |message, out| {
        out.result(
            message,
            json!([{ "title": "Clash", "kind": "refactor.move",
                     "edit": { "documentChanges": [
                { "textDocument": { "uri": here, "version": null }, "edits": [
                    { "range": range((0, 0), (0, 0)), "newText": "# no\n" } ] },
                { "kind": "create", "uri": tools_uri },
            ] } }]),
        );
    });
    actions_of_kind(window, "refactor.move", cx);
    wait(cx, "the refusal", |cx| {
        editor_error(window, cx).is_some_and(|error| error.contains("already exists"))
    });
    assert!(input.read_with(cx, |input, _| input.content.starts_with("from tools")));
    assert_eq!(
        std::fs::read_to_string(&tools).unwrap(),
        "def tool(): pass\n"
    );
}

/// What the hub is told a change moved or deleted, from now on.
fn watch_moves(
    cx: &mut VisualTestContext,
) -> Arc<std::sync::Mutex<Vec<code_editor::refactor::FileMoved>>> {
    let moved = Arc::new(std::sync::Mutex::new(Vec::new()));
    let told = moved.clone();
    cx.update(|_, cx| {
        let hub = code_editor::hub(cx);
        cx.subscribe(
            &hub,
            move |_, event: &code_editor::refactor::FileMoved, _| {
                told.lock().unwrap().push(event.clone())
            },
        )
        .detach();
    });
    moved
}

/// A path nothing is at.
fn rig_missing() -> PathBuf {
    std::env::temp_dir().join("monocode-no-such-file-for-a-code-action.py")
}

#[gpui::test]
fn renaming_an_open_file_keeps_its_edits_in_the_editor_and_tells_the_hub(cx: &mut TestAppContext) {
    let rig = Rig::with(refactoring_server, cx);
    let text = "a = 1\n";
    let path = rig.write("app.py", text);
    let (window, mut cx, pyright, input) = open_ready(&rig, &path, cx);
    let cx = &mut cx;
    let moved = watch_moves(cx);
    let main = rig.root.join("main.py");
    let (here, there) = (uri(&path), uri(&main));
    pyright.on("textDocument/codeAction", move |message, out| {
        out.result(
            message,
            json!([{ "title": "Rename module", "kind": "refactor.rewrite",
                     "edit": { "documentChanges": [
                { "textDocument": { "uri": here, "version": 1 }, "edits": [
                    { "range": range((0, 0), (0, 1)), "newText": "b" } ] },
                { "kind": "rename", "oldUri": here, "newUri": there },
                // Meant for the same file at its new name.
                { "textDocument": { "uri": there, "version": null }, "edits": [
                    { "range": range((0, 4), (0, 5)), "newText": "2" } ] },
            ] } }]),
        );
    });
    actions_of_kind(window, "refactor.rewrite", cx);
    wait(cx, "the move", |_| !moved.lock().unwrap().is_empty());
    assert_eq!(editor_error(window, cx), None);
    // Both edits are in the buffer, which is the file's; the disk moved.
    assert_eq!(
        input.read_with(cx, |input, _| input.content.to_string()),
        "b = 2\n"
    );
    assert!(!path.exists());
    assert_eq!(std::fs::read_to_string(&main).unwrap(), text);
    assert_eq!(
        *moved.lock().unwrap(),
        [code_editor::refactor::FileMoved::Renamed {
            from: path.clone(),
            to: main.clone(),
        }]
    );
}
