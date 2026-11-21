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

/// An open close question: the sessions it is about and the close to run.
pub(super) struct CloseDialog {
    sessions: Vec<i64>,
    then: Option<ThenClose>,
    /// Save the answer as "Closing a session tab".
    remember: bool,
    focus: FocusHandle,
}

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
    pub(super) fn ask_close_sessions(
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
        self.close_dialog = Some(CloseDialog {
            sessions,
            then: Some(then),
            remember: false,
            focus: cx.focus_handle(),
        });
        self.close_menu(cx);
        cx.notify();
    }

    /// Answer the open close dialog; `None` cancels it.
    pub(super) fn answer_close_dialog(
        &mut self,
        choice: Option<CloseChoice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut dialog) = self.close_dialog.take() else {
            return;
        };
        cx.notify();
        let (Some(choice), Some(then)) = (choice, dialog.then.take()) else {
            return;
        };
        if dialog.remember {
            self.workspace.preferences.close_session_tab = match choice {
                CloseChoice::Archive => "archive",
                CloseChoice::Keep => "keep",
            }
            .into();
            self.apply_preferences(cx);
            self.sync_settings(cx);
            self.persist(cx);
        }
        let archive = match choice {
            CloseChoice::Archive => dialog.sessions,
            CloseChoice::Keep => Vec::new(),
        };
        then(self, archive, window, cx);
    }

    /// The close dialog over the window: the session, the two ways to close
    /// it as choices, and Cancel.
    pub(super) fn close_session_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.close_dialog.as_ref()?;
        let focus = dialog.focus.clone();
        if !focus.contains_focused(window, cx) {
            window.focus(&focus, cx);
        }
        let many = dialog.sessions.len() > 1;
        let remember = dialog.remember;
        let working = dialog
            .sessions
            .iter()
            .any(|id| self.session_metadata(*id, cx).busy);
        let subject: AnyElement = match dialog.sessions.as_slice() {
            [id] => {
                let meta = self.session_metadata(*id, cx);
                div()
                    .mt(px(12.))
                    .h(px(34.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(9.))
                    .bg(content(0.05))
                    .text_size(px(13.))
                    .text_color(content(0.85))
                    .child(crate::theme::provider_icon(&meta.summary.provider, 14.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(meta.display_title())),
                    )
                    .into_any_element()
            }
            ids => crate::modal::body(format!(
                "{} sessions stay in the sidebar unless you archive them.",
                ids.len()
            ))
            .mt(px(6.))
            .into_any_element(),
        };
        let option = |id: &'static str,
                      glyph: &'static str,
                      label: &'static str,
                      detail: String,
                      choice: CloseChoice,
                      cx: &mut Context<Self>| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::Button)
                .aria_label(label)
                .group(id)
                .w_full()
                .px(px(12.))
                .py(px(10.))
                .flex()
                .items_center()
                .gap(px(12.))
                .rounded(px(10.))
                .border_1()
                .border_color(content(0.1))
                .cursor_pointer()
                .hover(|s| s.bg(content(0.06)).border_color(content(0.18)))
                .child(
                    div()
                        .size(px(30.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.))
                        .bg(content(0.07))
                        .child(
                            icon(glyph, 15.)
                                .text_color(content(0.7))
                                .group_hover(id, |s| s.text_color(content(1.))),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(content(1.))
                                .child(label),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(16.))
                                .text_color(content(0.5))
                                .child(detail),
                        ),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.answer_close_dialog(Some(choice), window, cx)
                }))
        };
        let (it, its) = if many {
            ("them", "their")
        } else {
            ("it", "its")
        };
        let archive_detail = format!(
            "Takes {it} out of the sidebar; find {it} under Archived.{}",
            if working {
                " Stops the running turn."
            } else {
                ""
            }
        );
        let keep_detail = format!("Leaves {it} in the sidebar to reopen with {its} history.");
        let checkbox = div()
            .id("close-session-remember")
            .debug_selector(|| "close-session-remember-box".into())
            .role(Role::CheckBox)
            .aria_label("Remember my choice")
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(content(if remember { 0.85 } else { 0.55 }))
            .hover(|s| s.text_color(content(0.9)))
            .child(
                div()
                    .size(px(14.))
                    .flex_shrink_0()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(content(if remember { 0.9 } else { 0.35 }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(remember, |d| {
                        d.bg(content(0.9))
                            .child(icon("check", 10.).text_color(background_base()))
                    }),
            )
            .child("Remember my choice")
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(dialog) = &mut this.close_dialog {
                    dialog.remember = !dialog.remember;
                    cx.notify();
                }
            }));
        let card = crate::modal::card().w(px(420.)).child(
            crate::modal::card_body()
                .child(crate::modal::title(if many {
                    format!("Close {} sessions?", dialog.sessions.len())
                } else {
                    "Close this session?".to_owned()
                }))
                .child(subject)
                .child(
                    div()
                        .mt(px(16.))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(option(
                            "close-session-archive",
                            "archive",
                            "Close and archive",
                            archive_detail,
                            CloseChoice::Archive,
                            cx,
                        ))
                        .child(option(
                            "close-session-keep",
                            "x",
                            "Close tab only",
                            keep_detail,
                            CloseChoice::Keep,
                            cx,
                        )),
                )
                .child(
                    div()
                        .mt(px(18.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(checkbox)
                        .child(
                            crate::modal::ghost_button("close-session-cancel", "Cancel")
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.answer_close_dialog(None, window, cx)
                                })),
                        ),
                )
                .when(remember, |d| {
                    d.child(
                        div()
                            .mt(px(6.))
                            .text_size(px(11.))
                            .text_color(content(0.4))
                            .child("Change it later in Settings › General."),
                    )
                }),
        );
        let entity = cx.weak_entity();
        Some(
            div()
                .absolute()
                .inset_0()
                .track_focus(&focus)
                .key_context("CloseSessionDialog")
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    let choice = match event.keystroke.key.as_str() {
                        "escape" => None,
                        "enter" => Some(CloseChoice::Archive),
                        _ => return,
                    };
                    cx.stop_propagation();
                    this.answer_close_dialog(choice, window, cx);
                }))
                .child(crate::modal::overlay(
                    "close-session",
                    card,
                    move |window, cx| {
                        let _ = entity
                            .update(cx, |this, cx| this.answer_close_dialog(None, window, cx));
                    },
                ))
                .into_any_element(),
        )
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

    /// ⌘W and a split pane's close button: one tab of a group or pane of a
    /// split, else the title tab.
    pub(super) fn request_close_pane(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.closes_inside_its_tab(id) {
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

    fn asking(window: gpui::WindowHandle<Shell>, visual: &mut VisualTestContext) -> bool {
        window
            .update(visual, |shell, _, _| shell.close_dialog.is_some())
            .unwrap()
    }

    fn answer(
        window: gpui::WindowHandle<Shell>,
        visual: &mut VisualTestContext,
        choice: Option<CloseChoice>,
    ) {
        window
            .update(visual, |shell, w, cx| {
                shell.answer_close_dialog(choice, w, cx)
            })
            .unwrap();
        visual.run_until_parked();
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
        assert!(asking(window, visual));
        answer(window, visual, None);
        assert_eq!(
            state(window, visual, a),
            (true, false),
            "Cancel keeps the tab"
        );

        close(window, visual, a);
        answer(window, visual, Some(CloseChoice::Keep));
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
        answer(window, visual, Some(CloseChoice::Archive));
        assert_eq!(state(window, visual, a), (false, true));
    }

    #[gpui::test]
    fn a_blank_session_and_a_saved_answer_close_without_asking(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, a, b) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        close(window, visual, b);
        assert!(
            !asking(window, visual),
            "a blank session never shows in the sidebar"
        );

        window
            .update(visual, |shell, _, _| {
                shell.workspace.preferences.close_session_tab = "archive".into();
            })
            .unwrap();
        visual.run_until_parked();
        close(window, visual, a);
        assert!(!asking(window, visual));
        assert_eq!(state(window, visual, a), (false, true));
    }

    #[gpui::test]
    fn remembering_the_answer_saves_it_and_clicking_an_option_closes(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, a, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        close(window, visual, a);
        let remember = visual.debug_bounds("close-session-remember-box").unwrap();
        visual.simulate_click(remember.center(), Default::default());
        visual.run_until_parked();
        let keep = visual.debug_bounds("close-session-keep").unwrap();
        visual.simulate_click(keep.center(), Default::default());
        visual.run_until_parked();
        assert!(!asking(window, visual));
        assert_eq!(state(window, visual, a), (false, false));
        window
            .update(visual, |shell, _, _| {
                assert_eq!(shell.workspace.preferences.close_session_tab, "keep")
            })
            .unwrap();
    }
}
