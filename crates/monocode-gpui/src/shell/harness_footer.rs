//! The footer's harness updates chip: a quiet warning while a newer Claude
//! Code, Codex, … waits, in place of a top-right notice. Its menu updates
//! one harness or all of them, or dismisses the lot.
use super::*;
use crate::notifications::harness_updates::RowView;

const WARNING: u32 = 0xfbbf24;

impl Shell {
    /// Redraws the chip and, when it is open, the menu.
    pub(super) fn harness_updates_changed(&mut self, cx: &mut Context<Self>) {
        let visible = self.notification_state.harness.read(cx).is_visible();
        if let Some(open) = self
            .menu
            .as_ref()
            .filter(|m| m.target == MenuTarget::HarnessUpdates)
        {
            if visible {
                let menu = open.menu.clone();
                let items = self.harness_updates_menu(cx);
                menu.update(cx, |menu, cx| menu.replace_items(items, cx));
            } else {
                self.close_menu(cx);
            }
        }
        self.invalidate_chrome(cx);
        cx.notify();
    }

    pub(super) fn harness_updates_menu(&self, cx: &App) -> Vec<MenuItem> {
        let notice = self.notification_state.harness.read(cx);
        let rows = notice.rows();
        let updated = rows.iter().any(|(_, r)| matches!(r, RowView::Updated(_)));
        let mut items = vec![
            MenuItem::Heading(
                if rows.len() == 1 {
                    "Harness update available"
                } else {
                    "Harness updates available"
                }
                .into(),
                "".into(),
            ),
            MenuItem::Separator,
        ];
        for (update, row) in &rows {
            let id = update.harness.id();
            let versions = format!("{} → {}", update.installed, update.latest);
            let action = MenuAction::new(format!("update:{id}"), update.harness.label()).image(id);
            items.push(
                match row {
                    RowView::Pending => action.value(versions).description("Update"),
                    RowView::Updating => action
                        .value(versions)
                        .description("Updating…")
                        .disabled(true),
                    RowView::Updated(version) => action
                        .description(format!("✓ Updated to {version}"))
                        .disabled(true),
                    RowView::Failed(error) => action
                        .value(versions)
                        .description(format!("Failed: {error} · Click to retry")),
                }
                .into(),
            );
        }
        let busy = notice.is_busy();
        if notice.pending_count() > 1 || !busy {
            items.push(MenuItem::Separator);
        }
        if notice.pending_count() > 1 {
            items.push(
                MenuAction::new("update-all", "Update all")
                    .icon("refresh-cw")
                    .into(),
            );
        }
        if !busy {
            items.push(MenuAction::new("dismiss", "Dismiss").icon("x").into());
        }
        items.push(MenuItem::Separator);
        items.push(MenuItem::Note(
            if updated {
                "Model picker refreshed with the new version’s models."
            } else {
                "New models often need the latest version."
            }
            .into(),
        ));
        items
    }

    pub(super) fn run_harness_updates_menu(&mut self, action: &str, cx: &mut Context<Self>) {
        let harness = self.notification_state.harness.clone();
        if let Some(id) = action.strip_prefix("update:") {
            harness.update(cx, |notice, cx| notice.update_one(id, cx));
        } else if action == "update-all" {
            harness.update(cx, |notice, cx| notice.update_all(cx));
        } else if action == "dismiss" {
            harness.update(cx, |notice, cx| notice.dismiss(cx));
        }
    }

    /// The footer's chip, only while updates wait.
    pub(super) fn harness_updates_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notice = self.notification_state.harness.read(cx);
        if !notice.is_visible() {
            return None;
        }
        let rows = notice.rows();
        let busy = notice.is_busy();
        let failed = rows.iter().any(|(_, r)| matches!(r, RowView::Failed(_)));
        let waiting = notice.pending_count();
        let label: SharedString = if busy {
            "Updating…".into()
        } else if waiting == 0 {
            "Updated".into()
        } else if waiting == 1 {
            "Update available".into()
        } else {
            format!("{waiting} updates").into()
        };
        let description: SharedString = rows
            .iter()
            .map(|(u, _)| format!("{} {} → {}", u.harness.label(), u.installed, u.latest))
            .collect::<Vec<_>>()
            .join(", ")
            .into();
        let open = self.menu_open(&MenuTarget::HarnessUpdates);
        let classic = self.workspace.boxy_layout();
        let dot: gpui::Hsla = if failed {
            rgb(0xf87171).into()
        } else if waiting == 0 {
            rgb(0x34d399).into()
        } else {
            rgb(WARNING).into()
        };
        Some(
            div()
                .id("footer-harness-updates")
                .debug_selector(|| "footer-harness-updates".into())
                .role(Role::Button)
                .aria_label(SharedString::from(format!("Harness updates. {label}")))
                .aria_description(description)
                .aria_expanded(open)
                .relative()
                .h(px(20.))
                .px(px(if classic { 6. } else { 8. }))
                .map(|d| {
                    if classic {
                        d.rounded_md()
                    } else {
                        d.rounded_full()
                    }
                })
                .mr(px(terminal_dock::FOOTER_GAP))
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(6.))
                .whitespace_nowrap()
                .text_size(px(11.))
                .cursor_pointer()
                .text_color(content(if open { 1. } else { 0.55 }))
                .when(open, |d| d.bg(content(0.08)))
                .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                .child(self.harness_updates_anchor.probe())
                .child(icon("alert-circle", 12.).text_color(dot))
                .child(label)
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.menu_open(&MenuTarget::HarnessUpdates) {
                        this.close_menu(cx);
                    } else {
                        let anchor = Anchor::Slot(this.harness_updates_anchor.clone());
                        this.open_menu(MenuTarget::HarnessUpdates, anchor, window, cx);
                    }
                }))
                .into_any_element(),
        )
    }
}
