//! Browser mode's Site Control Center (docs/native/modes.md §Site Control
//! Center), as SOCode's: one panel under the address field's Site Controls
//! button for what the page's toolbar had. Tiles first (Capture, Grab,
//! History, DevTools), then the page's zoom, its downloads and the site's
//! saved passwords, the extensions' actions, the site's Settings (each
//! permission it was allowed or blocked, a press toggles it; Developer
//! Mode), and at the foot the connection (its press opens the site's
//! details and certificate state, K62) and More: Clear Cache and Refresh,
//! Clear Cookies and Refresh, All Site Settings….
//!
//! In Browser mode the panel, and the page's other popovers (the site's
//! details, the downloads, the saved passwords) and Capture, draw in the
//! child window over the live page ([`BrowserPage::page_popovers`],
//! `shell/overlay_window.rs`); elsewhere the page draws them itself.
use super::*;
use crate::popover::Side;
use sites::SiteSecurity;

/// The panel's width, as SOCode's: four 48-point tiles, their gaps and
/// padding.
const WIDTH: f32 = 236.;
const PADDING: f32 = 9.;
const TILE_WIDTH: f32 = 48.;
const TILE_HEIGHT: f32 = 32.;
const TILE_GAP: f32 = 8.;

/// The panel's state in a page.
#[derive(Default)]
pub(crate) struct SiteControls {
    open: bool,
    /// More's list is out.
    more: bool,
    /// The address field's Site Controls button, as last drawn.
    button: AnchorSlot,
    /// The address field, for when its button was not drawn yet (opened from
    /// the Command Bar).
    field: AnchorSlot,
    /// The developer bar's button.
    pub(super) bar_anchor: AnchorSlot,
    /// What the panel opened under.
    at: Option<Bounds<Pixels>>,
    /// The origin it opened for: it closes when the page leaves it.
    origin: Option<String>,
}

/// What the Site Control Center clears before the page loads again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClearSiteData {
    /// The profile's cache (Chromium's cache is not kept per site).
    Cache,
    /// The site's cookies and storage.
    Cookies,
}

impl ClearSiteData {
    /// The CDP call that clears it for `origin`, as SOCode's.
    pub(crate) fn call(self, origin: &str) -> (&'static str, serde_json::Value) {
        match self {
            Self::Cache => ("Network.clearBrowserCache", serde_json::json!({})),
            Self::Cookies => (
                "Storage.clearDataForOrigin",
                serde_json::json!({"origin": origin, "storageTypes": "all"}),
            ),
        }
    }
}

/// What a page asks of the shell from its Site Control Center or Capture.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SiteAsk {
    /// A captured area to save into the project or send to a chat.
    Capture(capture::Captured, capture::CaptureTo),
    /// Settings → Browser: every site's settings, and the extensions.
    BrowserSettings,
}

impl BrowserPage {
    pub(crate) fn site_controls_open(&self) -> bool {
        self.site_controls.open
    }

    /// Goes in the address field: where the panel opens when its button
    /// has not been drawn.
    pub(crate) fn address_field_probe(&self) -> AnyElement {
        self.site_controls.field.probe().into_any_element()
    }

    /// Opens the panel under the address field's button, or closes it.
    pub(crate) fn toggle_site_controls(&mut self, cx: &mut Context<Self>) {
        let at = (self.site_controls.button.get()).or_else(|| self.site_controls.field.get());
        self.toggle_site_controls_at(at, cx);
    }

    pub(super) fn toggle_site_controls_at(
        &mut self,
        at: Option<Bounds<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        if self.site_controls.open {
            self.close_site_controls(cx);
            return;
        }
        self.close_page_popovers(cx);
        self.site_controls.open = true;
        self.site_controls.more = false;
        self.site_controls.at = at;
        self.site_controls.origin = monocode_core::browser::sites::origin_of(&self.url);
        cx.notify();
    }

    pub(crate) fn close_site_controls(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.site_controls.open) {
            self.site_controls.more = false;
            cx.notify();
        }
    }

    /// Where the panel's other lists open: under the panel's button.
    fn controls_anchor(&self) -> Option<Bounds<Pixels>> {
        (self.site_controls.at)
            .or_else(|| self.site_controls.button.get())
            .or_else(|| self.site_controls.field.get())
    }

    /// Clears the site's cookies or the cache, then loads the page again;
    /// false when the page cannot be reached.
    pub(crate) fn clear_site_data(&mut self, kind: ClearSiteData, cx: &mut Context<Self>) -> bool {
        let Some(origin) = monocode_core::browser::sites::origin_of(&self.url) else {
            return false;
        };
        let Some(cdp) = self.cdp() else {
            if kind == ClearSiteData::Cache {
                self.reload(true);
                return true;
            }
            return false;
        };
        self.close_site_controls(cx);
        let (method, params) = kind.call(&origin);
        let url = self.url.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = cdp.call(method, params, None).await {
                log::warn!("browser: {method} failed: {e:?}");
            }
            let _ = this.update(cx, |page, _| {
                // Only the page that was cleared.
                if page.url == url {
                    page.reload(true);
                }
            });
        })
        .detach();
        true
    }

    /// Dispatches `action` in the window the page draws in: what the
    /// child window shows belongs to the main one.
    pub(super) fn dispatch_in_page_window(&self, action: Box<dyn gpui::Action>, cx: &mut App) {
        let Some(main) = self.window else {
            return;
        };
        cx.defer(move |cx| {
            let _ = main.update(cx, |_, window, cx| window.dispatch_action(action, cx));
        });
    }

    /// Runs `f` with the window the page draws in, as what is picked in
    /// Browser mode's child window must for focus.
    fn in_page_window(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(main) = self.window else {
            return;
        };
        let page = cx.entity();
        cx.defer(move |cx| {
            let _ = main.update(cx, |_, window, cx| {
                page.update(cx, |page, cx| f(page, window, cx));
            });
        });
    }

    /// The page's popovers are open: the Site Control Center, the site's
    /// details, the downloads, the saved passwords, or Capture.
    pub(crate) fn page_popover_open(&self) -> bool {
        self.site_controls.open
            || self.site_info_open()
            || (self.downloads_open && !self.downloads.is_empty())
            || self.passwords_open()
            || self.capturing()
    }

    /// Closes them all (a press beside them, Esc, a moved window).
    pub(crate) fn close_page_popovers(&mut self, cx: &mut Context<Self>) {
        let open = self.page_popover_open();
        self.site_controls.open = false;
        self.site_controls.more = false;
        self.close_site_info();
        self.downloads_open = false;
        self.close_passwords();
        self.capture = None;
        if open {
            cx.notify();
        }
    }

    /// The page's popovers, for whichever window draws them.
    pub(crate) fn page_popovers(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if self.site_controls.open
            && self.site_controls.origin != monocode_core::browser::sites::origin_of(&self.url)
        {
            // The page left the site: the panel was for that one.
            let page = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = page.update(cx, |page, cx| page.close_site_controls(cx));
            });
        }
        [
            self.site_popover(cx),
            self.downloads_popover(cx),
            self.passwords_popover(cx),
            self.site_controls_panel(cx),
            self.capture_layer(cx),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The address field's Site Controls button.
    pub(super) fn site_controls_button(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .relative()
            .flex_shrink_0()
            .child(self.site_controls.button.probe())
            .child(
                icon_button("browse-site-controls", "Site Controls", "sliders")
                    .debug_selector(|| "browse-site-controls".into())
                    .size(px(24.))
                    .when(self.site_controls.open, |b| b.bg(content(0.1)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_site_controls(cx);
                    })),
            )
            .into_any_element()
    }

    /// The panel, under its button.
    fn site_controls_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.site_controls.open {
            return None;
        }
        let anchor = AnchorSlot::default();
        anchor.set(self.controls_anchor());
        let weak = cx.weak_entity();
        let tile = |id: &'static str, label: &'static str, path: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .role(gpui::Role::Button)
                .aria_label(label)
                .flex_none()
                .w(px(TILE_WIDTH))
                .h(px(TILE_HEIGHT))
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(content(0.06))
                .border_1()
                .border_color(content(0.08))
                .hover(|s| s.bg(content(0.11)))
                .child(icon(path, 17.).text_color(content(0.75)))
        };
        let heading = |text: &'static str| {
            div()
                .pt(px(12.))
                .pb(px(6.))
                .text_size(px(13.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(content(0.85))
                .child(text)
        };
        let ready = self.host.view.borrow().is_some();
        let tiles = div()
            .flex()
            .gap(px(TILE_GAP))
            .child(
                tile("site-capture", "Capture", "scan")
                    .when(!ready, |t| t.opacity(0.45))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.start_capture(cx);
                    })),
            )
            .child(
                tile("site-grab", "Send an Element to Agent", "grab")
                    .when(self.picking(), |t| t.bg(content(0.14)))
                    .when(!ready, |t| t.opacity(0.45))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close_site_controls(cx);
                        // The picker gives the page the keyboard: in its window.
                        this.in_page_window(cx, |page, window, cx| page.toggle_grab(window, cx));
                    })),
            )
            .child(
                tile("site-history", "History", "clock").on_click(cx.listener(|this, _, _, cx| {
                    this.close_site_controls(cx);
                    this.dispatch_in_page_window(Box::new(history_view::ShowBrowserHistory), cx);
                })),
            )
            .child(
                tile("site-devtools", "DevTools", "code")
                    .when(self.devtools.is_some(), |t| t.bg(content(0.14)))
                    .when(!ready, |t| t.opacity(0.45))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close_site_controls(cx);
                        this.toggle_dev_tools(cx);
                    })),
            );
        // The page's own: zoom, downloads, passwords.
        let mut page_rows: Vec<AnyElement> = vec![self.zoom_row(cx)];
        if !self.downloads.is_empty() {
            let active = (self.downloads.iter())
                .filter(|d| d.state == monocode_cef::DownloadState::InProgress)
                .count();
            let state = if active > 0 {
                format!("{active} in progress")
            } else {
                format!("{}", self.downloads.len())
            };
            page_rows.push(
                setting_row(
                    "site-downloads",
                    "arrow-down-circle",
                    true,
                    "Downloads",
                    state,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    let at = this.controls_anchor();
                    this.close_site_controls(cx);
                    if at.is_some() && !this.downloads_shown_in_field() {
                        this.downloads_anchor.set(at);
                    }
                    this.downloads_open = true;
                    cx.notify();
                }))
                .into_any_element(),
            );
        }
        if let Some((count, offer)) = self.site_passwords(cx) {
            let state = match (count, offer) {
                (_, true) => "Save the new password?".to_owned(),
                (1, _) => "1 saved".to_owned(),
                (n, _) => format!("{n} saved"),
            };
            page_rows.push(
                setting_row("site-passwords", "key", true, "Passwords", state)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let at = this.controls_anchor();
                        this.close_site_controls(cx);
                        this.show_passwords(at, cx);
                    }))
                    .into_any_element(),
            );
        }
        let extensions = extensions::toolbar::buttons(
            &self.host.options.borrow().profile,
            self.host.shared.tab_id.0,
        );
        let web = monocode_core::browser::sites::origin_of(&self.url).is_some();
        // The site's Settings: what it was allowed or blocked, then
        // Developer Mode.
        let mut settings: Vec<AnyElement> = Vec::new();
        for (ix, row) in self.site_permission_rows().into_iter().enumerate() {
            let (name, once) = (row.name.clone(), row.once);
            let reset_name = name.clone();
            let state = match (row.allowed, row.once) {
                (true, false) => "Allowed",
                (false, false) => "Blocked",
                (true, true) => "Allowed on this page",
                (false, true) => "Blocked on this page",
            };
            let group: gpui::SharedString = format!("site-permission-{ix}").into();
            settings.push(
                setting_row(
                    ("site-permission", ix),
                    permission_icon(&row.name),
                    row.allowed,
                    row.title,
                    state.to_owned(),
                )
                .group(group.clone())
                .on_click(
                    cx.listener(move |this, _, _, cx| this.flip_site_permission(&name, once, cx)),
                )
                .child(
                    div()
                        .id(("site-permission-reset", ix))
                        .role(gpui::Role::Button)
                        .aria_label("Reset")
                        .size(px(22.))
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .invisible()
                        .group_hover(group, |s| s.visible())
                        .hover(|s| s.bg(content(0.1)))
                        .child(icon("close", 12.).text_color(content(0.55)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.reset_site_permission(&reset_name, once, cx)
                        })),
                )
                .into_any_element(),
            );
        }
        if web {
            let on = self.developer_mode();
            settings.push(
                setting_row(
                    "site-developer-mode",
                    "hammer",
                    on,
                    "Developer Mode",
                    if on { "On" } else { "Off" }.to_owned(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_developer_mode(cx)))
                .into_any_element(),
            );
        }
        let security = self.site_security();
        let more = self.site_controls.more;
        let dismiss = weak.clone();
        Some(
            popover("browse-site-controls-panel", Anchor::Slot(anchor))
                .side(Side::Bottom)
                .align(Align::Center)
                .gap(10.)
                .width(WIDTH)
                .max_height(640.)
                .padding(PADDING)
                .on_dismiss(move |_, cx| {
                    let _ = dismiss.update(cx, |this, cx| this.close_site_controls(cx));
                })
                .child(
                    div()
                        .id("browse-site-controls")
                        .debug_selector(|| "browse-site-controls-panel".into())
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .text_size(px(13.))
                        .child(tiles)
                        .child(
                            div()
                                .pt(px(8.))
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .children(page_rows),
                        )
                        .when(!extensions.is_empty(), |d| {
                            d.child(heading("Extensions")).child(
                                div().flex().flex_wrap().gap(px(TILE_GAP)).children(
                                    extensions.into_iter().enumerate().map(|(ix, button)| {
                                        div()
                                            .id(("site-extension", ix))
                                            .w(px(TILE_WIDTH))
                                            .h(px(TILE_HEIGHT))
                                            .rounded(px(8.))
                                            .bg(content(0.06))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(button)
                                    }),
                                ),
                            )
                        })
                        .when(!settings.is_empty(), |d| {
                            d.child(heading("Settings"))
                                .child(div().flex().flex_col().gap(px(4.)).children(settings))
                        })
                        .child(
                            div()
                                .mt(px(12.))
                                .pt(px(9.))
                                .border_t_1()
                                .border_color(content(0.1))
                                .flex()
                                .items_center()
                                .justify_between()
                                .children(
                                    security.map(|security| self.security_button(security, cx)),
                                )
                                .when(security.is_none(), |d| d.child(div()))
                                .child(
                                    div()
                                        .id("site-more")
                                        .debug_selector(|| "site-more".into())
                                        .role(gpui::Role::Button)
                                        .aria_label("More")
                                        .size(px(TILE_HEIGHT))
                                        .rounded_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .bg(content(if more { 0.14 } else { 0.06 }))
                                        .hover(|s| s.bg(content(0.12)))
                                        .child(
                                            icon("more-horizontal", 16.).text_color(content(0.75)),
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.site_controls.more = !this.site_controls.more;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .when(more, |d| d.child(self.more_list(web, cx))),
                )
                .into_any_element(),
        )
    }

    /// Whether the address field shows the downloads button itself (while
    /// something downloads), which its list opens under.
    pub(super) fn downloads_shown_in_field(&self) -> bool {
        (self.downloads.iter()).any(|d| d.state == monocode_cef::DownloadState::InProgress)
    }

    /// The zoom row: −, the percentage (a press goes back to 100%), +.
    fn zoom_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let step = |id: &'static str, label: &'static str, path: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .role(gpui::Role::Button)
                .aria_label(label)
                .size(px(26.))
                .rounded(px(7.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(content(0.06))
                .hover(|s| s.bg(content(0.12)))
                .child(icon(path, 13.).text_color(content(0.75)))
        };
        div()
            .id("site-zoom")
            .debug_selector(|| "site-zoom".into())
            .h(px(33.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(tile_icon("zoom-glass", self.zoom != 100))
            .child(
                div()
                    .flex_1()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Zoom"),
            )
            .child(step("site-zoom-out", "Zoom Out", "minus").on_click(
                cx.listener(|this, _, _, cx| this.set_zoom(zoom_step(this.zoom, -1), cx)),
            ))
            .child(
                div()
                    .id("site-zoom-reset")
                    .debug_selector(|| "site-zoom-reset".into())
                    .role(gpui::Role::Button)
                    .aria_label("Zoom to Actual Size")
                    .w(px(44.))
                    .flex()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(content(0.7))
                    .child(format!("{}%", self.zoom))
                    .on_click(cx.listener(|this, _, _, cx| this.set_zoom(100, cx))),
            )
            .child(
                step("site-zoom-in", "Zoom In", "plus").on_click(
                    cx.listener(|this, _, _, cx| this.set_zoom(zoom_step(this.zoom, 1), cx)),
                ),
            )
            .into_any_element()
    }

    /// The foot's connection button: its press opens the site's details.
    fn security_button(&self, security: SiteSecurity, cx: &mut Context<Self>) -> AnyElement {
        let warn = matches!(security, SiteSecurity::NotSecure | SiteSecurity::Untrusted);
        let ink = if warn {
            crate::modal::danger(1.)
        } else {
            content(0.85)
        };
        div()
            .id("site-security")
            .debug_selector(move || {
                if warn {
                    "site-security-insecure".into()
                } else {
                    "site-security-secure".into()
                }
            })
            .role(gpui::Role::Button)
            .aria_label("Connection details")
            .h(px(TILE_HEIGHT))
            .px(px(9.))
            .rounded(px(8.))
            .flex()
            .items_center()
            .gap(px(5.))
            .cursor_pointer()
            .bg(content(0.06))
            .hover(|s| s.bg(content(0.11)))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(ink)
            .child(icon(if warn { "alert-circle" } else { "square-lock" }, 14.).text_color(ink))
            .child(security.label())
            .on_click(cx.listener(|this, _, _, cx| {
                let at = this.controls_anchor();
                this.close_site_controls(cx);
                this.show_site_info(at, cx);
            }))
            .into_any_element()
    }

    /// More's list, under the foot.
    fn more_list(&self, web: bool, cx: &mut Context<Self>) -> AnyElement {
        let row = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .role(gpui::Role::Button)
                .aria_label(label)
                .h(px(26.))
                .px(px(8.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .cursor_pointer()
                .hover(|s| s.bg(content(0.08)))
                .child(label)
        };
        div()
            .mt(px(6.))
            .flex()
            .flex_col()
            .when(web, |d| {
                d.child(
                    row("site-clear-cache", "Clear Cache and Refresh").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.clear_site_data(ClearSiteData::Cache, cx);
                        },
                    )),
                )
                .child(
                    row("site-clear-cookies", "Clear Cookies and Refresh").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.clear_site_data(ClearSiteData::Cookies, cx);
                        },
                    )),
                )
                .child(div().my(px(4.)).mx(px(6.)).h(px(1.)).bg(content(0.08)))
            })
            .child(
                row("site-all-settings", "All Site Settings…").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.close_site_controls(cx);
                        cx.emit(PageRequest::SiteControls(SiteAsk::BrowserSettings));
                    },
                )),
            )
            .into_any_element()
    }
}

/// A Settings row: a tile with the setting's icon, lit when on, its name
/// and its state.
fn setting_row(
    id: impl Into<gpui::ElementId>,
    path: &'static str,
    on: bool,
    title: &'static str,
    state: String,
) -> gpui::Stateful<gpui::Div> {
    let id: gpui::ElementId = id.into();
    let name = id.to_string();
    div()
        .id(id)
        .debug_selector(move || name.clone())
        .h(px(36.))
        .px(px(2.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|s| s.bg(content(0.06)))
        .child(tile_icon(path, on))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .line_height(px(16.))
                        .child(title),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(11.5))
                        .line_height(px(14.))
                        .text_color(content(0.5))
                        .child(state),
                ),
        )
}

/// A row's 32-point round tile: lit in the accent when on.
fn tile_icon(path: &'static str, on: bool) -> gpui::Div {
    let accent = crate::theme::accent_color();
    div()
        .flex_none()
        .size(px(30.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(if on {
            accent.opacity(0.9)
        } else {
            content(0.1)
        })
        .child(icon(path, 15.).text_color(if on { gpui::white() } else { content(0.6) }))
}

/// A permission's icon in the Settings list.
fn permission_icon(name: &str) -> &'static str {
    match name {
        "camera" => "camera",
        "microphone" => "mic",
        "geolocation" => "map-pin",
        "notifications" => "bell",
        "clipboard_read" => "copy",
        _ => "settings",
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// A page's DevTools agent that answers every call with `answer`, and
    /// the calls it got.
    pub(in crate::browser) fn fake_cdp(
        answer: impl Fn(&Value) -> Value + 'static,
    ) -> (Rc<CdpClient>, Rc<RefCell<Vec<Value>>>) {
        let calls: Rc<RefCell<Vec<Value>>> = Rc::default();
        let client: Rc<RefCell<Weak<CdpClient>>> = Rc::default();
        let (seen, peer) = (calls.clone(), client.clone());
        let cdp = CdpClient::new(Box::new(move |bytes| {
            let message: Value = serde_json::from_slice(bytes).unwrap();
            let reply = json!({"id": message["id"], "result": answer(&message)});
            seen.borrow_mut().push(message);
            if let Some(cdp) = peer.borrow().upgrade() {
                cdp.on_message(Some(reply.to_string().as_bytes()));
            }
            true
        }));
        *client.borrow_mut() = Rc::downgrade(&cdp);
        (cdp, calls)
    }

    fn browse_page<'a>(
        root: &Path,
        cx: &'a mut gpui::TestAppContext,
    ) -> (Entity<BrowserPage>, &'a mut gpui::VisualTestContext) {
        cx.update(super::super::bind);
        let root = root.to_path_buf();
        let (page, cx) = cx.add_window_view(move |_, cx| {
            let mut page =
                BrowserPage::new("https://a.test/x".into(), String::new(), 100, root, cx);
            page.sites_follow_address("https://a.test/x");
            // In Browser mode's sidebar: no toolbar.
            page.nav_in_sidebar = true;
            page
        });
        (page, cx)
    }

    #[gpui::test]
    fn the_settings_list_what_the_site_was_allowed_and_a_press_flips_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let (page, cx) = browse_page(root.path(), cx);
        page.update(cx, |page, cx| {
            page.host.shown.set(Placement::Shown);
            let ask = monocode_cef::PermissionRequest::detached(
                1,
                "https://a.test/",
                vec!["geolocation"],
            );
            page.on_permission_request(ask, cx);
            page.answer_permission(true, cx);
            let rows = page.site_permission_rows();
            assert_eq!(rows.len(), 1);
            assert_eq!(
                (rows[0].title, rows[0].allowed, rows[0].once),
                ("Location", true, false)
            );
            page.flip_site_permission("geolocation", false, cx);
            assert!(!page.site_permission_rows()[0].allowed);
        });
        let saved = monocode_core::browser::sites::SitePermissions::load(
            &root.path().join("Profiles/default"),
        );
        assert_eq!(
            saved.get("https://a.test", "geolocation"),
            Some(monocode_core::browser::sites::Setting::Block)
        );
        page.update(cx, |page, cx| {
            page.reset_site_permission("geolocation", false, cx);
            assert!(page.site_permission_rows().is_empty());
        });
    }

    #[gpui::test]
    fn the_panel_opens_its_lists_and_more_clears_the_sites_cookies(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (page, cx) = browse_page(root.path(), cx);
        let (cdp, calls) = fake_cdp(|_| json!({}));
        page.update(cx, |page, cx| {
            page.set_cdp_for_test(cdp);
            page.toggle_site_controls(cx);
            assert!(page.site_controls_open() && page.page_popover_open());
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("browse-site-controls-panel").is_some());
        // The connection opens the site's details instead.
        let security = cx.debug_bounds("site-security-secure").expect("secure");
        cx.simulate_click(security.center(), Default::default());
        cx.run_until_parked();
        page.read_with(cx, |page, _| {
            assert!(!page.site_controls_open());
            assert!(page.site_info_open());
        });
        page.update(cx, |page, cx| {
            page.close_page_popovers(cx);
            assert!(!page.page_popover_open());
            page.toggle_site_controls(cx);
        });
        cx.run_until_parked();
        let more = cx.debug_bounds("site-more").expect("More");
        cx.simulate_click(more.center(), Default::default());
        cx.run_until_parked();
        let clear = cx
            .debug_bounds("site-clear-cookies")
            .expect("Clear Cookies");
        cx.simulate_click(clear.center(), Default::default());
        cx.run_until_parked();
        let calls = calls.borrow();
        let call = (calls.iter())
            .find(|c| c["method"] == "Storage.clearDataForOrigin")
            .expect("cleared");
        assert_eq!(call["params"]["origin"], "https://a.test");
        assert!(!page.read_with(cx, |page, _| page.site_controls_open()));
    }

    #[gpui::test]
    fn developer_mode_is_per_site_and_shows_the_developer_bar(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (page, cx) = browse_page(root.path(), cx);
        page.update(cx, |page, cx| page.toggle_site_controls(cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("browser-developer-bar").is_none());
        let row = cx.debug_bounds("site-developer-mode").expect("the row");
        cx.simulate_click(row.center(), Default::default());
        cx.run_until_parked();
        assert!(page.read_with(cx, |page, _| page.developer_mode()));
        assert!(cx.debug_bounds("browser-developer-bar").is_some());
        let saved = std::fs::read_to_string(
            root.path()
                .join("Profiles/default/monocode-developer-sites.json"),
        )
        .unwrap();
        assert!(saved.contains("a.test"), "{saved}");
    }

    #[test]
    fn clearing_calls_what_socode_calls() {
        let (method, params) = ClearSiteData::Cookies.call("https://example.com");
        assert_eq!(method, "Storage.clearDataForOrigin");
        assert_eq!(params["origin"], "https://example.com");
        assert_eq!(params["storageTypes"], "all");
        assert_eq!(
            ClearSiteData::Cache.call("https://example.com").0,
            "Network.clearBrowserCache"
        );
    }
}
