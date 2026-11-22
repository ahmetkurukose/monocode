//! The shell's side of an agent's presence on a browser page (context.md
//! K47): the badge on the page's tab while the agent counts as active, and
//! the session title its toolbar label shows. The page keeps the state
//! (`browser_presence.rs`).
use super::*;

impl Shell {
    /// Whether an agent counts as using the page of `pane` now.
    pub(super) fn browser_agent_active(&self, pane: i64, cx: &App) -> bool {
        (self.browser_pages.get(&pane))
            .is_some_and(|(page, _)| page.read(cx).active_agent().is_some())
    }

    /// The title an agent session goes by in a page's toolbar.
    pub(super) fn agent_session_title(&self, session: i64, cx: &App) -> String {
        if !self.session_summaries.contains_key(&session) {
            return String::new();
        }
        self.session_metadata(session, cx).display_title()
    }
}

/// The small mark on the tile of a tab an agent is using, in the corner
/// like the strip's unsaved dot.
pub(super) fn agent_badge(id: i64) -> impl IntoElement {
    div()
        .debug_selector(move || format!("group-tab-agent-{id}"))
        .absolute()
        .top(px(-1.))
        .right(px(-1.))
        .size(px(6.))
        .rounded_full()
        .bg(accent_color())
}

#[cfg(test)]
mod tests {
    use crate::shell::browser_agent::AppHost;
    use gpui::{TestAppContext, VisualTestContext};
    use monocode_core::browser::{Host, Scope, UserInput};
    use std::time::{Duration, Instant};

    const S: Duration = Duration::from_secs(1);

    #[gpui::test]
    fn an_agents_tab_shows_a_badge_and_its_toolbar_a_label_for_20_s(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = super::super::tests::window(cx);
        let pane = window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                let pane =
                    shell.open_agent_browser_page(session, "https://a.test/".into(), false, cx);
                // Shown, so its toolbar draws.
                shell.open_browser_pane(pane, window, cx);
                pane
            })
            .unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();
        let page = window
            .update(cx, |shell, _, cx| shell.browser_page(pane, cx).unwrap())
            .unwrap();
        let t = Instant::now();
        page.update(cx, |page, _| page.set_clock_for_test(t));
        let badge: &'static str = Box::leak(format!("group-tab-agent-{pane}").into_boxed_str());
        assert!(cx.debug_bounds(badge).is_none());
        assert!(cx.debug_bounds("browser-agent-label").is_none());

        let host = AppHost::new(cx.to_async());
        let scope = Scope {
            session,
            project: "/work/monocode".into(),
        };
        host.page(&scope, pane).unwrap().agent_used(&scope);
        cx.run_until_parked();
        assert!(cx.debug_bounds(badge).is_some());
        assert!(cx.debug_bounds("browser-agent-label").is_some());
        assert!(cx.debug_bounds("browser-agent-give-back").is_none());
        let title = window
            .update(cx, |shell, _, cx| shell.agent_session_title(session, cx))
            .unwrap();
        let label = page.read_with(cx, |page, _| page.presence_label().unwrap());
        if title.trim().is_empty() {
            assert_eq!(label.agent, "Agent");
        } else {
            assert_eq!(label.agent, format!("Agent · {}", title.trim()));
        }

        // The user clicks the page: control, a countdown, and the button.
        page.read_with(cx, |page, _| page.user_input_for_test(UserInput::Click));
        cx.run_until_parked();
        assert!(host.page(&scope, pane).unwrap().user_control().is_some());
        assert!(cx.debug_bounds("browser-agent-control").is_some());
        let give_back = cx.debug_bounds("browser-agent-give-back").unwrap();
        cx.simulate_click(give_back.center(), Default::default());
        cx.run_until_parked();
        assert!(host.page(&scope, pane).unwrap().user_control().is_none());
        assert!(cx.debug_bounds("browser-agent-give-back").is_none());

        // 20 s after the command, both go by themselves.
        page.update(cx, |page, _| page.set_clock_for_test(t + 20 * S));
        cx.executor().advance_clock(20 * S);
        cx.run_until_parked();
        assert!(cx.debug_bounds(badge).is_none());
        assert!(cx.debug_bounds("browser-agent-label").is_none());
    }
}
