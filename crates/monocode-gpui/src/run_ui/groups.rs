//! Run groups in the interface: pressing a group starts its members through
//! `start::begin_in`, the way a single configuration starts, and the core's
//! `Sequence` says what comes next. This module only carries it out: it
//! starts members, waits for a delay or a port off the UI thread, stops
//! members, and tells the sequence what the registry reports. One press is one
//! `Active`, kept per checkout and group so that a second press during the
//! start does not run a second sequence, and kept for as long as one of its
//! members runs so that the footer can show the group as one row.
//!
//! Everything here runs on the UI thread outside the shell's own update (a
//! press is deferred by its caller, the rest comes from tasks and registry
//! events), so it may call into `code_shell` like `start` does; it never reads
//! the shell while the footer draws, which only calls `running`.
use super::start::{self, GroupTab, Request, Target};
use crate::code_shell::{self, context::Checkout, settings};
use gpui::{App, Global, Task, Window};
use monocode_core::{
    code::{
        run::{
            GroupAction, GroupEvent, Phase, RunConfig, RunContext, RunEvent, RunId, RunState,
            Sequence, StartedBy, Wait, port_open,
        },
        services::services,
    },
    files::path_key,
};
use std::{collections::VecDeque, time::Duration};

/// How often a port is looked at while a member waits for it.
const PROBE_EVERY: Duration = Duration::from_millis(250);

struct Active {
    token: u64,
    /// The checkout's folder and the group's id.
    key: (String, String),
    project: String,
    checkout: Checkout,
    context: RunContext,
    group: String,
    name: String,
    sequence: Sequence,
    /// The configurations as they were when the group was pressed, by member;
    /// none for a member whose configuration is gone.
    configs: Vec<Option<RunConfig>>,
    runs: Vec<Option<RunId>>,
    /// A member's tab has been shown, so the dock is open.
    revealed: bool,
    /// Starts that have not reported yet.
    inflight: usize,
    waits: Vec<Option<Task<()>>>,
}

#[derive(Default)]
struct Groups {
    last_token: u64,
    active: Vec<Active>,
}
impl Global for Groups {}

pub(super) fn init(cx: &mut App) {
    cx.set_global(Groups::default());
    let events = services().runs().subscribe();
    cx.spawn(async move |cx| {
        while let Ok(event) = events.recv().await {
            if let RunEvent::Changed(id) = event {
                cx.update(|cx| member_changed(id, cx));
            }
        }
    })
    .detach();
}

fn active(token: u64, cx: &mut App) -> Option<&mut Active> {
    (cx.global_mut::<Groups>().active.iter_mut()).find(|active| active.token == token)
}

/// Starts the project's group `group` in `checkout`, or completes it when
/// some of its members run already. The caller has left any update of the
/// shell or an editor.
pub(super) fn press(
    group: &str,
    checkout: Checkout,
    context: RunContext,
    window: &mut Window,
    cx: &mut App,
) {
    let runs = settings::runs(&checkout.project, cx);
    let Some(spec) = runs.group(group) else {
        start::notice("This group no longer exists.", window, cx);
        return;
    };
    let key = (path_key(&checkout.folder), spec.id.clone());
    let busy = cx.global::<Groups>().active.iter().any(|known| {
        known.key == key && (known.inflight > 0 || known.sequence.phase() == Phase::Starting)
    });
    if busy {
        let sentence = format!("\u{201c}{}\u{201d} is still starting.", spec.name);
        start::notice(sentence, window, cx);
        return;
    }
    let sequence = match Sequence::new(spec, |id| runs.get(id).map(RunConfig::label)) {
        Ok(sequence) => sequence,
        Err(sentence) => {
            start::notice(sentence, window, cx);
            return;
        }
    };
    let configs: Vec<Option<RunConfig>> = (spec.members.iter())
        .map(|member| runs.get(&member.config).cloned())
        .collect();
    let count = configs.len();
    let state = cx.global_mut::<Groups>();
    state.last_token += 1;
    let token = state.last_token;
    // An earlier press of the same group is over; its running members are
    // found again by what they are.
    state.active.retain(|known| known.key != key);
    state.active.push(Active {
        token,
        key,
        project: checkout.project.clone(),
        checkout,
        context,
        group: spec.id.clone(),
        name: spec.name.clone(),
        sequence,
        configs,
        runs: vec![None; count],
        revealed: false,
        inflight: 0,
        waits: (0..count).map(|_| None).collect(),
    });
    let actions = match active(token, cx) {
        Some(active) => active.sequence.begin(),
        None => return,
    };
    let follow = perform(token, actions, window, cx);
    deliver(token, follow, window, cx);
}

/// Stops the group's running members, last first.
pub(super) fn stop_all(project: &str, group: &str, window: &mut Window, cx: &mut App) {
    let key = path_key(project);
    let token = (cx.global::<Groups>().active.iter())
        .rev()
        .find(|known| path_key(&known.project) == key && known.group == group)
        .map(|known| known.token);
    if let Some(token) = token {
        deliver(token, vec![GroupEvent::StopAll], window, cx);
    }
}

/// Feeds the sequence events, and what it answers back to it until it has
/// nothing more to do.
fn deliver(token: u64, events: Vec<GroupEvent>, window: &mut Window, cx: &mut App) {
    let mut queue: VecDeque<GroupEvent> = events.into();
    while let Some(event) = queue.pop_front() {
        let Some(active) = active(token, cx) else {
            return;
        };
        match &event {
            GroupEvent::StopAll => active.waits.iter_mut().for_each(|wait| *wait = None),
            GroupEvent::WaitMet(at)
            | GroupEvent::WaitTimedOut(at)
            | GroupEvent::Exited(at, _)
            | GroupEvent::Stopped(at) => active.waits[*at] = None,
            _ => {}
        }
        let actions = active.sequence.event(event);
        queue.extend(perform(token, actions, window, cx));
    }
    // Over, and nothing of it runs any more: it is not listed again.
    let over = |known: &Active| {
        known.token == token
            && known.inflight == 0
            && known.sequence.phase() != Phase::Starting
            && !(known.runs.iter().flatten()).any(|id| running(*id))
    };
    cx.global_mut::<Groups>()
        .active
        .retain(|known| !over(known));
    code_shell::refresh(cx);
}

fn running(id: RunId) -> bool {
    (services().runs().get(id)).is_some_and(|info| info.state.is_running())
}

/// Does what the sequence asked. What it learns on the way (a member that
/// was running already) is returned as events.
fn perform(
    token: u64,
    actions: Vec<GroupAction>,
    window: &mut Window,
    cx: &mut App,
) -> Vec<GroupEvent> {
    let mut follow = Vec::new();
    for action in actions {
        match action {
            GroupAction::Start(at) => follow.extend(start_member(token, at, cx)),
            GroupAction::Wait(at, wait, timeout) => {
                let task = wait_task(token, at, wait, timeout, cx);
                if let Some(active) = active(token, cx) {
                    active.waits[at] = Some(task);
                }
            }
            GroupAction::Stop(members) => {
                let ids: Vec<RunId> = match active(token, cx) {
                    Some(active) => (members.iter()).filter_map(|at| active.runs[*at]).collect(),
                    None => Vec::new(),
                };
                for id in ids {
                    start::stop(id);
                }
            }
            GroupAction::Reveal(at) => {
                let id = active(token, cx).and_then(|active| active.runs[at]);
                if let Some(id) = id {
                    start::reveal(id, window, cx);
                }
            }
            GroupAction::Notice(text) => start::notice(text, window, cx),
            GroupAction::Up => {}
        }
    }
    follow
}

/// Starts member `at` unless it runs already, in which case it counts as
/// started. A start reports later, from its own task.
fn start_member(token: u64, at: usize, cx: &mut App) -> Option<GroupEvent> {
    let (config, checkout, context, name, reveal) = {
        let active = active(token, cx)?;
        let Some(config) = active.configs[at].clone() else {
            let why = "Its run configuration no longer exists.".to_owned();
            return Some(GroupEvent::StartFailed(at, why));
        };
        let reveal = !std::mem::replace(&mut active.revealed, true);
        (
            config,
            active.checkout.clone(),
            active.context.clone(),
            active.name.clone(),
            reveal,
        )
    };
    if let Some(id) = start::running_config(&config.id, &checkout.folder, cx) {
        let active = active(token, cx)?;
        active.runs[at] = Some(id);
        // Its tab is open already; the dock is what the first start shows.
        active.revealed |= !reveal;
        return Some(GroupEvent::Started(at));
    }
    active(token, cx)?.inflight += 1;
    let outcome = start::begin_in(
        Request {
            target: Target::Config { config, context },
            checkout,
            by: StartedBy::User,
            debug: false,
        },
        Some(GroupTab {
            group: name,
            reveal,
        }),
        cx,
    );
    cx.spawn(async move |cx| {
        let result = (outcome.recv().await)
            .unwrap_or_else(|_| Err("The window is not ready yet.".to_owned()));
        cx.update(|cx| {
            code_shell::in_window(cx, |window, cx| {
                member_started(token, at, result, window, cx)
            });
        });
    })
    .detach();
    None
}

/// A start reported: the member's run, or the sentence saying why not.
fn member_started(
    token: u64,
    at: usize,
    result: Result<RunId, String>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(active) = active(token, cx) else {
        return;
    };
    active.inflight = active.inflight.saturating_sub(1);
    let events = match result {
        Ok(id) => {
            active.runs[at] = Some(id);
            let mut events = vec![GroupEvent::Started(at)];
            // It may have ended before this was told.
            events.extend(ended(at, id));
            events
        }
        Err(sentence) => vec![GroupEvent::StartFailed(at, sentence)],
    };
    deliver(token, events, window, cx);
}

/// What the registry says about a run that is not running, as an event.
fn ended(at: usize, id: RunId) -> Option<GroupEvent> {
    match services().runs().get(id)?.state {
        RunState::Running => None,
        RunState::Exited(code) => Some(GroupEvent::Exited(at, code)),
        RunState::Stopped => Some(GroupEvent::Stopped(at)),
    }
}

/// A run's state changed: if it is a member of a group, the group hears of
/// it ending.
fn member_changed(id: RunId, cx: &mut App) {
    let Some(state) = cx.try_global::<Groups>() else {
        return;
    };
    let mut heard: Vec<(u64, GroupEvent)> = Vec::new();
    for active in &state.active {
        for (at, run) in active.runs.iter().enumerate() {
            if *run == Some(id)
                && let Some(event) = ended(at, id)
            {
                heard.push((active.token, event));
            }
        }
    }
    if heard.is_empty() {
        return;
    }
    code_shell::in_window(cx, |window, cx| {
        for (token, event) in heard {
            deliver(token, vec![event], window, cx);
        }
    });
}

/// Waits for what a member's wait says, on the background executor: a delay is
/// a timer, a port is a short connect tried again and again until the
/// timeout. A port that is open already passes at once.
fn wait_task(token: u64, at: usize, wait: Wait, timeout: u32, cx: &mut App) -> Task<()> {
    cx.spawn(async move |cx| {
        let met = match wait {
            Wait::None => true,
            Wait::Seconds(seconds) => {
                let delay = Duration::from_secs(seconds.into());
                cx.background_executor().timer(delay).await;
                true
            }
            Wait::Port(port) => {
                // Counted in tries, not read off a clock, so that the same
                // holds when time is not real.
                let tries = (u64::from(timeout) * 1000 / PROBE_EVERY.as_millis() as u64).max(1);
                let mut open = false;
                for _ in 0..tries {
                    let probe = cx
                        .background_executor()
                        .spawn(async move { port_open(port) });
                    if probe.await {
                        open = true;
                        break;
                    }
                    cx.background_executor().timer(PROBE_EVERY).await;
                }
                open
            }
        };
        let event = match met {
            true => GroupEvent::WaitMet(at),
            false => GroupEvent::WaitTimedOut(at),
        };
        cx.update(|cx| {
            code_shell::in_window(cx, |window, cx| deliver(token, vec![event], window, cx));
        });
    })
}

/// A group that has something starting or running, for the footer.
pub(super) struct RunningGroup {
    pub id: String,
    pub name: String,
    /// "Dev environment · 2 of 3 running", or what it waits for.
    pub text: String,
    /// The members that run, in list order, with the name of their
    /// configuration.
    pub members: Vec<(String, RunId)>,
    /// Every run the group has had, which the footer lists only here.
    pub covers: Vec<RunId>,
}

/// The project's groups that are starting or have members running. Reads
/// globals only, since the footer asks while the shell draws.
pub(super) fn running_groups(project: &str, cx: &App) -> Vec<RunningGroup> {
    let Some(state) = cx.try_global::<Groups>() else {
        return Vec::new();
    };
    let key = path_key(project);
    let mut listed = Vec::new();
    for active in &state.active {
        if path_key(&active.project) != key {
            continue;
        }
        let members: Vec<(String, RunId)> = (active.runs.iter().enumerate())
            .filter_map(|(at, run)| Some((at, (*run)?)))
            .filter(|(_, id)| running(*id))
            .map(|(at, id)| (active.sequence.label(at).to_owned(), id))
            .collect();
        let starting = active.inflight > 0 || active.sequence.phase() == Phase::Starting;
        if members.is_empty() && !starting {
            continue;
        }
        let total = (0..active.sequence.member_count())
            .filter(|at| !active.sequence.missing(*at))
            .count();
        let progress = match (active.sequence.waiting_for(), active.sequence.starting()) {
            (Some(label), _) => format!("waiting for {label}"),
            (None, Some(label)) => format!("starting {label}"),
            (None, None) => format!("{} of {total} running", members.len()),
        };
        listed.push(RunningGroup {
            id: active.group.clone(),
            name: active.name.clone(),
            text: format!("{} \u{00b7} {progress}", active.name),
            covers: active.runs.iter().flatten().copied().collect(),
            members,
        });
    }
    listed
}
