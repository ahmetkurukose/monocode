//! Running subagents in the tree, as Orca lists them under the agent that
//! spawned them. A session's child list starts collapsed: its row then
//! carries a count and its most urgent child's state. Opened, each running
//! subagent gets a row (state, name - detail, model, age), and a click opens
//! its own view in the session's pane. Finished subagents leave the list.
use super::project_rail::ProjectRailRow;
use super::*;
use crate::transcript::subagents::Subagent;
use monocode_core::subagents::{self as model, State};

/// One step in from the session's own content.
const SUBAGENT_INDENT: f32 = 20.;

impl Shell {
    /// A session's running subagents, as its live chat knows them.
    pub(super) fn running_subagents(&self, session: i64, cx: &App) -> Vec<Subagent> {
        self.sessions
            .get(&session)
            .map_or_else(Vec::new, |view| view.read(cx).running_subagents(cx))
    }

    /// The tree's rows with each open session's running subagents under it.
    pub(super) fn with_subagent_rows(
        &self,
        rows: Vec<ProjectRailRow>,
        cx: &App,
    ) -> Vec<ProjectRailRow> {
        if self.expanded_subagents.is_empty() {
            return rows;
        }
        let mut out = Vec::with_capacity(rows.len());
        let mut inserted = false;
        for row in rows {
            let parent = match &row {
                ProjectRailRow::Session { project, id, .. } => Some((project.clone(), *id, false)),
                ProjectRailRow::WorktreeSession { project, id, .. } => {
                    Some((project.clone(), *id, true))
                }
                _ => None,
            };
            out.push(row);
            let Some((project, session, nested)) = parent else {
                continue;
            };
            if !self.expanded_subagents.contains(&session) {
                continue;
            }
            for subagent in self.running_subagents(session, cx) {
                inserted = true;
                out.push(ProjectRailRow::Subagent {
                    project: project.clone(),
                    session,
                    id: subagent.row.id,
                    nested,
                    last: false,
                });
            }
        }
        // The project's final child carries the gap before the next project.
        if inserted {
            for row in &mut out {
                set_last(row, false);
            }
            if let Some(row) = out.last_mut() {
                super::project_rail_worktrees::mark_last(row);
            }
        }
        out
    }

    pub(super) fn toggle_subagent_list(&mut self, session: i64, cx: &mut Context<Self>) {
        if !self.expanded_subagents.remove(&session) {
            self.expanded_subagents.insert(session);
        }
        self.projects_frame.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// A session row's subagent count and its disclosure. Collapsed, the
    /// count shows with its most urgent child's state and the chevron waits
    /// for hover; open, the chevron stays.
    pub(super) fn subagent_disclosure(
        &mut self,
        session: i64,
        cx: &mut Context<Self>,
    ) -> Option<(AnyElement, AnyElement)> {
        let running = self.running_subagents(session, cx);
        if running.is_empty() {
            return None;
        }
        let count = running.len();
        let expanded = self.expanded_subagents.contains(&session);
        let signal = model::collapsed_signal(running.iter().map(|s| s.row.state));
        let badge = div()
            .debug_selector(move || format!("rail-subagent-count-{session}"))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(2.))
            .text_size(px(10.))
            .text_color(content(0.5))
            .when(expanded, |d| d.invisible())
            .children(signal.map(|state| {
                let mark = (state == State::Working)
                    .then(|| self.subagent_mark(&format!("session:{session}"), cx));
                crate::subagent_dot::dot(state, mark.as_ref())
            }))
            .child(format!("+{count}"))
            .into_any_element();
        let label = format!(
            "{} {count} child agent{}",
            if expanded { "Hide" } else { "Show" },
            if count == 1 { "" } else { "s" }
        );
        let chevron = div()
            .id(("rail-subagent-toggle", session as u64 as usize))
            .debug_selector(move || format!("rail-subagent-toggle-{session}"))
            .role(Role::Button)
            .aria_label(label)
            .aria_expanded(expanded)
            .size(px(18.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .when(!expanded, |d| {
                d.invisible()
                    .group_hover("rail-session-hover", |s| s.visible())
            })
            .hover(|s| s.bg(content(0.12)))
            .child(
                icon("chevron-down", 12.)
                    .text_color(content(0.6))
                    .when(!expanded, |d| {
                        d.with_transformation(gpui::Transformation::rotate(gpui::percentage(0.75)))
                    }),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_subagent_list(session, cx);
            }))
            .into_any_element();
        Some((badge, chevron))
    }

    fn subagent_mark(&mut self, key: &str, cx: &mut Context<Self>) -> Entity<SessionMark> {
        if let Some(mark) = self.subagent_marks.get(key) {
            return mark.clone();
        }
        // Forget the spinners of subagents no longer listed.
        if self.subagent_marks.len() > 64 {
            let listed: HashSet<String> = (self.project_rail_list.rows.iter())
                .filter_map(|row| match row {
                    ProjectRailRow::Subagent { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect();
            self.subagent_marks
                .retain(|key, _| key.starts_with("session:") || listed.contains(key));
        }
        let mark = crate::subagent_dot::working_mark(cx);
        self.subagent_marks.insert(key.to_owned(), mark.clone());
        mark
    }

    /// Open the session at its main agent, closing a subagent's view.
    pub(super) fn open_main_agent(
        &mut self,
        session: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open(session, window, cx);
        if let Some(view) = self.sessions.get(&session).cloned() {
            view.update(cx, |chat, cx| {
                if chat.open_subagent_id(cx).is_some() {
                    chat.open_subagent(None, cx);
                }
            });
        }
        self.projects_frame.update(cx, |_, cx| cx.notify());
    }

    /// Open the session, then the subagent's own view in its pane.
    fn open_subagent_from_rail(
        &mut self,
        session: i64,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open(session, window, cx);
        if let Some(view) = self.sessions.get(&session).cloned() {
            view.update(cx, |chat, cx| chat.open_subagent(Some(id), cx));
        }
        self.projects_frame.update(cx, |_, cx| cx.notify());
    }

    pub(super) fn project_rail_subagent_row(
        &mut self,
        project: String,
        session: i64,
        id: String,
        nested: bool,
        last: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let subagent = self
            .running_subagents(session, cx)
            .into_iter()
            .find(|s| s.row.id == id);
        let ordinal = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::hash::DefaultHasher::new();
            (session, &id).hash(&mut hasher);
            hasher.finish() as usize
        };
        let frame = self.project_rail_child_frame(
            ("rail-subagent-row", ordinal).into(),
            &project,
            last,
            cx,
        );
        let Some(subagent) = subagent else {
            return frame.into_any_element();
        };
        let now = monocode_core::time::now_ms();
        let row = &subagent.row;
        let (lead, trail) = row.lead_trail(subagent.clock.last_change, now);
        let selected = self
            .workspace
            .active
            .map(|a| self.workspace.session_for_pane(a))
            == Some(session)
            && self
                .sessions
                .get(&session)
                .and_then(|view| view.read(cx).open_subagent_id(cx))
                .as_deref()
                == Some(id.as_str());
        let mark = (row.state == State::Working).then(|| self.subagent_mark(&id, cx));
        let title = if trail.is_empty() {
            lead.clone()
        } else {
            format!("{lead} - {trail}")
        };
        let model = row.model.clone();
        let age = model::short_time_ago(subagent.clock.first_seen, now);
        let indent = SUBAGENT_INDENT
            + if nested {
                super::project_rail_tree::NESTED_INDENT
            } else {
                0.
            };
        let target = id.clone();
        frame
            .child(
                self.project_rail_child_button(
                    &project,
                    ("rail-subagent", ordinal),
                    format!("Subagent, {title}"),
                    selected,
                )
                .debug_selector(move || format!("rail-subagent-{target}"))
                .ml(px(22. + indent))
                .gap(px(6.))
                .text_size(px(12.))
                .child(crate::subagent_dot::dot(row.state, mark.as_ref()))
                .child(
                    div()
                        .id(("rail-subagent-text", ordinal))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .tooltip({
                            let title = title.clone();
                            move |_, cx| {
                                cx.new(|_| super::project_rail_tree::RailTip(title.clone().into()))
                                    .into()
                            }
                        })
                        .child(
                            div()
                                .flex()
                                .min_w_0()
                                .child(div().flex_shrink_0().max_w_full().truncate().child(lead))
                                .when(!trail.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(content(0.45))
                                            .child(format!(" - {trail}")),
                                    )
                                }),
                        ),
                )
                .when_some(model, |d, model| {
                    d.child(
                        div()
                            .min_w_0()
                            .max_w(px(96.))
                            .flex_shrink_0()
                            .truncate()
                            .font_family(crate::fonts::mono())
                            .text_size(px(10.))
                            .text_color(content(0.45))
                            .child(model),
                    )
                })
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(10.))
                        .text_color(content(0.4))
                        .child(age),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_subagent_from_rail(session, id.clone(), window, cx)
                })),
            )
            .into_any_element()
    }
}

fn set_last(row: &mut ProjectRailRow, value: bool) {
    match row {
        ProjectRailRow::Session { last, .. }
        | ProjectRailRow::Terminal { last, .. }
        | ProjectRailRow::More { last, .. }
        | ProjectRailRow::Worktree { last, .. }
        | ProjectRailRow::WorktreeSession { last, .. }
        | ProjectRailRow::PendingWorktree { last, .. }
        | ProjectRailRow::Subagent { last, .. } => *last = value,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::window;
    use super::*;
    use gpui::{TestAppContext, VisualTestContext, size};
    use monocode_core::tool::tasks::{AgentRun, AgentStep, Panel};
    use monocode_core::tool::{ToolCall, ToolState};
    use monocode_core::transcript::Row;

    fn spawn_row(id: &str, description: &str) -> Row {
        let mut call = ToolCall::new(id, "Agent");
        call.input = serde_json::json!({
            "description": description,
            "prompt": "Read the rail builder and report back",
            "subagent_type": "Explore",
        })
        .to_string();
        call.panel = Some(Panel::Agent(AgentRun {
            ids: vec![format!("task-{id}")],
            agent_type: Some("Explore".into()),
            steps: vec![AgentStep {
                id: "s1".into(),
                kind: "tool".into(),
                text: "Read".into(),
                state: Some(ToolState::Running),
                ..AgentStep::default()
            }],
            ..AgentRun::default()
        }));
        Row {
            role: "tool".into(),
            text: serde_json::to_string(&call).unwrap(),
            first: true,
            sent_at: None,
        }
    }

    #[gpui::test]
    fn running_subagents_fold_under_their_session_and_a_click_opens_one(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, a, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.simulate_resize(size(px(1500.), px(900.)));
        window
            .update(visual, |shell, w, cx| shell.open(a, w, cx))
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                let view = shell.sessions[&a].clone();
                view.update(cx, |chat, cx| {
                    chat.transcript.update(cx, |t, cx| {
                        let at = t.source_len();
                        t.streaming(true, cx);
                        t.patch_view_for_tests(
                            at,
                            vec![
                                Row {
                                    role: "user".into(),
                                    text: "Map the rail".into(),
                                    first: true,
                                    sent_at: None,
                                },
                                spawn_row("call-1", "Map the rail"),
                                spawn_row("call-2", "Check the tests"),
                            ],
                            cx,
                        );
                    });
                    chat.publish_subagents(cx);
                });
                shell.workspace.classic_layout = true;
                shell.workspace.project_rail_open = true;
                shell.workspace.preferences.monos = false;
                shell.workspace.project_rail_expanded =
                    ["/work/another-project".to_string()].into();
                shell.invalidate_chrome(cx);
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        let at = |visual: &mut VisualTestContext, id: String| {
            visual.debug_bounds(Box::leak(id.into_boxed_str()))
        };
        // Collapsed, the session carries the count and no child rows.
        assert!(at(visual, format!("rail-subagent-count-{a}")).is_some());
        assert!(at(visual, "rail-subagent-call-1".into()).is_none());

        // Collapsed, the chevron shows on the row's hover.
        let toggle = at(visual, format!("rail-subagent-toggle-{a}")).unwrap();
        visual.simulate_mouse_move(toggle.center(), None, Default::default());
        visual.run_until_parked();
        visual.simulate_click(toggle.center(), Default::default());
        visual.run_until_parked();
        let session = at(visual, format!("rail-session-{a}")).unwrap();
        let first = at(visual, "rail-subagent-call-1".into()).expect("open, each child has a row");
        let second = at(visual, "rail-subagent-call-2".into()).unwrap();
        assert!(first.top() >= session.bottom() && second.top() >= first.bottom());
        assert!(first.left() > session.left(), "children are indented");

        visual.simulate_click(first.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                assert_eq!(shell.workspace.active, Some(a));
                let chat = shell.sessions[&a].read(cx);
                assert_eq!(chat.open_subagent_id(cx).as_deref(), Some("call-1"));
            })
            .unwrap();
        assert!(
            at(visual, "subagent-status".into()).is_some(),
            "the status bar replaces the composer"
        );
        assert!(at(visual, "subagent-prompt".into()).is_some());
        assert!(at(visual, "subagent-path".into()).is_some());
        // Codex names no task to a stop.
        assert!(at(visual, "subagent-stop".into()).is_none());

        let back = at(visual, "subagent-back".into()).unwrap();
        visual.simulate_click(back.center(), Default::default());
        visual.run_until_parked();
        assert!(at(visual, "subagent-status".into()).is_none());

        // From a subagent's view, the session's own row leads back to the
        // main agent.
        let first = at(visual, "rail-subagent-call-1".into()).unwrap();
        visual.simulate_click(first.center(), Default::default());
        visual.run_until_parked();
        assert!(at(visual, "subagent-status".into()).is_some());
        let session = at(visual, format!("rail-session-{a}")).unwrap();
        visual.simulate_click(session.center(), Default::default());
        visual.run_until_parked();
        assert!(at(visual, "subagent-status".into()).is_none());
        window
            .update(visual, |shell, _, cx| {
                assert_eq!(shell.sessions[&a].read(cx).open_subagent_id(cx), None);
            })
            .unwrap();

        // The turn ends: finished subagents leave the rail.
        window
            .update(visual, |shell, _, cx| {
                let view = shell.sessions[&a].clone();
                view.update(cx, |chat, cx| {
                    chat.transcript.update(cx, |t, cx| t.streaming(false, cx));
                    chat.publish_subagents(cx);
                });
            })
            .unwrap();
        visual.run_until_parked();
        assert!(at(visual, "rail-subagent-call-1".into()).is_none());
        assert!(at(visual, format!("rail-subagent-count-{a}")).is_none());
    }
}
