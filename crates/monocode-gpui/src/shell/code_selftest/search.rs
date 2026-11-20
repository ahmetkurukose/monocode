//! Search Everywhere (⇧⇧) and the command palette (⌘P), driven through
//! the window's own event path and pictured at each step.
use super::*;

impl Driver {
    async fn shift_tap(&self, cx: &mut Cx) {
        for shift in [true, false] {
            let _ = cx.update(|window, cx| {
                window.dispatch_event(
                    PlatformInput::ModifiersChanged(gpui::ModifiersChangedEvent {
                        modifiers: Modifiers {
                            shift,
                            ..Default::default()
                        },
                        capslock: Default::default(),
                    }),
                    cx,
                );
            });
            self.pause(cx, 40).await;
        }
    }

    pub(super) async fn search(&mut self, cx: &mut Cx) {
        self.shift_tap(cx).await;
        self.shift_tap(cx).await;
        self.pause(cx, 600).await;
        let open = self
            .shell(cx, |shell, _, _| shell.everywhere.is_some())
            .unwrap_or(false);
        self.record("s1", "⇧⇧ opens Search Everywhere", open, json!({}));
        self.shot(cx, "everywhere-empty").await;
        self.type_text(cx, "shop").await;
        self.pause(cx, 900).await;
        self.shot(cx, "everywhere-all").await;
        self.keys(cx, "cmd-2").await;
        self.pause(cx, 600).await;
        self.shot(cx, "everywhere-files").await;
        self.keys(cx, "cmd-3").await;
        self.pause(cx, 1200).await;
        let rows = self
            .shell(cx, |shell, _, cx| {
                (shell.everywhere.as_ref())
                    .map(|open| open.view.read(cx).row_labels())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        self.record("s2", "contents are found", !rows.is_empty(), json!(rows));
        self.shot(cx, "everywhere-content").await;
        self.keys(cx, "cmd-7").await;
        self.pause(cx, 500).await;
        self.shot(cx, "everywhere-actions").await;
        self.keys(cx, "escape").await;
        self.pause(cx, 300).await;
        self.keys(cx, "cmd-p").await;
        self.pause(cx, 500).await;
        self.shot(cx, "palette-empty").await;
        self.type_text(cx, "split").await;
        self.pause(cx, 400).await;
        self.shot(cx, "palette-query").await;
        self.keys(cx, "escape").await;
    }
}
