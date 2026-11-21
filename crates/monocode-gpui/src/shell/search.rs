use super::tab_router::OpenIn;
use super::*;
use crate::search::{Event, SearchView, Sources};
use monocode_core::search::Target;

pub(super) struct Open {
    pub(super) view: Entity<SearchView>,
    /// Floating command panel rather than a page with its own tab.
    pub(super) panel: bool,
    restore: Option<FocusHandle>,
    _events: Subscription,
}

impl Shell {
    /// The panel floats over whatever is open, like Quick Open, so nothing
    /// underneath is left or rearranged while searching.
    pub(super) fn search_modal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.app_search.as_ref().filter(|s| s.panel)?;
        let viewport = window.viewport_size();
        let width = 680f32.min(viewport.width.as_f32() - 32.);
        let anchor = Anchor::Point(gpui::point(
            px((viewport.width.as_f32() - width) / 2.),
            px((viewport.height.as_f32() * 0.14).max(32.)),
        ));
        let weak = cx.weak_entity();
        Some(
            div()
                .absolute()
                .size_full()
                .child(crate::modal::scrim("app-search-scrim"))
                .child(
                    popover("app-search-modal", anchor)
                        .modal_surface(self.workspace.boxy_layout())
                        .width(width)
                        .gap(0.)
                        .priority(80)
                        .on_dismiss(move |window, cx| {
                            let _ = weak.update(cx, |this, cx| this.close_app_search(window, cx));
                        })
                        .child(open.view.clone()),
                )
                .into_any_element(),
        )
    }
    pub(super) fn leave_app_search(&mut self, cx: &mut Context<Self>) {
        if let Some(search) = self.app_search.take() {
            self.saved_search = Some(search.view.read(cx).draft(cx));
        }
    }
    /// The open Search pages: the floating panel's and each Search tab's.
    pub(super) fn search_views(&self) -> Vec<Entity<SearchView>> {
        (self.app_search.iter())
            .chain(self.search_panes.values())
            .map(|open| open.view.clone())
            .collect()
    }
    pub(super) fn close_app_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = self.app_search.take() {
            self.saved_search = Some(search.view.read(cx).draft(cx));
            self.update_chat_visibility(cx);
            if let Some(restore) = search.restore {
                window.focus(&restore, cx);
            } else {
                self.focus_active(window, cx);
            }
            if let Some(tab) = self.active_title_tab(cx) {
                self.remember_title_tab(tab);
            }
            self.persist(cx);
            self.invalidate_chrome(cx);
            cx.notify();
        }
    }
    /// ⌘K: the floating panel, or the scope's Search tab.
    pub(super) fn open_app_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = &self.app_search {
            window.focus(&search.view.focus_handle(cx), cx);
            return;
        }
        self.close_menu(cx);
        self.navigation_revision += 1;
        if !self.workspace.preferences.search_panel {
            self.open_view_tab(TitleTab::Search, OpenIn::default(), window, cx);
            return;
        }
        if self.palette.is_some() || self.everywhere.is_some() {
            self.close_palette(window, cx);
            self.close_everywhere(window, cx);
        }
        let open = self.make_search(None, window, cx);
        window.focus(&open.view.focus_handle(cx), cx);
        self.app_search = Some(open);
        cx.notify();
    }
    /// A Search page: the floating panel's (`pane` none) or a Search tab's.
    pub(super) fn make_search(
        &mut self,
        pane: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Open {
        let panel = pane.is_none();
        let restore = window.focused(cx);
        let conversations = self
            .sessions
            .values()
            .cloned()
            .chain(
                self.monos
                    .as_ref()
                    .map(|m| m.read(cx).search_conversations())
                    .unwrap_or_default(),
            )
            .collect::<Vec<_>>();
        let mut summaries: HashMap<_, _> = self
            .order
            .iter()
            .map(|&id| (id, self.session_metadata(id, cx).summary.clone()))
            .collect();
        summaries.extend(
            conversations
                .iter()
                .map(|s| (s.read(cx).summary.id, s.read(cx).summary.clone())),
        );
        let summaries = summaries.into_values().collect();
        let live = conversations
            .iter()
            // A paged Mono's saved search covers turns not loaded yet.
            .filter(|s| {
                s.read(cx).history_loaded() && s.read(cx).transcript.read(cx).history_complete()
            })
            .map(|s| {
                (
                    s.read(cx).summary.id,
                    Arc::new(s.read(cx).transcript.read(cx).history_rows().to_vec()),
                )
            })
            .collect();
        let cwd = self.working_copy().to_owned();
        let store = self.store.clone();
        let projects = self.workspace.projects.clone();
        let recent = self
            .workspace
            .recent_files
            .get(&cwd)
            .cloned()
            .unwrap_or_default();
        let view = cx.new(|cx| {
            SearchView::new(
                cwd,
                store,
                Sources {
                    summaries,
                    live,
                    projects,
                    recent,
                    classic: self.workspace.boxy_layout(),
                    panel,
                    logos: self
                        .workspace
                        .project_appearance
                        .iter()
                        .filter_map(|(path, a)| a.logo.clone().map(|logo| (path.clone(), logo)))
                        .collect(),
                },
                cx,
            )
        });
        if let Some(draft) = self.saved_search.clone() {
            view.update(cx, |v, cx| v.restore_draft(draft, cx));
        }
        let events =
            cx.subscribe_in(
                &view,
                window,
                move |this, _, event: &Event, window, cx| match event {
                    Event::Close => match pane {
                        Some(pane) => this.request_close_pane(pane, window, cx),
                        None => this.close_app_search(window, cx),
                    },
                    Event::Open(target, query) => {
                        // The panel goes once used; a Search tab stays open.
                        if pane.is_none() {
                            this.close_app_search(window, cx);
                        }
                        match target {
                            Target::Conversation(id) => {
                                this.open_search_session(*id, None, window, cx)
                            }
                            Target::Message { session, row } => this.open_search_session(
                                *session,
                                Some((*row, query.clone())),
                                window,
                                cx,
                            ),
                            Target::File { path, location } => {
                                let cwd = this.working_copy().to_owned();
                                this.open_review(
                                    crate::review::ReviewTab::File {
                                        cwd,
                                        path: path.clone(),
                                    },
                                    window,
                                    cx,
                                );
                                if let Some((line, column)) = location
                                    && let Some(pane) = this.active_review(cx)
                                {
                                    pane.update(cx, |p, cx| {
                                        p.navigate_file(*line as usize, *column as usize, cx)
                                    });
                                }
                            }
                            Target::Project(path) => this.select_project(path.clone(), window, cx),
                        }
                    }
                },
            );
        Open {
            view,
            panel,
            restore,
            _events: events,
        }
    }
    /// An archived conversation opened from search is back in use: it leaves
    /// the archive so the rail lists the tab it now has.
    fn unarchive_opened(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.session_metadata(id, cx).summary.archived {
            self.toggle_archive(id, cx);
        }
    }
    pub(super) fn open_search_session(
        &mut self,
        id: i64,
        jump: Option<(usize, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.ensure_session(id, cx) {
            self.open(id, window, cx);
            self.unarchive_opened(id, cx);
            if let Some((row, query)) = jump {
                view.update(cx, |v, cx| v.reveal_search(row, query, cx));
            }
            return;
        }
        if let Some(monos) = &self.monos
            && let Some(view) = monos.update(cx, |m, cx| m.open_search_session(id, window, cx))
        {
            if let Some((row, query)) = jump {
                view.update(cx, |v, cx| v.reveal_search(row, query, cx));
            }
            return;
        }
        let store = self.store.clone();
        let revision = self.navigation_revision;
        let work = cx.background_executor().spawn(async move {
            locked(&store)?
                .sessions()?
                .into_iter()
                .find(|s| s.id == id)
                .ok_or_else(|| "Conversation no longer exists".to_owned())
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.navigation_revision != revision {
                    return;
                }
                match result {
                    Ok(summary) => {
                        this.add(summary, cx);
                        this.open(id, window, cx);
                        this.unarchive_opened(id, cx);
                        if let Some((row, query)) = jump {
                            this.sessions[&id].update(cx, |v, cx| v.reveal_search(row, query, cx));
                        }
                    }
                    Err(error) => {
                        this.error = Some(error);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
    pub(super) fn sync_mono_search(&mut self, cx: &mut Context<Self>) {
        let searches = self.search_views();
        if searches.is_empty() {
            return;
        }
        let views = self
            .monos
            .as_ref()
            .map(|m| m.read(cx).search_conversations())
            .unwrap_or_default();
        for view in views {
            let transcript = view.read(cx).transcript.read(cx);
            if !transcript.history_complete() {
                continue;
            }
            let rows = Arc::new(transcript.history_rows().to_vec());
            let summary = view.read(cx).summary.clone();
            for search in &searches {
                let (summary, rows) = (summary.clone(), rows.clone());
                search.update(cx, |v, cx| v.update_session(summary, rows, cx));
            }
        }
    }
    pub(super) fn sync_search_session(&mut self, id: i64, cx: &mut Context<Self>) {
        let searches = self.search_views();
        if searches.is_empty() {
            return;
        }
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let session = session.read(cx);
        let transcript = session.transcript.read(cx);
        if !transcript.history_complete() {
            return;
        }
        let summary = session.summary.clone();
        let rows = Arc::new(transcript.history_rows().to_vec());
        for search in searches {
            let (summary, rows) = (summary.clone(), rows.clone());
            search.update(cx, |v, cx| v.update_session(summary, rows, cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    #[gpui::test]
    fn opening_an_archived_conversation_from_search_unarchives_it(cx: &mut TestAppContext) {
        use super::super::project_rail::ProjectRailRow;
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, a, _) = crate::shell::tests::window(cx);
        let project = "/work/another-project";
        let listed = |shell: &Shell, cx: &App| {
            shell
                .project_rail_tree_children(project, cx)
                .iter()
                .any(|row| matches!(row, ProjectRailRow::Session { id, .. } if *id == a))
        };
        window
            .update(cx, |shell, _, cx| {
                assert!(listed(shell, cx));
                shell.toggle_archive(a, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, w, cx| {
                assert!(shell.session_metadata(a, cx).summary.archived);
                assert!(
                    shell.workspace.tab_of(a).is_none(),
                    "archiving closes the tab"
                );
                assert!(!listed(shell, cx));
                shell.open_search_session(a, None, w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert_eq!(shell.workspace.active, Some(a));
                assert!(!shell.session_metadata(a, cx).summary.archived);
                let stored = locked(&shell.store).unwrap().sessions().unwrap();
                assert!(!stored.iter().find(|s| s.id == a).unwrap().archived);
                assert!(
                    listed(shell, cx),
                    "the reopened session is back in the rail"
                );
            })
            .unwrap();
    }
    #[gpui::test]
    fn search_everywhere_keeps_the_draft_and_opens_a_cross_project_conversation(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            input::bind(cx);
            super::super::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (window, a, b) = crate::shell::tests::window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.open(b, w, cx);
                let input = shell.sessions[&b].read(cx).input.clone();
                input.update(cx, |i, cx| i.set_text("preserve this draft".into(), cx));
                w.focus(&input.focus_handle(cx), cx);
                shell.sessions[&a]
                    .update(cx, |v, _| v.summary.title = "Needle conversation".into());
                shell.focus_search(w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        cx.refresh().unwrap();
        window
            .update(cx, |shell, w, cx| {
                let view = &shell.everywhere.as_ref().unwrap().view;
                assert!(view.focus_handle(cx).is_focused(w));
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), "escape");
        cx.run_until_parked();
        window
            .update(cx, |shell, w, cx| {
                assert!(shell.everywhere.is_none());
                let input = &shell.sessions[&b].read(cx).input;
                assert_eq!(input.read(cx).content, "preserve this draft");
                assert!(input.focus_handle(cx).is_focused(w));
                shell.focus_search(w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        cx.simulate_input(window.into(), "Needle");
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(400));
        cx.run_until_parked();
        cx.simulate_keystrokes(window.into(), "enter");
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(shell.everywhere.is_none());
                assert_eq!(shell.workspace.active, Some(a));
                assert_eq!(shell.workspace.project, "/work/another-project");
                assert_eq!(
                    shell.sessions[&b].read(cx).input.read(cx).content,
                    "preserve this draft"
                );
            })
            .unwrap();
    }
    #[gpui::test]
    fn search_is_a_tab_of_the_focused_group_that_keeps_its_draft(cx: &mut TestAppContext) {
        cx.update(|cx| {
            input::bind(cx);
            super::super::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (window, _, active) = crate::shell::tests::window(cx);
        let pane = window
            .update(cx, |shell, w, cx| {
                let tabs = shell.workspace.tabs.clone();
                shell.open_app_search(w, cx);
                let pane = shell.workspace.active.unwrap();
                assert_eq!(shell.focused_view(), Some(&TitleTab::Search));
                assert_eq!(shell.workspace.group_tabs(pane), [active, pane]);
                assert_eq!(
                    shell.workspace.tabs.len(),
                    tabs.len(),
                    "no title tab is added"
                );
                assert!(shell.app_search.is_none(), "not the floating panel");
                // ⌘K again finds the same tab.
                shell.open(active, w, cx);
                shell.open_app_search(w, cx);
                assert_eq!(shell.workspace.active, Some(pane));
                assert_eq!(shell.tab_flash.map(|(id, _)| id), Some(pane));
                assert_eq!(shell.workspace.group_tabs(pane).len(), 2);
                // Tab keys walk it like any tab.
                shell.cycle(-1, w, cx);
                assert_eq!(shell.workspace.active, Some(active));
                shell.cycle(1, w, cx);
                assert_eq!(shell.workspace.active, Some(pane));
                pane
            })
            .unwrap();
        cx.run_until_parked();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("app-search").is_some(),
            "drawn in its pane"
        );
        window
            .update(visual, |shell, w, cx| {
                let view = shell.search_panes[&pane].view.clone();
                assert!(view.focus_handle(cx).is_focused(w));
                shell.request_close_pane(pane, w, cx);
                assert_eq!(shell.workspace.active, Some(active));
                assert!(shell.workspace.tab_of(pane).is_none());
                assert!(shell.search_panes.is_empty());
                assert!(shell.saved_search.is_some(), "its draft is kept");
            })
            .unwrap();
    }
    #[gpui::test]
    fn search_panel_preference_floats_over_the_workspace_without_a_tab(cx: &mut TestAppContext) {
        cx.update(|cx| {
            input::bind(cx);
            super::super::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (window, _, active) = crate::shell::tests::window(cx);
        let tabs = window
            .update(cx, |shell, w, cx| {
                assert!(
                    !shell.workspace.preferences.search_panel,
                    "search opens as a page by default"
                );
                shell.workspace.preferences.search_panel = true;
                let tabs = shell.visible_tabs(cx);
                shell.open_app_search(w, cx);
                tabs
            })
            .unwrap();
        cx.run_until_parked();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        assert!(visual.debug_bounds("app-search").is_some());
        assert!(
            visual
                .debug_bounds(Box::leak(format!("group-strip-{active}").into_boxed_str()))
                .is_some()
        );
        window
            .update(visual, |shell, _, cx| {
                assert!(shell.search_panes.is_empty());
                assert_eq!(shell.visible_tabs(cx), tabs);
                assert!(!shell.workspace.title_order.contains(&TitleTab::Search));
                assert_eq!(shell.active_title_tab(cx), Some(TitleTab::Session(active)));
            })
            .unwrap();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        window
            .update(visual, |shell, w, cx| {
                assert!(shell.app_search.is_none());
                assert!(shell.saved_search.is_some());
                // Switching back to the page style takes effect on the next open.
                shell.workspace.preferences.search_panel = false;
                shell.open_app_search(w, cx);
                assert_eq!(shell.focused_view(), Some(&TitleTab::Search));
            })
            .unwrap();
    }
}
