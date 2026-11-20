//! Browser pages in the pane tree (docs/native/browser/context.md §4.5).
//! A page borrows its owner session's project, like a terminal pane, and is
//! opened from the title strip's New tab menu.
use super::*;
use crate::{
    browser::{BrowserPage, PageChanged},
    workspace::BrowserPane,
};

impl Shell {
    /// Where this data mode keeps its browser profiles.
    fn browser_profiles(&self) -> std::path::PathBuf {
        locked(&self.store)
            .ok()
            .and_then(|store| store.data_dir().map(|dir| dir.join("Browser")))
            .unwrap_or_else(|| std::env::temp_dir().join("monocode-browser"))
    }

    fn browser_page(&mut self, pane: i64, cx: &mut Context<Self>) -> Option<Entity<BrowserPage>> {
        if let Some((page, _)) = self.browser_pages.get(&pane) {
            return Some(page.clone());
        }
        let state = self.workspace.browser_panes.get(&pane)?.clone();
        let profiles = self.browser_profiles();
        let zoom = state.zoom.unwrap_or(100);
        let page = cx.new(|cx| BrowserPage::new(state.url, state.title, zoom, profiles, cx));
        let changed = cx.subscribe(&page, move |this, page, _: &PageChanged, cx| {
            let (url, title, zoom) = {
                let page = page.read(cx);
                let zoom = (page.zoom != 100).then_some(page.zoom);
                (page.url.clone(), page.title.clone(), zoom)
            };
            if let Some(state) = this.workspace.browser_panes.get_mut(&pane)
                && (state.url != url || state.title != title || state.zoom != zoom)
            {
                state.url = url;
                state.title = title;
                state.zoom = zoom;
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

    /// A new page as its own title tab, for the focused session's project.
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
        let pane = self.workspace.next_pane_id();
        self.workspace.browser_panes.insert(
            pane,
            BrowserPane {
                owner,
                url: url.unwrap_or_default(),
                ..Default::default()
            },
        );
        self.workspace.open(pane);
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

    /// A browser pane: its split header over the page.
    pub(super) fn browser_pane_surface(
        &mut self,
        pane: i64,
        split: Option<f32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let page = self.browser_page(pane, cx);
        div()
            .id(("browser-pane", pane.unsigned_abs() as usize))
            .debug_selector(move || format!("browser-pane-{pane}"))
            .role(Role::Region)
            .aria_label("Browser")
            .size_full()
            .flex()
            .flex_col()
            .when_some(split.filter(|p| *p > 0.), |d, p| {
                d.child(self.split_header(pane, p, true, cx))
            })
            .child(div().flex_1().min_h_0().min_w_0().children(page))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        pane_layout::{SplitDir, leaf},
        workspace::{BrowserPane, TitleTab, Workspace},
    };

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
    }
}
