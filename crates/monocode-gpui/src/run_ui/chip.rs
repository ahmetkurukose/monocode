//! The footer's chip for running work: shown while something in the focused
//! project is running, with the count. Its list has what is running and what
//! ended lately, whoever started it (the user or an agent, in a tab or
//! without one), and what can be done with each: show it, stop it, run it
//! again, open the address it announced.
use super::{output, start};
use crate::code_shell::{
    chips::{self, ChipItem, ChipState, FooterChip, Tone},
    context::{self, Checkout},
};
use gpui::{App, SharedString, Window};
use monocode_core::{
    code::{
        run::{RunId, RunInfo, RunState, StartedBy},
        services::services,
    },
    files::path_key,
};
use std::{path::Path, rc::Rc, time::SystemTime};

pub(crate) const CHIP: &str = "code-running";
/// How many ended runs the list keeps below the running ones.
const ENDED_SHOWN: usize = 8;

pub(super) fn init(cx: &mut App) {
    chips::register(
        FooterChip {
            id: CHIP,
            label: "Runs",
            state: Rc::new(state),
            items: Rc::new(items),
            pick: Rc::new(pick),
        },
        cx,
    );
}

/// The checkout a run belongs to, as the shell knows it; a folder of no
/// open project stands for itself.
pub(super) fn checkout_of(info: &RunInfo, cx: &App) -> Checkout {
    let folder = info.checkout.to_string_lossy().into_owned();
    context::checkout_for_path(&folder, cx).unwrap_or(Checkout {
        project: folder.clone(),
        folder,
    })
}

/// The runs of the project in front: the running ones, newest first, then
/// the last few that ended.
pub(crate) fn runs(cx: &App) -> Vec<RunInfo> {
    let Some(focus) = context::focused_checkout(cx) else {
        return Vec::new();
    };
    let project = path_key(&focus.project);
    let mut runs: Vec<RunInfo> = (services().runs().list().into_iter())
        .filter(|info| path_key(&checkout_of(info, cx).project) == project)
        .collect();
    runs.sort_by_key(|info| {
        (
            !info.state.is_running(),
            std::cmp::Reverse(info.ended_at.unwrap_or(info.started_at)),
        )
    });
    let running = runs.iter().filter(|info| info.state.is_running()).count();
    runs.truncate(running + ENDED_SHOWN);
    runs
}

fn state(cx: &App) -> Option<ChipState> {
    let running = (runs(cx).iter())
        .filter(|info| info.state.is_running())
        .count();
    (running > 0).then(|| ChipState {
        icon: "play",
        text: format!("{running} running").into(),
        tone: Tone::Busy,
        tooltip: "What is running in this project".into(),
    })
}

fn elapsed(info: &RunInfo) -> String {
    let end = info.ended_at.unwrap_or_else(SystemTime::now);
    let seconds = (end.duration_since(info.started_at)).map_or(0, |took| took.as_secs());
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

fn state_text(state: RunState) -> (&'static str, String, Tone) {
    match state {
        RunState::Running => ("play", "Running".into(), Tone::Busy),
        RunState::Exited(Some(0)) => ("check", "Exit 0".into(), Tone::Good),
        RunState::Exited(Some(code)) => ("circle-x", format!("Exit {code}"), Tone::Error),
        RunState::Exited(None) => ("circle-x", "Ended".into(), Tone::Error),
        RunState::Stopped => ("square", "Stopped".into(), Tone::Quiet),
    }
}

/// One run as a row: what it is and where, then its state, how long it has
/// taken and who started it.
pub(crate) fn item(info: &RunInfo, cx: &App) -> ChipItem {
    let checkout = checkout_of(info, cx);
    let mut title = info.label.clone();
    if path_key(&checkout.folder) != path_key(&checkout.project)
        && let Some(name) = Path::new(&checkout.folder).file_name()
    {
        title = format!("{title} \u{00b7} {}", name.to_string_lossy());
    }
    let who: SharedString = match info.started_by {
        StartedBy::User => "you".into(),
        StartedBy::Agent(session) => start::agent_name(session, cx),
    };
    let (icon, state, tone) = state_text(info.state);
    let id = info.id.0;
    let action = |verb: &str, label: String| -> (SharedString, SharedString) {
        (format!("{verb}:{id}").into(), label.into())
    };
    let tab = start::tab_of(info.id, cx).is_some();
    let mut actions = vec![action(
        "show",
        if tab { "Show its tab" } else { "Show output" }.into(),
    )];
    if let Some(url) = &info.url {
        actions.push(action("open", format!("Open {url} in the browser")));
    }
    if info.state.is_running() {
        actions.push(action("stop", "Stop".into()));
    }
    actions.push(action("rerun", "Run again".into()));
    if !info.state.is_running() {
        actions.push(action("forget", "Remove from this list".into()));
    }
    ChipItem {
        id: format!("show:{id}").into(),
        title: title.into(),
        detail: Some(format!("{state} \u{00b7} {} \u{00b7} {who}", elapsed(info)).into()),
        icon: Some(icon),
        tone,
        actions,
    }
}

fn items(cx: &App) -> Vec<ChipItem> {
    runs(cx).iter().map(|info| item(info, cx)).collect()
}

/// A row or one of its actions was chosen: `verb:run`.
pub(crate) fn pick(id: &str, window: &mut Window, cx: &mut App) {
    let Some((verb, run)) = id.split_once(':') else {
        return;
    };
    let Some(id) = run.parse().ok().map(RunId) else {
        return;
    };
    let registry = services().runs();
    let Some(info) = registry.get(id) else {
        start::notice("This run is no longer listed.", window, cx);
        return;
    };
    match verb {
        "show" => match start::tab_of(id, cx) {
            Some(tab) => {
                tab.reveal(window, cx);
            }
            None => output::show(&info, window, cx),
        },
        "stop" => start::stop(id),
        "rerun" => {
            // As the user's own run, in a tab, whoever ran it before.
            start::begin(
                start::Request {
                    target: start::Target::Spawn(info.spawn.clone()),
                    checkout: checkout_of(&info, cx),
                    by: StartedBy::User,
                    debug: false,
                },
                cx,
            );
        }
        "open" => {
            if let Some(url) = &info.url
                && !context::open_url(url, window, cx)
            {
                start::notice(
                    "Open a session in this project to show the page beside it.",
                    window,
                    cx,
                );
            }
        }
        "forget" => {
            registry.remove(id);
        }
        _ => {}
    }
}
