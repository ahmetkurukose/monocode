//! Closing a session's tab leaves the session in the sidebar, so a close the
//! user asks for first asks what should happen to it: archive it out of the
//! sidebar as well, or close only the tab. "Closing a session tab" in
//! Settings › General answers once for every close.
use super::*;

/// What a close does to the sessions it leaves behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseChoice {
    Archive,
    Keep,
}

impl CloseChoice {
    /// The saved answer, or `None` to ask.
    fn saved(preference: &str) -> Option<Self> {
        match preference {
            "archive" => Some(Self::Archive),
            "keep" => Some(Self::Keep),
            _ => None,
        }
    }
}

type ThenClose = Box<dyn FnOnce(&mut Shell, Vec<i64>, &mut Window, &mut Context<Shell>)>;

impl Shell {
    /// The sessions among `panes` that stay listed in the sidebar once their
    /// tab closes: a started, unarchived conversation. A blank session never
    /// shows there, so it closes without a word.
    pub(super) fn sessions_left_in_sidebar(&self, panes: &[i64], cx: &App) -> Vec<i64> {
        let mut ids = Vec::new();
        for &id in panes {
            if ids.contains(&id) || !self.session_summaries.contains_key(&id) {
                continue;
            }
            let summary = &self.session_metadata(id, cx).summary;
            if summary.has_messages && !summary.archived && !self.archiving.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    /// Ask what closing `panes` does to their sessions, then run `then` with
    /// the sessions to archive once they have closed. Cancel runs nothing.
    fn ask_close_sessions(
        &mut self,
        panes: &[i64],
        window: &mut Window,
        cx: &mut Context<Self>,
        then: ThenClose,
    ) {
        let sessions = self.sessions_left_in_sidebar(panes, cx);
        if sessions.is_empty() {
            then(self, Vec::new(), window, cx);
            return;
        }
        match CloseChoice::saved(&self.workspace.preferences.close_session_tab) {
            Some(CloseChoice::Archive) => {
                then(self, sessions, window, cx);
                return;
            }
            Some(CloseChoice::Keep) => {
                then(self, Vec::new(), window, cx);
                return;
            }
            None => {}
        }
        let working = sessions
            .iter()
            .any(|id| self.session_metadata(*id, cx).busy);
        let message = match sessions.as_slice() {
            [id] => format!(
                "Close “{}”?",
                self.session_metadata(*id, cx).display_title()
            ),
            many => format!("Close {} sessions?", many.len()),
        };
        let mut detail = String::from(if sessions.len() == 1 {
            "Archive it to take it out of the sidebar as well, or close only the tab and \
             keep it in the sidebar to reopen later."
        } else {
            "Archive them to take them out of the sidebar as well, or close only the tabs and \
             keep them in the sidebar to reopen later."
        });
        if working {
            detail.push_str(" Archiving stops a session that is still working.");
        }
        detail.push_str("\n\nChoose what closing always does in Settings › General.");
        let answer = window.prompt(
            gpui::PromptLevel::Info,
            &message,
            Some(&detail),
            &["Close and Archive", "Close Tab Only", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let archive = match answer.await {
                Ok(0) => sessions,
                Ok(1) => Vec::new(),
                _ => return,
            };
            let _ = this.update_in(cx, move |this, window, cx| then(this, archive, window, cx));
        })
        .detach();
    }

    /// Archive the sessions a close just took out of the title bar. One that
    /// is still open, because a running terminal kept its tab, stays put.
    pub(super) fn archive_closed_sessions(&mut self, ids: &[i64], cx: &mut Context<Self>) {
        for &id in ids {
            if self.workspace.tab_of(id).is_none()
                && self
                    .session_summaries
                    .get(&id)
                    .is_some_and(|summary| !summary.archived)
            {
                self.toggle_archive(id, cx);
            }
        }
    }

    /// The tab close button, its menu and a middle click.
    pub(crate) fn request_close(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tab_can_close(id, cx) {
            return;
        }
        self.close_menu(cx);
        let Some(tab) = self.workspace.tab_of(id) else {
            return;
        };
        self.request_close_title_tabs(vec![TitleTab::Session(tab)], None, window, cx);
    }

    /// A close of title tabs the user asked for, the bulk closes included.
    pub(super) fn request_close_title_tabs(
        &mut self,
        tabs: Vec<TitleTab>,
        preferred: Option<TitleTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_close_title_tabs_then(tabs, preferred, window, cx, |_, _, _| {});
    }

    /// As `request_close_title_tabs`, running `after` once the tabs closed.
    pub(super) fn request_close_title_tabs_then(
        &mut self,
        tabs: Vec<TitleTab>,
        preferred: Option<TitleTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
        after: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let panes = self.title_tab_panes(&tabs);
        self.ask_close_sessions(
            &panes,
            window,
            cx,
            Box::new(move |this, archive, window, cx| {
                this.close_title_tabs_archiving(&tabs, preferred, archive, window, cx);
                after(this, window, cx);
            }),
        );
    }

    /// ⌘W and a split pane's close button: one pane of a split, or the tab.
    pub(super) fn request_close_pane(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let split = self
            .workspace
            .tab_of(id)
            .is_some_and(|tab| self.workspace.layout(tab).is_split());
        if !split {
            self.request_close(id, window, cx);
            return;
        }
        self.ask_close_sessions(
            &[id],
            window,
            cx,
            Box::new(move |this, archive, window, cx| {
                this.close_pane_archiving(id, archive, window, cx)
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::window;
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    #[test]
    fn a_saved_answer_skips_the_question() {
        assert_eq!(CloseChoice::saved("archive"), Some(CloseChoice::Archive));
        assert_eq!(CloseChoice::saved("keep"), Some(CloseChoice::Keep));
        assert_eq!(CloseChoice::saved("ask"), None);
        assert_eq!(CloseChoice::saved(""), None);
    }

    /// Whether `id` still has a tab, and whether it was archived.
    fn state(
        window: gpui::WindowHandle<Shell>,
        visual: &mut VisualTestContext,
        id: i64,
    ) -> (bool, bool) {
        window
            .update(visual, |shell, _, cx| {
                (
                    shell.workspace.tab_of(id).is_some(),
                    shell.session_metadata(id, cx).summary.archived,
                )
            })
            .unwrap()
    }

    fn close(window: gpui::WindowHandle<Shell>, visual: &mut VisualTestContext, id: i64) {
        window
            .update(visual, |shell, w, cx| shell.request_close(id, w, cx))
            .unwrap();
        visual.run_until_parked();
    }

    #[gpui::test]
    fn closing_a_started_session_asks_whether_to_archive_it(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, a, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        close(window, visual, a);
        assert!(visual.has_pending_prompt());
        visual.simulate_prompt_answer("Cancel");
        visual.run_until_parked();
        assert_eq!(
            state(window, visual, a),
            (true, false),
            "Cancel keeps the tab"
        );

        close(window, visual, a);
        visual.simulate_prompt_answer("Close Tab Only");
        visual.run_until_parked();
        assert_eq!(
            state(window, visual, a),
            (false, false),
            "the tab closes and the session stays in the sidebar"
        );
    }

    #[gpui::test]
    fn close_and_archive_takes_the_session_out_of_the_sidebar(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, a, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        close(window, visual, a);
        visual.simulate_prompt_answer("Close and Archive");
        visual.run_until_parked();
        assert_eq!(state(window, visual, a), (false, true));
    }

    #[gpui::test]
    fn a_blank_session_and_a_saved_answer_close_without_asking(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, a, b) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        close(window, visual, b);
        assert!(
            !visual.has_pending_prompt(),
            "a blank session never shows in the sidebar"
        );

        window
            .update(visual, |shell, _, _| {
                shell.workspace.preferences.close_session_tab = "archive".into();
            })
            .unwrap();
        visual.run_until_parked();
        close(window, visual, a);
        assert!(!visual.has_pending_prompt());
        assert_eq!(state(window, visual, a), (false, true));
    }
}
