//! Who drives a page, the user or an agent (context.md K47, K48): the
//! page's [`ControlState`], fed by the app's input watcher and the agent's
//! commands, and the toolbar's "Agent · <session>" label with its control
//! line and "Give back to agent" button. The label goes 20 s after the
//! agent's last command; a timer repaints at that moment, and once a second
//! while the user's countdown runs, never every frame.
use super::*;
use monocode_core::browser::control::{AGENT_ACTIVE, ControlState, UserControl, UserInput};

/// The agent session last using a page.
#[derive(Clone, Debug, PartialEq)]
struct AgentMark {
    session: i64,
    title: String,
    at: Instant,
}

/// A page's control bookkeeping, shared by its hosts.
#[derive(Default)]
pub(super) struct Presence {
    control: RefCell<ControlState>,
    agent: RefCell<Option<AgentMark>>,
    /// Asks the page to repaint (and reschedule) from outside GPUI.
    changed: RefCell<Option<async_channel::Sender<()>>>,
    /// Tests set the time.
    #[cfg(test)]
    clock: Cell<Option<Instant>>,
}

/// What the toolbar shows while an agent uses the page.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PresenceLabel {
    /// "Agent · <session title>".
    pub agent: String,
    /// "You have control · gives back in Ns" while the user holds it.
    pub control: Option<String>,
}

/// The kind of input an AppKit event is, for [`ControlState::user_input`].
#[cfg(target_os = "macos")]
pub(super) fn input_of(kind: objc2_app_kit::NSEventType) -> Option<UserInput> {
    use objc2_app_kit::NSEventType as T;
    match kind {
        T::KeyDown => Some(UserInput::Key),
        T::LeftMouseDown | T::RightMouseDown | T::OtherMouseDown => Some(UserInput::Click),
        T::ScrollWheel => Some(UserInput::Scroll),
        T::MouseMoved | T::LeftMouseDragged | T::RightMouseDragged | T::OtherMouseDragged => {
            Some(UserInput::MouseMove)
        }
        _ => None,
    }
}

/// Whether a new page takes focus: an agent's actions never move it.
fn popup_foreground(foreground: bool, agent_active: bool, user_control: bool) -> bool {
    foreground && (!agent_active || user_control)
}

/// Whole seconds left, rounded up: what a countdown shows.
fn seconds_left(left: Duration) -> u128 {
    left.as_nanos().div_ceil(1_000_000_000)
}

fn label_at(
    agent: Option<&AgentMark>,
    control: &ControlState,
    now: Instant,
) -> Option<PresenceLabel> {
    let agent = agent.filter(|_| control.agent_active(now))?;
    let title = agent.title.trim();
    Some(PresenceLabel {
        agent: if title.is_empty() {
            "Agent".to_owned()
        } else {
            format!("Agent · {title}")
        },
        control: control.user_control(now).map(|c| {
            format!(
                "You have control · gives back in {}s",
                seconds_left(c.resumes_in)
            )
        }),
    })
}

/// How long until [`label_at`] reads differently on its own: the agent's
/// expiry, or the countdown's next step. `None` while nothing shows.
fn next_change(
    agent: Option<&AgentMark>,
    control: &ControlState,
    now: Instant,
) -> Option<Duration> {
    let agent = agent.filter(|_| control.agent_active(now))?;
    let expiry = (agent.at + AGENT_ACTIVE).saturating_duration_since(now);
    let step = control.user_control(now).map(|c| {
        let part = Duration::from_nanos((c.resumes_in.as_nanos() % 1_000_000_000) as u64);
        if part.is_zero() {
            Duration::from_secs(1)
        } else {
            part
        }
    });
    Some(step.map_or(expiry, |step| step.min(expiry)))
}

impl Shared {
    pub(super) fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = self.presence.clock.get() {
            return now;
        }
        Instant::now()
    }

    /// Asks the page to repaint (and its tab), from outside GPUI.
    pub(super) fn repaint(&self) {
        if let Some(changed) = self.presence.changed.borrow().as_ref() {
            let _ = changed.try_send(());
        }
    }

    /// The user gave the page input (the app's input watcher).
    pub(super) fn user_input(&self, input: UserInput, now: Instant) {
        self.last_input.set(Some(now));
        let presence = &self.presence;
        let held = {
            let mut control = presence.control.borrow_mut();
            control.user_input(input, now);
            control.user_control(now).is_some()
        };
        if held && let Some(changed) = presence.changed.borrow().as_ref() {
            let _ = changed.try_send(());
        }
    }
}

impl BrowserPage {
    /// Repaints whenever the input watcher hands the user control.
    pub(super) fn watch_presence(self, cx: &mut Context<Self>) -> Self {
        let (tx, rx) = async_channel::unbounded();
        *self.host.shared.presence.changed.borrow_mut() = Some(tx);
        cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                if this
                    .update(cx, |this, cx| this.presence_changed(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        self
    }

    /// Session `session`, titled `title`, sent the page a command now.
    pub(crate) fn agent_used(&mut self, session: i64, title: String, cx: &mut Context<Self>) {
        let shared = &self.host.shared;
        let now = shared.now();
        shared.presence.control.borrow_mut().agent_used(now);
        *shared.presence.agent.borrow_mut() = Some(AgentMark {
            session,
            title,
            at: now,
        });
        self.presence_changed(cx);
    }

    /// The agent session last using the page while it counts as active.
    pub(crate) fn active_agent(&self) -> Option<i64> {
        let shared = &self.host.shared;
        let now = shared.now();
        let control = shared.presence.control.borrow();
        let agent = shared.presence.agent.borrow();
        agent
            .as_ref()
            .filter(|_| control.agent_active(now))
            .map(|a| a.session)
    }

    /// Whether the user holds control of the page now (K48).
    pub(crate) fn user_control(&self) -> Option<UserControl> {
        let shared = &self.host.shared;
        shared.presence.control.borrow().user_control(shared.now())
    }

    /// Whether a page this one opens takes the user's focus: as Chromium
    /// asks, except while an agent drives the page (its clicks open tabs
    /// in the background, K39) and the user has not taken control.
    pub(super) fn popup_foreground(&self, foreground: bool) -> bool {
        popup_foreground(
            foreground,
            self.active_agent().is_some(),
            self.user_control().is_some(),
        )
    }

    /// The toolbar's "Give back to agent".
    pub(crate) fn give_back(&mut self, cx: &mut Context<Self>) {
        self.host.shared.presence.control.borrow_mut().give_back();
        self.presence_changed(cx);
    }

    /// What the toolbar shows about the agent now, if anything.
    pub(crate) fn presence_label(&self) -> Option<PresenceLabel> {
        let shared = &self.host.shared;
        let agent = shared.presence.agent.borrow();
        label_at(
            agent.as_ref(),
            &shared.presence.control.borrow(),
            shared.now(),
        )
    }

    /// Repaints the page and its tab, and wakes again at the next change.
    fn presence_changed(&mut self, cx: &mut Context<Self>) {
        let wait = {
            let shared = &self.host.shared;
            let agent = shared.presence.agent.borrow();
            next_change(
                agent.as_ref(),
                &shared.presence.control.borrow(),
                shared.now(),
            )
        };
        self.presence_timer = wait.map(|wait| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |this, cx| this.presence_changed(cx));
            })
        });
        cx.emit(PageChanged);
        cx.notify();
    }

    /// The toolbar's agent label, control line and button.
    pub(super) fn presence_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let label = self.presence_label()?;
        Some(
            div()
                .id("browser-agent-presence")
                .flex()
                .items_center()
                .gap_1()
                .min_w_0()
                .flex_shrink(1.)
                .text_size(px(11.))
                .child(
                    div()
                        .id("browser-agent-label")
                        .debug_selector(|| "browser-agent-label".into())
                        .flex()
                        .items_center()
                        .gap_1()
                        .min_w(px(60.))
                        .max_w(px(220.))
                        .h(px(24.))
                        .px_2()
                        .rounded_md()
                        .bg(crate::theme::accent_color().opacity(0.12))
                        .text_color(content(0.75))
                        .child(icon("bot", 12.).flex_shrink_0())
                        .child(div().min_w_0().truncate().child(label.agent)),
                )
                .when_some(label.control, |d, control| {
                    d.child(
                        div()
                            .debug_selector(|| "browser-agent-control".into())
                            .flex_shrink_0()
                            .whitespace_nowrap()
                            .text_color(content(0.6))
                            .child(control),
                    )
                    .child(
                        div()
                            .id("browser-agent-give-back")
                            .debug_selector(|| "browser-agent-give-back".into())
                            .role(gpui::Role::Button)
                            .aria_label("Give back to agent")
                            .flex_shrink_0()
                            .px_2()
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .rounded_md()
                            .cursor_pointer()
                            .whitespace_nowrap()
                            .text_color(content(0.75))
                            .bg(content(0.06))
                            .hover(|s| s.bg(content(0.12)).text_color(content(1.)))
                            .child("Give back to agent")
                            .on_click(cx.listener(|this, _, _, cx| this.give_back(cx))),
                    )
                })
                .into_any_element(),
        )
    }

    /// Sets the page's clock (tests).
    #[cfg(test)]
    pub(crate) fn set_clock_for_test(&self, now: Instant) {
        self.host.shared.presence.clock.set(Some(now));
    }

    /// The input watcher saw `input` on the page (tests).
    #[cfg(test)]
    pub(crate) fn user_input_for_test(&self, input: UserInput) {
        let shared = &self.host.shared;
        shared.user_input(input, shared.now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn mark(at: Instant) -> AgentMark {
        AgentMark {
            session: 7,
            title: "Fix the login form".into(),
            at,
        }
    }

    #[test]
    fn tabs_an_agents_clicks_open_stay_in_the_background() {
        assert!(popup_foreground(true, false, false));
        assert!(!popup_foreground(true, true, false));
        // The user's own click while an agent works took control.
        assert!(popup_foreground(true, true, true));
        assert!(!popup_foreground(false, false, true));
    }

    #[test]
    fn the_label_names_the_session_and_counts_down_the_users_hold() {
        let t = Instant::now();
        let mut control = ControlState::default();
        assert_eq!(label_at(None, &control, t), None);
        control.agent_used(t);
        let agent = mark(t);
        assert_eq!(
            label_at(Some(&agent), &control, t),
            Some(PresenceLabel {
                agent: "Agent · Fix the login form".into(),
                control: None,
            })
        );
        control.user_input(UserInput::Click, t + S);
        let label = label_at(Some(&agent), &control, t + S + S / 2).unwrap();
        assert_eq!(
            label.control.as_deref(),
            Some("You have control · gives back in 16s")
        );
        let label = label_at(Some(&agent), &control, t + 2 * S).unwrap();
        assert_eq!(
            label.control.as_deref(),
            Some("You have control · gives back in 15s")
        );
        // Gone 20 s after the agent's last command.
        assert!(label_at(Some(&agent), &control, t + 19 * S).is_some());
        assert_eq!(label_at(Some(&agent), &control, t + 20 * S), None);
        let untitled = AgentMark {
            title: " ".into(),
            ..mark(t)
        };
        assert_eq!(
            label_at(Some(&untitled), &control, t).unwrap().agent,
            "Agent"
        );
    }

    #[test]
    fn the_label_wakes_at_expiry_or_the_countdowns_next_step() {
        let t = Instant::now();
        let mut control = ControlState::default();
        let agent = mark(t);
        assert_eq!(next_change(Some(&agent), &control, t), None);
        control.agent_used(t);
        assert_eq!(next_change(Some(&agent), &control, t + 5 * S), Some(15 * S));
        control.user_input(UserInput::Key, t + 5 * S);
        assert_eq!(
            next_change(Some(&agent), &control, t + 5 * S + S / 4),
            Some(S * 3 / 4)
        );
        assert_eq!(next_change(Some(&agent), &control, t + 6 * S), Some(S));
        // The expiry comes first near the end.
        assert_eq!(
            next_change(Some(&agent), &control, t + 19 * S + S / 2),
            Some(S / 2)
        );
        assert_eq!(next_change(Some(&agent), &control, t + 20 * S), None);
    }

    fn page(cx: &mut gpui::TestAppContext) -> Entity<BrowserPage> {
        cx.new(|cx| BrowserPage::new(String::new(), String::new(), 100, "/tmp".into(), cx))
    }

    #[gpui::test]
    fn a_click_takes_control_and_a_scroll_does_not(cx: &mut gpui::TestAppContext) {
        let page = page(cx);
        let t = Instant::now();
        page.update(cx, |page, cx| {
            page.set_clock_for_test(t);
            // No agent: a click takes nothing.
            page.user_input_for_test(UserInput::Click);
            assert_eq!(page.user_control(), None);
            page.agent_used(7, "Fix it".into(), cx);
            page.user_input_for_test(UserInput::Scroll);
            page.user_input_for_test(UserInput::MouseMove);
            assert_eq!(page.user_control(), None);
            // Scrolling still counts as recent input (K34).
            assert!(page.user_active());
            page.user_input_for_test(UserInput::Click);
            assert_eq!(
                page.user_control(),
                Some(UserControl { resumes_in: 16 * S })
            );
            page.give_back(cx);
            assert_eq!(page.user_control(), None);
            assert_eq!(page.presence_label().unwrap().control, None);
            page.user_input_for_test(UserInput::Key);
            assert!(page.user_control().is_some());
        });
    }

    #[gpui::test]
    fn the_label_goes_by_itself_20_s_after_the_last_command(cx: &mut gpui::TestAppContext) {
        let page = page(cx);
        let changes = Rc::new(Cell::new(0));
        cx.update(|cx| {
            let changes = changes.clone();
            cx.subscribe(&page, move |_, _: &PageChanged, _| {
                changes.set(changes.get() + 1)
            })
            .detach();
        });
        let t = Instant::now();
        page.update(cx, |page, cx| {
            page.set_clock_for_test(t);
            page.agent_used(7, "Fix it".into(), cx);
            assert_eq!(page.active_agent(), Some(7));
        });
        cx.run_until_parked();
        let after_command = changes.get();
        assert!(after_command >= 1);
        // Nothing changes until the expiry: no repaint either.
        page.update(cx, |page, _| page.set_clock_for_test(t + 10 * S));
        cx.executor().advance_clock(10 * S);
        cx.run_until_parked();
        assert_eq!(changes.get(), after_command);
        page.update(cx, |page, _| page.set_clock_for_test(t + 20 * S));
        cx.executor().advance_clock(10 * S);
        cx.run_until_parked();
        assert_eq!(changes.get(), after_command + 1);
        page.read_with(cx, |page, _| {
            assert_eq!(page.presence_label(), None);
            assert_eq!(page.active_agent(), None);
        });
        // And no timer runs after that.
        cx.executor().advance_clock(60 * S);
        cx.run_until_parked();
        assert_eq!(changes.get(), after_command + 1);
    }

    #[gpui::test]
    fn the_input_watcher_repaints_when_the_user_takes_control(cx: &mut gpui::TestAppContext) {
        let page = page(cx);
        let changes = Rc::new(Cell::new(0));
        cx.update(|cx| {
            let changes = changes.clone();
            cx.subscribe(&page, move |_, _: &PageChanged, _| {
                changes.set(changes.get() + 1)
            })
            .detach();
        });
        let t = Instant::now();
        page.update(cx, |page, cx| {
            page.set_clock_for_test(t);
            page.agent_used(7, "Fix it".into(), cx);
        });
        cx.run_until_parked();
        let before = changes.get();
        // From outside GPUI, as the AppKit monitor does.
        page.read_with(cx, |page, _| page.user_input_for_test(UserInput::Scroll));
        cx.run_until_parked();
        assert_eq!(changes.get(), before);
        page.read_with(cx, |page, _| page.user_input_for_test(UserInput::Click));
        cx.run_until_parked();
        assert_eq!(changes.get(), before + 1);
        page.read_with(cx, |page, _| {
            assert_eq!(
                page.presence_label().unwrap().control.as_deref(),
                Some("You have control · gives back in 16s")
            );
        });
        // The countdown steps once a second.
        page.update(cx, |page, _| page.set_clock_for_test(t + S));
        cx.executor().advance_clock(S);
        cx.run_until_parked();
        assert_eq!(changes.get(), before + 2);
        page.read_with(cx, |page, _| {
            assert_eq!(
                page.presence_label().unwrap().control.as_deref(),
                Some("You have control · gives back in 15s")
            );
        });
    }
}
