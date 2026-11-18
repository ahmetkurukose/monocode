//! What can run where: the run context the editor knows (file, caret,
//! symbol, selection), the configurations and built-in tasks offered for it,
//! which of them the run button starts, and how the editor's run markers map
//! onto the core's runnables. Nothing here starts anything.
use crate::{
    code_editor::{
        self, Hub,
        symbols::{self, Symbol, SymbolKind},
    },
    code_shell::{
        context::{self, Checkout},
        settings,
    },
};
use gpui::{App, Entity, Global};
use monocode_core::code::{
    run::{self, ProjectRuns, RunConfig, RunContext, Runnable, RunnableKind, TestRunner},
    toolchain::Toolchain,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    ops::Range,
    path::Path,
    time::{Duration, Instant},
};

/// How long a checkout's toolchain is reused by the lists that are built
/// while drawing or typing; a run always looks afresh.
pub(super) const TOOLCHAIN_AGE: Duration = Duration::from_secs(5);

/// The hub, kept so code that only has `&App` can read a document, and the
/// toolchains the menus and quick open last looked up.
pub(super) struct Shared {
    pub(super) hub: Entity<Hub>,
    toolchains: RefCell<HashMap<String, (Instant, Toolchain)>>,
}
impl Global for Shared {}

pub(super) fn init(cx: &mut App) {
    let hub = code_editor::hub(cx);
    cx.set_global(Shared {
        hub,
        toolchains: RefCell::default(),
    });
}

/// The checkout's toolchain for building a list. Looks at the disk at most
/// once per `TOOLCHAIN_AGE` and checkout.
pub(super) fn toolchain(checkout: &Checkout, cx: &App) -> Toolchain {
    let Some(shared) = cx.try_global::<Shared>() else {
        return context::toolchain(checkout, cx);
    };
    let mut known = shared.toolchains.borrow_mut();
    if let Some((at, toolchain)) = known.get(&checkout.folder)
        && at.elapsed() < TOOLCHAIN_AGE
    {
        return toolchain.clone();
    }
    let toolchain = context::toolchain(checkout, cx);
    known.insert(checkout.folder.clone(), (Instant::now(), toolchain.clone()));
    toolchain
}

/// What of the focused file a value derived from it depends on: its path
/// and pane, the caret, the selection and the hub's version of its text.
pub(super) type FileKey = (String, i64, usize, Range<usize>, u64);

/// The `FileKey` of a focused file, `None` once its editor is gone.
pub(super) fn file_key(file: &context::FocusedFile, cx: &App) -> Option<FileKey> {
    let editor = file.editor.upgrade()?;
    let input = editor.read(cx).input.read(cx);
    let version = cx
        .try_global::<Shared>()
        .and_then(|shared| shared.hub.read(cx).document(Path::new(&file.path)))
        .map_or(0, |open| open.version);
    Some((
        file.path.clone(),
        file.pane,
        input.caret(),
        input.selection_range(),
        version,
    ))
}

/// The version of a file's text the hub last heard of, 0 when it is not open.
pub(super) fn document_version(path: &Path, cx: &App) -> u64 {
    (cx.try_global::<Shared>())
        .and_then(|shared| shared.hub.read(cx).document(path))
        .map_or(0, |open| open.version)
}

/// The checkout a file belongs to: the one the shell knows, else the folder
/// its editor was opened from.
pub(super) fn checkout_of(path: &Path, root: &Path, cx: &App) -> Option<Checkout> {
    context::checkout_for_path(&path.to_string_lossy(), cx).or_else(|| {
        let root = root.to_string_lossy();
        (!root.is_empty()).then(|| Checkout {
            project: root.to_string(),
            folder: root.into_owned(),
        })
    })
}

/// A context with no focused file.
pub(super) fn bare_context(checkout: &Checkout) -> RunContext {
    RunContext::new(&checkout.folder, &checkout.project)
}

/// The symbol a caret is in, as `$SYMBOL` and the built-in tasks take it:
/// the classes around it and the first function, joined with `::`.
pub(super) fn symbol_at(path: &str, text: &str, offset: usize) -> Option<String> {
    symbol_in(&symbols::outline(path, text), offset)
}

/// `symbol_at` from an outline that exists already, such as the one a file's
/// editor keeps: nothing is parsed.
pub(super) fn symbol_in(outline: &[Symbol], offset: usize) -> Option<String> {
    let mut names = Vec::new();
    for symbol in symbols::chain_at(outline, offset) {
        let ends = matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method);
        names.push(symbol.name.as_str());
        // A function inside a function cannot be addressed by a test runner.
        if ends {
            break;
        }
    }
    (!names.is_empty()).then(|| names.join("::"))
}

/// Fills in what a caret at `offset` of `text` says: line, column, symbol.
/// The symbol comes from `outline` when the caller has the file's, else the
/// text is parsed for it.
fn place(
    run: &mut RunContext,
    shown: &str,
    text: &str,
    offset: Option<usize>,
    outline: Option<&[Symbol]>,
) {
    let Some(offset) = offset.filter(|offset| *offset <= text.len()) else {
        return;
    };
    let (line, column) = code_editor::position_at(text, offset);
    run.row = Some(line as u32 + 1);
    run.column = Some(column as u32 + 1);
    run.symbol = match outline {
        Some(outline) => symbol_in(outline, offset),
        None => symbol_at(shown, text, offset),
    };
}

/// The context of a file as the hub last heard of it, with the caret at
/// `offset` when one is known. Safe while the file's editor is being
/// updated or drawn, which is when the header asks; it has no selection.
pub(super) fn document_context(
    path: &Path,
    checkout: &Checkout,
    offset: Option<usize>,
    cx: &App,
) -> RunContext {
    let mut run = bare_context(checkout).with_file(path);
    let known = cx
        .try_global::<Shared>()
        .and_then(|shared| shared.hub.read(cx).document(path))
        .map(|open| open.text.clone());
    if let Some(text) = known {
        place(&mut run, &path.to_string_lossy(), &text, offset, None);
    }
    run
}

/// The context of a file with everything its editor knows: the caret (at
/// `offset`, else where it is) and the selection, when the focused editor
/// shows this file. Reads that editor, so it must not be called from inside
/// one of the editor's own callbacks; defer first.
pub(super) fn editor_context(
    path: &Path,
    checkout: &Checkout,
    offset: Option<usize>,
    cx: &App,
) -> RunContext {
    let shown = path.to_string_lossy();
    let focused = context::focused_file(cx)
        .filter(|focused| focused.path == shown)
        .and_then(|focused| focused.editor.upgrade());
    let Some(editor) = focused else {
        return document_context(path, checkout, offset, cx);
    };
    let mut run = bare_context(checkout).with_file(path);
    let editor = editor.read(cx);
    // The editor names its symbols after every edit; quick open and the
    // settings page ask here while typing and drawing.
    let outline = editor.outline();
    let input = editor.input.read(cx);
    let selection = input.selection_range();
    if !selection.is_empty()
        && let Some(selected) = input.content.get(selection)
    {
        run.selected_text = Some(selected.to_owned());
    }
    let offset = offset.unwrap_or(input.caret());
    place(
        &mut run,
        &shown,
        &input.content,
        Some(offset),
        Some(&outline),
    );
    run
}

/// What is offered in a context: the project's configurations that resolve
/// there, then the built-in tasks for its file.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Choices {
    pub user: Vec<RunConfig>,
    pub builtin: Vec<RunConfig>,
}
impl Choices {
    pub(crate) fn new(runs: &ProjectRuns, context: &RunContext, toolchain: &Toolchain) -> Self {
        Self {
            user: (run::offered(runs, context, toolchain).into_iter())
                .cloned()
                .collect(),
            builtin: run::builtin_tasks(context, runs.test_runner),
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.user.is_empty() && self.builtin.is_empty()
    }
    pub(crate) fn all(&self) -> impl Iterator<Item = &RunConfig> {
        self.user.iter().chain(&self.builtin)
    }
    pub(crate) fn get(&self, id: &str) -> Option<&RunConfig> {
        self.all().find(|config| config.id == id)
    }
    /// What the run button starts: the project's last run when it is one of
    /// these, else the first built-in task, else the first configuration.
    pub(crate) fn default(&self, runs: &ProjectRuns) -> Option<&RunConfig> {
        (runs.last_used())
            .and_then(|last| self.get(&last.id))
            .or_else(|| self.builtin.first())
            .or_else(|| self.user.first())
    }
}

/// The choices for a context, with the project's configurations as saved.
pub(super) fn choices(checkout: &Checkout, context: &RunContext, cx: &App) -> Choices {
    let runs = settings::runs(&checkout.project, cx);
    Choices::new(&runs, context, &toolchain(checkout, cx))
}

/// The editor's run marker as the core's runnable. Tests only count in a
/// file the test runners collect; the main guard counts in any Python file
/// and is named after it.
pub(crate) fn core_runnable(path: &Path, found: &symbols::Runnable) -> Option<Runnable> {
    use symbols::RunnableKind as Found;
    let test_file = run::is_python_test_file(path);
    let (kind, name) = match found.kind {
        Found::TestFunction | Found::TestMethod if test_file => {
            (RunnableKind::PythonTestFunction, found.name.clone())
        }
        Found::TestClass if test_file => (RunnableKind::PythonTestClass, found.name.clone()),
        Found::Main => (
            RunnableKind::PythonMain,
            path.file_name()?.to_string_lossy().into_owned(),
        ),
        _ => return None,
    };
    Some(Runnable {
        kind,
        name,
        class_name: found.class_name.clone(),
        row: found.row as u32,
    })
}

/// A runnable's name as `$SYMBOL` would say it.
pub(super) fn qualified(runnable: &Runnable) -> Option<String> {
    match runnable.kind {
        RunnableKind::PythonMain | RunnableKind::GoMain | RunnableKind::RustMain => None,
        _ => Some(match &runnable.class_name {
            Some(classes) if !classes.is_empty() => format!("{classes}::{}", runnable.name),
            _ => runnable.name.clone(),
        }),
    }
}

/// Whether a marker has a task under the project's test runner: unittest
/// cannot address a test function outside a class.
pub(super) fn addressable(runnable: &Runnable, runner: TestRunner) -> bool {
    !(runner == TestRunner::Unittest
        && runnable.kind == RunnableKind::PythonTestFunction
        && runnable.class_name.as_deref().is_none_or(str::is_empty))
}
