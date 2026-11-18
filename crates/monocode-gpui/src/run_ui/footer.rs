//! Run and debug in the footer's Actions button, the place to start work in
//! the project in front whether or not a file is open: the run part of the
//! button's menu (what the focused file and the project's configurations
//! offer, Debug, Rerun last, a Stop for each run), what its main part shows
//! and repeats, and what a pick or a click starts. The shell draws the
//! button and builds the menu from inside its own update, so everything here
//! reads globals (the saved runs, the registry, the focused file) and never
//! the shell; what a pick or a click starts waits until that update is over.
use super::{
    chip,
    commands::offered,
    header::{PAGE, RERUN_KEY, shown_key},
    start, targets,
};
use crate::{
    code_shell::{
        context::{self, Checkout},
        launch, settings,
    },
    popover::{MenuAction, MenuItem},
    workspace::ProjectAction,
};
use gpui::{App, SharedString, Window};
use monocode_core::{
    code::{
        run::{RunConfig, RunId},
        services::services,
    },
    files::path_key,
};
use std::time::Duration;

/// What `workspace.last_actions` holds for a project whose most recent start
/// was a run and not a saved action. Action ids are hex numbers, so this is
/// never one, and upstream's own lookups skip an id that is no action.
pub(crate) const LAST_RUN: &str = "@last-run";

/// What the button's main part shows and repeats.
pub(crate) enum Main {
    /// The saved action at this index.
    Action(usize),
    Run(RunPart),
    /// Nothing to repeat and nothing offered: the plain "Actions" label.
    Empty,
}

pub(crate) struct RunPart {
    pub label: SharedString,
    /// What the button will do, for assistive technology.
    pub aria: SharedString,
    pub command: SharedString,
}

/// The checkout the footer starts things in: the one in front when it is
/// this project's, else the project's own folder.
fn checkout_of(project: &str, cx: &App) -> Checkout {
    context::focused_checkout(cx)
        .filter(|focus| path_key(&focus.project) == path_key(project))
        .unwrap_or_else(|| Checkout {
            project: project.to_owned(),
            folder: project.to_owned(),
        })
}

/// What a project's footer can start, as quick open and the file header
/// work it out. `max_age` is how long a toolchain may have been looked up.
fn choices(project: &str, max_age: Duration, cx: &App) -> (Checkout, targets::Choices) {
    let checkout = checkout_of(project, cx);
    let (_, choices) = offered(&checkout, max_age, cx);
    (checkout, choices)
}

/// A built-in task is already called "Run main.py".
fn said(label: &str) -> String {
    format!("Run {}", label.strip_prefix("Run ").unwrap_or(label))
}

fn part(config: &RunConfig) -> RunPart {
    let label = config.label();
    RunPart {
        aria: said(&label).into(),
        label: label.into(),
        command: config.command.clone().into(),
    }
}

/// What the main part shows: the most recent thing started in the project,
/// whichever kind it was; before anything was, the default run target of
/// what is focused, else the first saved action. Asked on every draw, so it
/// works from what the run side has cached.
pub(crate) fn main(project: &str, last: Option<&str>, actions: &[ProjectAction], cx: &App) -> Main {
    if let Some(ix) = last.and_then(|id| actions.iter().position(|a| a.id == id)) {
        return Main::Action(ix);
    }
    let runs = settings::runs(project, cx);
    let (_, offered) = choices(project, Duration::MAX, cx);
    // A task keeps the name it was remembered under, unless the file in
    // front has one of the same kind, which then names that file.
    let shown = match runs.last_used() {
        Some(used) => Some(offered.get(&used.id).unwrap_or(used)),
        None => offered.default(&runs),
    };
    match shown {
        Some(config) => Main::Run(part(config)),
        None if !actions.is_empty() => Main::Action(0),
        None => Main::Empty,
    }
}

/// What a project's latest start was: a run now comes before the saved
/// action the button showed.
pub(crate) fn remember(project: &str, window: &mut Window, cx: &mut App) {
    let project = project.to_owned();
    crate::code_shell::with_shell(window, cx, |shell, _, cx| {
        shell.code_remember_run(&project, cx)
    });
}

/// The run part of the menu, top to bottom: the focused file's built-in
/// tasks, the project's configurations, Debug, Rerun last and a Stop for
/// each run in the project. Empty when there is nothing to put in it.
pub(crate) fn menu_items(project: &str, cx: &App) -> Vec<MenuItem> {
    let (_, offered) = choices(project, targets::TOOLCHAIN_AGE, cx);
    let mut items: Vec<MenuItem> = Vec::new();
    let mut list = |title: &'static str, configs: &[RunConfig]| {
        if configs.is_empty() {
            return;
        }
        items.push(MenuItem::Header(title.into()));
        for config in configs {
            items.push(
                MenuAction::new(format!("go:{}", config.id), config.label())
                    .icon("play")
                    .into(),
            );
        }
    };
    list("This file", &offered.builtin);
    list("Run configurations", &offered.user);
    let mut more: Vec<MenuItem> = Vec::new();
    if launch::can_debug(cx) && !offered.is_empty() {
        let targets = (offered.builtin.iter().chain(&offered.user)).map(|config| {
            MenuAction::new(format!("debug:{}", config.id), config.label()).icon("play")
        });
        more.push(
            MenuAction::new("debug", "Debug")
                .icon("bug")
                .submenu(targets.collect())
                .into(),
        );
    }
    if start::last_project(cx).is_some_and(|last| path_key(&last) == path_key(project)) {
        more.push(
            MenuAction::new("rerun", "Rerun last")
                .icon("rotate-ccw")
                .shortcut(shown_key(RERUN_KEY))
                .into(),
        );
    }
    let key = path_key(project);
    for info in services().runs().list() {
        if info.state.is_running() && path_key(&chip::checkout_of(&info, cx).project) == key {
            more.push(
                MenuAction::new(
                    format!("stop:{}", info.id.0),
                    format!("Stop {}", info.label),
                )
                .icon("square")
                .into(),
            );
        }
    }
    if !items.is_empty() && !more.is_empty() {
        items.push(MenuItem::Separator);
    }
    items.extend(more);
    items
}

/// The way from the menu to the project's run configurations.
pub(crate) fn settings_item() -> MenuItem {
    MenuAction::new("run-settings", "Edit run configurations\u{2026}")
        .icon("settings")
        .into()
}

/// Whether the menu has something to start or stop, apart from the saved
/// actions and the way to add one.
pub(crate) fn has_items(project: &str, cx: &App) -> bool {
    !menu_items(project, cx).is_empty()
}

/// Handles a pick of the run part of the menu. False when `id` is not one
/// of its rows. Called inside the shell's update, so the work waits.
pub(crate) fn pick(id: &str, project: &str, window: &mut Window, cx: &mut App) -> bool {
    let known = matches!(id, "rerun" | "run-settings")
        || ["go:", "debug:", "stop:"]
            .iter()
            .any(|prefix| id.starts_with(prefix));
    if !known {
        return false;
    }
    let (id, project) = (id.to_owned(), project.to_owned());
    window.defer(cx, move |window, cx| match id.as_str() {
        "rerun" => {
            if !start::rerun_last(cx) {
                start::notice("There is nothing to run again yet.", window, cx);
            }
        }
        "run-settings" => {
            settings::open(&project, PAGE, window, cx);
        }
        _ => {
            if let Some(run) = id.strip_prefix("stop:").and_then(|run| run.parse().ok()) {
                start::stop(RunId(run));
            } else if let Some(config) = id.strip_prefix("go:") {
                go(config, &project, false, window, cx);
            } else if let Some(config) = id.strip_prefix("debug:") {
                go(config, &project, true, window, cx);
            }
        }
    });
    true
}

/// Starts or debugs the offered target `id` where the project is in front.
fn go(id: &str, project: &str, debug: bool, window: &mut Window, cx: &mut App) {
    let checkout = checkout_of(project, cx);
    let (context, offered) = offered(&checkout, targets::TOOLCHAIN_AGE, cx);
    match offered.get(id).cloned() {
        Some(config) if debug => {
            start::debug(config, context, checkout, cx);
        }
        Some(config) => {
            start::start(config, context, checkout, cx);
        }
        None => start::notice("This can no longer be run here.", window, cx),
    }
}

/// The main part was clicked while it shows a run: do that again. What was
/// last started goes again with the file in front; when that cannot be
/// resolved here (it names a file that is not focused), the very process
/// goes again. Without any history, the default target starts. Called inside
/// the shell's update, so the work waits.
pub(crate) fn click(project: &str, window: &mut Window, cx: &mut App) {
    let project = project.to_owned();
    window.defer(cx, move |window, cx| {
        let runs = settings::runs(&project, cx);
        let checkout = checkout_of(&project, cx);
        let (context, offered) = offered(&checkout, targets::TOOLCHAIN_AGE, cx);
        let config = match runs.last_used() {
            Some(used) => offered.get(&used.id),
            None => offered.default(&runs),
        };
        if let Some(config) = config.cloned() {
            start::start(config, context, checkout, cx);
            return;
        }
        let again = runs.last_used().is_some()
            && start::last_project(cx).is_some_and(|last| path_key(&last) == path_key(&project))
            && start::rerun_last(cx);
        if !again {
            start::notice("This can no longer be run here.", window, cx);
        }
    });
}
