//! Settings → Browser: every setting of the browser pane in one page —
//! links, power and memory, external CDP, site permissions, importing,
//! saved passwords and extensions.
use crate::settings::{SettingsView, widgets::secondary_button};
use gpui::{AnyElement, Context, prelude::*};

impl SettingsView {
    pub(super) fn browser_page(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut page = Vec::new();
        // Only macOS has the browser pane these rows act on.
        if cfg!(target_os = "macos") {
            let browsing = self.browsing_rows(cx);
            page.push(self.group(None, true, "Browsing", None, None, browsing));
            page.push(self.link_routing_group(cx));
            let mut data = self.site_permission_rows(true, cx);
            let first = data.is_empty();
            data.extend(self.browser_import_rows(first, cx));
            page.push(self.group(None, false, "Sites and data", None, None, data));
        }
        let first = page.is_empty();
        page.extend(self.password_groups(first, cx));
        page.extend(self.extension_groups(false, cx));
        page
    }

    fn browsing_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = self.prefs.clone();
        let memory_saver = p.browser_memory_saver.to_string();
        vec![
            self.row(
                Some("browser-all-links"),
                true,
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
            ),
            self.row(
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
            ),
            self.row(
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
            ),
            self.row(
                Some("browser-auto-pip"),
                false,
                "Picture in Picture when you leave a video tab",
                Some("A video playing with sound keeps playing in a floating window when you switch to another tab, and comes back when you show its tab again.".into()),
                vec![self.toggle(
                    "browser-auto-pip",
                    "Picture in Picture when you leave a video tab",
                    p.browser_auto_pip,
                    false,
                    cx,
                    |this, on, cx| this.update(cx, |p| p.browser_auto_pip = on),
                )],
            ),
            self.external_cdp_row(&p, cx),
        ]
    }

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
}
