//! Browser pages share the session pane tree, like terminal panes: a page
//! is its own title tab or sits beside a chat, and borrows its owner
//! session's project and worktree scope (docs/native/browser.md).
use super::*;
use crate::{
    browser::{BrowserEvent, BrowserView},
    workspace::BrowserPane,
};

impl Shell {
    /// The page of a browser pane. A pane restored with the workspace gets
    /// a sleeping page: nothing loads until it is shown and woken.
    pub(super) fn browser_pane_view(
        &mut self,
        pane: i64,
        cx: &mut Context<Self>,
    ) -> Option<Entity<BrowserView>> {
        if let Some(view) = self.browser_views.get(&pane) {
            return Some(view.clone());
        }
        let state = self.workspace.browser_panes.get(&pane)?.clone();
        let asleep = !state.url.is_empty() && state.url != "about:blank";
        let (viewport, keep_awake) = (state.viewport, state.keep_awake);
        let view = cx.new(|cx| {
            let mut page = BrowserView::new(state.url, state.title, state.user_agent, asleep, cx);
            page.viewport = viewport;
            page.keep_awake = keep_awake;
            page
        });
        self.watch_browser_pane(pane, &view, cx);
        self.browser_views.insert(pane, view.clone());
        Some(view)
    }

    pub(super) fn watch_browser_pane(
        &mut self,
        pane: i64,
        view: &Entity<BrowserView>,
        cx: &mut Context<Self>,
    ) {
        self.observations.push(
            cx.subscribe(
                view,
                move |this, view, event: &BrowserEvent, cx| match event {
                    BrowserEvent::Changed => {
                        // What is saved with the pane: where the page is and
                        // how it is set up, never its content.
                        let page = view.read(cx);
                        let saved = crate::workspace::BrowserPane {
                            owner: 0,
                            url: page.url.clone(),
                            title: page.title.clone(),
                            user_agent: page.user_agent.clone(),
                            viewport: page.viewport,
                            keep_awake: page.keep_awake,
                        };
                        if let Some(state) = this.workspace.browser_panes.get_mut(&pane) {
                            let saved = crate::workspace::BrowserPane {
                                owner: state.owner,
                                ..saved
                            };
                            if *state != saved {
                                *state = saved;
                                this.persist(cx);
                                cx.notify();
                            }
                        }
                    }
                    // The switch on a page's banner: it holds for the session,
                    // on every page of it.
                    BrowserEvent::StopAgent(session) | BrowserEvent::AllowAgent(session) => {
                        let stopped = matches!(event, BrowserEvent::StopAgent(_));
                        if stopped {
                            this.browser_agent_stopped.insert(*session);
                        } else {
                            this.browser_agent_stopped.remove(session);
                        }
                        for page in this.browser_views.values() {
                            page.update(cx, |page, cx| {
                                page.set_agent_stopped(*session, stopped, cx)
                            });
                        }
                        cx.notify();
                    }
                    // The page's console errors, for the chat of the session
                    // the page belongs to.
                    BrowserEvent::AddToChat(text) => {
                        let owner = this.workspace.session_for_pane(pane);
                        this.add_to_chat(owner, text, cx);
                    }
                    // A page asked for a new window. There is no window to open
                    // it in here; the next frame has one.
                    BrowserEvent::OpenNew(url) => {
                        this.pending_browser_pages.push(url.clone());
                        cx.notify();
                    }
                    // A click in the page happens outside GPUI: make its pane
                    // the focused one, as a click on any other pane does.
                    BrowserEvent::Focused => {
                        if this.workspace.active != Some(pane)
                            && this.workspace.tab_of(pane).is_some()
                        {
                            this.workspace.open(pane);
                            this.review_focused = false;
                            this.persist(cx);
                            cx.notify();
                        }
                    }
                },
            ),
        );
    }

    pub(super) fn browser_pane_title(&self, pane: i64, cx: &App) -> Option<String> {
        let state = self.workspace.browser_panes.get(&pane)?;
        Some(match self.browser_views.get(&pane) {
            Some(view) => view.read(cx).label(),
            None if state.title.trim().is_empty() => monocode_core::browser::label(&state.url),
            None => state.title.clone(),
        })
    }

    /// A new page as its own title tab, at `url` or empty with the address
    /// field focused. It borrows the scope of the focused session, else of
    /// one of the project's.
    pub(super) fn new_browser_pane(
        &mut self,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(owner) = self
            .terminal_owner()
            .or_else(|| self.project_session_for_terminal())
        else {
            self.error =
                Some("Open a session in this project before opening a browser page".into());
            cx.notify();
            return;
        };
        let url = url
            .map(|url| monocode_core::browser::address(&url))
            .unwrap_or_default();
        let pane = self.workspace.next_pane_id();
        self.workspace.browser_panes.insert(
            pane,
            BrowserPane {
                owner,
                url: url.clone(),
                ..Default::default()
            },
        );
        // A page opened now loads now; only restored ones start asleep.
        let view = cx.new(|cx| BrowserView::new(url, String::new(), None, false, cx));
        self.watch_browser_pane(pane, &view, cx);
        self.browser_views.insert(pane, view);
        self.workspace.open(pane);
        self.pane_motion = None;
        self.fit_sidebar(cx);
        self.open_browser_pane(pane, window, cx);
    }

    /// The project's browser: its page shown last, or a new one.
    pub(super) fn open_project_browser(
        &mut self,
        project: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.project != project {
            self.select_project(project.clone(), window, cx);
        }
        let key = monocode_core::files::path_key(&project);
        let mut panes: Vec<i64> = (self.workspace.browser_panes.iter())
            .filter(|(pane, page)| {
                self.workspace.tab_of(**pane).is_some()
                    && (self.session_summaries.get(&page.owner))
                        .is_some_and(|owner| monocode_core::files::path_key(&owner.cwd) == key)
            })
            .map(|(pane, _)| *pane)
            .collect();
        // Pane ids count down, so the lowest is the newest.
        panes.sort_unstable();
        let focused = (self.workspace.project_focus.get(&project).copied())
            .filter(|pane| panes.contains(pane));
        match focused.or_else(|| panes.first().copied()) {
            Some(pane) => self.open_browser_pane(pane, window, cx),
            None => self.new_browser_pane(None, window, cx),
        }
    }

    /// Focus a browser pane, showing the tab that holds it.
    pub(super) fn open_browser_pane(
        &mut self,
        pane: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(owner) = self.workspace.browser_panes.get(&pane).map(|b| b.owner) else {
            return;
        };
        let Some(tab) = self.workspace.tab_of(pane) else {
            return;
        };
        if let Some(session) = self.ensure_session(owner, cx) {
            self.workspace.project = session.read(cx).summary.cwd.clone();
        }
        if let Some(monos) = &self.monos {
            monos.update(cx, |m, cx| m.close(cx));
        }
        self.navigation_revision += 1;
        self.leave_app_views(cx);
        self.leave_app_search(cx);
        self.close_preview(cx);
        self.workspace.open(pane);
        self.workspace
            .project_focus
            .insert(self.workspace.project.clone(), pane);
        self.history.record(pane);
        self.review_focused = false;
        self.remember_title_tab(TitleTab::Session(tab));
        self.reveal_active_tab(cx);
        self.focus_browser_pane(window, cx);
        self.persist(cx);
        cx.notify();
    }

    /// The focused browser pane's page takes the keyboard.
    pub(super) fn focus_browser_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(pane) = self
            .workspace
            .active
            .filter(|id| self.workspace.browser_panes.contains_key(id))
        else {
            return false;
        };
        match self.browser_pane_view(pane, cx) {
            Some(view) => view.update(cx, |view, cx| view.focus_page(window, cx)),
            None => window.focus(&self.focus, cx),
        }
        true
    }

    /// Native pages sit above everything GPUI draws, and stay where they
    /// were put until told otherwise. This element goes after the panes in
    /// the frame: once they have painted, a page none of them placed (its
    /// tab is not in front, its pane was closed) is taken off screen.
    pub(super) fn browser_frame(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let covered = self.browser_covered(cx);
        let frames: Vec<crate::browser::PageFrame> = (self.browser_views.values())
            .map(|view| {
                view.update(cx, |view, cx| {
                    view.set_covered(covered, cx);
                    view.begin_frame()
                })
            })
            .collect();
        gpui::canvas(
            |_, _, _| {},
            move |_, _, _, _| {
                for frame in &frames {
                    frame.finish();
                }
            },
        )
        .absolute()
        .size_0()
        .into_any_element()
    }

    /// Whether the Shell is drawing something that floats over the panes:
    /// a menu, a dialog, a picker, a drag. A browser page is a native view
    /// and would sit on top of any of them.
    fn browser_covered(&self, cx: &App) -> bool {
        self.menu.is_some()
            || self.quick_open.is_some()
            || self.image_lightbox.is_some()
            || self.remote_project_picker.is_some()
            || self.link_dialog.is_some()
            || self.action_dialog.is_some()
            || self.background_dialog.is_some()
            || self.session_preview.is_some()
            || self.pending_folder.is_some()
            || self.tab_drag.is_some()
            || self.pane_drag.as_ref().is_some_and(|drag| drag.active)
            || self.app_search.as_ref().is_some_and(|search| search.panel)
            || self.notification_state.whats_new.is_some()
            || self.resources.read(cx).is_open()
            || (self.right_panel.as_ref())
                .is_some_and(|panel| panel.read(cx).changes.read(cx).has_overlay())
    }

    /// Frees what loaded pages hold when the rules say so: off screen too
    /// long, too many loaded, or the system short of memory. Called every
    /// half minute.
    pub(super) fn tend_browser_pages(&mut self, cx: &mut Context<Self>) {
        let loads: Vec<monocode_core::browser::PageLoad> = (self.browser_views.iter())
            .filter_map(|(pane, view)| view.read(cx).load(*pane))
            .collect();
        if loads.is_empty() {
            return;
        }
        let preferences = &self.workspace.preferences;
        let idle_limit = (preferences.browser_sleep_minutes > 0)
            .then(|| u64::from(preferences.browser_sleep_minutes) * 60);
        let asleep = monocode_core::browser::pages_to_sleep(
            &loads,
            idle_limit,
            preferences.browser_max_live.max(1) as usize,
            memory_is_short(),
        );
        for pane in asleep {
            if let Some(view) = self.browser_views.get(&pane) {
                view.update(cx, |page, cx| page.sleep(cx));
            }
        }
    }

    /// Frees every page of the app without closing its pane.
    pub(super) fn sleep_browser_pages(&mut self, cx: &mut Context<Self>) {
        for view in self.browser_views.values() {
            view.update(cx, |view, cx| view.sleep(cx));
        }
        cx.notify();
    }

    /// Closes every browser pane of the app.
    pub(super) fn close_browser_pages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panes: Vec<i64> = self.workspace.browser_panes.keys().copied().collect();
        for pane in panes {
            if self.workspace.tab_of(pane).is_some() {
                self.close_pane(pane, window, cx);
            }
        }
        self.persist(cx);
        cx.notify();
    }

    /// A browser pane: its split header over the page.
    pub(super) fn browser_pane_surface(
        &mut self,
        pane: i64,
        split: Option<f32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = self.browser_pane_view(pane, cx);
        div()
            .id(("browser-pane", pane.unsigned_abs() as usize))
            .debug_selector(move || format!("browser-pane-{pane}"))
            .size_full()
            .flex()
            .flex_col()
            .when_some(split.filter(|p| *p > 0.), |d, p| {
                d.child(self.split_header(pane, p, true, cx))
            })
            .child(div().flex_1().min_h_0().min_w_0().children(view))
            .into_any_element()
    }
}

/// Whether macOS says memory is tight (its pressure level is "warning" or
/// worse): the moment to give back what pages nobody is looking at hold.
fn memory_is_short() -> bool {
    #[cfg(target_os = "macos")]
    {
        let mut level: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        let read = unsafe {
            libc::sysctlbyname(
                c"kern.memorystatus_vm_pressure_level".as_ptr(),
                (&raw mut level).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        // 1 normal, 2 warning, 4 critical.
        read == 0 && level >= 2
    }
    #[cfg(not(target_os = "macos"))]
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{shell::tests::window, workspace::Workspace};
    use gpui::{TestAppContext, VisualTestContext};

    #[gpui::test]
    fn a_browser_page_is_a_tab_of_its_project_and_goes_with_it(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let pane = window
            .update(visual, |shell, w, cx| {
                shell.open(session, w, cx);
                shell.new_browser_pane(Some("localhost:3000".into()), w, cx);
                let pane = shell.workspace.active.unwrap();
                // Its own tab, in the session's project, named after its host.
                assert!(pane < 0);
                assert_eq!(shell.workspace.tab_of(pane), Some(pane));
                assert_eq!(shell.workspace.session_for_pane(pane), session);
                let page = &shell.workspace.browser_panes[&pane];
                assert_eq!(
                    (page.owner, page.url.as_str()),
                    (session, "http://localhost:3000")
                );
                assert_eq!(
                    shell.browser_pane_title(pane, cx).as_deref(),
                    Some("localhost:3000")
                );
                // Opened now, so awake.
                assert!(!shell.browser_views[&pane].read(cx).asleep());
                pane
            })
            .unwrap();
        visual.run_until_parked();
        assert!(visual.debug_bounds("browser-address-row").is_some());
        window
            .update(visual, |shell, w, cx| {
                // Where the page goes is saved with the pane.
                shell.browser_views[&pane].update(cx, |page, cx| page.navigate("example.com", cx));
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, w, cx| {
                assert_eq!(
                    shell.workspace.browser_panes[&pane].url,
                    "https://example.com"
                );
                // The project's browser is this page while it is open.
                let project = shell.workspace.project.clone();
                shell.open(session, w, cx);
                shell.open_project_browser(project, w, cx);
                assert_eq!(shell.workspace.active, Some(pane));
                assert_eq!(shell.workspace.browser_panes.len(), 1);
                // Closing its tab frees the page.
                shell.close_title_tabs(&[TitleTab::Session(pane)], None, w, cx);
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(shell.workspace.browser_panes.is_empty());
                assert!(shell.browser_views.is_empty());
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_page_restored_with_the_workspace_sleeps_until_woken(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        window
            .update(visual, |shell, w, cx| {
                shell.open(session, w, cx);
                shell.new_browser_pane(Some("https://example.com/docs".into()), w, cx);
                let pane = shell.workspace.active.unwrap();
                // As after a restart: the pane is saved, its page is not made.
                shell.browser_views.clear();
                let page = shell.browser_pane_view(pane, cx).unwrap();
                assert!(page.read(cx).asleep(), "nothing loads at launch");
                assert_eq!(page.read(cx).url, "https://example.com/docs");
                page.update(cx, |page, cx| page.wake(cx));
                assert!(!page.read(cx).asleep());
            })
            .unwrap();
        visual.run_until_parked();
    }

    #[test]
    fn restore_keeps_a_page_whose_session_is_still_there() {
        let sessions = vec![(1, "/a".to_string())];
        let mut state = Workspace::default();
        state.open(1);
        state.project = "/a".into();
        for (pane, owner) in [(-1, 1), (-2, 99)] {
            state.browser_panes.insert(
                pane,
                BrowserPane {
                    owner,
                    url: "http://localhost:3000".into(),
                    ..Default::default()
                },
            );
            state.open(pane);
        }
        let restored = Workspace::restore(Some(&state.saved_json()), &sessions, "/a");
        assert_eq!(
            restored.browser_panes.keys().copied().collect::<Vec<_>>(),
            [-1]
        );
        assert!(restored.tab_of(-1).is_some());
        assert!(restored.tab_of(-2).is_none(), "its session is gone");
        // The next pane takes an id below every pane there is.
        assert_eq!(restored.next_pane_id(), -2);
    }
}
