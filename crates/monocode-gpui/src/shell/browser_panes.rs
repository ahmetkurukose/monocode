//! Browser pages in the pane tree (docs/native/browser/context.md §4.5).
//! A page borrows its owner session's project, like a terminal pane, and is
//! opened from the title strip's New tab menu.
use super::*;
use crate::{
    browser::{BrowserPage, PageChanged, PageRequest},
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
        let profile = self.browser_profile(state.owner);
        let page = cx.new(|cx| {
            let mut page = BrowserPage::new(state.url, state.title, profiles, cx);
            page.set_profile(profile, cx);
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
            let (url, title) = {
                let page = page.read(cx);
                (page.url.clone(), page.title.clone())
            };
            if let Some(state) = this.workspace.browser_panes.get_mut(&pane)
                && (state.url != url || state.title != title)
            {
                state.url = url;
                state.title = title;
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
                title: String::new(),
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
        }
    }

    /// A page `opener` opened: its own title tab right after the opener's,
    /// for the same owner. It takes focus only when `foreground`.
    fn open_browser_page_beside(
        &mut self,
        opener: i64,
        url: String,
        foreground: bool,
        popup: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(owner) = self.workspace.browser_panes.get(&opener).map(|b| b.owner) else {
            return;
        };
        let pane = self.workspace.next_pane_id();
        self.workspace.browser_panes.insert(
            pane,
            BrowserPane {
                owner,
                url,
                title: String::new(),
            },
        );
        let active = self.workspace.active;
        self.workspace.open(pane);
        if let Some(after) = self.workspace.tab_of(opener) {
            self.workspace.place_tab_after(pane, after);
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
            .filter_map(|(pane, (page, _))| {
                let owner = self.workspace.browser_panes.get(pane)?.owner;
                Some((page.clone(), self.browser_profile(owner)))
            })
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
}
