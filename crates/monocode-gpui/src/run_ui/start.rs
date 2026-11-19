//! Starting a run, the one way everything else does it: the focused file is
//! saved, the configuration is resolved for the checkout off the UI thread,
//! the process gets a dock tab, and the core's registry is told what the tab
//! does (process id, output, exit) so agents and the footer see the same
//! runs. A stop asked for through the registry stops the tab. Starting what
//! is already in a tab restarts it there instead of opening another.
use super::footer;
use crate::{
    code_shell::{
        self,
        context::{self, Checkout},
        dock::{self, DockTabHandle, Run, RunEvent as TabEvent, RunExit, RunRequest},
        launch,
        processes::{self, ProcessKind, Registration},
        settings,
    },
    file_editor::FileEditor,
};
use gpui::{App, Entity, Global, SharedString, Subscription, Window};
use monocode_core::code::{
    run::{self, RunConfig, RunContext, RunEvent, RunId, StartedBy},
    services::services,
    spawn::Spawn,
    toolchain::Toolchain,
};
use std::{collections::HashMap, path::Path};

/// What to start.
pub(crate) enum Target {
    /// A configuration or built-in task, resolved for the checkout first.
    Config {
        config: RunConfig,
        context: RunContext,
    },
    /// A process that is already resolved: a rerun.
    Spawn(Spawn),
}

pub(crate) struct Request {
    pub target: Target,
    pub checkout: Checkout,
    pub by: StartedBy,
    /// Hand the process to the debugger instead of a run tab.
    pub debug: bool,
}

/// How a start ended: the run's id in the registry, or the sentence the
/// user was shown. A debug start that worked sends nothing and closes.
pub(crate) type Outcome = async_channel::Receiver<Result<RunId, String>>;

/// A run that has a dock tab.
struct Live {
    /// The registry's entry for the tab's current process.
    id: RunId,
    run: Entity<Run>,
    /// The configuration it came from, whose finished tab a new start of it
    /// replaces.
    config: Option<String>,
    checkout: Checkout,
    spawn: Spawn,
    by: StartedBy,
    /// The tab is being restarted from here and its new process is already
    /// in the registry.
    restarting: bool,
    process: Option<Registration>,
    _events: Subscription,
}

#[derive(Clone)]
struct Last {
    spawn: Spawn,
    checkout: Checkout,
}

#[derive(Default)]
pub(super) struct State {
    live: Vec<Live>,
    last: Option<Last>,
    /// The last sentence shown to the user.
    notice: Option<String>,
    /// Agent sessions by id, as the lists name them.
    agents: HashMap<i64, SharedString>,
    /// The processes of runs that have no tab, listed as running work for
    /// as long as they run.
    headless: HashMap<RunId, Registration>,
}
impl Global for State {}

pub(super) fn init(cx: &mut App) {
    cx.set_global(State::default());
    let events = services().runs().subscribe();
    cx.spawn(async move |cx| {
        while let Ok(event) = events.recv().await {
            cx.update(|cx| registry_event(event, cx));
        }
    })
    .detach();
}

/// Runs `config` in `context` as the user: in a dock tab, remembered as the
/// project's last run.
pub(crate) fn start(
    config: RunConfig,
    context: RunContext,
    checkout: Checkout,
    cx: &mut App,
) -> Outcome {
    begin(
        Request {
            target: Target::Config { config, context },
            checkout,
            by: StartedBy::User,
            debug: false,
        },
        cx,
    )
}

/// Resolves `config` like a run and hands the process to the debugger.
pub(crate) fn debug(
    config: RunConfig,
    context: RunContext,
    checkout: Checkout,
    cx: &mut App,
) -> Outcome {
    begin(
        Request {
            target: Target::Config { config, context },
            checkout,
            by: StartedBy::User,
            debug: true,
        },
        cx,
    )
}

/// Runs `config` in a dock tab on behalf of an agent session: the tab and
/// the lists say who started it, and the project's last run is left alone.
// For the agents' `run_start` tool, which starts its runs headless today.
#[allow(dead_code)]
pub(crate) fn start_for_agent(
    session: i64,
    config: RunConfig,
    context: RunContext,
    checkout: Checkout,
    cx: &mut App,
) -> Outcome {
    begin(
        Request {
            target: Target::Config { config, context },
            checkout,
            by: StartedBy::Agent(session),
            debug: false,
        },
        cx,
    )
}

/// Starts the last thing the user ran again, in its tab if that is still
/// open. False when nothing has been run yet.
pub(crate) fn rerun_last(cx: &mut App) -> bool {
    let Some(last) = cx.global::<State>().last.clone() else {
        return false;
    };
    begin(
        Request {
            target: Target::Spawn(last.spawn),
            checkout: last.checkout,
            by: StartedBy::User,
            debug: false,
        },
        cx,
    );
    true
}

/// Whether there is a last run to repeat.
pub(crate) fn has_last(cx: &App) -> bool {
    (cx.try_global::<State>()).is_some_and(|state| state.last.is_some())
}

/// The project of the last run, whose process Rerun last would start.
pub(crate) fn last_project(cx: &App) -> Option<String> {
    let last = cx.try_global::<State>()?.last.as_ref()?;
    Some(last.checkout.project.clone())
}

/// Asks for a run to end, whoever started it and wherever it runs.
pub(crate) fn stop(id: RunId) {
    services().runs().stop(id);
}

/// The tab of a run, while it has one that is open.
pub(crate) fn tab_of(id: RunId, cx: &App) -> Option<DockTabHandle> {
    let state = cx.try_global::<State>()?;
    let live = state.live.iter().find(|live| live.id == id)?;
    (live.run.read(cx).is_open()).then(|| DockTabHandle::of_run(&live.run, cx))
}

/// What the lists call the agent session that started a run.
pub(crate) fn agent_name(session: i64, cx: &App) -> SharedString {
    cx.try_global::<State>()
        .and_then(|state| state.agents.get(&session).cloned())
        .unwrap_or_else(|| "an agent".into())
}

/// The last sentence a start showed the user.
#[cfg(any(test, feature = "selftest"))]
pub(crate) fn last_notice(cx: &App) -> Option<String> {
    cx.global::<State>().notice.clone()
}

/// Shows the user why something did not happen.
pub(super) fn notice(text: impl Into<String>, window: &mut Window, cx: &mut App) {
    let text = text.into();
    cx.global_mut::<State>().notice = Some(text.clone());
    context::notice(text, window, cx);
}

/// Saves the focused file if it has unsaved edits. The receiver resolves
/// once the save is over, whether or not it worked: a failed save is the
/// editor's to report, and the run then sees the file as it is on disk.
fn save_focused(cx: &mut App) -> Option<(async_channel::Receiver<()>, Subscription)> {
    let editor: Entity<FileEditor> = context::focused_file(cx)?.editor.upgrade()?;
    if !editor.read(cx).dirty {
        return None;
    }
    editor.update(cx, |editor, cx| editor.save(cx));
    if !editor.read(cx).saving {
        return None;
    }
    let (done, saved) = async_channel::bounded(1);
    let watch = cx.observe(&editor, move |editor, cx| {
        if !editor.read(cx).saving {
            let _ = done.try_send(());
        }
    });
    Some((saved, watch))
}

/// Starts `request`. Returns at once; the work continues on the UI thread
/// once the file is saved and the configuration resolved.
pub(crate) fn begin(request: Request, cx: &mut App) -> Outcome {
    let (report, outcome) = async_channel::bounded(1);
    cx.spawn(async move |cx| {
        // Not read here, before the spawn: a gutter menu's click runs inside
        // an update of the file's editor, and the save reads that editor.
        let project = request.checkout.project.clone();
        let (saving, setup) =
            cx.update(|cx| (save_focused(cx), context::terminal_setup(&project, cx)));
        if let Some((saved, _watch)) = saving {
            let _ = saved.recv().await;
        }
        let Request {
            target,
            checkout,
            by,
            debug,
        } = request;
        let (resolved, config) = match target {
            Target::Spawn(spawn) => (Ok(spawn), None),
            Target::Config { config, context } => {
                let (asked, folder, project) = (
                    config.clone(),
                    checkout.folder.clone(),
                    checkout.project.clone(),
                );
                // Finding the interpreter reads the disk.
                let resolved = cx
                    .background_executor()
                    .spawn(async move {
                        let toolchain =
                            Toolchain::for_folder(&setup, Path::new(&folder), Path::new(&project));
                        run::resolve(&asked, &context, &toolchain)
                    })
                    .await;
                (resolved, Some(config))
            }
        };
        cx.update(|cx| {
            code_shell::in_window(cx, |window, cx| {
                let result = match resolved {
                    Ok(spawn) => started(spawn, config, checkout, by, debug, window, cx),
                    Err(unresolved) => {
                        let label = config.map(|config| config.label()).unwrap_or_default();
                        Err(format!("\u{201c}{label}\u{201d} cannot run. {unresolved}"))
                    }
                };
                match result {
                    Ok(Some(id)) => {
                        let _ = report.try_send(Ok(id));
                    }
                    Ok(None) => {}
                    Err(sentence) => {
                        notice(sentence.clone(), window, cx);
                        let _ = report.try_send(Err(sentence));
                    }
                }
            });
        });
    })
    .detach();
    outcome
}

/// The resolved process goes to its tab or to the debugger, and a user's
/// run is remembered.
fn started(
    spawn: Spawn,
    config: Option<RunConfig>,
    checkout: Checkout,
    by: StartedBy,
    debug: bool,
    window: &mut Window,
    cx: &mut App,
) -> Result<Option<RunId>, String> {
    let id = if debug {
        if !launch::debug(spawn.clone(), checkout.clone(), window, cx) {
            return Err("No debugger is installed.".into());
        }
        None
    } else {
        let config = config.as_ref().map(|config| config.id.as_str());
        Some(launch(spawn.clone(), &checkout, by, config, window, cx)?)
    };
    if by != StartedBy::User {
        return Ok(id);
    }
    // The footer's button repeats the latest start, of either kind.
    footer::remember(&checkout.project, window, cx);
    if !debug {
        cx.global_mut::<State>().last = Some(Last {
            spawn,
            checkout: checkout.clone(),
        });
    }
    if let Some(config) = config {
        let mut runs = settings::runs(&checkout.project, cx);
        if runs.last_used.as_ref() != Some(&config) {
            runs.mark_used(&config);
            settings::save_runs(&checkout.project, runs, window, cx);
        }
    }
    Ok(id)
}

fn agent_label(session: i64, cx: &mut App) -> SharedString {
    if let Some(known) = cx.global::<State>().agents.get(&session) {
        return known.clone();
    }
    let title = context::session_title(session, cx)
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty());
    let Some(title) = title else {
        return "an agent".into();
    };
    let label = SharedString::from(title);
    cx.global_mut::<State>()
        .agents
        .insert(session, label.clone());
    label
}

/// Puts `spawn` in a dock tab and the registry. The same process already in
/// an open tab of this checkout is restarted there, as the tab's own rerun
/// does; a finished tab of the same configuration makes way for the new one.
fn launch(
    spawn: Spawn,
    checkout: &Checkout,
    by: StartedBy,
    config: Option<&str>,
    window: &mut Window,
    cx: &mut App,
) -> Result<RunId, String> {
    let registry = services().runs();
    let folder = Path::new(&checkout.folder);
    let same = cx.global::<State>().live.iter().position(|live| {
        live.by == by
            && live.checkout.folder == checkout.folder
            && live.spawn == spawn
            && live.run.read(cx).is_open()
    });
    if let Some(ix) = same {
        let (run, old) = {
            let live = &cx.global::<State>().live[ix];
            (live.run.clone(), live.id)
        };
        if run.read(cx).state().running() {
            registry.stop(old);
            registry.finish(old, None);
        }
        let id = registry.register(spawn, folder, by);
        let live = &mut cx.global_mut::<State>().live[ix];
        live.id = id;
        live.restarting = true;
        live.process = None;
        run.update(cx, |run, cx| run.rerun(cx));
        // Showing a tab gives it the keyboard; a run leaves it where it was.
        let focused = window.focused(cx);
        DockTabHandle::of_run(&run, cx).reveal(window, cx);
        if let Some(focused) = focused {
            window.focus(&focused, cx);
        }
        return Ok(id);
    }

    let stale: Vec<Entity<Run>> = (cx.global::<State>().live.iter())
        .filter(|live| {
            config.is_some()
                && live.config.as_deref() == config
                && live.by == by
                && live.checkout.folder == checkout.folder
                && !live.run.read(cx).state().running()
        })
        .map(|live| live.run.clone())
        .collect();
    for run in stale {
        forget(&run, cx);
        DockTabHandle::of_run(&run, cx).close(window, cx);
    }

    let agent = match by {
        StartedBy::Agent(session) => Some(agent_label(session, cx)),
        StartedBy::User => None,
    };
    let request = RunRequest {
        agent,
        ..RunRequest::new(spawn.clone(), checkout.clone())
    };
    let run = dock::run(request, window, cx)?;
    let id = registry.register(spawn.clone(), folder, by);
    let (pid, running, tail) = {
        let run = run.read(cx);
        (run.pid(), run.state().running(), run.tail().to_owned())
    };
    let mut process = None;
    match pid {
        Some(pid) => {
            registry.set_pid(id, pid);
            process = Some(processes::register(
                pid,
                spawn.label.clone(),
                ProcessKind::Run,
            ));
        }
        // It could not be started; the tab says why.
        None if !running => {
            registry.append_output(id, &tail);
            registry.finish(id, None);
        }
        None => {}
    }
    let events = cx.subscribe(&run, |run, event: &TabEvent, cx| tab_event(&run, event, cx));
    cx.global_mut::<State>().live.push(Live {
        id,
        run,
        config: config.map(str::to_owned),
        checkout: checkout.clone(),
        spawn,
        by,
        restarting: false,
        process,
        _events: events,
    });
    code_shell::refresh(cx);
    Ok(id)
}

fn forget(run: &Entity<Run>, cx: &mut App) {
    cx.global_mut::<State>()
        .live
        .retain(|live| live.run != *run);
}

fn finish(id: RunId, exit: &RunExit) {
    let registry = services().runs();
    if exit.stopped {
        // Stopped from the tab: the registry records it as stopped too.
        registry.stop(id);
    }
    registry.finish(id, exit.code);
}

fn running(id: RunId) -> bool {
    (services().runs().get(id)).is_some_and(|info| info.state.is_running())
}

/// What a run's tab reports goes into the registry.
fn tab_event(run: &Entity<Run>, event: &TabEvent, cx: &mut App) {
    let registry = services().runs();
    if matches!(event, TabEvent::Closed) {
        forget(run, cx);
        code_shell::refresh(cx);
        return;
    }
    let state = cx.global_mut::<State>();
    let Some(live) = state.live.iter_mut().find(|live| live.run == *run) else {
        return;
    };
    match event {
        TabEvent::Started { pid } => {
            // Restarted from the tab's header: a new process, a new entry.
            if !std::mem::take(&mut live.restarting) && !running(live.id) {
                let folder = Path::new(&live.checkout.folder);
                live.id = registry.register(live.spawn.clone(), folder, live.by);
            }
            registry.set_pid(live.id, *pid);
            live.process = Some(processes::register(
                *pid,
                live.spawn.label.clone(),
                ProcessKind::Run,
            ));
        }
        TabEvent::Output(text) => {
            if running(live.id) {
                registry.append_output(live.id, text);
            }
            return;
        }
        TabEvent::Exited(exit) => {
            // The process this module replaced is already recorded.
            if live.restarting && exit.stopped {
                return;
            }
            live.process = None;
            if running(live.id) {
                finish(live.id, exit);
            }
        }
        TabEvent::Closed => {}
    }
    code_shell::refresh(cx);
}

/// What the registry reports that the interface acts on: a stop asked for
/// by an agent or the footer ends the tab's process, and the lists redraw.
fn registry_event(event: RunEvent, cx: &mut App) {
    match event {
        RunEvent::Output(_) => return,
        RunEvent::StopRequested(id) => {
            let run = (cx.global::<State>().live.iter())
                .find(|live| live.id == id)
                .map(|live| live.run.clone());
            if let Some(run) = run {
                run.update(cx, |run, cx| run.stop(cx));
            }
        }
        RunEvent::Started(id) => {
            if let Some(info) = services().runs().get(id)
                && let StartedBy::Agent(session) = info.started_by
            {
                agent_label(session, cx);
            }
            follow_headless(id, cx);
        }
        RunEvent::Changed(id) | RunEvent::Removed(id) => follow_headless(id, cx),
    }
    code_shell::refresh(cx);
}

/// Keeps a run without a tab among the app's running work while it runs.
fn follow_headless(id: RunId, cx: &mut App) {
    let process = (services().runs().get(id))
        .filter(|info| info.headless && info.state.is_running())
        .and_then(|info| Some((info.pid?, info.label)));
    let listed = &mut cx.global_mut::<State>().headless;
    match process {
        Some((pid, label)) => {
            listed
                .entry(id)
                .or_insert_with(|| processes::register(pid, label, ProcessKind::Run));
        }
        None => {
            listed.remove(&id);
        }
    }
}
