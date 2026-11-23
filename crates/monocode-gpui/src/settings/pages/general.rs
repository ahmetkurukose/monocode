//! GeneralPage and UpdateRow.
use crate::notifications::updater::{self, Action};
use crate::settings::{
    SettingsView,
    widgets::{primary_button, secondary_button},
};
use crate::theme::*;
use gpui::{AnyElement, Context, FontWeight, div, prelude::*, px};

impl SettingsView {
    /// "Allow external CDP connections" and its "Copy endpoint" (context.md
    /// K27): what Playwright's `connectOverCDP` or dev-browser `--connect`
    /// take for the selected project's pages.
    fn external_cdp_row(
        &self,
        p: &crate::settings::Preferences,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut description = "Tools outside Monocode, such as Playwright or dev-browser, may drive this project's pages through a private local address. Sessions Monocode starts get it on their own.".to_owned();
        match &self.browser_endpoint {
            Some(Ok(())) => description.push_str(" Endpoint copied."),
            Some(Err(error)) => description.push_str(&format!(" {error}.")),
            None => {}
        }
        let can_copy = p.browser_external_cdp && self.ctx.browser_scope.is_some();
        self.row(
            Some("browser-external-cdp"),
            false,
            "Allow external CDP connections (Playwright, dev-browser)",
            Some(description.into()),
            vec![
                secondary_button(
                    "browser-copy-cdp-endpoint",
                    "Copy endpoint",
                    false,
                    !can_copy,
                )
                .child("Copy endpoint")
                .when(can_copy, |d| {
                    d.on_click(cx.listener(|this, _, _, cx| this.copy_cdp_endpoint(cx)))
                })
                .into_any_element(),
                self.toggle(
                    "browser-external-cdp",
                    "Allow external CDP connections",
                    p.browser_external_cdp,
                    false,
                    cx,
                    |this, on, cx| {
                        this.browser_endpoint = None;
                        this.update(cx, |p| p.browser_external_cdp = on)
                    },
                ),
            ],
        )
    }

    /// Copies the selected project's CDP endpoint, or says why it cannot.
    pub(in crate::settings) fn copy_cdp_endpoint(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.ctx.browser_scope.clone() else {
            return;
        };
        self.browser_endpoint = Some(
            monocode_core::browser::proxy::endpoint(&scope)
                .map(|url| cx.write_to_clipboard(gpui::ClipboardItem::new_string(url))),
        );
        cx.notify();
    }

    pub(super) fn general_page(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = self.prefs.clone();
        let alerts = vec![
            self.row(
                Some("sounds"),
                true,
                "Sounds",
                Some("Play sounds for activity, completed turns, and updates.".into()),
                vec![
                    self.toggle("sounds", "Sounds", p.sounds, false, cx, |this, on, cx| {
                        this.update(cx, |p| p.sounds = on)
                    }),
                ],
            ),
            self.row(
                Some("notifications"),
                false,
                "Notifications",
                Some("Alert when reminders are due or agents finish or need input.".into()),
                {
                    let mut controls = self.notification_permission_controls(cx);
                    controls.push(self.toggle(
                        "notifications",
                        "Notifications",
                        p.notifications,
                        false,
                        cx,
                        |this, on, cx| {
                            this.update(cx, |p| p.notifications = on);
                            if on {
                                this.probe_notification_permission(true, cx);
                            }
                        },
                    ));
                    controls
                },
            ),
        ];
        let mut workspace = vec![
            self.row(
                Some("file-word-wrap"),
                true,
                "Word wrap",
                Some("Wrap long lines to fit the editor.".into()),
                vec![self.toggle(
                    "file-word-wrap",
                    "Word wrap",
                    p.file_word_wrap,
                    false,
                    cx,
                    |this, on, cx| this.update(cx, |p| p.file_word_wrap = on),
                )],
            ),
            self.row(
                Some("close-session-tab"),
                false,
                "Closing a session tab",
                Some("Archive takes the session out of the sidebar too; Keep leaves it there to reopen.".into()),
                vec![self.segmented(
                    "close-session-tab",
                    "Closing a session tab",
                    &p.close_session_tab,
                    &[("ask", "Ask"), ("archive", "Archive"), ("keep", "Keep")],
                    cx,
                    |this, v, cx| this.update(cx, |p| p.close_session_tab = v.into()),
                )],
            ),
            self.row(
                Some("tab-animations"),
                false,
                "Tab animations",
                Some("Animate tabs as they open and close.".into()),
                vec![self.toggle(
                    "tab-animations",
                    "Tab animations",
                    p.tab_animations,
                    false,
                    cx,
                    |this, on, cx| this.update(cx, |p| p.tab_animations = on),
                )],
            ),
            self.row(
                Some("notes"),
                false,
                "Notes",
                Some("Show Notes in project navigation. Mention saved notes with @note.".into()),
                vec![
                    self.toggle("notes", "Notes", p.notes, false, cx, |this, on, cx| {
                        this.update(cx, |p| p.notes = on)
                    }),
                ],
            ),
        ];
        if cfg!(target_os = "macos") {
            workspace.push(self.row(
                Some("quick-composer"),
                false,
                "Quick composer",
                Some(format!(
                    "Press {} from any app. Return starts a session in the background; ⌘Return opens it. Change the shortcut in Keybindings.",
                    super::keybindings::quick_composer_label(&p)
                ).into()),
                {let mut controls=Vec::new();if let Some(error)=&self.ctx.quick_error{controls.push(super::super::widgets::note(error.clone()));}controls.push(self.toggle(
                    "quick-composer",
                    "Quick composer",
                    p.quick_composer,
                    false,
                    cx,
                    |this, on, cx| this.update(cx, |p| p.quick_composer = on),
                ));controls},
            ));
        }
        workspace.push(self.row(
            Some("working-agents"),
            false,
            "Working agents",
            Some("Show active chats across projects. Finished chats stay until opened.".into()),
            vec![self.toggle(
                "working-agents",
                "Working agents",
                p.working_agents,
                false,
                cx,
                |this, on, cx| this.update(cx, |p| p.working_agents = on),
            )],
        ));
        workspace.push(self.row(
            Some("action-opens-terminal"),
            false,
            "Show terminal for actions",
            Some(
                "Open the terminal when an action runs. Off, actions run in the background.".into(),
            ),
            vec![self.toggle(
                "action-opens-terminal",
                "Show terminal for actions",
                p.action_opens_terminal,
                false,
                cx,
                |this, on, cx| this.update(cx, |p| p.action_opens_terminal = on),
            )],
        ));
        workspace.push(self.row(
            Some("terminal-placement"),
            false,
            "Terminal",
            Some("Where new terminals open: docked below the chat, as a tab in the right panel (Focused layout), or as their own tabs in the title bar.".into()),
            vec![self.segmented(
                "terminal-placement",
                "Terminal",
                p.terminal_placement(),
                &[("bottom", "Bottom"), ("panel", "Side panel"), ("title", "Title bar")],
                cx,
                |this, v, cx| this.update(cx, |p| p.set_terminal_placement(v)),
            )],
        ));
        // Only macOS has the browser pane the links would open in.
        if cfg!(target_os = "macos") {
            workspace.push(self.row(
                Some("browser-all-links"),
                false,
                "Open all links in Monocode",
                Some("Web links in replies and terminals open in a browser tab. Off, only local addresses such as a dev server's do, and the rest open in your browser. ⇧-click a link to open it in the other one.".into()),
                vec![self.toggle(
                    "browser-all-links",
                    "Open all links in Monocode",
                    p.browser_all_links,
                    false,
                    cx,
                    |this, on, cx| this.update(cx, |p| p.browser_all_links = on),
                )],
            ));
            workspace.push(self.external_cdp_row(&p, cx));
            workspace.push(self.row(
                Some("browser-battery-saver"),
                false,
                "Browser battery saver",
                Some("When the battery is low, pages in background tabs pause until you show them. The page you see and pages an agent uses keep full speed. Off, background pages never pause.".into()),
                vec![self.toggle(
                    "browser-battery-saver",
                    "Browser battery saver",
                    p.browser_battery_saver,
                    false,
                    cx,
                    |this, on, cx| this.update(cx, |p| p.browser_battery_saver = on),
                )],
            ));
            workspace.extend(self.site_permission_rows(cx));
            workspace.extend(self.browser_import_rows(cx));
            let memory_saver = p.browser_memory_saver.to_string();
            workspace.push(self.row(
                Some("browser-memory-saver"),
                false,
                "Browser memory saver",
                Some("Frees the memory of a background tab you have not used for this long; it loads again when you show it. Tabs playing sound, with an unsent form, with DevTools open, pinned or used by an agent are kept.".into()),
                vec![self.segmented(
                    "browser-memory-saver",
                    "Browser memory saver",
                    &memory_saver,
                    &[("0", "Off"), ("15", "15 min"), ("30", "30 min"), ("60", "1 h"), ("240", "4 h")],
                    cx,
                    |this, v, cx| {
                        let minutes = v.parse().unwrap_or(0);
                        this.update(cx, |p| p.browser_memory_saver = minutes)
                    },
                )],
            ));
        }
        if cfg!(target_os = "windows") {
            workspace.push(self.row(
                Some("close-to-tray"),
                false,
                "Close to tray",
                Some("Closing a window hides it to the system tray instead of quitting, so running agents keep going. Reopen from the tray icon, and quit for real from its menu. Turn this off to have close end the window.".into()),
                vec![self.toggle(
                    "close-to-tray",
                    "Close to tray",
                    p.close_to_tray,
                    false,
                    cx,
                    |this, on, cx| this.update(cx, |p| p.close_to_tray = on),
                )],
            ));
        }
        let version = crate::notifications::updater::version();
        let about = vec![
            self.row(
                Some("update"),
                true,
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child("Version")
                    .child(
                        div()
                            .font_family(crate::fonts::mono())
                            .text_size(px(12.))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(content(0.45))
                            .child(version),
                    ),
                Some(
                    cx.try_global::<updater::Shared>()
                        .and_then(|shared| updater::status(&shared.0, cx))
                        .unwrap_or_else(|| {
                            "View release notes and check for app and agent updates.".into()
                        })
                        .into(),
                ),
                vec![
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            secondary_button("whats-new", "What's new", false, false)
                                .child("What's new")
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(crate::settings::SettingsEvent::WhatsNew)
                                })),
                        )
                        .child(
                            secondary_button(
                                "check-harness-updates",
                                "Check harness updates",
                                false,
                                false,
                            )
                            .child("Check harness updates")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(crate::settings::SettingsEvent::CheckHarnessUpdates)
                            })),
                        )
                        .child(self.app_update_button(cx))
                        .into_any_element(),
                ],
            ),
        ];
        vec![
            self.group(None, true, "Alerts", None, None, alerts),
            self.group(None, false, "Workspace", None, None, workspace),
            self.group(None, false, "About", None, None, about),
        ]
    }
}
impl SettingsView {
    /// Check for updates, or once one is found, install it in place.
    fn app_update_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let updater = cx
            .try_global::<updater::Shared>()
            .map(|shared| shared.0.clone());
        match updater.as_ref().map(|u| updater::action(u, cx)) {
            Some(Action::Install(version)) => {
                let label = format!("Update to {version}");
                let mut ink = shell_fill();
                ink.a = 1.;
                primary_button("check-updates", label.clone(), false)
                    .child(icon("arrow-down-circle", 14.).text_color(ink))
                    .child(label)
                    .on_click(move |_, _, cx| {
                        if let Some(updater) = &updater {
                            updater::install(updater, cx)
                        }
                    })
                    .into_any_element()
            }
            Some(Action::Busy(label)) => {
                secondary_button("check-updates", label.clone(), false, true)
                    .child(icon("loader", 14.).text_color(content(0.7)))
                    .child(label)
                    .into_any_element()
            }
            Some(Action::Check) | None => {
                secondary_button("check-updates", "Check for updates", false, false)
                    .child(icon("refresh-cw", 14.).text_color(content(0.7)))
                    .child("Check for updates")
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(crate::settings::SettingsEvent::CheckAppUpdates)
                    }))
                    .into_any_element()
            }
        }
    }
}
