//! The rules for starting and running a group, as a state machine with no
//! processes and no clock. The caller feeds it what happened (a member
//! started, ended, was stopped by hand, its wait was met or ran out) and
//! does what it answers: start this member, wait like this, stop these,
//! tell the user. One `Sequence` is one press on a group.
use super::group::{OnFailedStart, OnMemberExit, RunGroup, StartOrder, Wait};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Whether something on this machine accepts connections on `port`. A
/// refused or unanswered connect is a no; it never waits for long, but it is
/// still a blocking call, so callers keep it off the UI thread.
pub fn port_open(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
}

/// What happened to a member, as the caller saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The member's process is running (it was already, or it was started
    /// for this group).
    Started(usize),
    /// The member could not be started; the sentence says why.
    StartFailed(usize, String),
    /// The member ended on its own; the code is absent when a signal did it.
    Exited(usize, Option<i32>),
    /// The user stopped the member. Not a failure.
    Stopped(usize),
    WaitMet(usize),
    WaitTimedOut(usize),
    /// The user asked to stop the whole group.
    StopAll,
}

/// What the caller is to do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Start the member, then report `Started` or `StartFailed`. The caller
    /// does not start one that is running already; it reports `Started`.
    Start(usize),
    /// Watch for the member's wait, then report `WaitMet` or
    /// `WaitTimedOut`. The timeout is in seconds.
    Wait(usize, Wait, u32),
    /// Stop these members, in this order.
    Stop(Vec<usize>),
    /// Show the member's tab: it failed.
    Reveal(usize),
    /// Tell the user.
    Notice(String),
    /// Every member has been started or skipped.
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Members are still being started.
    Starting,
    /// All members were started or skipped.
    Up,
    /// Not starting any more members, the ones started keep running.
    Halted,
    /// Stopped as a whole.
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Pending,
    /// Being started; its process is not known to be running yet.
    Starting,
    Waiting,
    Up,
    /// Not started, on purpose or because it is gone.
    Skipped,
    /// It failed; its process may still run (a wait that ran out).
    Failed {
        running: bool,
    },
    /// It ended or was stopped.
    Gone,
}

impl State {
    fn running(self) -> bool {
        matches!(
            self,
            State::Waiting | State::Up | State::Failed { running: true }
        )
    }
}

struct Member {
    label: String,
    wait: Wait,
    timeout: u32,
    missing: bool,
}

pub struct Sequence {
    name: String,
    order: StartOrder,
    on_failed: OnFailedStart,
    on_exit: OnMemberExit,
    members: Vec<Member>,
    state: Vec<State>,
    phase: Phase,
}

impl Sequence {
    /// The sequence for `group`. `label_of` names a configuration that still
    /// exists. An error is the sentence for a group that cannot be started
    /// at all.
    pub fn new(
        group: &RunGroup,
        label_of: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        if group.members.is_empty() {
            return Err(format!(
                "\u{201c}{}\u{201d} has no run configurations.",
                group.name
            ));
        }
        let members: Vec<Member> = (group.members.iter())
            .map(|member| {
                let found = label_of(&member.config);
                let label = match &found {
                    Some(label) => label.clone(),
                    None if member.label.is_empty() => "A run configuration".to_owned(),
                    None => member.label.clone(),
                };
                Member {
                    label,
                    wait: member.wait,
                    timeout: member.timeout,
                    missing: found.is_none(),
                }
            })
            .collect();
        if members.iter().all(|member| member.missing) {
            return Err(format!(
                "\u{201c}{}\u{201d} cannot start: none of its run configurations exist any more.",
                group.name
            ));
        }
        let count = members.len();
        Ok(Self {
            name: group.name.clone(),
            order: group.order,
            on_failed: group.on_failed_start,
            on_exit: group.on_member_exit,
            members,
            state: vec![State::Pending; count],
            phase: Phase::Starting,
        })
    }

    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn label(&self, member: usize) -> &str {
        &self.members[member].label
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Whether the member is one that was left out because its
    /// configuration is gone.
    pub fn missing(&self, member: usize) -> bool {
        self.members[member].missing
    }

    /// The member being waited for, while the group waits.
    pub fn waiting_for(&self) -> Option<&str> {
        self.member_in(State::Waiting)
    }

    /// The member being started, while the group does.
    pub fn starting(&self) -> Option<&str> {
        self.member_in(State::Starting)
    }

    fn member_in(&self, wanted: State) -> Option<&str> {
        let at = self.state.iter().position(|state| *state == wanted)?;
        Some(&self.members[at].label)
    }

    /// The first actions: members that are gone are skipped with a notice,
    /// then the first member (or all of them, in parallel) is started.
    pub fn begin(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        for (at, member) in self.members.iter().enumerate() {
            if member.missing {
                self.state[at] = State::Skipped;
                actions.push(Action::Notice(format!(
                    "\u{201c}{}\u{201d} is skipped in \u{201c}{}\u{201d}: its run configuration no longer exists.",
                    member.label, self.name
                )));
            }
        }
        match self.order {
            StartOrder::Sequential => actions.extend(self.advance()),
            StartOrder::Parallel => {
                for at in 0..self.members.len() {
                    if self.state[at] == State::Pending {
                        self.state[at] = State::Starting;
                        actions.push(Action::Start(at));
                    }
                }
            }
        }
        actions
    }

    pub fn event(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::StopAll => self.stop_all(),
            Event::Started(at) => self.started(at),
            Event::StartFailed(at, why) => match self.state[at] {
                State::Starting => self.fail(at, why, false),
                _ => Vec::new(),
            },
            Event::WaitMet(at) => match self.state[at] {
                State::Waiting => {
                    self.state[at] = State::Up;
                    self.advance()
                }
                _ => Vec::new(),
            },
            Event::WaitTimedOut(at) => match (self.state[at], self.members[at].wait) {
                (State::Waiting, Wait::Port(port)) => {
                    let reason = format!(
                        "\u{201c}{}\u{201d} did not open port {port} within {} seconds.",
                        self.members[at].label, self.members[at].timeout
                    );
                    self.fail(at, reason, true)
                }
                _ => Vec::new(),
            },
            Event::Exited(at, code) => self.exited(at, code),
            Event::Stopped(at) => self.stopped(at),
        }
    }

    fn started(&mut self, at: usize) -> Vec<Action> {
        if self.state[at] != State::Starting {
            return Vec::new();
        }
        if self.phase == Phase::Stopped {
            // Stopped while this one was being started.
            self.state[at] = State::Gone;
            return vec![Action::Stop(vec![at])];
        }
        let member = &self.members[at];
        let waits = self.order == StartOrder::Sequential && member.wait != Wait::None;
        if !waits {
            self.state[at] = State::Up;
            return self.advance();
        }
        self.state[at] = State::Waiting;
        vec![Action::Wait(at, member.wait, member.timeout)]
    }

    fn exited(&mut self, at: usize, code: Option<i32>) -> Vec<Action> {
        let ended = match code {
            Some(code) => format!("exited with code {code}"),
            None => "ended".to_owned(),
        };
        let label = self.members[at].label.clone();
        match self.state[at] {
            State::Starting | State::Waiting => {
                let reason = format!("\u{201c}{label}\u{201d} {ended} before it was ready.");
                self.fail(at, reason, false)
            }
            State::Up | State::Failed { running: true } => {
                self.state[at] = State::Gone;
                if self.on_exit != OnMemberExit::StopGroup || self.phase == Phase::Stopped {
                    return Vec::new();
                }
                let mut actions = vec![Action::Notice(format!(
                    "\u{201c}{}\u{201d} was stopped because \u{201c}{label}\u{201d} {ended}.",
                    self.name
                ))];
                actions.extend(self.stop_everything());
                actions
            }
            _ => Vec::new(),
        }
    }

    fn stopped(&mut self, at: usize) -> Vec<Action> {
        let before = self.state[at];
        if !matches!(
            before,
            State::Starting | State::Waiting | State::Up | State::Failed { running: true }
        ) {
            return Vec::new();
        }
        self.state[at] = State::Gone;
        let starting = matches!(before, State::Starting | State::Waiting);
        if starting && self.order == StartOrder::Sequential && self.phase == Phase::Starting {
            // The user ended the member the sequence was waiting on: that
            // is their answer, not a failure.
            self.phase = Phase::Halted;
            self.skip_pending();
            return vec![Action::Notice(format!(
                "\u{201c}{}\u{201d} stopped starting because \u{201c}{}\u{201d} was stopped.",
                self.name, self.members[at].label
            ))];
        }
        self.settle()
    }

    /// A member failed to start. What follows is the group's setting.
    fn fail(&mut self, at: usize, reason: String, running: bool) -> Vec<Action> {
        self.state[at] = State::Failed { running };
        let name = &self.name;
        match self.on_failed {
            OnFailedStart::StopGroup => {
                let mut actions = vec![
                    Action::Notice(format!("\u{201c}{name}\u{201d} was stopped. {reason}")),
                    Action::Reveal(at),
                ];
                actions.extend(self.stop_everything());
                actions
            }
            OnFailedStart::DoNotStartRest => {
                let mut actions = vec![
                    Action::Notice(format!(
                        "\u{201c}{name}\u{201d} did not start the rest. {reason}"
                    )),
                    Action::Reveal(at),
                ];
                if self.phase == Phase::Starting {
                    self.phase = Phase::Halted;
                    self.skip_pending();
                }
                actions.extend(self.settle());
                actions
            }
            OnFailedStart::Continue => {
                let mut actions = vec![
                    Action::Notice(format!(
                        "\u{201c}{name}\u{201d} skipped \u{201c}{}\u{201d}. {reason}",
                        self.members[at].label
                    )),
                    Action::Reveal(at),
                ];
                actions.extend(self.advance());
                actions
            }
        }
    }

    /// Starts the next member of a sequential group, or finishes.
    fn advance(&mut self) -> Vec<Action> {
        if self.phase != Phase::Starting {
            return Vec::new();
        }
        if self.order == StartOrder::Sequential {
            if let Some(next) = (self.state.iter()).position(|state| *state == State::Pending) {
                self.state[next] = State::Starting;
                return vec![Action::Start(next)];
            }
        }
        self.settle()
    }

    /// Up once nothing is left to start or wait for.
    fn settle(&mut self) -> Vec<Action> {
        let open = (self.state.iter())
            .any(|state| matches!(state, State::Pending | State::Starting | State::Waiting));
        if self.phase == Phase::Starting && !open {
            self.phase = Phase::Up;
            return vec![Action::Up];
        }
        Vec::new()
    }

    fn skip_pending(&mut self) {
        for state in &mut self.state {
            if *state == State::Pending {
                *state = State::Skipped;
            }
        }
    }

    fn stop_all(&mut self) -> Vec<Action> {
        if self.phase == Phase::Stopped {
            return Vec::new();
        }
        self.stop_everything()
    }

    /// Ends the group: nothing more is started, what runs is stopped last
    /// member first.
    fn stop_everything(&mut self) -> Vec<Action> {
        self.phase = Phase::Stopped;
        self.skip_pending();
        let mut running: Vec<usize> = (0..self.state.len())
            .filter(|at| self.state[*at].running())
            .collect();
        running.reverse();
        for at in &running {
            self.state[*at] = State::Gone;
        }
        match running.is_empty() {
            true => Vec::new(),
            false => vec![Action::Stop(running)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::run::group::{GroupMember, DEFAULT_WAIT_TIMEOUT};

    fn group(order: StartOrder, waits: &[Wait]) -> RunGroup {
        RunGroup {
            id: "g".into(),
            name: "Dev".into(),
            order,
            members: waits
                .iter()
                .enumerate()
                .map(|(at, wait)| GroupMember {
                    config: format!("c{at}"),
                    label: ["Tunnel", "Backend", "Frontend", "Extra"][at].into(),
                    wait: *wait,
                    timeout: DEFAULT_WAIT_TIMEOUT,
                })
                .collect(),
            ..RunGroup::default()
        }
    }

    fn sequence(group: &RunGroup) -> Sequence {
        let labels: Vec<(String, String)> = (group.members.iter())
            .map(|member| (member.config.clone(), member.label.clone()))
            .collect();
        Sequence::new(group, |id| {
            labels
                .iter()
                .find(|(config, _)| config == id)
                .map(|(_, label)| label.clone())
        })
        .unwrap()
    }

    fn notices(actions: &[Action]) -> Vec<&str> {
        (actions.iter())
            .filter_map(|action| match action {
                Action::Notice(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_sequential_group_starts_each_member_after_the_previous_ones_wait() {
        let waits = [Wait::None, Wait::Seconds(3), Wait::Port(8000)];
        let mut run = sequence(&group(StartOrder::Sequential, &waits));
        assert_eq!(run.begin(), [Action::Start(0)]);
        // No wait: the next one follows at once.
        assert_eq!(run.event(Event::Started(0)), [Action::Start(1)]);
        assert_eq!(run.starting(), Some("Backend"));
        assert_eq!(
            run.event(Event::Started(1)),
            [Action::Wait(1, Wait::Seconds(3), 60)]
        );
        assert_eq!(run.waiting_for(), Some("Backend"));
        assert_eq!(run.event(Event::WaitMet(1)), [Action::Start(2)]);
        assert_eq!(
            run.event(Event::Started(2)),
            [Action::Wait(2, Wait::Port(8000), 60)]
        );
        assert_eq!(run.event(Event::WaitMet(2)), [Action::Up]);
        assert_eq!(run.phase(), Phase::Up);
        assert_eq!(run.waiting_for(), None);
        // Events that no longer apply change nothing.
        assert!(run.event(Event::WaitMet(2)).is_empty());
        assert!(run.event(Event::WaitTimedOut(2)).is_empty());
    }

    #[test]
    fn a_parallel_group_starts_everything_and_ignores_waits() {
        let waits = [Wait::Seconds(30), Wait::Port(1), Wait::None];
        let mut run = sequence(&group(StartOrder::Parallel, &waits));
        assert_eq!(
            run.begin(),
            [Action::Start(0), Action::Start(1), Action::Start(2)]
        );
        assert!(run.event(Event::Started(2)).is_empty());
        assert!(run.event(Event::Started(0)).is_empty());
        assert_eq!(run.phase(), Phase::Starting);
        assert_eq!(run.event(Event::Started(1)), [Action::Up]);
    }

    /// Member 1 of three fails after member 0 is up, by each way a start can
    /// fail, under each setting.
    #[test]
    fn each_failed_start_setting_decides_what_follows() {
        let failures: [(&str, Event, bool); 3] = [
            (
                "cannot run",
                Event::StartFailed(1, "cannot run".into()),
                false,
            ),
            ("exits", Event::Exited(1, Some(2)), false),
            ("times out", Event::WaitTimedOut(1), true),
        ];
        for (name, failure, running) in failures {
            for on_failed in [
                OnFailedStart::StopGroup,
                OnFailedStart::DoNotStartRest,
                OnFailedStart::Continue,
            ] {
                let mut spec = group(
                    StartOrder::Sequential,
                    &[Wait::None, Wait::Port(9), Wait::None],
                );
                spec.on_failed_start = on_failed;
                let mut run = sequence(&spec);
                run.begin();
                run.event(Event::Started(0));
                if name != "cannot run" {
                    run.event(Event::Started(1));
                }
                let actions = run.event(failure.clone());
                let at = format!("{name}, {on_failed:?}");
                assert_eq!(notices(&actions).len(), 1, "{at}");
                assert!(actions.contains(&Action::Reveal(1)), "{at}");
                match on_failed {
                    OnFailedStart::StopGroup => {
                        let mut stopped = vec![0];
                        if running {
                            stopped.insert(0, 1);
                        }
                        assert!(
                            actions.contains(&Action::Stop(stopped)),
                            "{at}: {actions:?}"
                        );
                        assert_eq!(run.phase(), Phase::Stopped, "{at}");
                    }
                    OnFailedStart::DoNotStartRest => {
                        assert!(
                            !actions
                                .iter()
                                .any(|a| matches!(a, Action::Start(_) | Action::Stop(_))),
                            "{at}"
                        );
                        assert_eq!(run.phase(), Phase::Halted, "{at}");
                    }
                    OnFailedStart::Continue => {
                        assert!(actions.contains(&Action::Start(2)), "{at}");
                        assert!(
                            !actions.iter().any(|a| matches!(a, Action::Stop(_))),
                            "{at}"
                        );
                        assert_eq!(run.event(Event::Started(2)), [Action::Up], "{at}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_notice_names_the_member_and_the_reason() {
        let mut run = sequence(&group(
            StartOrder::Sequential,
            &[Wait::None, Wait::Port(8000), Wait::None],
        ));
        run.begin();
        run.event(Event::Started(0));
        run.event(Event::Started(1));
        let actions = run.event(Event::WaitTimedOut(1));
        assert_eq!(
            notices(&actions),
            ["\u{201c}Dev\u{201d} did not start the rest. \u{201c}Backend\u{201d} did not open port 8000 within 60 seconds."]
        );
        let mut run = sequence(&group(
            StartOrder::Sequential,
            &[Wait::Seconds(5), Wait::None],
        ));
        run.begin();
        run.event(Event::Started(0));
        let actions = run.event(Event::Exited(0, None));
        assert_eq!(
            notices(&actions),
            ["\u{201c}Dev\u{201d} did not start the rest. \u{201c}Tunnel\u{201d} ended before it was ready."]
        );
    }

    #[test]
    fn a_later_exit_stops_the_group_or_leaves_the_others() {
        for on_exit in [OnMemberExit::StopGroup, OnMemberExit::LeaveOthers] {
            let mut spec = group(
                StartOrder::Sequential,
                &[Wait::None, Wait::None, Wait::None],
            );
            spec.on_member_exit = on_exit;
            let mut run = sequence(&spec);
            run.begin();
            run.event(Event::Started(0));
            run.event(Event::Started(1));
            run.event(Event::Started(2));
            let actions = run.event(Event::Exited(1, Some(1)));
            match on_exit {
                OnMemberExit::StopGroup => {
                    assert_eq!(notices(&actions).len(), 1);
                    assert!(notices(&actions)[0].contains("Backend"));
                    assert!(actions.contains(&Action::Stop(vec![2, 0])));
                    assert_eq!(run.phase(), Phase::Stopped);
                }
                OnMemberExit::LeaveOthers => {
                    assert!(actions.is_empty());
                    assert_eq!(run.phase(), Phase::Up);
                }
            }
        }
    }

    #[test]
    fn a_member_the_user_stopped_is_never_a_failure() {
        // While the group is up, under the strictest settings.
        let mut spec = group(StartOrder::Sequential, &[Wait::None, Wait::None]);
        spec.on_member_exit = OnMemberExit::StopGroup;
        spec.on_failed_start = OnFailedStart::StopGroup;
        let mut run = sequence(&spec);
        run.begin();
        run.event(Event::Started(0));
        run.event(Event::Started(1));
        assert!(run.event(Event::Stopped(0)).is_empty());
        assert_eq!(run.phase(), Phase::Up);

        // While the sequence waits for it: it ends quietly, nothing is stopped.
        let mut run = sequence(&group(
            StartOrder::Sequential,
            &[Wait::Seconds(5), Wait::None],
        ));
        run.begin();
        run.event(Event::Started(0));
        let actions = run.event(Event::Stopped(0));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::Stop(_) | Action::Start(_))));
        assert_eq!(run.phase(), Phase::Halted);

        // In a parallel group.
        let mut spec = group(StartOrder::Parallel, &[Wait::None, Wait::None]);
        spec.on_member_exit = OnMemberExit::StopGroup;
        let mut run = sequence(&spec);
        run.begin();
        run.event(Event::Started(0));
        run.event(Event::Started(1));
        assert!(run.event(Event::Stopped(1)).is_empty());
        assert_ne!(run.phase(), Phase::Stopped);
    }

    #[test]
    fn running_members_are_started_through_the_same_events() {
        // The caller reports a member that was running already as started, so
        // a second press only starts the missing ones and passes a wait whose
        // port is open at once.
        let mut run = sequence(&group(
            StartOrder::Sequential,
            &[Wait::Port(5), Wait::None, Wait::None],
        ));
        assert_eq!(run.begin(), [Action::Start(0)]);
        run.event(Event::Started(0));
        assert_eq!(run.event(Event::WaitMet(0)), [Action::Start(1)]);
    }

    #[test]
    fn stop_all_stops_the_running_members_last_first() {
        let mut run = sequence(&group(
            StartOrder::Sequential,
            &[Wait::None, Wait::None, Wait::Seconds(9), Wait::None],
        ));
        run.begin();
        run.event(Event::Started(0));
        run.event(Event::Started(1));
        run.event(Event::Started(2));
        run.event(Event::Exited(0, Some(0)));
        // Member 2 is waiting, 3 was never started, 0 is gone.
        assert_eq!(run.event(Event::StopAll), [Action::Stop(vec![2, 1])]);
        assert_eq!(run.phase(), Phase::Stopped);
        assert!(run.event(Event::StopAll).is_empty());
        // What the stop produces is not news.
        assert!(run.event(Event::Stopped(2)).is_empty());
        assert!(run.event(Event::WaitMet(2)).is_empty());
    }

    #[test]
    fn a_member_that_starts_after_the_group_was_stopped_is_stopped_too() {
        let mut run = sequence(&group(StartOrder::Parallel, &[Wait::None, Wait::None]));
        run.begin();
        run.event(Event::Started(0));
        assert_eq!(run.event(Event::StopAll), [Action::Stop(vec![0])]);
        assert_eq!(run.event(Event::Started(1)), [Action::Stop(vec![1])]);
    }

    #[test]
    fn a_parallel_failure_stops_the_group_or_leaves_the_rest() {
        let mut spec = group(StartOrder::Parallel, &[Wait::None, Wait::None, Wait::None]);
        spec.on_failed_start = OnFailedStart::StopGroup;
        let mut run = sequence(&spec);
        run.begin();
        run.event(Event::Started(0));
        let actions = run.event(Event::StartFailed(1, "No.".into()));
        assert!(actions.contains(&Action::Stop(vec![0])));
        assert_eq!(run.event(Event::Started(2)), [Action::Stop(vec![2])]);

        spec.on_failed_start = OnFailedStart::DoNotStartRest;
        let mut run = sequence(&spec);
        run.begin();
        run.event(Event::Started(0));
        let actions = run.event(Event::StartFailed(1, "No.".into()));
        assert!(!actions.iter().any(|a| matches!(a, Action::Stop(_))));
        assert!(run.event(Event::Started(2)).is_empty());
    }

    #[test]
    fn members_whose_configurations_are_gone_are_skipped_with_a_notice() {
        let spec = group(
            StartOrder::Sequential,
            &[Wait::None, Wait::None, Wait::None],
        );
        let mut run =
            Sequence::new(&spec, |id| (id == "c2").then(|| "Frontend".to_owned())).unwrap();
        let actions = run.begin();
        assert_eq!(notices(&actions).len(), 2);
        assert!(notices(&actions)[0].contains("Tunnel"));
        assert!(notices(&actions)[1].contains("Backend"));
        assert_eq!(actions.last(), Some(&Action::Start(2)));
        assert!(run.missing(0) && !run.missing(2));
        assert_eq!(run.event(Event::Started(2)), [Action::Up]);

        let error = Sequence::new(&spec, |_| None).err().unwrap();
        assert!(
            error.contains("none of its run configurations exist"),
            "{error}"
        );
        let empty = RunGroup::named("Empty");
        assert_eq!(
            Sequence::new(&empty, |_| None).err().unwrap(),
            "\u{201c}Empty\u{201d} has no run configurations."
        );
    }

    #[test]
    fn a_closed_port_is_not_open_and_a_listening_one_is() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(port_open(port));
        drop(listener);
        assert!(!port_open(port));
    }
}
