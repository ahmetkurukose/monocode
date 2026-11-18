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
