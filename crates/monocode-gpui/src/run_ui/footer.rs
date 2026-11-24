//! Run and debug in the footer's Actions button, the place to start work in
//! the project in front whether or not a file is open: the run part of the
//! button's menu (what the focused file and the project's configurations
//! offer, Debug, Rerun last, a Stop for each run), what its main part shows
//! and repeats, and what a pick or a click starts. The shell draws the
//! button and builds the menu from inside its own update, so everything here
//! reads globals (the saved runs, the registry, the focused file) and never
//! the shell; what a pick or a click starts waits until that update is over.
//! A group that is starting or running is one row with a submenu, in place of
//! the Stop rows of its members.
use super::{
    chip,
    commands::offered,
    groups,
    header::{PAGE, RERUN_KEY, shown_key},
    start, targets,
};
use crate::{
    code_shell::{
        context::{self, Checkout},
        launch, settings,
    },
    popover::{MenuAction, MenuItem},
};
use gpui::{App, Window};
use monocode_core::{
    code::{
        dap::SessionState,
        run::{RunConfig, RunId, RunInfo},
        services::services,
    },
    files::path_key,
};
use std::{collections::HashSet, time::Duration};

/// What `workspace.last_actions` holds for a project whose most recent start
/// was a run and not a saved action. Action ids are hex numbers, so this is
/// never one, and upstream's own lookups skip an id that is no action.
pub(crate) const LAST_RUN: &str = "@last-run";

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

/// The project's runs that are running.
fn running_runs(project: &str, cx: &App) -> Vec<RunInfo> {
    let key = path_key(project);
    (services().runs().list().into_iter())
        .filter(|info| {
            info.state.is_running() && path_key(&chip::checkout_of(info, cx).project) == key
        })
        .collect()
}

/// A debug session belongs to no project here: one that is alive is worth
/// ending from wherever the user is.
fn live_sessions() -> Vec<std::sync::Arc<monocode_core::code::dap::Session>> {
    (services().dap().sessions().into_iter())
        .filter(|session| {
            session.parent().is_none()
                && !matches!(session.state(), SessionState::Terminated { .. })
        })
        .collect()
}

/// What is going on in the project, as menu rows that end it: a row with a
/// submenu for each group that is starting or has members running, then the
/// other runs still running and the debug sessions that have not ended.
fn running(project: &str, cx: &App) -> Vec<MenuItem> {
    let mut rows: Vec<MenuItem> = Vec::new();
    let mut grouped: HashSet<RunId> = HashSet::new();
    for group in groups::running_groups(project, cx) {
        grouped.extend(group.covers.iter().copied());
        let stop = |id: String, label: String| MenuAction::new(id, label).icon("square");
        let mut submenu = vec![stop(format!("group-stop:{}", group.id), "Stop all".into())];
        for (label, id) in &group.members {
            submenu.push(stop(format!("stop:{}", id.0), format!("Stop {label}")));
        }
        rows.push(
            MenuAction::new(format!("group-running:{}", group.id), group.text)
                .icon("play")
                .submenu(submenu)
                .into(),
        );
    }
    for info in running_runs(project, cx) {
        if grouped.contains(&info.id) {
            continue;
        }
        rows.push(
            MenuAction::new(
                format!("stop:{}", info.id.0),
                format!("Stop {}", info.label),
            )
            .icon("square")
            .into(),
        );
    }
    for session in live_sessions() {
        rows.push(
            MenuAction::new(
                format!("stop-debug:{}", session.id()),
                format!("Stop debugging {}", session.label()),
            )
            .icon("square")
            .into(),
        );
    }
    rows
}

/// How many things the menu can stop, for the button's label: a group counts
/// its running members, as the runs they are.
pub(crate) fn running_count(project: &str, cx: &App) -> usize {
    running_runs(project, cx).len() + live_sessions().len()
}

/// Ends a debug session: asks the debuggee to end, and cuts the session
/// off when it has not within a few seconds (a program that ignores the
/// request, an adapter that hangs).
fn stop_debugging(id: u64) {
    let Some(session) = (services().dap().sessions().into_iter()).find(|s| s.id() == id) else {
        return;
    };
    if session.state() == SessionState::Starting {
        session.disconnect();
        return;
    }
    session.terminate();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        if !matches!(session.state(), SessionState::Terminated { .. }) {
            session.disconnect();
        }
    });
}

/// What a project's latest start was: a run now comes before the saved
/// action the button showed.
pub(crate) fn remember(project: &str, window: &mut Window, cx: &mut App) {
    let project = project.to_owned();
    crate::code_shell::with_shell(window, cx, |shell, _, cx| {
        shell.code_remember_run(&project, cx)
    });
}

/// The run part of the menu, top to bottom: the running work, the focused
/// file's built-in tasks, the project's groups and configurations, Debug and
/// Rerun last. Empty when there is nothing to put in it.
pub(crate) fn menu_items(project: &str, cx: &App) -> Vec<MenuItem> {
    let (_, offered) = choices(project, targets::TOOLCHAIN_AGE, cx);
    let mut items: Vec<MenuItem> = Vec::new();
    let running = running(project, cx);
    if !running.is_empty() {
        items.push(MenuItem::Header("Running".into()));
        items.extend(running);
    }
    fn list(items: &mut Vec<MenuItem>, title: &'static str, configs: &[RunConfig]) {
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
    }
    list(&mut items, "This file", &offered.builtin);
    let saved = settings::runs(project, cx);
    if !saved.groups.is_empty() {
        items.push(MenuItem::Header("Groups".into()));
        for group in &saved.groups {
            items.push(
                MenuAction::new(format!("group-go:{}", group.id), group.name.clone())
                    .icon("play")
                    .into(),
            );
        }
    }
    list(&mut items, "Run configurations", &offered.user);
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
        || ["go:", "debug:", "stop:", "stop-debug:", "group-"]
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
            if let Some(session) = (id.strip_prefix("stop-debug:")).and_then(|id| id.parse().ok()) {
                stop_debugging(session);
            } else if let Some(run) = id.strip_prefix("stop:").and_then(|run| run.parse().ok()) {
                start::stop(RunId(run));
            } else if let Some(group) = id.strip_prefix("group-go:") {
                let checkout = checkout_of(&project, cx);
                let (context, _) = offered(&checkout, targets::TOOLCHAIN_AGE, cx);
                groups::press(group, checkout, context, window, cx);
            } else if let Some(group) = id.strip_prefix("group-stop:") {
                groups::stop_all(&project, group, window, cx);
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
