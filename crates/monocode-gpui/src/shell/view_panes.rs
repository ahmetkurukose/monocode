//! App views (Inbox, Notes, Automations, Database, API), Search and Monos
//! are ordinary tabs (docs/native/tab-groups.md): each is a view pane in a
//! group of its scope, at most one of a kind per scope. A view pane draws
//! its view filling the pane; the view entities are shared, as only one
//! scope is on screen at a time.
use super::tab_router::OpenIn;
use super::*;
use monocode_core::files::path_key;

impl Shell {
    /// What a view or Mono pane shows.
    pub(super) fn view_of(&self, id: i64) -> Option<&TitleTab> {
        (self.workspace.view_panes.get(&id)).or_else(|| self.workspace.mono_panes.get(&id))
    }

    /// The focused pane's view, when a view or Mono pane has focus.
    pub(super) fn focused_view(&self) -> Option<&TitleTab> {
        self.view_of(self.workspace.active?)
    }

    /// The scope a view or Mono pane belongs to.
    pub(super) fn view_pane_scope(&self, id: i64) -> Option<String> {
        if let Some(TitleTab::Mono {
            project, worktree, ..
        }) = self.workspace.mono_panes.get(&id)
        {
            return Some(format!("{}\0{}", path_key(project), path_key(worktree)));
        }
        self.workspace.view_scopes.get(&id).cloned()
    }

    /// The open pane of `scope` showing `view`: views are singletons per
    /// scope.
    pub(super) fn scope_view_pane(&self, view: &TitleTab, scope: &str) -> Option<i64> {
        let same = |tab: &TitleTab| match (tab, view) {
            (TitleTab::Mono { id: a, .. }, TitleTab::Mono { id: b, .. }) => a == b,
            (a, b) => a == b,
        };
        let mut panes: Vec<i64> = (self.workspace.view_panes.iter())
            .chain(self.workspace.mono_panes.iter())
            .filter(|(id, tab)| same(tab) && self.workspace.tab_of(**id).is_some())
            .map(|(id, _)| *id)
            .filter(|id| self.view_pane_scope(*id).as_deref() == Some(scope))
            .collect();
        panes.sort_unstable();
        panes.last().copied()
    }

    /// The view pane of the current scope showing `view`, made and placed
    /// by the router when the scope has none. True when it was open already.
    fn place_view_pane(
        &mut self,
        view: TitleTab,
        how: OpenIn,
        cx: &mut Context<Self>,
    ) -> (i64, bool) {
        let scope = self.tab_scope();
        if let Some(pane) = self.scope_view_pane(&view, &scope) {
            return (pane, true);
        }
        let pane = self.workspace.next_pane_id();
        if matches!(view, TitleTab::Mono { .. }) {
            self.workspace.mono_panes.insert(pane, view);
        } else {
            self.workspace.view_panes.insert(pane, view);
            self.workspace.view_scopes.insert(pane, scope);
        }
        self.place_or_open(pane, how, cx);
        self.fit_sidebar(cx);
        (pane, false)
    }

    /// Open `view` as a tab of the current scope: the open one is shown and
    /// flashed, else a new one joins the focused group.
    pub(super) fn open_view_tab(
        &mut self,
        view: TitleTab,
        how: OpenIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> i64 {
        let (pane, open) = self.place_view_pane(view, how, cx);
        if open {
            self.flash_tab(pane);
        }
        self.show_view_pane(pane, cx);
        self.focus_view_pane(pane, window, cx);
        pane
    }

    /// The current scope's tab of `view` closes, when it has one.
    pub(super) fn close_view_tab(
        &mut self,
        view: &TitleTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self.scope_view_pane(view, &self.tab_scope()) {
            self.request_close_pane(pane, window, cx);
        }
    }

    /// Focus a view pane someone clicked or switched to: its view is made
    /// ready for this scope first.
    pub(super) fn open_view_pane(
        &mut self,
        pane: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.view_of(pane).cloned() else {
            return;
        };
        if self.workspace.tab_of(pane).is_none() {
            return;
        }
        if let TitleTab::App(app) = view {
            self.close_menu(cx);
            self.ready_app_view(app, window, cx);
        }
        self.show_view_pane(pane, cx);
        self.focus_view_pane(pane, window, cx);
    }

    /// Bring a view pane on screen and give it focus in the workspace.
    fn show_view_pane(&mut self, pane: i64, cx: &mut Context<Self>) {
        if self.workspace.tab_of(pane).is_none() {
            return;
        }
        self.navigation_revision += 1;
        if self.workspace.active != Some(pane) {
            self.leave_app_views(cx);
        }
        self.leave_app_search(cx);
        self.close_preview(cx);
        self.history.record(pane);
        self.workspace.open(pane);
        self.review_focused = false;
        self.pending_composer_focus = None;
        // Showing a hidden tab can rekey its title tab: look it up after.
        if let Some(tab) = self.workspace.tab_of(pane) {
            self.remember_title_tab(TitleTab::Session(tab));
        }
        self.sync_mono_focus(cx);
        self.reveal_active_tab(cx);
        self.update_chat_visibility(cx);
        self.sync_right_panel(cx);
        self.invalidate_chrome(cx);
        self.persist(cx);
        cx.notify();
    }

    /// The keyboard goes to the focused view pane's view.
    pub(super) fn focus_view_pane(
        &mut self,
        pane: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.is_some() {
            self.close_settings(window, cx);
        }
        if self.workspace.mono_panes.contains_key(&pane) {
            self.sync_mono_focus(cx);
            if let Some(monos) = self.monos.clone().filter(|_| self.mono_active(cx)) {
                monos.update(cx, |m, cx| m.focus_active(window, cx));
            }
            return;
        }
        if self.workspace.view_panes.get(&pane) == Some(&TitleTab::Search) {
            let view = self.search_pane_view(pane, window, cx);
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        self.focus_app_view(window, cx);
    }

    /// The focused pane's Search page, made from the last draft if need be.
    pub(super) fn search_pane_view(
        &mut self,
        pane: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<crate::search::SearchView> {
        if let Some(open) = self.search_panes.get(&pane) {
            return open.view.clone();
        }
        let open = self.make_search(Some(pane), window, cx);
        let view = open.view.clone();
        self.search_panes.insert(pane, open);
        view
    }

    /// A view pane's body: its view filling the pane.
    pub(super) fn view_pane_surface(
        &mut self,
        pane: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body = match self.workspace.view_panes.get(&pane).cloned() {
            Some(TitleTab::Search) => {
                Some(self.search_pane_view(pane, window, cx).into_any_element())
            }
            Some(TitleTab::App(view)) => {
                self.ensure_app_entity(view, window, cx);
                self.app_view_element(view)
            }
            _ => None,
        };
        div()
            .debug_selector(move || format!("view-pane-{pane}"))
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .overflow_hidden()
            .children(body)
            .into_any_element()
    }

    /// The Monos entity draws one Mono's full page, the focused Mono pane's
    /// (its details then fill the right panel); other Mono panes on screen
    /// draw their chat alone.
    pub(super) fn sync_mono_focus(&mut self, cx: &mut Context<Self>) {
        let Some(monos) = self.monos.clone() else {
            return;
        };
        let have = monos.read(cx).active.clone();
        let want = (self.workspace.active)
            .and_then(|pane| self.mono_pane_id(pane))
            .filter(|_| self.settings.is_none())
            .map(str::to_owned);
        if want == have {
            return;
        }
        monos.update(cx, |m, cx| match &want {
            Some(id) => {
                m.show(id, cx);
            }
            None => m.close(cx),
        });
    }

    /// View panes closed since the last look: Search keeps its draft, Notes
    /// saves, and their views go.
    pub(super) fn forget_closed_view_panes(&mut self, cx: &mut Context<Self>) {
        let gone = self.workspace.prune_view_panes();
        let closed: Vec<i64> = (self.search_panes.keys().copied())
            .filter(|pane| gone.contains(pane) || !self.workspace.view_panes.contains_key(pane))
            .collect();
        for pane in closed {
            if let Some(open) = self.search_panes.remove(&pane) {
                self.saved_search = Some(open.view.read(cx).draft(cx));
            }
        }
        if !gone.is_empty()
            && let Some(notes) = &self.notes
        {
            notes.update(cx, |notes, cx| notes.flush(cx));
        }
        self.workspace.settle_mono_panes();
    }

    /// Older workspaces kept app views, Search and Monos as title-strip
    /// tabs: each becomes a view pane in the scope it was focused in (else
    /// the current one), stacked in that scope's focused group and shown
    /// when it was in front. Entries with nowhere to go are dropped.
    pub(super) fn migrate_view_tabs(&mut self, cx: &App) {
        let views: Vec<TitleTab> = (self.workspace.title_order.iter())
            .filter(|tab| !matches!(tab, TitleTab::Session(_)))
            .cloned()
            .collect();
        if views.is_empty() {
            return;
        }
        let current = self.tab_scope();
        for view in views {
            let focused_in = (self.workspace.scope_focus.iter())
                .find(|(_, tab)| **tab == view)
                .map(|(scope, _)| scope.clone());
            let (stored, scope) = match &view {
                TitleTab::Mono {
                    project, worktree, ..
                } => (
                    view.clone(),
                    format!("{}\0{}", path_key(project), path_key(worktree)),
                ),
                TitleTab::ProjectApp { view: app, project } => (
                    TitleTab::App(*app),
                    focused_in
                        .clone()
                        .unwrap_or_else(|| self.scope_key(project)),
                ),
                _ => (
                    view.clone(),
                    focused_in.clone().unwrap_or_else(|| current.clone()),
                ),
            };
            let pinned = self.workspace.pinned_tabs.contains(&view);
            let target = self.scope_title_tab(&scope, cx);
            if let Some(tab) = target
                && self.scope_view_pane(&stored, &scope).is_none()
            {
                let pane = self.workspace.next_pane_id();
                if matches!(stored, TitleTab::Mono { .. }) {
                    self.workspace.mono_panes.insert(pane, stored);
                } else {
                    self.workspace.view_panes.insert(pane, stored);
                    self.workspace.view_scopes.insert(pane, scope.clone());
                }
                let focus = self.workspace.tab_focus(tab);
                let into = self.workspace.visible_of(focus).unwrap_or(focus);
                let front = focused_in.is_some();
                self.workspace.add_to_group(pane, into, None, false);
                if pinned {
                    self.workspace.set_pinned(pane, true);
                }
                if front {
                    let active = self.workspace.active;
                    let in_front = active.and_then(|id| self.workspace.tab_of(id)) == Some(tab);
                    self.workspace.show(pane);
                    // Only the scope in front keeps the focus.
                    if !in_front {
                        self.workspace.active = active;
                    }
                }
                if let Some(tab) = self.workspace.tab_of(pane) {
                    for entry in self.workspace.scope_focus.values_mut() {
                        if *entry == view {
                            *entry = TitleTab::Session(tab);
                        }
                    }
                }
            }
            self.workspace.title_order.retain(|tab| *tab != view);
            self.workspace.pinned_tabs.retain(|tab| *tab != view);
            self.workspace.scope_focus.retain(|_, tab| *tab != view);
        }
    }
}

#[cfg(test)]
#[path = "view_panes_tests.rs"]
mod tests;
