//! Pinned title tabs: a title tab can be pinned so bulk closes keep it.
//! The title strip that drew them is gone; group strips pin their own tabs
//! (`Workspace::set_pinned`).
use super::*;
use crate::workspace::TitleTab;

impl Shell {
    pub(super) fn tab_pinned(&self, tab: &TitleTab) -> bool {
        self.workspace.pinned_tabs.contains(tab)
    }

    /// Pins `tab` after the other pins, or puts it back first among the rest.
    #[cfg(test)]
    pub(super) fn toggle_pin_tab(&mut self, tab: TitleTab, cx: &mut Context<Self>) {
        if !self.workspace.title_order.contains(&tab) {
            return;
        }
        if self.tab_pinned(&tab) {
            self.workspace.pinned_tabs.retain(|t| *t != tab);
            self.reorder_title_tab(&tab, 0, cx);
        } else {
            self.workspace.pinned_tabs.push(tab);
        }
        self.persist(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::tests::window;
    use gpui::{TestAppContext, WindowHandle};

    /// A new Mono, a tab in the scope's group: its pane.
    fn add_mono(window: WindowHandle<Shell>, cx: &mut TestAppContext) -> i64 {
        window
            .update(cx, |shell, w, cx| {
                shell
                    .mono_manager()
                    .update(cx, |m, cx| m.create(Vec::new(), w, cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| shell.workspace.active.unwrap())
            .unwrap()
    }

    #[gpui::test]
    fn pinned_mono_tabs_lead_their_group_and_survive_bulk_closes(cx: &mut TestAppContext) {
        let (window, _, first) = window(cx);
        let b = add_mono(window, cx);
        let c = add_mono(window, cx);
        let (store, summaries, saved) = window
            .update(cx, |shell, w, cx| {
                assert_eq!(shell.workspace.group_tabs(first), [first, b, c]);
                shell.workspace.set_pinned(c, true);
                shell.workspace.set_pinned(b, true);
                assert_eq!(shell.workspace.group_tabs(first), [c, b, first]);
                // Close others keeps the pins.
                shell.open(first, w, cx);
                shell.close_other_tabs(w, cx);
                assert_eq!(shell.workspace.group_tabs(first), [c, b, first]);
                shell.workspace.set_pinned(b, false);
                assert_eq!(shell.group_pinned(first), [c]);
                let db = locked(&shell.store).unwrap();
                (
                    shell.store.clone(),
                    db.sessions().unwrap(),
                    serde_json::to_string(&shell.workspace).unwrap(),
                )
            })
            .unwrap();
        let restored = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| Shell::new(store, summaries, "/work/monocode".into(), Some(saved), cx))
            })
            .unwrap()
        });
        cx.run_until_parked();
        restored
            .update(cx, |shell, w, cx| {
                assert_eq!(shell.group_pinned(first), [c]);
                assert!(shell.mono_pane_id(c).is_some());
                shell.request_close_pane(c, w, cx);
                assert!(shell.group_pinned(first).is_empty());
                assert!(!shell.workspace.mono_panes.contains_key(&c));
            })
            .unwrap();
    }

    #[gpui::test]
    fn cmd_w_leaves_a_pinned_title_tab_open_and_closes_tabs_inside_it(cx: &mut TestAppContext) {
        let (window, _, first) = window(cx);
        cx.update(bind_keys);
        window
            .update(cx, |shell, w, cx| {
                shell.toggle_pin_tab(TitleTab::Session(first), cx);
                shell.open(first, w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_keystrokes("cmd-w");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                assert_eq!(shell.workspace.pinned_tabs, [TitleTab::Session(first)]);
                assert_eq!(shell.active_title_tab(cx), Some(TitleTab::Session(first)));
            })
            .unwrap();
        let mono = add_mono(window, visual);
        visual.simulate_keystrokes("cmd-w");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(
                    shell.workspace.tab_of(mono).is_none(),
                    "the Mono tab closed"
                );
                assert_eq!(shell.workspace.active, Some(first));
                assert_eq!(shell.workspace.pinned_tabs, [TitleTab::Session(first)]);
            })
            .unwrap();
    }
}
