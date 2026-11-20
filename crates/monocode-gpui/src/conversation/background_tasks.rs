//! LiveFoldTitle's "running in background": once the agent has answered with
//! commands or subagents still going, they sit on the composer's ledge above
//! the message queue until they finish or the agent picks the thread back up.
use super::*;
use crate::transcript::subagents::Subagent;
use monocode_core::provider::BackgroundTask;
use std::collections::HashSet;

const ROW_HEIGHT: f32 = 38.;
/// More rows than this scroll inside the card.
const VISIBLE_ROWS: f32 = 4.;
const LIVE: u32 = 0x4ade80;
/// A subagent's own tasks sit one step in under it.
const NESTED_INDENT: f32 = 22.;

#[derive(Default)]
pub(super) struct BackgroundTasks {
    pub tasks: Vec<BackgroundTask>,
    pub collapsed: bool,
    pub motion: composer_shelf::Motion,
    /// Rows opened to their details.
    pub expanded: HashSet<String>,
    /// Tasks asked to stop, waiting for the provider to drop them.
    pub stopping: HashSet<String>,
}

impl Conversation {
    pub(super) fn set_background(&mut self, tasks: Vec<BackgroundTask>, cx: &mut Context<Self>) {
        let animate = !cx.reduce_motion();
        for task in &tasks {
            if !self.background.tasks.iter().any(|old| old.id == task.id) {
                self.background
                    .motion
                    .insert(&task.id, self.background.tasks.is_empty(), animate);
            }
        }
        let live: HashSet<&str> = tasks.iter().map(|task| task.id.as_str()).collect();
        (self.background.expanded).retain(|id| live.contains(id.as_str()));
        (self.background.stopping).retain(|id| live.contains(id.as_str()));
        self.background.tasks = tasks;
        self.publish_subagents(cx);
        cx.notify();
    }

    pub(super) fn clear_background(&mut self) {
        self.background.tasks.clear();
        self.background.expanded.clear();
        self.background.stopping.clear();
    }

    /// Only Claude stops one task and leaves the turn running.
    pub(super) fn can_stop_task(&self) -> bool {
        self.controls.is_some() && Kind::from_id(&self.summary.provider) == Kind::Claude
    }

    pub(super) fn stop_background_task(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(controls) = &self.controls else {
            return;
        };
        let _ = controls.send(Control {
            id: id.to_owned(),
            decision: Decision::StopTask,
        });
        self.background.stopping.insert(id.to_owned());
        cx.notify();
    }

    /// A row's click: a subagent opens its own view, a command its details.
    fn open_background_task(&mut self, task: &BackgroundTask, cx: &mut Context<Self>) {
        if task.agent
            && let Some(id) = subagent_of(task, &self.subagents(cx))
        {
            self.open_subagent(Some(id), cx);
            return;
        }
        if !self.background.expanded.remove(&task.id) {
            self.background.expanded.insert(task.id.clone());
        }
        cx.notify();
    }

    fn stop_button(&self, task: &BackgroundTask, cx: &mut Context<Self>) -> impl IntoElement {
        let stopping = self.background.stopping.contains(&task.id);
        let id = task.id.clone();
        div()
            .id(SharedString::from(format!("background-stop-{}", task.id)))
            .debug_selector({
                let id = task.id.clone();
                move || format!("background-stop-{id}")
            })
            .role(Role::Button)
            .aria_label(if stopping { "Stopping…" } else { "Stop task" })
            .size(px(22.))
            .flex_shrink_0()
            .rounded_md()
            .flex()
            .items_center()
            .justify_center()
            .text_color(content(0.45))
            .when(stopping, |d| d.opacity(0.4))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .when(!stopping, |d| {
                d.cursor_pointer()
                    .hover(|s| s.bg(content(0.1)).text_color(content(0.95)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.stop_background_task(&id, cx);
                    }))
            })
            .child(icon("square", 10.))
    }

    fn background_row(
        &self,
        task: &BackgroundTask,
        nested: bool,
        subagents: &[Subagent],
        can_stop: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let expanded = self.background.expanded.contains(&task.id);
        let opens_subagent = task.agent && subagent_of(task, subagents).is_some();
        // A command whose subagent is not listed beside it names its owner.
        let owner = task.parent.as_deref().and_then(|parent| {
            (subagents.iter())
                .find(|s| s.row.id == parent)
                .map(|s| (s.row.id.clone(), s.row.display_name()))
        });
        let id = task.id.clone();
        let stop = can_stop.then(|| self.stop_button(task, cx));
        let summary = div()
            .min_h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap(px(10.))
            .when(nested, |d| d.pl(px(NESTED_INDENT)))
            .child(
                div()
                    .size(px(24.))
                    .flex_shrink_0()
                    .rounded(px(7.))
                    .bg(content(0.07))
                    .text_color(content(0.6))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(if task.agent { "bot" } else { "terminal" }, 13.)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_color(content(0.8))
                            .child(task.description.clone()),
                    )
                    .when_some(task.command.clone().filter(|_| !expanded), |d, command| {
                        d.child(
                            div()
                                .truncate()
                                .font_family(crate::fonts::mono())
                                .text_size(px(11.))
                                .text_color(content(0.4))
                                .child(format!("$ {}", first_line(&command))),
                        )
                    })
                    .when_some(owner.clone().filter(|_| !nested), |d, (_, name)| {
                        d.child(
                            div()
                                .truncate()
                                .text_size(px(11.))
                                .text_color(content(0.4))
                                .child(format!("from {name}")),
                        )
                    }),
            )
            .child(live_clock(task))
            .children(stop)
            .child(
                div()
                    .size(px(16.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(content(0.35))
                    .child(icon("chevron-right", 11.).when(expanded, |d| {
                        d.with_transformation(gpui::Transformation::rotate(gpui::percentage(0.25)))
                    })),
            );
        let details = expanded.then(|| {
            div()
                .debug_selector({
                    let id = task.id.clone();
                    move || format!("background-details-{id}")
                })
                .pl(px(34. + if nested { NESTED_INDENT } else { 0. }))
                .pr(px(8.))
                .pb(px(8.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .text_size(px(11.))
                .text_color(content(0.45))
                .when_some(task.command.clone(), |d, command| {
                    d.child(
                        div()
                            .w_full()
                            .rounded(px(6.))
                            .bg(content(0.05))
                            .px(px(8.))
                            .py(px(6.))
                            .font_family(crate::fonts::mono())
                            .text_color(content(0.75))
                            .whitespace_normal()
                            .child(format!("$ {}", command.trim())),
                    )
                })
                .child(format!("Task {}", task.id))
                .when_some(owner.clone(), |d, (owner, name)| {
                    d.child(
                        div()
                            .id(SharedString::from(format!("background-owner-{}", task.id)))
                            .role(Role::Link)
                            .cursor_pointer()
                            .text_color(content(0.6))
                            .hover(|s| s.text_color(content(0.95)).underline())
                            .child(format!("Started by {name}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_subagent(Some(owner.clone()), cx)
                            })),
                    )
                })
        });
        let open = task.clone();
        div()
            .id(SharedString::from(format!("background-{}", task.id)))
            .debug_selector(move || format!("background-task-{id}"))
            .role(Role::Button)
            .aria_label(if opens_subagent {
                format!("Open subagent: {}", task.description)
            } else {
                format!("Background task: {}", task.description)
            })
            .when(!opens_subagent, |d| d.aria_expanded(expanded))
            .border_t_1()
            .border_color(content(0.05))
            .cursor_pointer()
            .hover(|s| s.bg(content(0.03)))
            .on_click(cx.listener(move |this, _, _, cx| this.open_background_task(&open, cx)))
            .child(summary)
            .children(details)
    }

    pub(super) fn background_shelf(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let width = self.composer_anchor.get().map(|bounds| bounds.size.width);
        let tasks = &self.background.tasks;
        self.background.motion.retain(
            tasks.iter().map(|task| task.id.as_str()),
            !cx.reduce_motion() && width.is_some(),
        );
        if tasks.is_empty() {
            return div().into_any_element();
        }
        // Over a queued message the card tucks in behind it, a deck of two.
        let inset = if self.follow_ups.state.messages.is_empty() {
            px(8.)
        } else {
            px(20.)
        };
        let card_width = (width.unwrap_or_default() - inset * 2.).max(px(0.));
        let row_width = (card_width - px(22.)).max(px(0.));
        let count = tasks.len();
        // A lone task folds into the header: its title takes the subtitle's
        // place and its clock sits at the end, so the card stays one row.
        let single = (count == 1).then(|| &tasks[0]);
        let collapsible = count > 1;
        let collapsed = collapsible && self.background.collapsed;
        let title = if count == 1 {
            "Running in background".to_owned()
        } else {
            format!("{count} tasks running in background")
        };
        let agent = Kind::from_id(&self.summary.provider).label();
        let header = div()
            .id("background-tasks-header")
            .debug_selector(|| "background-tasks-header".into())
            .role(Role::Button)
            .aria_label(title.clone())
            .when(collapsible, |d| d.aria_expanded(!collapsed))
            .h(px(30.))
            .flex()
            .items_center()
            .gap(px(8.))
            .when(collapsible, |d| {
                d.cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
                    this.background.collapsed = !this.background.collapsed;
                    cx.notify();
                }))
            })
            // A lone task's header is its row: a click opens it.
            .when_some(single.cloned(), |d, task| {
                d.cursor_pointer().on_click(
                    cx.listener(move |this, _, _, cx| this.open_background_task(&task, cx)),
                )
            })
            .child(
                div()
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(crate::session_mark::spinner(
                        "background-tasks-spinner",
                        crate::pulse::Rate::Half,
                    )),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(content(0.8))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(title),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(content(0.35))
                    .child(if let Some(task) = single {
                        task.description.clone()
                    } else if collapsed {
                        tasks
                            .iter()
                            .map(|task| task.description.as_str())
                            .collect::<Vec<_>>()
                            .join(" · ")
                    } else {
                        format!("{agent} continues when they finish")
                    }),
            )
            .when_some(single, |d, task| d.child(live_clock(task)))
            .when_some(single.filter(|_| self.can_stop_task()), |d, task| {
                d.child(self.stop_button(task, cx))
            })
            .when(collapsible, |d| {
                d.child(
                    div()
                        .size(px(20.))
                        .flex_shrink_0()
                        .rounded_md()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(content(0.45))
                        .hover(|s| s.bg(content(0.08)).text_color(content(0.9)))
                        .child(icon(
                            if collapsed {
                                "chevron-up"
                            } else {
                                "chevron-down-2"
                            },
                            12.,
                        )),
                )
            });
        let subagents = self.subagents(cx);
        let can_stop = self.can_stop_task();
        let rows = div()
            .id("background-tasks-rows")
            .max_h(px(ROW_HEIGHT * VISIBLE_ROWS))
            .overflow_y_scroll()
            .children(nest(tasks).into_iter().map(|(task, nested)| {
                let row = self.background_row(task, nested, &subagents, can_stop, cx);
                self.background.motion.row(&task.id, row, row_width, window)
            }));
        let card = composer_shelf::card("background-tasks-card", self.classic_layout)
            .role(Role::Group)
            .aria_label("Background tasks")
            .child(header)
            .when(
                !collapsed && single.is_none_or(|task| self.background.expanded.contains(&task.id)),
                |d| d.child(rows),
            );
        composer_shelf::shelf("background-tasks")
            .px(inset)
            .child(self.background.motion.card(card, card_width, window))
            .into_any_element()
    }
}

/// The subagent behind an agent task: its spawning call or its task id.
fn subagent_of(task: &BackgroundTask, subagents: &[Subagent]) -> Option<String> {
    subagents
        .iter()
        .find(|s| {
            task.tool_use_id.as_deref() == Some(s.row.id.as_str())
                || s.row.task_ids.contains(&task.id)
        })
        .map(|s| s.row.id.clone())
}

/// Each subagent's own tasks right under it; the rest in start order.
fn nest(tasks: &[BackgroundTask]) -> Vec<(&BackgroundTask, bool)> {
    let spawner = |parent: &str| {
        tasks
            .iter()
            .any(|t| t.agent && t.tool_use_id.as_deref() == Some(parent))
    };
    let mut out = Vec::with_capacity(tasks.len());
    for task in tasks {
        if task.parent.as_deref().is_some_and(spawner) {
            continue;
        }
        out.push((task, false));
        if let Some(own) = task.tool_use_id.as_deref().filter(|_| task.agent) {
            out.extend(
                (tasks.iter())
                    .filter(|child| child.parent.as_deref() == Some(own))
                    .map(|child| (child, true)),
            );
        }
    }
    out
}

/// Elapsed time beside a breathing green dot.
fn live_clock(task: &BackgroundTask) -> impl IntoElement {
    let started_at = task.started_at;
    crate::pulse::looping(
        SharedString::from(format!("background-clock-{}", task.id)),
        std::time::Duration::from_millis(2_000),
        crate::pulse::Rate::Low,
        move |phase| {
            let ms = monocode_core::time::now_ms().saturating_sub(started_at);
            // A slow breath: the dot swells and its halo fades out.
            let breath = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
            let live: gpui::Hsla = gpui::rgb(LIVE).into();
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(content(0.45))
                        .child(elapsed(ms)),
                )
                .child(
                    div()
                        .size(px(12.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(live.opacity(0.25 * (1. - breath)))
                        .child(
                            div()
                                .size(px(6.))
                                .rounded_full()
                                .bg(live.opacity(0.65 + 0.35 * breath)),
                        ),
                )
        },
    )
}

fn first_line(command: &str) -> &str {
    command
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(command)
        .trim()
}

/// A ticking clock, so seconds stay padded once minutes appear.
fn elapsed(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let (minutes, rest) = (seconds / 60, seconds % 60);
    if minutes < 60 {
        return format!("{minutes}m {rest:02}s");
    }
    format!("{}h {:02}m", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use monocode_core::store::Store;
    use std::sync::Mutex;

    fn task(id: &str, agent: bool) -> BackgroundTask {
        BackgroundTask {
            id: id.into(),
            description: format!("Task {id}"),
            agent,
            command: (!agent).then(|| "npm run dev".into()),
            started_at: monocode_core::time::now_ms(),
            tool_use_id: None,
            parent: None,
        }
    }

    #[test]
    fn a_subagents_own_tasks_sit_under_it() {
        let mut agent = task("agent", true);
        agent.tool_use_id = Some("agent-call".into());
        let mut sleep = task("sleep", false);
        sleep.parent = Some("agent-call".into());
        let mut orphan = task("orphan", false);
        orphan.parent = Some("gone-call".into());
        let dev = task("dev", false);
        let tasks = [sleep, dev, agent, orphan];
        let order: Vec<_> = nest(&tasks)
            .into_iter()
            .map(|(t, nested)| (t.id.as_str(), nested))
            .collect();
        assert_eq!(
            order,
            [
                ("dev", false),
                ("agent", false),
                ("sleep", true),
                ("orphan", false)
            ]
        );
    }

    #[gpui::test]
    fn an_agent_row_opens_its_subagent(cx: &mut TestAppContext) {
        use monocode_core::tool::tasks::{AgentRun, Panel};
        use monocode_core::tool::{ToolCall, ToolState};
        let (window, _received) = busy_conversation(cx);
        window
            .update(cx, |view, _, cx| {
                cx.set_reduce_motion(true);
                let mut call = ToolCall::new("agent-call", "Agent");
                call.state = ToolState::Running;
                call.input =
                    serde_json::json!({"description": "Review", "prompt": "Review it"}).to_string();
                call.panel = Some(Panel::Agent(AgentRun {
                    ids: vec!["review".into()],
                    ..AgentRun::default()
                }));
                view.transcript.update(cx, |t, cx| {
                    let at = t.source_len();
                    t.patch_view_for_tests(
                        at,
                        vec![monocode_core::transcript::Row {
                            role: "tool".into(),
                            text: serde_json::to_string(&call).unwrap(),
                            first: true,
                            sent_at: None,
                        }],
                        cx,
                    );
                });
                let mut review = task("review", true);
                review.tool_use_id = Some("agent-call".into());
                view.set_background(vec![task("dev", false), review], cx);
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.refresh().unwrap();
        let row = visual.debug_bounds("background-task-review").unwrap();
        visual.simulate_click(row.center(), Default::default());
        visual.refresh().unwrap();
        window
            .update(visual, |view, _, cx| {
                assert_eq!(view.open_subagent_id(cx).as_deref(), Some("agent-call"));
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_command_row_opens_its_details_and_stop_names_the_task(cx: &mut TestAppContext) {
        let (window, received) = busy_conversation(cx);
        window
            .update(cx, |view, _, cx| {
                cx.set_reduce_motion(true);
                view.set_background(vec![task("dev", false), task("review", true)], cx);
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.refresh().unwrap();
        assert!(visual.debug_bounds("background-details-dev").is_none());
        let row = visual.debug_bounds("background-task-dev").unwrap();
        visual.simulate_click(row.center(), Default::default());
        visual.refresh().unwrap();
        assert!(visual.debug_bounds("background-details-dev").is_some());

        let stop = visual.debug_bounds("background-stop-dev").unwrap();
        visual.simulate_click(stop.center(), Default::default());
        visual.refresh().unwrap();
        let control = received.try_recv().expect("stop reaches the provider");
        assert_eq!(control.id, "dev");
        assert!(matches!(control.decision, Decision::StopTask));
        assert!(
            visual.debug_bounds("background-details-dev").is_some(),
            "stop does not toggle the row"
        );
    }

    #[gpui::test]
    fn background_work_sits_on_the_ledge_steers_new_messages_and_clears_on_stop(
        cx: &mut TestAppContext,
    ) {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let summary = store.create("claude", "/tmp").unwrap();
        let window = cx.update(|cx| {
            input::bind(cx);
            cx.set_reduce_motion(true);
            cx.set_global(crate::settings::Preferences {
                follow_up: "queue".into(),
                ..Default::default()
            });
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| {
                    let mut view = Conversation::new(summary, Arc::new(Mutex::new(store)), cx);
                    view.loaded = true;
                    view.summary.has_messages = true;
                    view.busy = true;
                    view.set_background(vec![task("dev", false), task("review", true)], cx);
                    view
                })
            })
            .unwrap()
        });
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.refresh().unwrap();
        assert!(visual.debug_bounds("background-task-dev").is_some());
        assert!(visual.debug_bounds("background-task-review").is_some());

        let header = visual.debug_bounds("background-tasks-header").unwrap();
        visual.simulate_click(header.center(), Default::default());
        visual.refresh().unwrap();
        assert!(visual.debug_bounds("background-task-dev").is_none());
        assert!(visual.debug_bounds("background-tasks-header").is_some());

        window
            .update(visual, |view, _, cx| {
                view.enqueue_follow_up("Is the server up?".into(), None, cx);
                assert!(
                    view.follow_ups.state.messages[0].steer,
                    "a message must not wait behind a server that never ends"
                );
                view.stop(cx);
                assert!(view.background.tasks.is_empty());
            })
            .unwrap();
        visual.refresh().unwrap();
        assert!(visual.debug_bounds("background-tasks-header").is_none());
    }

    #[gpui::test]
    fn messages_queued_while_the_agent_worked_steer_once_only_background_work_is_left(
        cx: &mut TestAppContext,
    ) {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let summary = store.create("claude", "/tmp").unwrap();
        let (controls, received) = std::sync::mpsc::channel();
        let window = cx.update(|cx| {
            input::bind(cx);
            cx.set_global(crate::settings::Preferences {
                follow_up: "queue".into(),
                ..Default::default()
            });
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| {
                    let mut view = Conversation::new(summary, Arc::new(Mutex::new(store)), cx);
                    view.loaded = true;
                    view.summary.has_messages = true;
                    view
                })
            })
            .unwrap()
        });
        window
            .update(cx, |view, _, cx| {
                view.busy = true;
                view.controls = Some(controls);
                view.follow_ups.ready = true;
                view.enqueue_follow_up("This one is ugly too".into(), None, cx);
                view.enqueue_follow_up("Also make it faster".into(), None, cx);
                view.enqueue_follow_up("Plan the next map".into(), Some("plan".into()), cx);
                assert!(!view.follow_ups.state.messages[0].steer);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            received.try_recv().is_err(),
            "queued messages wait while the agent is still working"
        );

        // The agent answers, but the dev server it started keeps running.
        window
            .update(cx, |view, _, cx| {
                view.set_background(vec![task("dev", false)], cx);
                view.pump_follow_ups(cx);
            })
            .unwrap();
        cx.run_until_parked();
        let control = received
            .try_recv()
            .expect("a queued message must not wait behind a server that never ends");
        let Decision::Steer(first) = control.decision else {
            panic!("expected steering")
        };
        assert_eq!(first.text, "This one is ugly too", "queue order holds");

        window
            .update(cx, |view, _, cx| view.follow_up_delivered(&first.id, cx))
            .unwrap();
        cx.run_until_parked();
        let control = received.try_recv().expect("the next message follows");
        let Decision::Steer(second) = control.decision else {
            panic!("expected steering")
        };
        assert_eq!(second.text, "Also make it faster");

        window
            .update(cx, |view, _, cx| view.follow_up_delivered(&second.id, cx))
            .unwrap();
        cx.run_until_parked();
        assert!(
            received.try_recv().is_err(),
            "a message with a mode cannot steer, so it waits for the turn to end"
        );
    }

    fn busy_conversation(
        cx: &mut TestAppContext,
    ) -> (
        gpui::WindowHandle<Conversation>,
        std::sync::mpsc::Receiver<Control>,
    ) {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let summary = store.create("claude", "/tmp").unwrap();
        let (controls, received) = std::sync::mpsc::channel();
        let window = cx.update(|cx| {
            input::bind(cx);
            cx.set_global(crate::settings::Preferences {
                follow_up: "queue".into(),
                ..Default::default()
            });
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| {
                    let mut view = Conversation::new(summary, Arc::new(Mutex::new(store)), cx);
                    view.loaded = true;
                    view.summary.has_messages = true;
                    view.busy = true;
                    view.controls = Some(controls);
                    view.follow_ups.ready = true;
                    view
                })
            })
            .unwrap()
        });
        (window, received)
    }

    #[gpui::test]
    fn background_steering_never_passes_a_queued_message_with_a_mode(cx: &mut TestAppContext) {
        let (window, received) = busy_conversation(cx);
        window
            .update(cx, |view, _, cx| {
                view.enqueue_follow_up("Plan the next map".into(), Some("plan".into()), cx);
                view.enqueue_follow_up("Also make it faster".into(), None, cx);
                view.set_background(vec![task("dev", false)], cx);
                view.pump_follow_ups(cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            received.try_recv().is_err(),
            "the plain message waits behind the plan message, in queue order"
        );
    }

    #[gpui::test]
    fn a_refused_steer_still_steers_a_later_turn_that_left_background_work(
        cx: &mut TestAppContext,
    ) {
        let (window, received) = busy_conversation(cx);
        window
            .update(cx, |view, _, cx| {
                view.enqueue_follow_up("Rename the map".into(), None, cx);
                let message = &mut view.follow_ups.state.messages[0];
                // Refused by the previous turn, then a new turn yields to a
                // dev server that never ends.
                message.steer = false;
                message.deferred = true;
                view.set_background(vec![task("dev", false)], cx);
                view.pump_follow_ups(cx);
            })
            .unwrap();
        cx.run_until_parked();
        let control = received
            .try_recv()
            .expect("a refused message must not wait behind a server that never ends");
        let Decision::Steer(message) = control.decision else {
            panic!("expected steering")
        };
        assert_eq!(message.text, "Rename the map");
    }

    #[gpui::test]
    fn a_lone_task_folds_into_the_header(cx: &mut TestAppContext) {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let summary = store.create("claude", "/tmp").unwrap();
        let window = cx.update(|cx| {
            cx.set_reduce_motion(true);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| {
                    let mut view = Conversation::new(summary, Arc::new(Mutex::new(store)), cx);
                    view.loaded = true;
                    view.summary.has_messages = true;
                    view.set_background(vec![task("dev", false)], cx);
                    view
                })
            })
            .unwrap()
        });
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.refresh().unwrap();
        assert!(visual.debug_bounds("background-tasks-header").is_some());
        assert!(visual.debug_bounds("background-task-dev").is_none());
    }

    #[test]
    fn elapsed_reads_as_a_running_clock() {
        assert_eq!(elapsed(4_200), "4s");
        assert_eq!(elapsed(72_000), "1m 12s");
        assert_eq!(elapsed(3_660_000), "1h 01m");
        assert_eq!(first_line("\n  npm run dev\nsecond"), "npm run dev");
    }
}
