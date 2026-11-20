//! A subagent's own view, as Orca opens one from the rail: above its
//! transcript the way back to the main agent and a switcher over every
//! subagent; in place of the composer its state, its Stop and the way back.
use super::*;
use crate::session_mark::SessionMark;
use crate::transcript::subagents::Subagent;
use gpui::{App, rgb};
use monocode_core::subagents::{self as model, State};
use std::collections::HashSet;

const MAIN: &str = "subagent-main";

#[derive(Default)]
pub(super) struct SubagentUi {
    stopping: HashSet<String>,
    switcher: Option<(Entity<Menu>, Subscription)>,
    switcher_anchor: AnchorSlot,
    mark: Option<Entity<SessionMark>>,
    clock: Option<gpui::Task<()>>,
    /// What the rail last heard of the running subagents.
    published: Vec<(String, State, String, String)>,
}

/// The running subagents changed: the rail redraws their rows.
pub(crate) struct SubagentsChanged;
impl EventEmitter<SubagentsChanged> for Conversation {}

impl Conversation {
    /// Every subagent of the session, running ones flagged.
    pub(crate) fn subagents(&self, cx: &App) -> Vec<Subagent> {
        self.subagents_with(false, cx)
    }

    /// The session's running subagents, as the rail lists them.
    pub(crate) fn running_subagents(&self, cx: &App) -> Vec<Subagent> {
        self.subagents_with(true, cx)
    }

    fn subagents_with(&self, running_only: bool, cx: &App) -> Vec<Subagent> {
        let background: HashSet<String> = (self.background.tasks.iter())
            .filter(|task| task.agent)
            .map(|task| task.id.clone())
            .collect();
        self.transcript
            .read(cx)
            .subagents(&background, running_only)
    }

    /// Tell the rail when a running subagent starts, ends or moves on.
    pub(crate) fn publish_subagents(&mut self, cx: &mut Context<Self>) {
        let now = monocode_core::time::now_ms();
        let running: Vec<_> = (self.running_subagents(cx).into_iter())
            .map(|s| {
                let (lead, trail) = s.row.lead_trail(s.clock.last_change, now);
                (s.row.id, s.row.state, lead, trail)
            })
            .collect();
        if running != self.subagent_ui.published {
            self.subagent_ui.published = running;
            cx.emit(SubagentsChanged);
        }
    }

    /// The open subagent's id, while its view replaces the session's.
    pub(crate) fn open_subagent_id(&self, cx: &App) -> Option<String> {
        self.transcript.read(cx).subagent_focus().map(str::to_owned)
    }

    /// Open a subagent's view, or the main agent's with `None`.
    pub(crate) fn open_subagent(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.subagent_ui.switcher = None;
        self.transcript
            .update(cx, |transcript, cx| transcript.set_subagent_focus(id, cx));
        // The rail moves its highlight to the open subagent's row.
        cx.emit(SubagentsChanged);
        cx.notify();
    }

    /// The open subagent and every subagent beside it, while it is open.
    pub(super) fn focused_subagent(&self, cx: &App) -> Option<(Subagent, Vec<Subagent>)> {
        let id = self.open_subagent_id(cx)?;
        let all = self.subagents(cx);
        let focus = all.iter().find(|s| s.row.id == id)?.clone();
        Some((focus, all))
    }

    /// Only Claude stops one subagent and leaves the turn running.
    fn can_stop_subagent(&self, row: &model::Row) -> bool {
        row.running
            && !row.task_ids.is_empty()
            && self.controls.is_some()
            && Kind::from_id(&self.summary.provider) == Kind::Claude
    }

    fn stop_subagent(&mut self, row: &model::Row, cx: &mut Context<Self>) {
        let Some(controls) = &self.controls else {
            return;
        };
        for task in &row.task_ids {
            let _ = controls.send(Control {
                id: task.clone(),
                decision: Decision::StopTask,
            });
        }
        self.subagent_ui.stopping.insert(row.id.clone());
        cx.notify();
    }

    /// Switcher labels: each spawner's children right under it, repeated or
    /// missing names told apart by ordinals.
    fn switcher_items(all: &[Subagent], current: &str) -> Vec<MenuItem> {
        let names: Vec<String> = all.iter().map(|s| s.row.display_name()).collect();
        let labels = model::disambiguate(&names);
        let mut items: Vec<MenuItem> = vec![
            MenuAction::new(MAIN, "Main agent").into(),
            MenuItem::Separator,
        ];
        for (subagent, label) in all.iter().zip(labels) {
            items.push(
                MenuAction::new(format!("subagent:{}", subagent.row.id), label)
                    .icon(state_icon(subagent.row.state))
                    .icon_color(state_color(subagent.row.state))
                    .value(subagent.row.state.heading())
                    .indent(subagent.depth as f32 * 14.)
                    .checked(subagent.row.id == current)
                    .into(),
            );
        }
        items
    }

    fn open_subagent_switcher(
        &mut self,
        all: &[Subagent],
        current: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.subagent_ui.switcher.take().is_some() {
            cx.notify();
            return;
        }
        let items = Self::switcher_items(all, current);
        let menu = cx.new(|cx| Menu::new("Subagents", items, window, cx));
        let events = cx.subscribe_in(&menu, window, |this, _, event: &MenuEvent, _, cx| {
            this.subagent_ui.switcher = None;
            if let MenuEvent::Picked(id, _) = event {
                let id = id.to_string();
                let target = id.strip_prefix("subagent:").map(str::to_owned);
                if id == MAIN || target.is_some() {
                    this.open_subagent(target, cx);
                }
            }
            cx.notify();
        });
        self.subagent_ui.switcher = Some((menu, events));
        cx.notify();
    }

    /// Above the subagent's transcript: its spawn chain back to the main
    /// agent, a switcher over every subagent, and its role and model.
    pub(super) fn subagent_bar(
        &mut self,
        focus: &Subagent,
        all: &[Subagent],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let names: Vec<String> = all.iter().map(|s| s.row.display_name()).collect();
        let labels = model::disambiguate(&names);
        let label_of = |id: &str| {
            all.iter()
                .position(|s| s.row.id == id)
                .map_or_else(|| "Subagent".to_owned(), |ix| labels[ix].clone())
        };
        let mut chain = Vec::new();
        let mut parent = focus.row.parent.clone();
        while let Some(id) = parent {
            parent = all
                .iter()
                .find(|s| s.row.id == id)
                .and_then(|s| s.row.parent.clone());
            chain.insert(0, id);
        }
        let crumb = || {
            div()
                .flex_shrink_0()
                .text_size(px(12.))
                .text_color(content(0.35))
                .child("/")
        };
        let chip = |text: String| {
            div()
                .min_w_0()
                .flex_shrink(1.)
                .truncate()
                .rounded_full()
                .border_1()
                .border_color(content(0.12))
                .px(px(6.))
                .text_size(px(11.))
                .text_color(content(0.45))
                .child(text)
        };
        let mut nav = div()
            .id("subagent-path")
            .debug_selector(|| "subagent-path".into())
            .role(Role::Navigation)
            .aria_label("Subagent path")
            .w_full()
            .max_w(px(COLUMN))
            .h(px(40.))
            .px(px(COLUMN_PAD - 6.))
            .flex()
            .items_center()
            .gap(px(4.))
            .min_w_0()
            .child(
                bar_button("subagent-main-agent")
                    .debug_selector(|| "subagent-main-agent".into())
                    .aria_label("Main agent")
                    .child(icon("chevron-left", 14.).text_color(content(0.6)))
                    .child("Main agent")
                    .on_click(cx.listener(|this, _, _, cx| this.open_subagent(None, cx))),
            );
        for ancestor in chain {
            let label = label_of(&ancestor);
            nav = nav.child(crumb()).child(
                bar_button(SharedString::from(format!("subagent-crumb-{ancestor}")))
                    .max_w(px(160.))
                    .child(div().min_w_0().truncate().child(label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_subagent(Some(ancestor.clone()), cx)
                    })),
            );
        }
        let current = focus.row.id.clone();
        let menu_all = all.to_vec();
        let mark = self.subagent_mark(focus.row.state, cx);
        nav = nav
            .child(crumb())
            .child(
                bar_button("subagent-switcher")
                    .debug_selector(|| "subagent-switcher".into())
                    .aria_label(format!("{}, switch subagent", label_of(&current)))
                    .aria_expanded(self.subagent_ui.switcher.is_some())
                    .relative()
                    .text_color(content(0.9))
                    .child(crate::subagent_dot::dot(focus.row.state, mark.as_ref()))
                    .child(div().min_w_0().truncate().child(label_of(&current)))
                    .child(icon("chevron-down", 12.).text_color(content(0.4)))
                    .child(self.subagent_ui.switcher_anchor.probe())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_subagent_switcher(&menu_all, &current, window, cx)
                    })),
            )
            .when_some(focus.row.agent_type.clone(), |d, agent_type| {
                let role = model::role_label(&agent_type);
                d.when(role != focus.row.name, |d| d.child(chip(role)))
            })
            .when_some(focus.row.model.clone(), |d, model| d.child(chip(model)));
        div()
            .flex_shrink_0()
            .w_full()
            .border_b_1()
            .border_color(content(0.08))
            .flex()
            .justify_center()
            .child(nav)
            .when_some(self.subagent_ui.switcher.as_ref(), |d, (menu, _)| {
                d.child(
                    menu_popover(
                        menu,
                        Anchor::Slot(self.subagent_ui.switcher_anchor.clone()),
                        cx,
                    )
                    .side(Side::Bottom),
                )
            })
            .into_any_element()
    }

    /// Stands in for the composer while a subagent's view is open.
    pub(super) fn subagent_status_bar(
        &mut self,
        focus: &Subagent,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = &focus.row;
        let state = row.state;
        let working = state == State::Working;
        if !working {
            self.subagent_ui.stopping.remove(&row.id);
        }
        self.keep_subagent_clock(working, cx);
        let now = monocode_core::time::now_ms();
        let end = if working {
            now
        } else {
            focus.clock.settled_at.unwrap_or(now)
        };
        let seconds = end.saturating_sub(focus.clock.first_seen) / 1000;
        let elapsed = (working || seconds > 0).then(|| model::elapsed(seconds));
        let stopping = self.subagent_ui.stopping.contains(&row.id);
        let stop = self.can_stop_subagent(row).then(|| {
            let target = row.clone();
            bar_button("subagent-stop")
                .debug_selector(|| "subagent-stop".into())
                .aria_label(if stopping { "Stopping…" } else { "Stop" })
                .when(stopping, |d| d.opacity(0.5).cursor_default())
                .child(if stopping { "Stopping…" } else { "Stop" })
                .when(!stopping, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| this.stop_subagent(&target, cx)))
                })
        });
        let back = match &row.parent {
            None => "Back to main agent".to_owned(),
            Some(parent) => {
                let name = self
                    .subagents(cx)
                    .into_iter()
                    .find(|s| &s.row.id == parent)
                    .map_or_else(|| "Subagent".to_owned(), |s| s.row.display_name());
                format!("Back to {name}")
            }
        };
        let back_target = row.parent.clone();
        let mark = self.subagent_mark(state, cx);
        div()
            .id("subagent-status")
            .debug_selector(|| "subagent-status".into())
            .w_full()
            .pt(px(8.))
            .pb(px(10.))
            .child(
                div()
                    .h(px(40.))
                    .w_full()
                    .min_w_0()
                    .px(px(12.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(content(0.1))
                    .bg(content(0.04))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .text_color(content(0.5))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .min_w_0()
                            .child(crate::subagent_dot::dot(state, mark.as_ref()))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(content(0.9))
                                    .child(state.heading()),
                            )
                            .children(elapsed.map(|elapsed| {
                                div().min_w_0().truncate().child(format!(" · {elapsed}"))
                            })),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .children(stop)
                            .child(
                                bar_button("subagent-back")
                                    .debug_selector(|| "subagent-back".into())
                                    .aria_label(back.clone())
                                    .child(icon("chevron-left", 14.).text_color(content(0.6)))
                                    .child(div().max_w(px(192.)).truncate().child(back))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_subagent(back_target.clone(), cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The working spinner's own view, made when a working subagent shows.
    fn subagent_mark(
        &mut self,
        state: State,
        cx: &mut Context<Self>,
    ) -> Option<Entity<SessionMark>> {
        if state != State::Working {
            return None;
        }
        Some(
            self.subagent_ui
                .mark
                .get_or_insert_with(|| crate::subagent_dot::working_mark(cx))
                .clone(),
        )
    }

    /// The elapsed clock ticks once a second while the subagent works.
    fn keep_subagent_clock(&mut self, working: bool, cx: &mut Context<Self>) {
        if !working {
            self.subagent_ui.clock = None;
            return;
        }
        if self.subagent_ui.clock.is_none() {
            self.subagent_ui.clock = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(1))
                        .await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
    }
}

fn bar_button(id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::Button)
        .h(px(24.))
        .min_w_0()
        .px(px(6.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .gap(px(4.))
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(content(0.6))
        .hover(|s| s.bg(content(0.08)).text_color(content(0.95)))
}

fn state_icon(state: State) -> &'static str {
    match state {
        State::Working => "loader",
        State::Waiting => "alert-circle",
        State::Failed => "circle-x",
        State::Interrupted => "circle-dot",
        State::Done => "check-circle-2",
        State::Unverifiable => "circle-dashed",
    }
}

fn state_color(state: State) -> gpui::Hsla {
    match state {
        State::Working => rgb(0x6f84a3).into(),
        State::Waiting => rgb(0xfbbf24).into(),
        State::Failed => rgb(0xf87171).into(),
        State::Done => rgb(0x34d399).into(),
        State::Interrupted | State::Unverifiable => content(0.45),
    }
}
