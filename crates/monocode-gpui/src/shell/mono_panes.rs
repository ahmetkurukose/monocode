//! A Mono is an ordinary tab (docs/native/tab-groups.md): a Mono pane in a
//! group of the scope it opened in. The Mono shown last with focus draws its
//! full page (`Monos`); other Mono panes on screen draw their chat alone.
use super::tab_router::OpenIn;
use super::*;

impl Shell {
    pub(super) fn mono_pane_id(&self, id: i64) -> Option<&str> {
        match self.workspace.mono_panes.get(&id)? {
            TitleTab::Mono { id, .. } => Some(id),
            _ => None,
        }
    }

    /// `Monos::open` showed a Mono (rail, palette, a notification...): it
    /// becomes the scope's tab of that Mono, found again rather than opened
    /// twice. `Monos` has focused its composer already.
    pub(super) fn mono_opened(&mut self, id: String, cx: &mut Context<Self>) {
        if !(self.monos.as_ref()).is_some_and(|m| m.read(cx).active.as_deref() == Some(&id)) {
            return;
        }
        let mono = self.mono_tab(id);
        let scope = self.tab_scope();
        let open = self.scope_view_pane(&mono, &scope);
        if open.is_some_and(|pane| self.workspace.active == Some(pane)) {
            return;
        }
        self.selected_sessions.clear();
        self.selection_anchor = None;
        self.sidebar_target = None;
        self.context_selection = false;
        let pane = match open {
            Some(pane) => {
                self.flash_tab(pane);
                pane
            }
            None => {
                let pane = self.workspace.next_pane_id();
                self.workspace.mono_panes.insert(pane, mono);
                self.place_or_open(pane, OpenIn::default(), cx);
                self.fit_sidebar(cx);
                pane
            }
        };
        self.navigation_revision += 1;
        self.leave_app_views(cx);
        self.leave_app_search(cx);
        self.close_preview(cx);
        self.history.record(pane);
        self.workspace.open(pane);
        self.review_focused = false;
        if let Some(tab) = self.workspace.tab_of(pane) {
            self.remember_title_tab(TitleTab::Session(tab));
        }
        self.reveal_active_tab(cx);
        self.update_chat_visibility(cx);
        self.sync_right_panel(cx);
        self.persist(cx);
        cx.notify();
    }

    /// Open the current scope's tab of Mono `id`.
    pub(super) fn open_mono_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(monos) = self.monos.clone() {
            monos.update(cx, |m, cx| m.open(id, window, cx));
        }
    }

    /// The focused Mono pane's composer.
    pub(super) fn focus_mono_pane(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(id) = self
            .workspace
            .active
            .and_then(|pane| self.mono_pane_id(pane))
        else {
            return false;
        };
        match self.monos.as_ref().and_then(|m| m.read(cx).open_chat(id)) {
            Some(chat) => window.focus(&chat.read(cx).input.focus_handle(cx), cx),
            None => window.focus(&self.focus, cx),
        }
        true
    }

    /// Mono panes whose Mono was deleted, to close.
    fn gone_mono_panes(&self, cx: &App) -> Vec<i64> {
        (self.workspace.mono_panes.keys().copied())
            .filter(|pane| {
                self.mono_pane_id(*pane).is_none_or(|id| {
                    self.monos
                        .as_ref()
                        .is_none_or(|m| m.read(cx).roster.get(id).is_none())
                })
            })
            .collect()
    }

    pub(super) fn mono_panes_unsettled(&self, cx: &App) -> bool {
        !self.gone_mono_panes(cx).is_empty()
    }

    /// Panes whose Mono was deleted close like any tab.
    pub(super) fn settle_mono_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let gone = self.gone_mono_panes(cx);
        if gone.is_empty() {
            return;
        }
        let active = self.workspace.active;
        for pane in gone {
            if !self.workspace.close_pane(pane) {
                self.workspace.close(pane);
            }
            self.workspace.mono_panes.remove(&pane);
        }
        self.pane_motion = None;
        if self.workspace.active != active
            && let Some(next) = self.workspace.active
        {
            self.open(next, window, cx);
        } else if self.workspace.active.is_none() {
            self.ensure_project_session(window, cx);
        }
        self.persist(cx);
    }

    /// A Mono pane: the Mono's full page when it is the one `Monos` shows,
    /// else its chat.
    pub(super) fn mono_pane_surface(
        &mut self,
        pane: i64,
        split: Option<f32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = self.mono_pane_id(pane).map(str::to_owned);
        let monos = self.monos.clone();
        let body = match (id, monos) {
            (Some(id), Some(monos)) if monos.read(cx).active.as_deref() == Some(&id) => {
                let session = monos.read(cx).active_notification_session(cx);
                let linked = session.and_then(|id| self.linked_notice(id, cx));
                Some(
                    div()
                        .relative()
                        .size_full()
                        .child(monos)
                        .children(linked.map(|notice| {
                            div()
                                .absolute()
                                .top(px(12.))
                                .right(px(12.))
                                .w(px(320.))
                                .max_w_full()
                                .child(notice)
                        }))
                        .into_any_element(),
                )
            }
            (Some(id), Some(monos)) => {
                let chat = monos.update(cx, |m, cx| m.pane_chat(&id, cx));
                if let Some(chat) = &chat {
                    chat.update(cx, |chat, cx| chat.set_beside_pane(split.is_some(), cx));
                }
                chat.map(|chat| {
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(chat)
                        .into_any_element()
                })
            }
            _ => None,
        };
        div()
            .debug_selector(move || format!("mono-pane-{pane}"))
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .children(body)
            .into_any_element()
    }

    pub(super) fn mono_pane_title(&self, pane: i64, cx: &App) -> Option<String> {
        let id = self.mono_pane_id(pane)?;
        Some(self.monos.as_ref()?.read(cx).roster.get(id)?.label())
    }

    pub(super) fn mono_pane_mark(&self, pane: i64, cx: &App) -> Option<PaneMark> {
        let id = self.mono_pane_id(pane)?;
        let mono = self.monos.as_ref()?.read(cx).roster.get(id)?.clone();
        Some(PaneMark::Mono(mono.mascot, mono.color))
    }
}
