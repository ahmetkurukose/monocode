//! The shell's side of a browser page's sound: the speaker on its tab while
//! it plays or is muted (a click mutes or unmutes), and "Mute Tab" in the
//! tab's menu. The page keeps the state (`browser_audio.rs`); the pane saves
//! the mute.
use super::*;

/// How a page's tab shows its sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabSound {
    Playing,
    Muted,
}

impl Shell {
    /// The sound of the page of `pane`, if its tab shows any: playing, or
    /// muted (whether it plays or not, so the mute can be undone).
    pub(super) fn browser_tab_sound(&self, pane: i64, cx: &App) -> Option<TabSound> {
        match self.browser_pages.get(&pane) {
            Some((page, _)) => {
                let page = page.read(cx);
                if page.muted() {
                    Some(TabSound::Muted)
                } else {
                    page.audible().then_some(TabSound::Playing)
                }
            }
            None => (self.workspace.browser_panes.get(&pane))
                .is_some_and(|state| state.muted)
                .then_some(TabSound::Muted),
        }
    }

    /// Mutes the tab of browser pane `pane`, or lets it play.
    pub(super) fn set_browser_muted(&mut self, pane: i64, muted: bool, cx: &mut Context<Self>) {
        let Some(state) = self.workspace.browser_panes.get_mut(&pane) else {
            return;
        };
        if state.muted != muted {
            state.muted = muted;
            self.persist(cx);
        }
        if let Some((page, _)) = self.browser_pages.get(&pane) {
            page.clone()
                .update(cx, |page, cx| page.set_muted(muted, cx));
        }
        cx.notify();
    }

    /// "Mute Tab" or "Unmute Tab" for a browser pane's tab menu.
    pub(super) fn browser_mute_menu(&self, pane: i64, cx: &App) -> Option<MenuItem> {
        if !self.workspace.browser_panes.contains_key(&pane) {
            return None;
        }
        let muted = self.browser_tab_sound(pane, cx) == Some(TabSound::Muted);
        Some(
            MenuAction::new(
                if muted { "unmute" } else { "mute" },
                if muted { "Unmute Tab" } else { "Mute Tab" },
            )
            .icon(if muted { "volume-high" } else { "volume-off" })
            .into(),
        )
    }

    /// The speaker on a tab whose page plays sound or is muted.
    pub(super) fn tab_sound_button(
        &self,
        id: i64,
        sound: TabSound,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let muted = sound == TabSound::Muted;
        div()
            .id(("group-tab-sound", id as usize))
            .debug_selector(move || format!("group-tab-sound-{id}"))
            .role(Role::Button)
            .aria_label(if muted { "Unmute Tab" } else { "Mute Tab" })
            .flex_shrink_0()
            .size(px(18.))
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(content(0.1)))
            .child(icon(if muted { "volume-off" } else { "volume-high" }, 13.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.set_browser_muted(id, !muted, cx);
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::TabSound;
    use gpui::{TestAppContext, VisualTestContext};

    #[gpui::test]
    fn a_tab_that_plays_shows_a_speaker_that_mutes_it_and_the_mute_is_saved(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = super::super::tests::window(cx);
        let pane = window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                let pane =
                    shell.open_agent_browser_page(session, "https://a.test/".into(), false, cx);
                shell.open_browser_pane(pane, window, cx);
                pane
            })
            .unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();
        let page = window
            .update(cx, |shell, _, cx| shell.browser_page(pane, cx).unwrap())
            .unwrap();
        let speaker: &'static str = Box::leak(format!("group-tab-sound-{pane}").into_boxed_str());
        let sound = |cx: &mut VisualTestContext| {
            window
                .update(cx, |shell, _, cx| shell.browser_tab_sound(pane, cx))
                .unwrap()
        };
        assert!(cx.debug_bounds(speaker).is_none());
        let label = |cx: &mut VisualTestContext| {
            window
                .update(cx, |shell, _, cx| {
                    let item = shell.browser_mute_menu(pane, cx).unwrap();
                    match item {
                        crate::popover::MenuItem::Action(action) => action.label.to_string(),
                        _ => String::new(),
                    }
                })
                .unwrap()
        };
        assert_eq!(label(cx), "Mute Tab");

        // It plays: a speaker; a click mutes it, and the pane saves it.
        page.read_with(cx, |page, _| page.set_audible_for_test(true));
        cx.run_until_parked();
        assert_eq!(sound(cx), Some(TabSound::Playing));
        let button = cx.debug_bounds(speaker).expect("a speaker on the tab");
        cx.simulate_click(button.center(), Default::default());
        cx.run_until_parked();
        assert_eq!(sound(cx), Some(TabSound::Muted));
        assert!(page.read_with(cx, |page, _| page.muted()));
        let saved = window
            .update(cx, |shell, _, _| shell.workspace.browser_panes[&pane].muted)
            .unwrap();
        assert!(saved);
        assert_eq!(label(cx), "Unmute Tab");

        // Muted, it keeps its speaker after the sound stops; the menu
        // unmutes it, and then the speaker goes with the sound.
        page.read_with(cx, |page, _| page.set_audible_for_test(false));
        cx.run_until_parked();
        assert!(cx.debug_bounds(speaker).is_some());
        window
            .update(cx, |shell, window, cx| {
                shell.run_group_tab_menu(pane, "unmute", window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(sound(cx), None);
        assert!(cx.debug_bounds(speaker).is_none());
        assert!(!page.read_with(cx, |page, _| page.muted()));
    }
}
