//! The project rail's tree view: a project row opens to list its sessions and
//! the terminals it holds, one compact row each. Everything here reads state
//! that already exists (summaries, the dock's terminal list); it loads no chat
//! and spawns no shell until a row is clicked.
use super::app_views::AppView;
use super::project_rail::{ProjectRailRow, RailTerminal};
use super::*;
use crate::workspace;
use gpui::Div;

/// Sessions listed under a project before "N more".
const MAX_SESSIONS: usize = 8;
const CHILD_HEIGHT: f32 = 30.;
/// The space between two projects' cards.
pub(super) const CARD_GAP: f32 = 6.;
/// The apps a project's row offers, in order: preference name, view, icon.
const PROJECT_APPS: [(&str, AppView, &str); 3] = [
    ("inbox", AppView::Inbox, "inbox"),
    ("notes", AppView::Notes, "note"),
    ("database", AppView::Database, "database"),
];

/// A plain tooltip for the rail's app icons.
struct RailTip(SharedString);
impl Render for RailTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(pane_fill())
            .border_1()
            .border_color(content(0.15))
            .text_size(px(12.))
            .text_color(content(1.))
            .child(self.0.clone())
    }
}

impl Shell {
    /// The tree is a Classic-layout option; Focused has no project rail.
    pub(super) fn project_rail_tree_on(&self) -> bool {
        self.workspace.classic_layout && self.workspace.preferences.project_rail_tree()
    }

    pub(super) fn project_rail_expanded(&self, path: &str) -> bool {
        self.project_rail_tree_on() && self.workspace.project_rail_expanded.contains(path)
    }

    pub(super) fn toggle_project_rail_expanded(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.workspace.project_rail_expanded.remove(path) {
            self.workspace.project_rail_expanded.insert(path.into());
        }
        self.persist(cx);
        self.projects_frame.update(cx, |_, cx| cx.notify());
    }

    /// Sessions, then terminals, of one project, as rail rows.
    pub(super) fn project_rail_children(&self, path: &str, cx: &App) -> Vec<ProjectRailRow> {
        let start = self
            .workspace
            .session_filters
            .time
            .start(monocode_core::time::now_ms());
        let mut ids: Vec<i64> = self
            .order
            .iter()
            .copied()
            .filter(|id| {
                let summary = &self.session_metadata(*id, cx).summary;
                summary.cwd == path
                    && summary.has_messages
                    && !self.sidebar_hidden_sessions.contains(id)
                    && self.filter_session(*id, start, cx)
                    && !self
                        .monos
                        .as_ref()
                        .is_some_and(|m| m.read(cx).hidden_session(*id))
                    && !self.orchestration_worker(*id)
            })
            .collect();
        ids.sort_by_key(|id| {
            std::cmp::Reverse((self.session_metadata(*id, cx).summary.updated_at, *id))
        });
        let hidden = ids.len().saturating_sub(MAX_SESSIONS);
        ids.truncate(MAX_SESSIONS);
        let docked = self
            .workspace
            .docks
            .get(path)
            .map_or(0, |dock| dock.terminals.len());
        let mut terminals: Vec<RailTerminal> = (0..docked).map(RailTerminal::Dock).collect();
        // Terminals pulled out of the dock live on their owner's project.
        let mut panes: Vec<i64> = self
            .workspace
            .terminal_panes
            .iter()
            .filter(|(pane, t)| {
                self.workspace.tab_of(**pane).is_some()
                    && self
                        .session_summaries
                        .get(&t.owner)
                        .is_some_and(|owner| owner.cwd == path)
            })
            .map(|(pane, _)| *pane)
            .collect();
        panes.sort_unstable_by(|a, b| b.cmp(a));
        terminals.extend(panes.into_iter().map(RailTerminal::Pane));

        let project = path.to_owned();
        let total = ids.len() + usize::from(hidden > 0) + terminals.len();
        let mut rows = Vec::with_capacity(total);
        let last = |n: usize| n + 1 == total;
        for id in ids {
            rows.push(ProjectRailRow::Session {
                project: project.clone(),
                id,
                last: last(rows.len()),
            });
        }
        if hidden > 0 {
            rows.push(ProjectRailRow::More {
                project: project.clone(),
                hidden,
                last: last(rows.len()),
            });
        }
        for source in terminals {
            rows.push(ProjectRailRow::Terminal {
                project: project.clone(),
                source,
                last: last(rows.len()),
            });
        }
        rows
    }

    /// In Classic with the tree rail, Inbox, Notes and Database open as tabs
    /// of the current project. Automations stays one global surface.
    pub(super) fn project_app_scope(&self, view: AppView) -> bool {
        self.project_rail_tree_on() && view != AppView::Automations
    }

    /// The title tab an app view has here: the current project's own tab, or
    /// the one global tab.
    pub(super) fn app_tab(&self, view: AppView) -> TitleTab {
        if self.project_app_scope(view) {
            TitleTab::ProjectApp {
                view,
                project: self.workspace.project.clone(),
            }
        } else {
            TitleTab::App(view)
        }
    }

    /// An app view drawn as a Classic surface, with its own header and no
    /// tab strip, rather than as a project tab.
    pub(super) fn classic_app_surface(&self) -> bool {
        self.active_app_view
            .is_some_and(|view| !self.project_app_scope(view))
    }

    /// Opens one of a project's apps, switching to the project first.
    pub(super) fn open_project_app(
        &mut self,
        view: AppView,
        project: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.project != project {
            self.select_project(project, window, cx);
        }
        match view {
            AppView::Inbox => self.open_inbox(window, cx),
            AppView::Notes => self.open_notes(window, cx),
            AppView::Database => self.open_database(window, cx),
            AppView::Automations => self.open_automations(window, cx),
        }
    }

    /// The project's apps as small icons beside its name, in tree mode.
    pub(super) fn project_rail_app_icons(
        &mut self,
        project: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.project_rail_tree_on() {
            return None;
        }
        let project = project.to_owned();
        let ix = self.rail_index(&project);
        let name = workspace::project_name(&project);
        let here = self.workspace.project == project;
        let mut icons = Vec::new();
        for (app, view, glyph) in PROJECT_APPS {
            if !self.workspace.preferences.project_app_shown(app) {
                continue;
            }
            let selected = here && self.active_app_view == Some(view);
            let unread = view == AppView::Inbox
                && self
                    .inbox
                    .as_ref()
                    .is_some_and(|inbox| inbox.read(cx).unread_count_in(&project) > 0);
            let target = project.clone();
            icons.push(
                div()
                    .id(SharedString::from(format!("rail-app-{app}-{ix}")))
                    .debug_selector(move || format!("rail-app-{app}-{ix}"))
                    .role(Role::Button)
                    .aria_label(format!("{} of {name}", view.label()))
                    .aria_selected(selected)
                    .relative()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(content(0.1)))
                    .hover(|s| s.bg(content(0.08)))
                    .child(icon(glyph, 13.).text_color(content(if selected { 0.9 } else { 0.55 })))
                    .when(unread, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(2.))
                                .right(px(2.))
                                .size(px(5.))
                                .rounded_full()
                                .bg(accent_color()),
                        )
                    })
                    .tooltip({
                        let label = view.label();
                        move |_, cx| cx.new(|_| RailTip(label.into())).into()
                    })
                    // Neither a drag of the project nor a click on its row.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_project_app(view, target.clone(), window, cx)
                    }))
                    .into_any_element(),
            );
        }
        (!icons.is_empty()).then(|| {
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_px()
                .children(icons)
                .into_any_element()
        })
    }

    /// One row's slice of its project's card: the plate behind a project and
    /// the rows under it, which sets it apart from its neighbours. The
    /// current project's card is the stronger one. `top` and `bottom` close
    /// the card; `inset` is the gap kept above it.
    pub(super) fn project_rail_card(
        &self,
        project: &str,
        top: bool,
        bottom: bool,
        inset: f32,
    ) -> Option<Div> {
        if !self.project_rail_tree_on() {
            return None;
        }
        let current = self.workspace.project == project;
        let offset = self.project_rail_project_offset(project);
        let ix = self.rail_index(project);
        let part = if top { "top" } else { "row" };
        Some(
            div()
                .debug_selector(move || format!("rail-card-{part}-{ix}"))
                .absolute()
                .left(px(3.))
                .right(px(5.))
                .top(px(inset + offset))
                .bottom(px(-offset))
                .bg(content(if current { 0.055 } else { 0.025 }))
                .border_color(content(if current { 0.14 } else { 0.07 }))
                .border_l_1()
                .border_r_1()
                .when(top, |d| d.rounded_t_lg().border_t_1())
                .when(bottom, |d| d.rounded_b_lg().border_b_1()),
        )
    }

    /// The frame every child row shares. A list item's root is placed by the
    /// list, so the drag offset goes on the button inside it.
    fn project_rail_child_frame(
        &mut self,
        id: gpui::ElementId,
        project: &str,
        last: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let frame = div()
            .id(id)
            .h(px(if last {
                CHILD_HEIGHT + 1.
            } else {
                CHILD_HEIGHT
            }))
            .pl(px(6.))
            .pr_2()
            .w_full()
            .flex()
            .relative()
            .children(self.project_rail_card(project, false, last, 0.))
            .when(last, |d| d.pb_px());
        if last {
            let slot = self
                .project_rail_list
                .child_bounds
                .entry(project.to_owned())
                .or_default()
                .clone();
            let _ = cx;
            return frame.child(slot.probe());
        }
        frame
    }

    fn project_rail_child_button(
        &self,
        project: &str,
        id: impl Into<gpui::ElementId>,
        label: String,
        selected: bool,
    ) -> gpui::Stateful<Div> {
        div()
            .id(id)
            .role(Role::TreeItem)
            .aria_label(label)
            .aria_selected(selected)
            .cursor_pointer()
            .h_full()
            .flex_1()
            .min_w_0()
            .relative()
            .top(px(self.project_rail_project_offset(project)))
            .ml(px(22.))
            .px_2()
            .rounded_md()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(13.))
            .text_color(content(if selected { 1. } else { 0.72 }))
            .when(selected, |d| d.bg(content(0.1)))
            .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
            // The thread that ties a project's rows to it.
            .child(
                div()
                    .absolute()
                    .left(px(-7.))
                    .top(px(-1.))
                    .bottom(px(-1.))
                    .w(px(1.))
                    .bg(content(0.1)),
            )
    }

    pub(super) fn project_rail_child_row(
        &mut self,
        row: ProjectRailRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let project = match &row {
            ProjectRailRow::Session { project, .. }
            | ProjectRailRow::Terminal { project, .. }
            | ProjectRailRow::More { project, .. } => project.clone(),
            _ => String::new(),
        };
        let element = match row {
            ProjectRailRow::Session { project, id, last } => {
                self.project_rail_session_row(project, id, last, cx)
            }
            ProjectRailRow::Terminal {
                project,
                source,
                last,
            } => self.project_rail_terminal_row(project, source, last, cx),
            ProjectRailRow::More {
                project,
                hidden,
                last,
            } => {
                let target = project.clone();
                self.project_rail_child_frame("rail-more".into(), &project, last, cx)
                    .child(
                        self.project_rail_child_button(
                            &project,
                            ("rail-more-button", self.rail_index(&project)),
                            format!("{hidden} more sessions"),
                            false,
                        )
                        .text_color(content(0.45))
                        .child(format!("{hidden} more…"))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.select_project(target.clone(), window, cx)
                            },
                        )),
                    )
                    .into_any_element()
            }
            _ => div().into_any_element(),
        };
        // A dragged project is raised above its neighbours; so are its rows.
        if self.project_rail_project_raised(&project) {
            gpui::deferred(element).with_priority(1).into_any()
        } else {
            element
        }
    }

    fn rail_index(&self, project: &str) -> usize {
        self.workspace
            .projects
            .iter()
            .position(|p| p == project)
            .unwrap_or(0)
    }

    fn project_rail_session_row(
        &mut self,
        project: String,
        id: i64,
        last: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, updated, provider, approval, working, archived) = {
            let meta = self.session_metadata(id, cx);
            (
                meta.display_title(),
                meta.summary.updated_at,
                meta.summary.provider.clone(),
                meta.needs_approval(),
                meta.responding() || meta.busy,
                meta.summary.archived,
            )
        };
        let done = self.unseen_finished.contains(&id);
        let selected = !self.app_view_open()
            && self
                .workspace
                .active
                .map(|a| self.workspace.session_for_pane(a))
                == Some(id);
        // The same states the session list shows, in one small mark.
        let status = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .text_size(px(11.))
            .text_color(content(0.4))
            .group_hover("rail-session-hover", |s| s.invisible());
        let status = if approval {
            status
                .text_color(rgb(0xfbbf24))
                .child(icon("circle-alert", 13.))
        } else if working {
            status
                .text_color(crate::theme::accent_color())
                .children(self.session_marks.get(&id).cloned())
        } else if done {
            status.text_color(rgb(0x34d399)).child(icon("check", 13.))
        } else {
            status.child(monocode_core::time::relative(
                updated,
                monocode_core::time::now_ms(),
            ))
        };
        let label = format!("{} {title}", if archived { "Unarchive" } else { "Archive" });
        let archive = div()
            .id(("rail-session-archive", id as u64 as usize))
            .debug_selector(move || format!("rail-archive-{id}"))
            .role(Role::Button)
            .aria_label(label)
            .group("rail-session-archive")
            .absolute()
            .right_0()
            .top(px(-3.))
            .size(px(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .invisible()
            .group_hover("rail-session-hover", |s| s.visible())
            .hover(|s| s.bg(content(0.12)))
            .opacity(if self.archiving.contains(&id) {
                0.4
            } else {
                1.
            })
            .child(
                icon("archive", 13.)
                    .group_hover("rail-session-archive", |s| s.text_color(content(1.))),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_archive(id, cx);
            }));
        self.project_rail_child_frame(
            ("rail-session-row", id as u64 as usize).into(),
            &project,
            last,
            cx,
        )
        .child(
            self.project_rail_child_button(
                &project,
                ("rail-session", id as u64 as usize),
                format!("Session, {title}"),
                selected,
            )
            .group("rail-session-hover")
            .debug_selector(move || format!("rail-session-{id}"))
            .child(crate::theme::provider_icon(&provider, 14.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .relative()
                    .flex_shrink_0()
                    .h(px(14.))
                    .flex()
                    .items_center()
                    .child(status)
                    .child(archive),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open(id, window, cx))),
        )
        .into_any_element()
    }

    fn project_rail_terminal_row(
        &mut self,
        project: String,
        source: RailTerminal,
        last: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (key, title, selected) = match &source {
            RailTerminal::Dock(ix) => {
                let title = self
                    .terminals
                    .get(&project)
                    .and_then(|t| t.get(*ix))
                    .map(|t| t.read(cx).title.clone())
                    .filter(|t| !t.is_empty())
                    .or_else(|| {
                        self.workspace
                            .docks
                            .get(&project)
                            .and_then(|d| d.terminals.get(*ix))
                            .map(|cwd| workspace::project_name(cwd))
                    })
                    .unwrap_or_else(|| "Terminal".into());
                (format!("{project}-{ix}"), title, false)
            }
            RailTerminal::Pane(pane) => {
                let title = self
                    .terminal_pane_views
                    .get(pane)
                    .map(|t| t.read(cx).title.clone())
                    .filter(|t| !t.is_empty())
                    .or_else(|| {
                        self.workspace
                            .terminal_panes
                            .get(pane)
                            .map(|t| workspace::project_name(&t.cwd))
                    })
                    .unwrap_or_else(|| "Terminal".into());
                (
                    format!("pane-{pane}"),
                    title,
                    !self.app_view_open() && self.workspace.active == Some(*pane),
                )
            }
        };
        let ix = self.rail_index(&project);
        let ordinal = match &source {
            RailTerminal::Dock(n) => *n,
            RailTerminal::Pane(p) => 1000 + p.unsigned_abs() as usize,
        };
        let button_id = ("rail-terminal", ix * 10_000 + ordinal);
        let target = project.clone();
        let click_source = source.clone();
        let selector = format!("rail-terminal-{key}");
        self.project_rail_child_frame(
            ("rail-terminal-row", ix * 10_000 + ordinal).into(),
            &project,
            last,
            cx,
        )
        .child(
            self.project_rail_child_button(
                &project,
                button_id,
                format!("Terminal, {title}"),
                selected,
            )
            .debug_selector(move || selector.clone())
            .child(
                div()
                    .size(px(14.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("terminal", 13.).text_color(content(0.55))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(title)),
            )
            .on_click(cx.listener(move |this, _, window, cx| match &click_source {
                RailTerminal::Dock(ix) => this.focus_dock_terminal(&target, *ix, window, cx),
                RailTerminal::Pane(pane) => this.open_terminal_pane(*pane, window, cx),
            })),
        )
        .into_any_element()
    }

    /// Switch to `project` and show its dock on terminal `ix`.
    pub(super) fn focus_dock_terminal(
        &mut self,
        project: &str,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.project != project || self.app_view_open() {
            self.select_project(project.to_owned(), window, cx);
        }
        if self.workspace.project != project {
            return;
        }
        let in_panel = self.terminal_in_panel(cx);
        let Some(dock) = self.workspace.docks.get_mut(project) else {
            return;
        };
        dock.active = ix.min(dock.terminals.len().saturating_sub(1));
        let (active, was_open) = (dock.active, dock.open);
        if !in_panel {
            dock.open = true;
        }
        self.dock_tab_scroll.scroll_to_item(active);
        if in_panel {
            self.show_panel_terminal(window, cx);
        } else {
            self.ensure_terminals(project, cx);
            if !was_open {
                self.slide_dock(true, cx);
            }
            self.focus_terminal(window, cx);
        }
        self.fit_sidebar(cx);
        self.persist(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::window;
    use super::*;
    use gpui::{TestAppContext, VisualTestContext, WindowHandle, size};

    fn tree(window: WindowHandle<Shell>, visual: &mut VisualTestContext, expanded: &[&str]) {
        visual.simulate_resize(size(px(1500.), px(900.)));
        window
            .update(visual, |shell, _, cx| {
                shell.workspace.classic_layout = true;
                shell.workspace.project_rail_open = true;
                shell.workspace.preferences.monos = false;
                shell.workspace.preferences.project_rail_view = "tree".into();
                shell.workspace.project_rail_expanded =
                    expanded.iter().map(|p| p.to_string()).collect();
                shell.invalidate_chrome(cx);
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
    }

    fn at(visual: &mut VisualTestContext, id: String) -> Option<gpui::Bounds<Pixels>> {
        visual.debug_bounds(Box::leak(id.into_boxed_str()))
    }

    fn project_ix(
        window: WindowHandle<Shell>,
        visual: &mut VisualTestContext,
        path: &str,
    ) -> usize {
        window
            .update(visual, |shell, _, _| {
                shell
                    .workspace
                    .projects
                    .iter()
                    .position(|p| p == path)
                    .unwrap()
            })
            .unwrap()
    }

    #[gpui::test]
    fn an_expanded_project_lists_its_sessions_under_it_and_a_click_opens_one(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, a, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let other = "/work/another-project";
        tree(window, visual, &[]);
        assert!(
            at(visual, format!("rail-session-{a}")).is_none(),
            "a collapsed project lists nothing"
        );
        tree(window, visual, &[other]);
        let ix = project_ix(window, visual, other);
        let button = at(visual, format!("rail-project-button-{ix}")).unwrap();
        let row = at(visual, format!("rail-session-{a}")).unwrap();
        assert!(row.top() >= button.bottom(), "{row:?} under {button:?}");
        assert!(row.left() > button.left(), "children are indented");
        let loaded = window.update(visual, |s, _, _| s.sessions.len()).unwrap();
        visual.simulate_click(row.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert_eq!(shell.workspace.project, other);
                assert_eq!(shell.workspace.active, Some(a));
                assert!(shell.sessions.len() <= loaded);
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_chevron_toggles_a_project_and_the_list_view_never_shows_children(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, a, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let other = "/work/another-project";
        tree(window, visual, &[]);
        let ix = project_ix(window, visual, other);
        let chevron = at(visual, format!("rail-project-chevron-{ix}")).unwrap();
        visual.simulate_click(chevron.center(), Default::default());
        visual.run_until_parked();
        assert!(at(visual, format!("rail-session-{a}")).is_some());
        window
            .update(visual, |shell, _, _| {
                assert!(shell.workspace.project_rail_expanded.contains(other));
                assert_ne!(shell.workspace.project, other, "the chevron only toggles");
            })
            .unwrap();
        visual.simulate_click(chevron.center(), Default::default());
        visual.run_until_parked();
        assert!(at(visual, format!("rail-session-{a}")).is_none());
        // Expanded but in the plain list view: nothing changes.
        window
            .update(visual, |shell, _, cx| {
                shell.workspace.project_rail_expanded.insert(other.into());
                shell.workspace.preferences.project_rail_view = "list".into();
                shell.invalidate_chrome(cx);
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        assert!(at(visual, format!("rail-session-{a}")).is_none());
        assert!(at(visual, format!("rail-project-chevron-{ix}")).is_none());
    }

    #[gpui::test]
    fn a_projects_terminals_are_listed_and_a_click_shows_that_dock(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::shell::bind_keys(cx);
            crate::terminal::bind(cx);
            cx.set_reduce_motion(true);
        });
        let (window, _, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let other = "/work/another-project";
        window
            .update(visual, |shell, _, _| {
                shell.workspace.docks.insert(
                    other.into(),
                    workspace::TerminalDock {
                        terminals: vec![other.into()],
                        ..Default::default()
                    },
                );
            })
            .unwrap();
        tree(window, visual, &[other]);
        let row = at(visual, format!("rail-terminal-{other}-0"))
            .expect("the dock's terminal is a row under its project");
        window
            .update(visual, |shell, _, _| {
                assert!(
                    !shell.terminals.contains_key(other),
                    "listing a terminal does not start its shell"
                );
            })
            .unwrap();
        visual.simulate_click(row.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert_eq!(shell.workspace.project, other);
                let dock = &shell.workspace.docks[other];
                assert!(dock.open && dock.active == 0);
                assert_eq!(shell.terminals[other].len(), 1);
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_tree_never_loads_the_chats_of_projects_it_lists(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        let count = window
            .update(cx, |shell, _, _| {
                for i in 0..1000 {
                    shell
                        .workspace
                        .add_project(&format!("/projects/project-{i}"));
                }
                shell.sessions.len()
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let all: Vec<String> = (0..1000)
            .map(|i| format!("/projects/project-{i}"))
            .collect();
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        tree(window, visual, &refs);
        assert!(visual.debug_bounds("rail-project-999").is_none());
        window
            .update(visual, |shell, _, _| {
                assert_eq!(shell.sessions.len(), count)
            })
            .unwrap();
    }

    #[gpui::test]
    fn dragging_an_expanded_project_takes_its_children_along(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, a, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let other = "/work/another-project";
        tree(window, visual, &[other]);
        let ix = project_ix(window, visual, other);
        let button = at(visual, format!("rail-project-button-{ix}")).unwrap();
        let child = at(visual, format!("rail-session-{a}")).unwrap();
        let down = button.center();
        visual.simulate_mouse_down(down, MouseButton::Left, Default::default());
        let to = down + gpui::point(px(0.), px(60.));
        visual.simulate_mouse_move(to, MouseButton::Left, Default::default());
        visual.run_until_parked();
        visual.simulate_mouse_move(
            to + gpui::point(px(0.), px(1.)),
            MouseButton::Left,
            Default::default(),
        );
        visual.run_until_parked();
        let moved_button = at(visual, format!("rail-project-button-{ix}")).unwrap();
        let moved_child = at(visual, format!("rail-session-{a}")).unwrap();
        assert_eq!(
            moved_child.top() - child.top(),
            moved_button.top() - button.top(),
            "the child row follows its project while it is dragged"
        );
        visual.simulate_mouse_up(to, MouseButton::Left, Default::default());
        visual.run_until_parked();
    }

    #[gpui::test]
    fn the_tree_replaces_the_workspace_column_and_the_list_keeps_it(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        tree(window, visual, &[]);
        assert!(
            visual.debug_bounds("sidebar-frame").is_none(),
            "sessions live in the tree, so the middle column is gone"
        );
        assert!(visual.debug_bounds("project-rail-frame").is_some());
        window
            .update(visual, |shell, _, cx| {
                shell.workspace.preferences.project_rail_view = "list".into();
                shell.invalidate_chrome(cx);
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        assert!(visual.debug_bounds("sidebar-frame").is_some());
    }

    #[gpui::test]
    fn the_session_filters_apply_to_the_tree_and_each_row_carries_its_provider(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, a, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let other = "/work/another-project";
        tree(window, visual, &[other]);
        let row = at(visual, format!("rail-session-{a}")).unwrap();
        assert!(row.size.height >= px(28.), "rows are tall enough to read");
        window
            .update(visual, |shell, _, cx| {
                shell.workspace.session_filters.status.working = true;
                shell.invalidate_chrome(cx);
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        assert!(
            at(visual, format!("rail-session-{a}")).is_none(),
            "an idle session is filtered out by Working"
        );
        window
            .update(visual, |shell, _, cx| {
                shell.workspace.session_filters = Default::default();
                shell.invalidate_chrome(cx);
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        assert!(at(visual, format!("rail-session-{a}")).is_some());
        assert!(
            visual.debug_bounds("sidebar-filter-button").is_some(),
            "the filter menu moves into the rail with the tree"
        );
    }

    #[gpui::test]
    fn a_session_row_archives_from_its_hover_button(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, a, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let other = "/work/another-project";
        tree(window, visual, &[other]);
        let row = at(visual, format!("rail-session-{a}")).unwrap();
        visual.simulate_mouse_move(row.center(), None, Default::default());
        visual.run_until_parked();
        let button = at(visual, format!("rail-archive-{a}")).unwrap();
        visual.simulate_click(button.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                assert!(shell.session_metadata(a, cx).summary.archived);
                assert_ne!(
                    shell.workspace.active,
                    Some(a),
                    "archiving does not also open the session"
                );
            })
            .unwrap();
        assert!(
            at(visual, format!("rail-session-{a}")).is_none(),
            "an archived session leaves the tree unless Archived is on"
        );
    }

    #[gpui::test]
    fn each_project_shows_its_apps_beside_its_name_and_each_icon_can_be_hidden(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let other = "/work/another-project";
        tree(window, visual, &[]);
        for path in [other, "/work/monocode"] {
            let ix = project_ix(window, visual, path);
            let row = at(visual, format!("rail-project-button-{ix}")).unwrap();
            for app in ["inbox", "notes", "database"] {
                let icon = at(visual, format!("rail-app-{app}-{ix}"))
                    .unwrap_or_else(|| panic!("{app} is offered for {path}"));
                assert!(
                    icon.top() >= row.top() && icon.bottom() <= row.bottom(),
                    "on the project's own row, collapsed or not"
                );
            }
        }
        let ix = project_ix(window, visual, other);
        window
            .update(visual, |shell, _, cx| {
                shell
                    .workspace
                    .preferences
                    .set_project_app_shown("inbox", false);
                shell.invalidate_chrome(cx);
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        assert!(at(visual, format!("rail-app-inbox-{ix}")).is_none());
        assert!(at(visual, format!("rail-app-notes-{ix}")).is_some());
        window
            .update(visual, |shell, _, cx| {
                shell.workspace.preferences.project_apps.clear();
                shell.invalidate_chrome(cx);
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        assert!(
            at(visual, format!("rail-app-notes-{ix}")).is_none(),
            "none left, none shown"
        );
        // The plain list never shows them.
        window
            .update(visual, |shell, _, cx| {
                shell.workspace.preferences.project_apps = vec!["notes".into()];
                shell.workspace.preferences.project_rail_view = "list".into();
                shell.invalidate_chrome(cx);
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        assert!(at(visual, format!("rail-app-notes-{ix}")).is_none());
    }

    #[gpui::test]
    fn an_app_icon_switches_to_its_project_and_opens_the_app(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let db = tempfile::tempdir().unwrap();
        let other = "/work/another-project";
        window
            .update(visual, |shell, _, _| {
                shell.database_dir = db.path().to_owned()
            })
            .unwrap();
        tree(window, visual, &[other]);
        let ix = project_ix(window, visual, other);
        for (app, view) in [("notes", AppView::Notes), ("database", AppView::Database)] {
            // Away first, so the last icon's tooltip is not in the way.
            visual.simulate_mouse_move(gpui::point(px(800.), px(500.)), None, Default::default());
            visual.run_until_parked();
            let icon = at(visual, format!("rail-app-{app}-{ix}")).unwrap();
            visual.simulate_click(icon.center(), Default::default());
            visual.run_until_parked();
            window
                .update(visual, |shell, _, _| {
                    assert_eq!(shell.workspace.project, other);
                    assert_eq!(shell.active_app_view, Some(view));
                })
                .unwrap();
        }
    }

    fn click_app(
        window: WindowHandle<Shell>,
        visual: &mut VisualTestContext,
        app: &str,
        project: &str,
    ) {
        let ix = project_ix(window, visual, project);
        visual.simulate_mouse_move(gpui::point(px(800.), px(500.)), None, Default::default());
        visual.run_until_parked();
        let icon = at(visual, format!("rail-app-{app}-{ix}")).unwrap();
        visual.simulate_click(icon.center(), Default::default());
        visual.run_until_parked();
    }

    fn app(view: AppView, project: &str) -> TitleTab {
        TitleTab::ProjectApp {
            view,
            project: project.into(),
        }
    }

    #[gpui::test]
    fn app_tabs_belong_to_their_project_and_come_back_with_it(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let db = tempfile::tempdir().unwrap();
        let (other, here) = ("/work/another-project", "/work/monocode");
        window
            .update(visual, |shell, _, _| {
                shell.database_dir = db.path().to_owned()
            })
            .unwrap();
        tree(window, visual, &[]);
        click_app(window, visual, "notes", other);
        window
            .update(visual, |shell, _, cx| {
                assert_eq!(shell.workspace.project, other);
                assert!(shell.visible_tabs(cx).contains(&app(AppView::Notes, other)));
                assert_eq!(shell.active_title_tab(cx), Some(app(AppView::Notes, other)));
                assert!(!shell.classic_surface_open(cx));
            })
            .unwrap();
        visual.refresh().unwrap();
        visual.run_until_parked();
        assert!(at(visual, "app-title-tab-Notes".into()).is_some());
        assert!(at(visual, "title-tab-strip".into()).is_some());
        assert!(at(visual, "classic-app-surface-header".into()).is_none());

        // Another project's strip has its own app tabs, not this one's.
        click_app(window, visual, "database", here);
        window
            .update(visual, |shell, _, cx| {
                assert_eq!(shell.workspace.project, here);
                assert_eq!(shell.active_app_view, Some(AppView::Database));
                let tabs = shell.visible_tabs(cx);
                assert!(tabs.contains(&app(AppView::Database, here)));
                assert!(!tabs.contains(&app(AppView::Notes, other)));
                assert!(!tabs.iter().any(|tab| matches!(tab, TitleTab::App(_))));
            })
            .unwrap();
        visual.refresh().unwrap();
        visual.run_until_parked();
        assert!(at(visual, "app-title-tab-Database".into()).is_some());
        assert!(at(visual, "app-title-tab-Notes".into()).is_none());

        // Going back to the project lands on the app tab it had open.
        window
            .update(visual, |shell, window, cx| {
                shell.select_project(other.into(), window, cx)
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, window, cx| {
                assert_eq!(shell.active_app_view, Some(AppView::Notes));
                // Without the tree the view is a Classic surface again and
                // the project tabs wait, hidden.
                shell.workspace.preferences.project_rail_view = "list".into();
                assert!(shell.classic_app_surface());
                assert!(shell.visible_tabs(cx).iter().all(|t| t.session().is_some()));
                shell.workspace.preferences.project_rail_view = "tree".into();
                // Closing the tab leaves the view and drops the tab.
                let tab = shell.app_tab(AppView::Notes);
                shell.close_title_tabs(&[tab.clone()], None, window, cx);
                assert_eq!(shell.active_app_view, None);
                assert!(!shell.workspace.title_order.contains(&tab));
                assert!((shell.workspace.title_order).contains(&app(AppView::Database, here)));
            })
            .unwrap();
    }

    #[gpui::test]
    fn removing_a_project_drops_its_app_tabs(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let db = tempfile::tempdir().unwrap();
        let other = "/work/another-project";
        window
            .update(visual, |shell, _, _| {
                shell.database_dir = db.path().to_owned()
            })
            .unwrap();
        tree(window, visual, &[]);
        click_app(window, visual, "notes", other);
        window
            .update(visual, |shell, window, cx| {
                shell.toggle_pin_tab(app(AppView::Notes, other), cx);
                assert!((shell.workspace.pinned_tabs).contains(&app(AppView::Notes, other)));
                shell.archive_project(other, window, cx);
                let gone = |tab: &TitleTab| *tab == app(AppView::Notes, other);
                assert!(!shell.workspace.title_order.iter().any(gone));
                assert!(!shell.workspace.pinned_tabs.iter().any(gone));
                assert!(!shell.workspace.scope_focus.values().any(gone));
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_tree_drops_the_rail_rows_of_apps_its_projects_carry(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let rows = |visual: &mut VisualTestContext| {
            visual.refresh().unwrap();
            visual.run_until_parked();
            ["Inbox", "Notes", "Database"]
                .map(|label| at(visual, format!("sidebar-navigation-{label}")).is_some())
        };
        tree(window, visual, &[]);
        assert_eq!(rows(visual), [false, false, false]);
        // An app hidden from the project rows keeps its rail row.
        window
            .update(visual, |shell, _, cx| {
                shell.workspace.preferences.project_apps = vec!["notes".into()];
                shell.invalidate_chrome(cx);
            })
            .unwrap();
        assert_eq!(rows(visual), [true, false, true]);
        window
            .update(visual, |shell, _, cx| {
                shell.workspace.preferences.project_rail_view = "list".into();
                shell.invalidate_chrome(cx);
            })
            .unwrap();
        assert_eq!(rows(visual), [true, true, true]);
    }

    #[gpui::test]
    fn each_project_is_a_card_set_apart_from_the_next(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let other = "/work/another-project";
        tree(window, visual, &[other]);
        let order = window
            .update(visual, |shell, _, _| shell.workspace.projects.clone())
            .unwrap();
        let ix = project_ix(window, visual, other);
        // The open project's card runs from its row to its last child.
        let top = at(visual, format!("rail-card-top-{ix}")).expect("the card's head");
        let rows = at(visual, format!("rail-card-row-{ix}")).expect("the card's rows");
        assert_eq!(rows.left(), top.left());
        assert_eq!(rows.right(), top.right());
        let button = at(visual, format!("rail-project-button-{ix}")).unwrap();
        assert!(top.left() < button.left() && button.right() < top.right());
        // The next project's card starts a gap below it.
        if let Some(next) = order.get(ix + 1) {
            let next_ix = project_ix(window, visual, next);
            let next = at(visual, format!("rail-card-top-{next_ix}")).unwrap();
            assert!(next.top() >= rows.bottom() + px(CARD_GAP) - px(0.5));
        } else {
            let previous = at(visual, format!("rail-card-top-{}", ix - 1)).unwrap();
            assert!(top.top() >= previous.bottom() + px(CARD_GAP) - px(0.5));
        }
        // The list view has no cards.
        window
            .update(visual, |shell, _, cx| {
                shell.workspace.preferences.project_rail_view = "list".into();
                shell.invalidate_chrome(cx);
            })
            .unwrap();
        visual.refresh().unwrap();
        visual.run_until_parked();
        assert!(at(visual, format!("rail-card-top-{ix}")).is_none());
    }
}
