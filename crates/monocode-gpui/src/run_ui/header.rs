//! The file header's Run and Debug buttons with their menu of what can run
//! here, and the key commands for the same: run or debug the focused file's
//! default, and run the last thing again.
use super::{start, targets};
use crate::{
    code_editor::{Crumb, DocumentInfo, HeaderAction, HeaderProvider},
    code_shell::{
        self,
        context::{self, Checkout},
        launch, settings,
    },
    popover::{MenuAction, MenuItem},
};
use gpui::{App, KeyBinding, Window, actions};
use monocode_core::{
    code::run::{self, RunConfig, TestRunner},
    files::path_key,
};
use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc};

actions!(run_ui, [RunFile, DebugFile, RerunLast]);

/// The id of the project settings page.
pub(crate) const PAGE: &str = "run-configurations";

pub(crate) const RUN_KEY: &str = "alt-shift-r";
pub(crate) const DEBUG_KEY: &str = "alt-shift-d";
pub(crate) const RERUN_KEY: &str = "ctrl-alt-r";

/// A key as the tooltips and quick open write it.
pub(super) fn shown_key(key: &str) -> String {
    let mac = cfg!(target_os = "macos");
    key.split('-')
        .map(|part| match (part, mac) {
            ("alt", true) => "\u{2325}".to_owned(),
            ("shift", true) => "\u{21e7}".to_owned(),
            ("ctrl", true) => "\u{2303}".to_owned(),
            ("alt", false) => "Alt+".to_owned(),
            ("shift", false) => "Shift+".to_owned(),
            ("ctrl", false) => "Ctrl+".to_owned(),
            (letter, _) => letter.to_uppercase(),
        })
        .collect()
}

pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(RUN_KEY, RunFile, Some("FileEditor")),
        KeyBinding::new(DEBUG_KEY, DebugFile, Some("FileEditor")),
        KeyBinding::new(RERUN_KEY, RerunLast, None),
    ]);
    // The key arrives inside the window's own update; the run starts after.
    cx.on_action(|_: &RunFile, cx| cx.defer(|cx| run_focused(false, cx)));
    cx.on_action(|_: &DebugFile, cx| cx.defer(|cx| run_focused(true, cx)));
    cx.on_action(|_: &RerunLast, cx| {
        cx.defer(|cx| {
            if !start::rerun_last(cx) {
                run_focused(false, cx);
            }
        })
    });
}

fn say(text: &'static str, cx: &mut App) {
    code_shell::in_window(cx, |window, cx| start::notice(text, window, cx));
}

/// Runs or debugs what the run button would for the focused file.
pub(crate) fn run_focused(debug: bool, cx: &mut App) {
    let Some(file) = context::focused_file(cx) else {
        say("Open a file to run it.", cx);
        return;
    };
    let path = PathBuf::from(&file.path);
    if !run_default(&path, &file.checkout, None, debug, cx) {
        say("Nothing is set up to run this file.", cx);
    }
}

/// Starts the default choice for a file with the caret at `offset`. False
/// when nothing is offered there.
fn run_default(
    path: &std::path::Path,
    checkout: &Checkout,
    offset: Option<usize>,
    debug: bool,
    cx: &mut App,
) -> bool {
    let context = targets::editor_context(path, checkout, offset, cx);
    let choices = targets::choices(checkout, &context, cx);
    let runs = settings::runs(&checkout.project, cx);
    let Some(config) = choices.default(&runs).cloned() else {
        return false;
    };
    go(config, context, checkout.clone(), debug, cx);
    true
}

fn go(config: RunConfig, context: run::RunContext, checkout: Checkout, debug: bool, cx: &mut App) {
    if debug {
        start::debug(config, context, checkout, cx);
    } else {
        start::start(config, context, checkout, cx);
    }
}

/// The rows of a file's run menu: the project's configurations offered
/// there, the built-in tasks for the file, and the way to the settings.
/// It is built while the file's editor is busy opening it, so it goes by
/// the document and the caret the header last drew with.
pub(crate) fn menu(doc: &DocumentInfo, caret: Option<usize>, cx: &App) -> Vec<MenuItem> {
    let mut items: Vec<MenuItem> = Vec::new();
    if let Some(checkout) = targets::checkout_of(&doc.path, &doc.root, cx) {
        let context = targets::document_context(&doc.path, &checkout, caret, cx);
        let choices = targets::choices(&checkout, &context, cx);
        let runs = settings::runs(&checkout.project, cx);
        let default = choices.default(&runs).map(|config| config.id.clone());
        let mut list = |title: &'static str, configs: &[RunConfig]| {
            if configs.is_empty() {
                return;
            }
            items.push(MenuItem::Header(title.into()));
            for config in configs {
                items.push(
                    MenuAction::new(format!("run:{}", config.id), config.label())
                        .checked(default.as_deref() == Some(config.id.as_str()))
                        .into(),
                );
            }
        };
        list("Configurations", &choices.user);
        list("This file", &choices.builtin);
        if !choices.is_empty() {
            items.push(MenuItem::Separator);
        }
    }
    items.push(
        MenuAction::new("edit", "Edit configurations\u{2026}")
            .icon("settings")
            .into(),
    );
    items
}

fn pick(
    id: &str,
    doc: &DocumentInfo,
    offset: usize,
    debug: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(checkout) = targets::checkout_of(&doc.path, &doc.root, cx) else {
        return;
    };
    if id == "edit" {
        settings::open(&checkout.project, PAGE, window, cx);
        return;
    }
    let Some(id) = id.strip_prefix("run:") else {
        return;
    };
    let context = targets::editor_context(&doc.path, &checkout, Some(offset), cx);
    let choices = targets::choices(&checkout, &context, cx);
    match choices.get(id).cloned() {
        Some(config) => go(config, context, checkout, debug, cx),
        None => start::notice("This can no longer be run here.", window, cx),
    }
}

/// Whether the project has configurations of its own, without copying them.
fn has_configurations(project: &str, cx: &App) -> bool {
    cx.try_global::<settings::SavedRuns>()
        .and_then(|saved| saved.0.get(&path_key(project)))
        .is_some_and(|runs| !runs.configs.is_empty())
}

/// What the header was told about a file the first time it drew it.
struct Known {
    doc: Rc<DocumentInfo>,
    /// The file has built-in tasks: its language has a provider.
    tasks: bool,
}

/// Where each file's caret was when its header was last drawn, for the
/// menu, which is listed without being told.
type Carets = Rc<RefCell<HashMap<PathBuf, usize>>>;

#[derive(Default)]
pub(super) struct RunHeader {
    files: RefCell<HashMap<PathBuf, Known>>,
    carets: Carets,
}

impl RunHeader {
    fn button(doc: &Rc<DocumentInfo>, carets: &Carets, debug: bool) -> HeaderAction {
        let (id, icon, name, key) = match debug {
            false => ("run-file", "play", "Run", RUN_KEY),
            true => ("debug-file", "bug", "Debug", DEBUG_KEY),
        };
        let (clicked, listed, picked) = (doc.clone(), doc.clone(), doc.clone());
        let carets = carets.clone();
        // Both handlers are called by the file's editor in the middle of its
        // own update; what they do reads that editor, so it waits.
        HeaderAction::new(
            id,
            icon,
            format!("{name} ({})", shown_key(key)),
            move |click, window, cx| {
                let (doc, offset) = (clicked.clone(), click.offset);
                window.defer(cx, move |window, cx| {
                    let Some(checkout) = targets::checkout_of(&doc.path, &doc.root, cx) else {
                        return;
                    };
                    if !run_default(&doc.path, &checkout, Some(offset), debug, cx) {
                        start::notice("Nothing is set up to run this file.", window, cx);
                    }
                });
            },
        )
        .with_menu(
            move |cx| {
                let caret = carets.borrow().get(&listed.path).copied();
                menu(&listed, caret, cx)
            },
            move |id, click, window, cx| {
                let (id, doc, offset) = (id.to_owned(), picked.clone(), click.offset);
                window.defer(cx, move |window, cx| {
                    pick(&id, &doc, offset, debug, window, cx)
                });
            },
        )
    }
}

impl HeaderProvider for RunHeader {
    fn actions(&self, doc: &DocumentInfo, cx: &App) -> Vec<HeaderAction> {
        let Some(checkout) = targets::checkout_of(&doc.path, &doc.root, cx) else {
            return Vec::new();
        };
        let mut files = self.files.borrow_mut();
        // Whether a language has tasks never changes for a file, and finding
        // out looks at the disk: once per file, not once per draw.
        let known = files.entry(doc.path.clone()).or_insert_with(|| {
            let context = targets::bare_context(&checkout).with_file(&doc.path);
            Known {
                doc: Rc::new(doc.clone()),
                tasks: !run::builtin_tasks(&context, TestRunner::Pytest).is_empty(),
            }
        });
        if !known.tasks && !has_configurations(&checkout.project, cx) {
            return Vec::new();
        }
        let mut actions = vec![Self::button(&known.doc, &self.carets, false)];
        if launch::can_debug(cx) {
            actions.push(Self::button(&known.doc, &self.carets, true));
        }
        actions
    }
    /// Never replaces the crumbs: this is only where the header says where
    /// the caret is, which the menu's list of tasks goes by.
    fn crumbs(&self, doc: &DocumentInfo, offset: usize, _: &App) -> Option<Vec<Crumb>> {
        let mut carets = self.carets.borrow_mut();
        match carets.get_mut(&doc.path) {
            Some(known) => *known = offset,
            None => {
                carets.insert(doc.path.clone(), offset);
            }
        }
        None
    }
}
