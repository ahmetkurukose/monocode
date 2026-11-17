//! Run markers in the gutter of Python files: a mark beside every test,
//! test class and `__main__` guard, found again whenever the document
//! changes. A click runs the marker's first task; a right click lists all of
//! them, each also under the debugger when one is installed.
use super::{start, targets};
use crate::{
    code_editor::{
        DocumentEvent, DocumentInfo, GutterButton, GutterClick, GutterMark, GutterMarkKind,
        GutterProvider, GutterSlot, Hub, symbols,
    },
    code_shell::{
        context::{self, Checkout},
        launch, settings,
    },
    popover::{MenuAction, MenuItem},
};
use gpui::{
    App, AppContext, Context, Entity, Pixels, Point, SharedString, Subscription, Task, Window,
};
use monocode_core::code::run::{self, RunConfig, RunContext, Runnable, RunnableKind};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

/// The runnables of the open Python files, in step with their marks.
pub(super) struct Marks {
    hub: Entity<Hub>,
    files: HashMap<PathBuf, Vec<Runnable>>,
    /// The search under way for each file; a newer text replaces it.
    pending: HashMap<PathBuf, Task<()>>,
    _events: Subscription,
}

fn tooltip(runnable: &Runnable) -> SharedString {
    match runnable.kind {
        RunnableKind::PythonTestClass => format!("Run the tests in {}", runnable.name),
        RunnableKind::PythonMain => format!("Run {}", runnable.name),
        _ => format!(
            "Run {}",
            targets::qualified(runnable).unwrap_or_else(|| runnable.name.clone())
        ),
    }
    .into()
}

impl Marks {
    pub(super) fn new(hub: Entity<Hub>, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe(&hub, |this, _, event: &DocumentEvent, cx| match event {
            DocumentEvent::Opened { doc, text, version }
            | DocumentEvent::Changed { doc, text, version } => {
                this.look(doc.clone(), text.clone(), *version, cx)
            }
            DocumentEvent::Closed { doc } => this.clear(&doc.path, cx),
            DocumentEvent::Saved { .. } => {}
        });
        Self {
            hub,
            files: HashMap::new(),
            pending: HashMap::new(),
            _events: events,
        }
    }

    /// Finds the file's runnables off the UI thread, since it parses it.
    fn look(
        &mut self,
        doc: Rc<DocumentInfo>,
        text: SharedString,
        version: u64,
        cx: &mut Context<Self>,
    ) {
        if doc.language != Some("python") {
            return;
        }
        let found = cx
            .background_executor()
            .spawn(async move { symbols::python_runnables(&text) });
        let path = doc.path.clone();
        let task = cx.spawn(async move |this, cx| {
            let found = found.await;
            let _ = this.update(cx, |this, cx| this.found(&doc, version, found, cx));
        });
        self.pending.insert(path, task);
    }

    fn found(
        &mut self,
        doc: &DocumentInfo,
        version: u64,
        found: Vec<symbols::Runnable>,
        cx: &mut Context<Self>,
    ) {
        // A text the document has left; its successor is being looked at.
        let current = (self.hub.read(cx).document(&doc.path)).map(|open| open.version);
        if current != Some(version) {
            return;
        }
        let runner = targets::checkout_of(&doc.path, &doc.root, cx)
            .map(|checkout| settings::runs(&checkout.project, cx).test_runner)
            .unwrap_or_default();
        let runnables: Vec<Runnable> = (found.iter())
            .filter_map(|found| targets::core_runnable(&doc.path, found))
            .filter(|runnable| targets::addressable(runnable, runner))
            .collect();
        let marks = (runnables.iter())
            .map(|runnable| GutterMark {
                row: runnable.row as usize,
                kind: GutterMarkKind::Run,
                tooltip: tooltip(runnable),
            })
            .collect();
        self.files.insert(doc.path.clone(), runnables);
        self.hub.update(cx, |hub, cx| {
            hub.set_gutter_marks(&doc.path, GutterSlot::Run, marks, cx)
        });
    }

    fn clear(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.pending.remove(path);
        if self.files.remove(path).is_some() {
            self.hub.update(cx, |hub, cx| {
                hub.set_gutter_marks(path, GutterSlot::Run, Vec::new(), cx)
            });
        }
    }

    /// Looks at every open file again: the project's test runner changed,
    /// and with it which tests can be run on their own.
    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        let open: Vec<_> = (self.hub.read(cx).open_documents())
            .map(|open| (open.doc.clone(), open.text.clone(), open.version))
            .collect();
        for (doc, text, version) in open {
            self.look(doc, text, version, cx);
        }
    }

    fn at(&self, path: &Path, row: usize) -> Option<Runnable> {
        (self.files.get(path)?.iter())
            .find(|runnable| runnable.row as usize == row)
            .cloned()
    }
}

/// The context a marker runs in: its file, its line and its own name.
fn marker_context(path: &Path, runnable: &Runnable, checkout: &Checkout) -> RunContext {
    let mut context = targets::bare_context(checkout).with_file(path);
    context.row = Some(runnable.row + 1);
    context.symbol = targets::qualified(runnable);
    context
}

/// What the marker on `row` of a file runs, the first being its click.
pub(crate) fn marker_tasks(
    marks: &Entity<Marks>,
    doc: &DocumentInfo,
    row: usize,
    cx: &App,
) -> Option<(Vec<RunConfig>, RunContext, Checkout)> {
    let runnable = marks.read(cx).at(&doc.path, row)?;
    let checkout = targets::checkout_of(&doc.path, &doc.root, cx)?;
    let context = marker_context(&doc.path, &runnable, &checkout);
    let runner = settings::runs(&checkout.project, cx).test_runner;
    let tasks = run::runnable_tasks(&runnable, &context, runner);
    Some((tasks, context, checkout))
}

/// The rows of a marker's menu: every task, then every task under the
/// debugger when one is installed.
pub(crate) fn marker_menu(tasks: &[RunConfig], cx: &App) -> Vec<MenuItem> {
    let mut items: Vec<MenuItem> = (tasks.iter().enumerate())
        .map(|(n, task)| {
            MenuAction::new(format!("run:{n}"), task.label())
                .icon("play")
                .into()
        })
        .collect();
    if launch::can_debug(cx) {
        items.push(MenuItem::Separator);
        items.extend(tasks.iter().enumerate().map(|(n, task)| {
            MenuAction::new(format!("debug:{n}"), format!("Debug {}", task.label()))
                .icon("bug")
                .into()
        }));
    }
    items
}

type Pick = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// Hangs the menu from the editor showing `path`. A press in a pane that
/// was not in front makes it the focused one a frame later, so the lookup
/// is tried again that long.
fn open_menu(
    path: PathBuf,
    items: Vec<MenuItem>,
    position: Point<Pixels>,
    pick: Pick,
    tries: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let editor = context::focused_file(cx)
        .filter(|focused| Path::new(&focused.path) == path)
        .and_then(|focused| focused.editor.upgrade());
    match editor {
        Some(editor) => editor.update(cx, |editor, cx| {
            editor.open_menu_at(
                "Run",
                items,
                position,
                move |id, window, cx| pick(id, window, cx),
                window,
                cx,
            )
        }),
        None if tries > 0 => {
            window.on_next_frame(move |window, cx| {
                open_menu(path, items, position, pick, tries - 1, window, cx)
            });
            window.refresh();
        }
        None => {}
    }
}

pub(super) struct RunMarkers(pub(super) Entity<Marks>);

impl GutterProvider for RunMarkers {
    fn click(&self, doc: &DocumentInfo, click: &GutterClick, window: &mut Window, cx: &mut App) {
        if click.slot != GutterSlot::Run || click.mark.is_none() {
            return;
        }
        let Some((tasks, context, checkout)) = marker_tasks(&self.0, doc, click.row, cx) else {
            return;
        };
        let (button, position, path) = (click.button, click.position, doc.path.clone());
        // The editor calls this in the middle of its own update, and a run
        // saves that editor first: it starts once the update is over.
        window.defer(cx, move |window, cx| {
            if tasks.is_empty() {
                start::notice(
                    "The project's test runner cannot run this on its own.",
                    window,
                    cx,
                );
                return;
            }
            match button {
                GutterButton::Left => {
                    start::start(tasks[0].clone(), context, checkout, cx);
                }
                GutterButton::Right => {
                    let items = marker_menu(&tasks, cx);
                    let pick: Pick = Rc::new(move |id, _, cx| {
                        let (debug, n) = match id.split_once(':') {
                            Some(("debug", n)) => (true, n),
                            Some((_, n)) => (false, n),
                            None => return,
                        };
                        let Some(task) = n.parse::<usize>().ok().and_then(|n| tasks.get(n)) else {
                            return;
                        };
                        let (task, context, checkout) =
                            (task.clone(), context.clone(), checkout.clone());
                        match debug {
                            true => start::debug(task, context, checkout, cx),
                            false => start::start(task, context, checkout, cx),
                        };
                    });
                    open_menu(path, items, position, pick, 2, window, cx);
                }
            }
        });
    }

    fn moved(&self, doc: &DocumentInfo, marks: &[GutterMark], cx: &mut App) {
        self.0.update(cx, |this, _| {
            // The marks keep their order through an edit; with as many as
            // before, each runnable follows its own. Otherwise lines were
            // joined, and the search after the edit sorts it out.
            if let Some(runnables) = this.files.get_mut(&doc.path)
                && runnables.len() == marks.len()
            {
                for (runnable, mark) in runnables.iter_mut().zip(marks) {
                    runnable.row = mark.row as u32;
                }
            }
        });
    }
}

/// Builds the marks' keeper on the application's hub.
pub(super) fn marks(hub: Entity<Hub>, cx: &mut App) -> Entity<Marks> {
    cx.new(|cx| Marks::new(hub, cx))
}
