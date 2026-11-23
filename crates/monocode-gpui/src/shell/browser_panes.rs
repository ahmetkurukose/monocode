//! Browser pages in the pane tree (docs/native/browser/context.md §4.5).
//! A page is a tab of a group (docs/native/tab-groups.md), borrows its owner
//! session's project, like a terminal pane, and is opened from a group
//! strip's `+` menu or by a link.
use super::*;
use crate::browser::{BrowserPage, PageChanged, PageRequest};

impl Shell {
    /// Where this data mode keeps its browser profiles.
    fn browser_profiles(&self) -> std::path::PathBuf {
        locked(&self.store)
            .ok()
            .and_then(|store| store.data_dir().map(|dir| dir.join("Browser")))
            .unwrap_or_else(|| std::env::temp_dir().join("monocode-browser"))
    }

    pub(super) fn browser_page(
        &mut self,
        pane: i64,
        cx: &mut Context<Self>,
    ) -> Option<Entity<BrowserPage>> {
        if let Some((page, _)) = self.browser_pages.get(&pane) {
            return Some(page.clone());
        }
        let state = self.workspace.browser_panes.get(&pane)?.clone();
        let profiles = self.browser_profiles();
        let zoom = state.zoom.unwrap_or(100);
        let profile = self.browser_pane_profile(pane);
        // The scope whose pages include this one (K21), for its DevTools.
        let devtools_scope =
            (self.session_summaries.get(&state.owner)).map(|owner| monocode_core::browser::Scope {
                session: state.owner,
                project: owner.cwd.clone(),
            });
        let page = cx.new(|cx| {
            let mut page = BrowserPage::new(state.url, state.title, zoom, profiles, cx);
            page.set_profile(profile, cx);
            page.devtools_scope = devtools_scope;
            page.restore_dev_tools(
                state.docked_devtools,
                state.docked_devtools_height.unwrap_or(0),
            );
            page.restore_muted(state.muted);
            page
        });
        // Asked from inside a page's event: handled once that is over, with
        // the window at hand.
        cx.subscribe(&page, move |_, _, request: &PageRequest, cx| {
            let request = request.clone();
            cx.spawn(async move |this, cx| {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.browser_page_request(pane, request, window, cx)
                });
            })
            .detach();
        })
        .detach();
        let changed = cx.subscribe(&page, move |this, page, _: &PageChanged, cx| {
            let (url, title, zoom, devtools, devtools_height, muted) = {
                let page = page.read(cx);
                let zoom = (page.zoom != 100).then_some(page.zoom);
                let (devtools, height) = page.dev_tools_saved();
                let height = (height != 0).then_some(height);
                let muted = page.muted();
                (
                    page.url.clone(),
                    page.title.clone(),
                    zoom,
                    devtools,
                    height,
                    muted,
                )
            };
            if let Some(state) = this.workspace.browser_panes.get_mut(&pane)
                && (state.url != url
                    || state.title != title
                    || state.zoom != zoom
                    || state.docked_devtools != devtools
                    || state.docked_devtools_height != devtools_height
                    || state.muted != muted)
            {
                state.url = url;
                state.title = title;
                state.zoom = zoom;
                state.docked_devtools = devtools;
                state.docked_devtools_height = devtools_height;
                state.muted = muted;
                this.persist(cx);
            }
            this.invalidate_chrome(cx);
            cx.notify();
        });
        self.browser_pages.insert(pane, (page.clone(), changed));
        Some(page)
    }

    pub(super) fn browser_pane_title(&self, pane: i64, cx: &App) -> Option<String> {
        let state = self.workspace.browser_panes.get(&pane)?;
        Some(match self.browser_pages.get(&pane) {
            Some((page, _)) => page.read(cx).display_title(),
            None => crate::browser::page_title(&state.url, &state.title),
        })
    }

    /// A new page as a tab of the focused group (the router decides), for
    /// the focused session's project. Without `url` its address field takes
    /// the keyboard.
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
            // No session to lend its project: make one for the page.
            self.with_pane_owner("a browser page", cx, move |this, window, cx| {
                this.new_browser_pane(url, window, cx)
            });
            return;
        };
        let pane = self.create_browser_pane(owner, url.unwrap_or_default());
        self.place_or_open(pane, OpenIn::default(), cx);
        self.pane_motion = None;
        self.fit_sidebar(cx);
        self.open_browser_pane(pane, window, cx);
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
        if self.workspace.tab_of(pane).is_none() {
            return;
        }
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
        self.merge_scope_tabs(cx);
        // Showing a hidden tab can rekey its title tab: look it up after.
        if let Some(tab) = self.workspace.tab_of(pane) {
            self.remember_title_tab(TitleTab::Session(tab));
        }
        self.reveal_active_tab(cx);
        self.focus_browser_pane(window, cx);
        self.persist(cx);
        cx.notify();
    }

    /// The focused browser pane takes the keyboard: its address field while
    /// it is blank, else the page.
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
        match self.browser_page(pane, cx) {
            Some(page) => page.update(cx, |page, cx| page.focus(window, cx)),
            None => window.focus(&self.focus, cx),
        }
        true
    }

    /// ⌥⌘I: the focused browser pane's DevTools open docked or close.
    pub(super) fn toggle_browser_dev_tools(&mut self, cx: &mut Context<Self>) {
        let Some(pane) = self
            .workspace
            .active
            .filter(|id| self.workspace.browser_panes.contains_key(id))
        else {
            return;
        };
        if let Some(page) = self.browser_page(pane, cx) {
            page.update(cx, |page, cx| page.toggle_dev_tools(cx));
        }
    }

    /// Development hook (`MONOCODE_BROWSER_OPEN_DEVTOOLS`): the focused
    /// page's DevTools open once its page can be inspected.
    pub(super) fn open_dev_tools_when_ready(&mut self, cx: &mut Context<Self>) {
        let Some(pane) = self.workspace.active else {
            return;
        };
        cx.spawn(async move |this, cx| {
            for _ in 0..300 {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                let opened = this.update(cx, |this, cx| {
                    let page = this.browser_pages.get(&pane)?.0.clone();
                    page.read(cx).cdp()?;
                    page.update(cx, |page, cx| page.toggle_dev_tools(cx));
                    Some(())
                });
                if !matches!(opened, Ok(None)) {
                    return;
                }
            }
        })
        .detach();
    }

    /// A browser pane's page, under its group's strip.
    pub(super) fn browser_pane_surface(&mut self, pane: i64, cx: &mut Context<Self>) -> AnyElement {
        let page = self.browser_page(pane, cx);
        div()
            .id(("browser-pane", pane.unsigned_abs() as usize))
            .debug_selector(move || format!("browser-pane-{pane}"))
            .role(Role::Region)
            .aria_label("Browser")
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().min_w_0().children(page))
            .into_any_element()
    }
}

// ---- Pages opened by pages, and profiles ----

impl Shell {
    /// The profile `owner`'s project browses with (K15): the shared default
    /// one, or the project's own.
    fn browser_profile(&self, owner: i64) -> crate::browser::Profile {
        let Some(cwd) = self.session_summaries.get(&owner).map(|s| s.cwd.as_str()) else {
            return crate::browser::Profile::Default;
        };
        let key = monocode_core::files::path_key(cwd);
        if self.workspace.browser_isolated_projects.contains(&key) {
            crate::browser::Profile::for_project(&key)
        } else {
            crate::browser::Profile::Default
        }
    }

    /// The profile pane `pane`'s page browses with: a throwaway one of its
    /// own when an agent asked for that (K22), else its project's.
    pub(super) fn browser_pane_profile(&self, pane: i64) -> crate::browser::Profile {
        match self.workspace.browser_panes.get(&pane) {
            Some(state) => match &state.ephemeral {
                Some(name) => crate::browser::Profile::Ephemeral(name.clone()),
                None => self.browser_profile(state.owner),
            },
            None => crate::browser::Profile::Default,
        }
    }

    fn browser_page_request(
        &mut self,
        pane: i64,
        request: PageRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.workspace.browser_panes.contains_key(&pane) {
            return;
        }
        match request {
            PageRequest::Open {
                url,
                foreground,
                popup,
            } => self.open_browser_page_beside(pane, url, foreground, popup, window, cx),
            PageRequest::Close => self.close_pane(pane, window, cx),
            PageRequest::Grab(result) => self.deliver_browser_grab(pane, result, cx),
        }
    }

    /// A page `opener` opened: a tab right after the opener in its group,
    /// for the same owner. It takes focus only when `foreground`; else it
    /// waits there hidden, like a background tab.
    fn open_browser_page_beside(
        &mut self,
        opener: i64,
        url: String,
        foreground: bool,
        popup: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(opener_state) = self.workspace.browser_panes.get(&opener).cloned() else {
            return;
        };
        let pane = self.create_browser_pane(opener_state.owner, url);
        // An agent's page opens agent pages, in its own profile.
        if let Some(state) = self.workspace.browser_panes.get_mut(&pane) {
            state.opened_by = opener_state.opened_by;
            state.ephemeral = opener_state.ephemeral;
        }
        let active = self.workspace.active;
        if self.workspace.tab_of(opener).is_some() {
            self.workspace.place_tab_after(pane, opener);
        } else {
            // The opener is in no tab any more: where the router says.
            self.place_or_open(pane, OpenIn::default(), cx);
        }
        if let Some(id) = popup
            && let Some(page) = self.browser_page(pane, cx)
        {
            page.update(cx, |page, cx| page.adopt_popup(id, cx));
        }
        if foreground {
            self.pane_motion = None;
            self.fit_sidebar(cx);
            self.open_browser_pane(pane, window, cx);
        } else {
            self.workspace.active = active;
            self.persist(cx);
            cx.notify();
        }
    }

    /// The project menu's "Isolated browser profile" switch.
    pub(super) fn browser_profile_menu(&self, path: &str) -> MenuAction {
        let on = (self.workspace.browser_isolated_projects)
            .contains(&monocode_core::files::path_key(path));
        MenuAction::new("browser-isolated", "Isolated browser profile")
            .description("Own cookies and storage for this project's pages. Open pages reload.")
            .icon("globe")
            .toggle(on)
    }

    /// Turns the project's own browser profile on or off; its open pages
    /// open again in the profile they now use.
    pub(super) fn toggle_browser_isolation(&mut self, path: &str, cx: &mut Context<Self>) {
        let key = monocode_core::files::path_key(path);
        let isolated = &mut self.workspace.browser_isolated_projects;
        if !isolated.remove(&key) {
            isolated.insert(key);
        }
        let pages: Vec<_> = (self.browser_pages.iter())
            .filter(|(pane, _)| self.workspace.browser_panes.contains_key(pane))
            .map(|(pane, (page, _))| (page.clone(), self.browser_pane_profile(*pane)))
            .collect();
        for (page, profile) in pages {
            page.update(cx, |page, cx| page.set_profile(profile, cx));
        }
        self.persist(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        pane_layout::{SplitDir, leaf},
        workspace::{BrowserPane, TitleTab, Workspace},
    };

    #[test]
    fn a_page_saved_with_docked_devtools_loads_without_it() {
        // Pages saved while DevTools docked under them reopened it at launch,
        // which CEF cannot do on macOS: it stopped the app.
        let saved =
            r#"{"owner":1,"url":"https://www.google.com/","devtools":{"open":true,"height":0}}"#;
        let page = serde_json::from_str::<BrowserPane>(saved).unwrap();
        assert_eq!(page.url, "https://www.google.com/");
        let again = serde_json::to_string(&page).unwrap();
        assert!(!again.contains("devtools"), "{again}");
    }

    #[gpui::test]
    fn a_page_keeps_its_docked_devtools_open_and_their_height(cx: &mut gpui::TestAppContext) {
        let saved = BrowserPane {
            owner: 1,
            url: "http://localhost:5173/".into(),
            docked_devtools: true,
            docked_devtools_height: Some(320),
            ..Default::default()
        };
        let json = serde_json::to_string(&saved).unwrap();
        assert_eq!(serde_json::from_str::<BrowserPane>(&json).unwrap(), saved);

        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.new_browser_pane(Some("http://localhost:5173/".into()), window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        let (pane, page) = window
            .update(cx, |shell, _, _| {
                let (pane, (page, _)) = shell.browser_pages.iter().next().unwrap();
                (*pane, page.clone())
            })
            .unwrap();
        let saved = |cx: &mut gpui::TestAppContext| {
            window
                .update(cx, |shell, _, _| {
                    let state = &shell.workspace.browser_panes[&pane];
                    (state.docked_devtools, state.docked_devtools_height)
                })
                .unwrap()
        };
        assert_eq!(saved(cx), (false, None));
        // What the page reports is saved with the pane.
        page.update(cx, |page, cx| {
            page.restore_dev_tools(true, 280);
            cx.emit(crate::browser::PageChanged);
        });
        cx.run_until_parked();
        assert_eq!(saved(cx), (true, Some(280)));
        // And a page made for the pane again starts from it.
        window
            .update(cx, |shell, _, cx| {
                shell.browser_pages.remove(&pane);
                let page = shell.browser_page(pane, cx).unwrap();
                assert_eq!(page.read(cx).dev_tools_saved(), (true, 280));
            })
            .unwrap();
    }

    #[test]
    fn restore_keeps_browser_pages_of_known_sessions_in_open_tabs() {
        let mut state = Workspace {
            tabs: vec![1, -2],
            title_order: vec![TitleTab::Session(1), TitleTab::Session(-2)],
            ..Workspace::default()
        };
        state
            .layouts
            .insert(1, leaf(1).split_pane(1, SplitDir::Right, -1));
        let page = |owner| BrowserPane {
            owner,
            url: "http://localhost:5173/".into(),
            title: "Vite".into(),
            ..Default::default()
        };
        state.browser_panes.insert(-1, page(1));
        // Its own tab, but its owner is gone.
        state.browser_panes.insert(-2, page(9));
        // No tab holds it.
        state.browser_panes.insert(-3, page(1));
        let saved = serde_json::to_string(&state).unwrap();
        let restored = Workspace::restore(Some(&saved), &[(1, "/w".into())], "/w");
        assert_eq!(
            restored.browser_panes.keys().copied().collect::<Vec<_>>(),
            [-1]
        );
        assert_eq!(restored.browser_panes[&-1], page(1));
        assert_eq!(restored.layout(1).leaf_ids(), [1, -1]);
        assert_eq!(restored.tabs, [1]);
        assert_eq!(restored.next_pane_id(), -2);
        assert_eq!(restored.session_for_pane(-1), 1);
        assert!(restored.is_detached_pane(-1));
    }

    #[gpui::test]
    fn pages_are_tabs_of_the_focused_group_and_popups_stack_behind_their_opener(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = super::super::tests::window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                let tabs = shell.workspace.tabs.len();
                // A new page joins the chat's group instead of a title tab
                // of its own.
                shell.new_browser_pane(Some("http://localhost:5173/".into()), window, cx);
                let page = shell.workspace.active.unwrap();
                assert!(shell.workspace.browser_panes.contains_key(&page));
                assert_eq!(shell.workspace.group_tabs(session), [session, page]);
                assert_eq!(
                    shell.workspace.tab_of(page),
                    shell.workspace.tab_of(session)
                );
                // (Showing it can rekey the title tab, never add one.)
                assert_eq!(shell.workspace.tabs.len(), tabs);

                // A background popup waits hidden right after its opener.
                shell.open_browser_page_beside(
                    page,
                    "http://localhost:5173/a".into(),
                    false,
                    None,
                    window,
                    cx,
                );
                let tabs = shell.workspace.group_tabs(page);
                assert_eq!(tabs.len(), 3);
                assert_eq!(tabs[..2], [session, page]);
                let background = tabs[2];
                assert_eq!(shell.workspace.active, Some(page));
                assert_eq!(shell.workspace.layout(page).leaf_ids(), [page]);

                // One in front goes between the opener and that one, and is
                // shown.
                shell.open_browser_page_beside(
                    page,
                    "http://localhost:5173/b".into(),
                    true,
                    None,
                    window,
                    cx,
                );
                let front = shell.workspace.active.unwrap();
                assert_eq!(
                    shell.workspace.group_tabs(page),
                    [session, page, front, background]
                );
                assert_eq!(shell.workspace.layout(front).leaf_ids(), [front]);
                assert_eq!(
                    shell.workspace.browser_panes[&front].url,
                    "http://localhost:5173/b"
                );
                assert_eq!(shell.workspace.browser_panes[&front].owner, session);
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_popup_closed_before_its_tab_adopted_it_opens_at_its_address(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = super::super::tests::window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                shell.new_browser_pane(Some("http://localhost:5173/".into()), window, cx);
                let opener = shell.workspace.active.unwrap();
                // A `window.open` popup that waited too long, or whose opener
                // closed: Chromium closed it, so there is nothing to adopt.
                shell.open_browser_page_beside(
                    opener,
                    "http://localhost:5173/login".into(),
                    false,
                    Some(u64::MAX),
                    window,
                    cx,
                );
                let tabs = shell.workspace.group_tabs(opener);
                let popup = *tabs.last().unwrap();
                assert_eq!(tabs, [session, opener, popup]);
                assert_eq!(shell.workspace.active, Some(opener));
                assert_eq!(
                    shell.workspace.browser_panes[&popup].url,
                    "http://localhost:5173/login"
                );
                let page = shell.browser_pages[&popup].0.read(cx);
                assert_eq!(page.url, "http://localhost:5173/login");
            })
            .unwrap();
    }

    #[test]
    fn a_page_saves_its_zoom_only_when_it_is_not_100_percent() {
        let page = BrowserPane {
            owner: 1,
            url: "http://localhost:5173/".into(),
            ..Default::default()
        };
        assert!(!serde_json::to_string(&page).unwrap().contains("zoom"));
        let zoomed = BrowserPane {
            zoom: Some(125),
            ..page
        };
        let saved = serde_json::to_string(&zoomed).unwrap();
        assert_eq!(serde_json::from_str::<BrowserPane>(&saved).unwrap(), zoomed);
    }

    #[gpui::test]
    fn a_focused_page_zooms_finds_and_takes_the_address_over_the_workspace(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            crate::input::bind(cx);
            super::super::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.new_browser_pane(Some("http://localhost:5173/".into()), window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        cx.refresh().unwrap();
        let (pane, page, scale) = window
            .update(cx, |shell, _, _| {
                let (pane, (page, _)) = shell.browser_pages.iter().next().unwrap();
                (*pane, page.clone(), shell.workspace.preferences.ui_scale)
            })
            .unwrap();
        let m = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        let keys = |cx: &mut gpui::TestAppContext, keys: &str| {
            cx.simulate_keystrokes(window.into(), keys);
            cx.run_until_parked();
        };
        let state = |cx: &mut gpui::TestAppContext| {
            window
                .update(cx, |shell, window, cx| {
                    let page = page.read(cx);
                    (
                        page.zoom,
                        shell.workspace.browser_panes[&pane].zoom,
                        shell.workspace.preferences.ui_scale,
                        page.keyboard_state(window, cx),
                    )
                })
                .unwrap()
        };

        keys(cx, &format!("{m}-= {m}-="));
        assert_eq!(state(cx), (125, Some(125), scale, (false, false, false)));
        keys(cx, &format!("{m}--"));
        assert_eq!(state(cx).0, 110);
        keys(cx, &format!("{m}-0"));
        assert_eq!(state(cx), (100, None, scale, (false, false, false)));

        keys(cx, &format!("{m}-f"));
        assert_eq!(state(cx).3, (true, false, true));
        keys(cx, "escape");
        assert_eq!(state(cx).3, (false, false, false));

        keys(cx, &format!("{m}-l"));
        assert_eq!(state(cx).3, (false, true, false));
        // From the address field, the page's shortcuts still apply.
        keys(cx, &format!("{m}-="));
        assert_eq!(state(cx).0, 110);

        // A click in the field also reaches the pane around it, which must
        // not hand the focus back to the page: typing went to Chromium.
        window
            .update(cx, |shell, window, cx| shell.focus_pane(pane, window, cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(state(cx).3, (false, true, false));
        window
            .update(cx, |shell, window, cx| {
                shell.focus_browser_pane(window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(state(cx).3, (false, true, false));
    }
}
