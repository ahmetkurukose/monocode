//! All title-strip commands operate on the same visible, ordered snapshot.
use super::*;
use crate::workspace::TitleTab;
use monocode_core::files::path_key;

impl Shell {
    /// The workspace keys a tab by one of its sessions. Replacing that session
    /// changes the key, but the same slot and selected width stay on screen.
    pub(super) fn replace_title_tab_key(&mut self, from: i64, to: i64) {
        if from == to {
            return;
        }
        self.strips.replace_key(from, to);
    }

    pub(super) fn tab_scope(&self) -> String {
        format!(
            "{}\0{}",
            path_key(&self.workspace.project),
            path_key(self.working_copy())
        )
    }

    pub(super) fn mono_tab(&self, id: String) -> TitleTab {
        TitleTab::Mono {
            id,
            project: self.workspace.project.clone(),
            worktree: self.working_copy().into(),
        }
    }

    /// Every tab this scope shows, pinned ones first in pin order, then the
    /// strip's own. Shortcuts and cycling walk this order.
    pub(super) fn visible_tabs(&self, cx: &App) -> Vec<TitleTab> {
        let mut tabs = self.scoped_tabs(cx);
        let pinned = &self.workspace.pinned_tabs;
        // Stable, so the strip keeps its own order behind the pins.
        tabs.sort_by_key(|tab| pinned.iter().position(|p| p == tab).unwrap_or(usize::MAX));
        tabs
    }

    /// The tabs drawn as plates in the strip, right of the pinned deck.
    /// Drags, drops and strip indices count only these.
    pub(super) fn strip_tabs(&self, cx: &App) -> Vec<TitleTab> {
        self.scoped_tabs(cx)
            .into_iter()
            .filter(|tab| !self.workspace.pinned_tabs.contains(tab))
            .collect()
    }

    /// The current scope's title tabs. App views, Search and Monos are
    /// tabs inside their groups, never title tabs of their own.
    fn scoped_tabs(&self, cx: &App) -> Vec<TitleTab> {
        let scope = self.tab_scope();
        self.workspace
            .title_order
            .iter()
            .filter(|tab| match tab {
                TitleTab::Session(tab) => {
                    self.workspace.tabs.contains(tab) && {
                        let id = self.workspace.scope_pane(*tab);
                        match self.view_pane_scope(id) {
                            Some(view_scope) => view_scope == scope,
                            None => {
                                let session = self.workspace.session_for_pane(id);
                                self.session_summaries.contains_key(&session)
                                    && self.session_metadata(session, cx).summary.cwd
                                        == self.workspace.project
                                    && self.in_worktree_scope(id, cx)
                            }
                        }
                    }
                }
                _ => false,
            })
            .cloned()
            .collect()
    }

    pub(super) fn active_title_tab(&self, _: &App) -> Option<TitleTab> {
        self.workspace.active_tab().map(TitleTab::Session)
    }

    pub(super) fn remember_title_tab(&mut self, tab: TitleTab) {
        if !matches!(tab, TitleTab::Session(_)) {
            return;
        }
        if !self.workspace.title_order.contains(&tab) {
            self.workspace.title_order.push(tab.clone());
        }
        self.workspace.scope_focus.insert(self.tab_scope(), tab);
    }

    pub(super) fn open_title_tab(
        &mut self,
        tab: &TitleTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match tab {
            TitleTab::Search => self.open_app_search(window, cx),
            TitleTab::App(view) => self.open_app(*view, window, cx),
            TitleTab::ProjectApp { view, project } => {
                self.open_project_app(*view, project.clone(), window, cx)
            }
            TitleTab::Session(id) => {
                let focus = self.workspace.tab_focus(*id);
                self.open(focus, window, cx);
            }
            TitleTab::Mono { id, .. } => self.open_mono_tab(id, window, cx),
        }
    }

    pub(super) fn restore_scope_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let tab = self.workspace.scope_focus.get(&self.tab_scope()).cloned();
        if let Some(tab) = tab.filter(|tab| self.visible_tabs(cx).contains(tab)) {
            self.open_title_tab(&tab, window, cx);
            return true;
        }
        false
    }

    /// The old title strip's tab menu, kept for the title-tab model until
    /// app views, Search and Monos become group tabs (Stage 4).
    #[cfg(test)]
    pub(super) fn run_title_tab_menu(
        &mut self,
        tab: TitleTab,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tabs = self.visible_tabs(cx);
        let Some(index) = tabs.iter().position(|entry| *entry == tab) else {
            return;
        };
        if matches!(action, "pin" | "unpin") {
            self.toggle_pin_tab(tab, cx);
            return;
        }
        if action == "close" {
            if let Some(id) = tab.session() {
                self.request_close(id, window, cx);
            } else {
                self.close_title_tabs(&[tab], None, window, cx);
            }
            return;
        }
        // Like a browser's pinned tabs, these survive the bulk closes.
        let closing = tabs
            .into_iter()
            .enumerate()
            .filter(|(_, entry)| !self.workspace.pinned_tabs.contains(entry))
            .filter(|(ix, entry)| match action {
                "others" => *entry != tab,
                "right" => *ix > index,
                "left" => *ix < index,
                "all" => true,
                _ => false,
            })
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>();
        self.request_close_title_tabs(closing, (action == "others").then_some(tab), window, cx);
    }

    pub(super) fn close_title_tabs(
        &mut self,
        tabs: &[TitleTab],
        preferred: Option<TitleTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_title_tabs_archiving(tabs, preferred, Vec::new(), window, cx);
    }

    /// The panes inside the open ones of `tabs`, hidden group tabs included.
    pub(super) fn title_tab_panes(&self, tabs: &[TitleTab]) -> Vec<i64> {
        tabs.iter()
            .filter(|tab| self.workspace.title_order.contains(tab))
            .filter_map(TitleTab::session)
            .flat_map(|id| self.workspace.layout(id).leaf_ids())
            .flat_map(|id| self.workspace.group_tabs(id))
            .collect()
    }

    /// Close `tabs`, then archive `archive` out of the sidebar.
    pub(super) fn close_title_tabs_archiving(
        &mut self,
        tabs: &[TitleTab],
        preferred: Option<TitleTab>,
        archive: Vec<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panes = self.title_tab_panes(tabs);
        let tabs = tabs.to_vec();
        self.confirm_close_terminal_panes(&panes, window, cx, move |this, window, cx| {
            this.finish_close_title_tabs(&tabs, preferred, window, cx);
            this.archive_closed_sessions(&archive, cx);
        });
    }

    fn finish_close_title_tabs(
        &mut self,
        tabs: &[TitleTab],
        preferred: Option<TitleTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.visible_tabs(cx);
        let closing: HashSet<_> = tabs
            .iter()
            .filter(|tab| self.workspace.title_order.contains(tab))
            .cloned()
            .collect();
        if closing.is_empty() {
            return;
        }
        let active = self.active_title_tab(cx);
        let replace = active.as_ref().is_some_and(|tab| closing.contains(tab));
        // Pins never close on their own, so the strip keeps one tab beside
        // them: closing its last falls back to a blank session, not a pin.
        let strip_left = before
            .iter()
            .any(|tab| !closing.contains(tab) && !self.workspace.pinned_tabs.contains(tab));
        let pins_left = before
            .iter()
            .any(|tab| !closing.contains(tab) && self.workspace.pinned_tabs.contains(tab));
        let next = if replace && pins_left && !strip_left {
            None
        } else if replace {
            preferred
                .filter(|tab| before.contains(tab) && !closing.contains(tab))
                .or_else(|| {
                    let index = before.iter().position(|tab| Some(tab) == active.as_ref())?;
                    // Browsers select the next tab to the right, then the one to the left.
                    before[index + 1..]
                        .iter()
                        .chain(before[..index].iter().rev())
                        .find(|tab| !closing.contains(*tab))
                        .cloned()
                })
        } else {
            None
        };
        self.navigation_revision += 1;
        self.close_menu(cx);
        self.strips.drag = None;
        self.strips.shifts.clear();
        self.cancel_pane_drag(cx);
        let session_active = self.workspace.active;
        if replace {
            self.leave_app_views(cx);
        }
        for tab in &closing {
            if let TitleTab::Session(id) = tab {
                for pane in self.title_tab_panes(std::slice::from_ref(tab)) {
                    self.remember_closed_tab(pane, cx);
                }
                self.workspace.close(*id);
            }
        }
        self.workspace
            .title_order
            .retain(|tab| !closing.contains(tab));
        self.workspace
            .scope_focus
            .retain(|_, tab| !closing.contains(tab));
        self.workspace.prune_pinned_tabs();
        if replace {
            if let Some(monos) = self.monos.clone() {
                monos.update(cx, |m, cx| m.close(cx));
            }
            self.workspace.active = None;
            self.review_focused = false;
            self.pane_motion = None;
            if let Some(tab) = next {
                self.open_title_tab(&tab, window, cx);
            } else {
                self.ensure_project_session(window, cx);
            }
        } else {
            self.workspace.active =
                session_active.filter(|id| self.workspace.tab_of(*id).is_some());
        }
        self.settle_mono_panes(window, cx);
        self.history
            .prune(&self.workspace.open_sessions(), self.workspace.active);
        if self.workspace.active.is_none() {
            self.focus_active(window, cx);
        }
        self.persist(cx);
        cx.notify();
    }

    /// Moves `tab` to strip slot `to`; pinned tabs keep their places.
    #[cfg(test)]
    pub(super) fn reorder_title_tab(&mut self, tab: &TitleTab, to: usize, cx: &App) {
        let mut visible = self.strip_tabs(cx);
        let Some(from) = visible.iter().position(|entry| entry == tab) else {
            return;
        };
        let entry = visible.remove(from);
        visible.insert(to.min(visible.len()), entry);
        let set: HashSet<_> = visible.iter().cloned().collect();
        let mut ordered = visible.into_iter();
        for entry in &mut self.workspace.title_order {
            if set.contains(entry) {
                *entry = ordered.next().unwrap();
            }
        }
        self.workspace.tabs = self
            .workspace
            .title_order
            .iter()
            .filter_map(TitleTab::session)
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::tests::window;
    use gpui::{TestAppContext, WindowHandle};

    fn add_session(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) -> i64 {
        let summary = locked(&shell.store)
            .unwrap()
            .create("codex", &shell.workspace.project)
            .unwrap();
        let id = summary.id;
        shell.add(summary, cx);
        let focus = shell.worktree_focus.get(&shell.workspace.project).cloned();
        shell.seed_worktree(id, focus.as_ref(), cx);
        shell.open(id, window, cx);
        id
    }

    /// A new Mono, opened as a tab of the current scope: its pane.
    fn add_mono(window: WindowHandle<Shell>, cx: &mut TestAppContext) -> i64 {
        window
            .update(cx, |shell, w, cx| {
                shell
                    .mono_manager()
                    .update(cx, |m, cx| m.create(Vec::new(), w, cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(shell.mono_active(cx));
                shell.workspace.active.unwrap()
            })
            .unwrap()
    }

    #[gpui::test]
    fn mixed_tabs_open_in_order_and_close_to_the_last_used(cx: &mut TestAppContext) {
        let (window, other_project, first) = window(cx);
        let mono = add_mono(window, cx);
        let second = window.update(cx, add_session).unwrap();
        cx.run_until_parked();
        let last = add_mono(window, cx);
        window
            .update(cx, |shell, w, cx| {
                // Sessions and Monos share the scope's one group.
                assert_eq!(
                    shell.workspace.group_tabs(first),
                    [first, mono, second, last]
                );
                assert_eq!(shell.visible_tabs(cx).len(), 1);
                shell.open(mono, w, cx);
                assert!(shell.mono_active(cx));
                shell.request_close_pane(mono, w, cx);
                assert_eq!(shell.workspace.active, Some(last), "the last used tab");
                shell.open(second, w, cx);
                assert!(!shell.mono_active(cx));
                shell.request_close_pane(last, w, cx);
                assert_eq!(
                    shell.workspace.active,
                    Some(second),
                    "inactive close preserves focus"
                );
                assert!(
                    shell.sessions[&second]
                        .read(cx)
                        .input
                        .focus_handle(cx)
                        .is_focused(w)
                );
                assert!(shell.workspace.mono_panes.is_empty());
                // Closing the session's tab hands its group to the other.
                shell.request_close_pane(second, w, cx);
                assert_eq!(shell.workspace.active, Some(first));
                assert_eq!(shell.workspace.project, "/work/monocode");
                assert!(shell.workspace.tabs.contains(&other_project));
            })
            .unwrap();
    }

    #[gpui::test]
    fn worktree_tabs_cycle_close_and_return_without_touching_other_scopes(cx: &mut TestAppContext) {
        let (window, other_project, main) = window(cx);
        let mono_main = add_mono(window, cx);
        let feature = crate::worktree_switcher::WorktreeFocus {
            path: "/work/feature".into(),
            branch: Some("feature".into()),
        };
        window
            .update(cx, |shell, w, cx| {
                shell.select_worktree(Some(feature.clone()), w, cx);
                // A copy with nothing open shows the empty workspace; its
                // first chat is started from there.
                assert!(shell.empty_workspace_showing(cx));
                shell.new_chat(cx);
            })
            .unwrap();
        cx.run_until_parked();
        let feature_session = window
            .update(cx, |shell, _, _| shell.workspace.active.unwrap())
            .unwrap();
        let mono_feature = add_mono(window, cx);
        window
            .update(cx, |shell, w, cx| {
                assert_eq!(
                    shell.workspace.group_tabs(feature_session),
                    [feature_session, mono_feature]
                );
                shell.cycle(1, w, cx);
                assert_eq!(shell.workspace.active, Some(feature_session));
                shell.activate_tab(-1, w, cx);
                assert_eq!(shell.workspace.active, Some(mono_feature));
                shell.select_worktree(None, w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, w, cx| {
                assert_eq!(shell.workspace.active, Some(mono_main));
                assert_eq!(shell.workspace.group_tabs(main), [main, mono_main]);
                assert!(shell.workspace.tabs.contains(&other_project));
                shell.select_worktree(Some(feature), w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, w, cx| {
                assert_eq!(shell.workspace.active, Some(mono_feature));
                shell.close_title_tabs(&shell.visible_tabs(cx), None, w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert_eq!(shell.workspace.project, "/work/monocode");
                assert_eq!(shell.working_copy(), "/work/feature");
                // Everything of this copy is closed and stays closed.
                assert_eq!(shell.visible_tabs(cx).len(), 0);
                assert!(shell.workspace.mono_panes.contains_key(&mono_main));
                assert!(!shell.workspace.mono_panes.contains_key(&mono_feature));
                assert!(shell.workspace.tabs.contains(&other_project));
            })
            .unwrap();
    }

    #[gpui::test]
    fn mixed_reorder_and_restart_preserve_order_and_selected_mono(cx: &mut TestAppContext) {
        let (window, _, first) = window(cx);
        let mono = add_mono(window, cx);
        let second = window.update(cx, add_session).unwrap();
        window
            .update(cx, |shell, w, cx| {
                shell.workspace.move_in_group(mono, 0);
                shell.open(mono, w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        let (store, summaries, saved) = window
            .update(cx, |shell, _, _| {
                assert_eq!(shell.workspace.group_tabs(second), [mono, first, second]);
                let db = locked(&shell.store).unwrap();
                (
                    shell.store.clone(),
                    db.sessions().unwrap(),
                    serde_json::to_string(&shell.workspace).unwrap(),
                )
            })
            .unwrap();
        let restored = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| Shell::new(store, summaries, "/work/monocode".into(), Some(saved), cx))
            })
            .unwrap()
        });
        cx.run_until_parked();
        restored
            .update(cx, |shell, _, cx| {
                assert_eq!(shell.workspace.group_tabs(second), [mono, first, second]);
                assert_eq!(shell.workspace.active, Some(mono));
                assert!(shell.mono_active(cx), "its page is back");
            })
            .unwrap();
    }

    #[gpui::test]
    fn rapid_new_tabs_follow_request_order(cx: &mut TestAppContext) {
        let (window, _, first) = window(cx);
        let providers = [Kind::Claude, Kind::Codex, Kind::Claude, Kind::Codex];
        window
            .update(cx, |shell, _, cx| {
                for provider in providers {
                    shell.create(provider, cx);
                }
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                // One title tab; the new sessions stack in its group in order.
                assert_eq!(shell.visible_tabs(cx).len(), 1);
                let tabs = shell.workspace.group_tabs(first);
                assert_eq!(tabs.len(), 5);
                assert_eq!(tabs[0], first);
                for (id, provider) in tabs[1..].iter().zip(providers) {
                    assert_eq!(shell.sessions[id].read(cx).summary.provider, provider.id());
                }
                assert_eq!(shell.workspace.active.as_ref(), tabs.last());
            })
            .unwrap();
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::*;
    use crate::shell::tests::window;
    use gpui::{Modifiers, TestAppContext, point};

    fn sidebar_highlight(
        window: gpui::WindowHandle<Shell>,
        selector: String,
        cx: &mut TestAppContext,
    ) -> bool {
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let selector = Box::leak(selector.into_boxed_str());
        let bounds = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("the sidebar row {selector} is visible"));
        window
            .update(visual, |_, window, _| {
                let scale = window.scale_factor();
                window.painted_quads().into_iter().any(|quad| {
                    (quad.bounds.left().0 - f32::from(bounds.left()) * scale).abs() < 1.
                        && (quad.bounds.top().0 - f32::from(bounds.top()) * scale).abs() < 1.
                        && (quad.bounds.size.width.0 - f32::from(bounds.size.width) * scale).abs()
                            < 1.
                        && (quad.bounds.size.height.0 - f32::from(bounds.size.height) * scale).abs()
                            < 1.
                        && quad
                            .background
                            .as_solid()
                            .is_some_and(|color| (color.a - 0.1).abs() < 0.001)
                })
            })
            .unwrap()
    }

    #[gpui::test]
    fn sidebar_highlights_only_the_visible_mono_or_session(cx: &mut TestAppContext) {
        let (window, other, session) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                // The project tree lists the project's sessions.
                shell.workspace.project_rail_open = true;
                shell
                    .workspace
                    .project_rail_expanded
                    .insert("/work/monocode".into());
                for id in [other, session] {
                    shell.sessions[&id].update(cx, |view, _| {
                        view.summary.cwd = "/work/monocode".into();
                        view.summary.has_messages = true;
                    });
                }
                shell
                    .mono_manager()
                    .update(cx, |m, cx| m.create(vec!["/work/monocode".into()], w, cx));
            })
            .unwrap();
        cx.run_until_parked();
        let id = window
            .update(cx, |shell, w, cx| {
                let id = shell.mono_manager().read(cx).active.clone().unwrap();
                shell.open(session, w, cx);
                id
            })
            .unwrap();
        cx.run_until_parked();
        cx.refresh().unwrap();
        let mono = window
            .update(cx, |shell, _, _| shell.mono_tab(id.clone()))
            .unwrap();
        let mono_row = format!("mono-rail-{id}");
        let session_row = format!("rail-session-{session}");
        assert!(sidebar_highlight(window, session_row.clone(), cx));
        assert!(!sidebar_highlight(window, mono_row.clone(), cx));
        let click = |selector: &str, cx: &mut TestAppContext| {
            let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
            let bounds = visual
                .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
                .unwrap();
            visual.simulate_click(bounds.center(), Default::default());
            visual.simulate_mouse_move(point(px(5.), px(5.)), None, Default::default());
            visual.run_until_parked();
            visual.refresh().unwrap();
        };
        click(&mono_row, cx);
        assert!(sidebar_highlight(window, mono_row.clone(), cx));
        assert!(
            !sidebar_highlight(window, session_row.clone(), cx),
            "a remembered session must not look active while a Mono is visible"
        );
        window
            .update(cx, |shell, _, _| {
                let active = shell.workspace.active.unwrap();
                assert_eq!(shell.mono_pane_id(active), Some(id.as_str()));
            })
            .unwrap();
        click(&session_row, cx);
        assert!(sidebar_highlight(window, session_row.clone(), cx));
        assert!(!sidebar_highlight(window, mono_row.clone(), cx));
        // Opening from the title strip must also clear an old bulk selection.
        window
            .update(cx, |shell, w, cx| {
                for id in [other, session] {
                    shell.select_sidebar_session(
                        id,
                        Modifiers {
                            platform: true,
                            ..Default::default()
                        },
                        w,
                        cx,
                    );
                }
                assert_eq!(shell.selected_sessions.len(), 2);
                shell.open_title_tab(&mono, w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        cx.refresh().unwrap();
        window
            .update(cx, |shell, _, _| {
                assert!(shell.selected_sessions.is_empty());
                assert!(shell.selection_anchor.is_none());
                assert!(shell.sidebar_target.is_none());
            })
            .unwrap();
        assert!(sidebar_highlight(window, mono_row, cx));
        assert!(!sidebar_highlight(window, session_row, cx));
        assert!(!sidebar_highlight(
            window,
            format!("rail-session-{other}"),
            cx
        ));
    }
}
