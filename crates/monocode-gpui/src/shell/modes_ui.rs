//! Drawing the modes (docs/native/modes.md): the mode bar above Settings,
//! the rail sliding between modes, Browser mode's sidebar (favourites, the
//! user's tabs, each project's pages) and its page on an inset card, all
//! washed with the Browser colour as far as the window is into the mode.
use super::browse_organize::{IconTarget, PinSpot};
use super::modes::BrowseTab;
use super::session_groups::RenameTarget;
use super::*;
use crate::modes::{AppMode, MAX_MODES, presence};
use crate::modes::{BrowseSection, PinnedRow};
use gpui::{AnimationExt, Hsla, ScrollWheelEvent, img, linear_color_stop, linear_gradient};

/// The mode bar's height, its inset and its buttons' height.
const BAR_HEIGHT: f32 = 26.;
const BAR_INSET: f32 = 2.;
/// Below this, a mode's button shows its icon alone.
const LABEL_MIN: f32 = 72.;
/// The compact rail's mode buttons.
const COMPACT_BUTTON: f32 = 26.;
/// Favourites per row at most, their tiles' height, gap and corners.
const FAVORITE_COLUMNS: usize = 4;
const FAVORITE_HEIGHT: f32 = 44.;
const FAVORITE_GAP: f32 = 8.;
const FAVORITE_RADIUS: f32 = 12.;
/// A tab row's plate, its corners and the gap between rows (Arc's 36 on a
/// 40 pitch).
const ROW_HEIGHT: f32 = 36.;
const ROW_RADIUS: f32 = 10.;
const ROW_GAP: f32 = 3.;
/// A row's favicon.
const ROW_ICON: f32 = 16.;
/// The address field at the sidebar's top.
const URL_HEIGHT: f32 = 36.;
/// The gap around Browser mode's page card.
const CARD_GAP: f32 = 8.;

/// A mode button being dragged to another place on the bar.
#[derive(Clone)]
pub(super) struct DraggedMode(pub AppMode);

impl Render for DraggedMode {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size(px(28.))
            .rounded(px(7.))
            .bg(crate::theme::floating_fill())
            .border_1()
            .border_color(content(0.15))
            .shadow_md()
            .flex()
            .items_center()
            .justify_center()
            .child(icon(self.0.icon(), 15.).text_color(content(0.9)))
    }
}

/// A tab of the user's own being dragged to another place in the sidebar.
#[derive(Clone)]
pub(super) struct DraggedBrowseTab {
    pub pane: i64,
    pub title: SharedString,
}

impl Render for DraggedBrowseTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        drag_chip(self.title.clone())
    }
}

/// What follows the pointer while a sidebar row is dragged: its title.
fn drag_chip(title: SharedString) -> gpui::Div {
    div()
        .max_w(px(220.))
        .h(px(28.))
        .px_2()
        .rounded(px(7.))
        .bg(crate::theme::floating_fill())
        .border_1()
        .border_color(content(0.15))
        .shadow_md()
        .flex()
        .items_center()
        .text_size(px(13.))
        .text_color(content(0.9))
        .child(div().truncate().child(title))
}

/// A project's page being dragged to another place among its project's.
#[derive(Clone)]
pub(super) struct DraggedBrowsePage {
    pub pane: i64,
    pub title: SharedString,
}

impl Render for DraggedBrowsePage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        drag_chip(self.title.clone())
    }
}

/// A project's heading being dragged to another place in the sidebar.
#[derive(Clone)]
pub(super) struct DraggedBrowseProject {
    pub key: String,
    pub name: SharedString,
}

impl Render for DraggedBrowseProject {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        drag_chip(self.name.clone()).font_weight(FontWeight::MEDIUM)
    }
}

/// A folder of pinned tabs being dragged to another place.
#[derive(Clone)]
pub(super) struct DraggedBrowseFolder {
    pub id: u64,
    pub name: SharedString,
}

impl Render for DraggedBrowseFolder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(220.))
            .h(px(28.))
            .px_2()
            .rounded(px(7.))
            .bg(crate::theme::floating_fill())
            .border_1()
            .border_color(content(0.15))
            .shadow_md()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(13.))
            .text_color(content(0.9))
            .child(icon("folder-01", 14.).text_color(content(0.6)))
            .child(div().truncate().child(self.name.clone()))
    }
}

/// A row new to the sidebar fades in this long.
const ROW_IN: std::time::Duration = std::time::Duration::from_millis(180);

/// How far each folder level is indented.
const FOLDER_INDENT: f32 = 16.;

/// A tooltip with a name and, dimmer, its shortcut.
struct ModeTip {
    label: SharedString,
    keys: Option<SharedString>,
}

impl Render for ModeTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(pane_fill())
            .border_1()
            .border_color(content(0.15))
            .text_size(px(12.))
            .text_color(content(1.))
            .flex()
            .items_center()
            .gap_2()
            .child(self.label.clone())
            .children(
                self.keys
                    .clone()
                    .map(|keys| div().text_color(content(0.45)).child(keys)),
            )
    }
}

fn tip(
    label: impl Into<SharedString>,
    keys: Option<String>,
) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView {
    let label = label.into();
    let keys: Option<SharedString> = keys.map(Into::into);
    move |_, cx| {
        let (label, keys) = (label.clone(), keys.clone());
        cx.new(|_| ModeTip { label, keys }).into()
    }
}

/// `alpha` of the way from `from` to `to`.
fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0., 1.);
    let a = gpui::Rgba::from(from);
    let b = gpui::Rgba::from(to);
    gpui::Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
    .into()
}

/// A colour for a site's monogram, the same for the same host.
fn monogram_color(host: &str) -> Hsla {
    let hash = host
        .bytes()
        .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
    gpui::hsla((hash % 360) as f32 / 360., 0.45, 0.48, 1.)
}

/// The host of `url` without `www.`, for monograms and tooltips.
pub(super) fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

impl Shell {
    // ---- The mode bar ----

    /// The bar's buttons over the rail's width, or stacked in the compact
    /// rail. The pill follows the rail as it slides.
    pub(super) fn mode_bar(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let shown = self.workspace.mode_bar.shown.clone();
        let hidden = !self.workspace.mode_bar.hidden().is_empty() && shown.len() < MAX_MODES;
        let position = self.mode_position();
        let browser = self.browser_presence();
        let light = crate::theme::is_light();
        let tint = self.workspace.browse.tint.highlight(light);
        let pill_fill = match tint {
            Some(tint) => mix(content(0.08), tint, browser),
            None => content(0.08),
        };
        let count = shown.len().max(1) as f32;
        let add_room = if hidden { 26. } else { 0. };
        // The rail's row inset: 6 left, 8 right, the border and the bar's own.
        let rail = if self.workspace.classic_layout {
            self.project_rail_drawn_width()
        } else {
            self.workspace.sidebar_width
        };
        let inner = (rail - 1. - 14. - 2. * BAR_INSET - add_room).max(40.);
        let slot = if compact {
            COMPACT_BUTTON
        } else {
            inner / count
        };
        let labels = !compact && slot >= LABEL_MIN;
        let browser_on = self.browser_mode();
        let buttons = shown.iter().copied().enumerate().map(|(ix, mode)| {
            let on = presence(ix, position);
            let shortcut = format!("⌃{}", ix + 1);
            div()
                .id(SharedString::from(format!("mode-{}", mode.id())))
                .debug_selector(move || format!("mode-{}", mode.id()))
                .role(Role::Tab)
                .aria_label(format!("{} mode", mode.label()))
                .aria_selected(self.workspace.mode == mode)
                .relative()
                .cursor_pointer()
                .map(|d| {
                    if compact {
                        d.size(px(COMPACT_BUTTON))
                    } else {
                        d.flex_1().h(px(BAR_HEIGHT - 2. * BAR_INSET))
                    }
                })
                .rounded(px(5.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(5.))
                .text_size(px(11.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(content(0.4 + 0.5 * on))
                .hover(|s| s.text_color(content(0.85)))
                .child(icon(mode.icon(), 13.).text_color(content(0.35 + 0.5 * on)))
                .when(labels, |d| d.child(mode.label()))
                // GPUI's tooltips draw in the main window, over the page,
                // which gives way to its picture and blinks: none in
                // Browser mode (the button's label says it).
                .when(!browser_on, |d| {
                    d.tooltip(tip(format!("{} mode", mode.label()), Some(shortcut)))
                })
                .on_click(cx.listener(move |this, _, window, cx| this.set_mode(mode, window, cx)))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.open_menu(
                            MenuTarget::Mode(mode),
                            Anchor::Point(event.position),
                            window,
                            cx,
                        );
                    }),
                )
                .on_drag(DraggedMode(mode), |dragged, _, _, cx| {
                    cx.new(|_| dragged.clone())
                })
                .drag_over::<DraggedMode>(|style, _, _, _| style.bg(content(0.08)))
                .on_drop(cx.listener(move |this, dragged: &DraggedMode, _, cx| {
                    this.move_mode(dragged.0, ix, cx)
                }))
                .into_any_element()
        });
        let add = hidden.then(|| {
            div()
                .id("mode-show-hidden")
                .debug_selector(|| "mode-show-hidden".into())
                .role(Role::Button)
                .aria_label("Show a hidden mode")
                .cursor_pointer()
                .flex_shrink_0()
                .size(px(if compact { COMPACT_BUTTON } else { 20. }))
                .rounded(px(5.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(content(0.35))
                .hover(|s| s.bg(content(0.08)).text_color(content(0.9)))
                .child(icon("plus", 13.))
                .when(!browser_on, |d| d.tooltip(tip("Show a hidden mode", None)))
                .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                    let mode = this.workspace.mode;
                    let at = event.position();
                    this.open_menu(MenuTarget::Mode(mode), Anchor::Point(at), window, cx);
                }))
        });
        // The pill under the mode in place, gliding with the rail.
        let offset = BAR_INSET + position.clamp(-0.2, count - 0.8) * slot;
        let pill = div()
            .absolute()
            .map(|d| {
                if compact {
                    d.left(px(BAR_INSET))
                        .top(px(offset))
                        .size(px(COMPACT_BUTTON))
                } else {
                    d.top(px(BAR_INSET))
                        .left(px(offset))
                        .w(px(slot))
                        .h(px(BAR_HEIGHT - 2. * BAR_INSET))
                }
            })
            .rounded(px(5.))
            .bg(pill_fill);
        div()
            .id("mode-bar")
            .debug_selector(|| "mode-bar".into())
            .role(Role::TabList)
            .aria_label("Modes")
            .relative()
            .flex_shrink_0()
            .p(px(BAR_INSET))
            .rounded(px(7.))
            .bg(content(0.03))
            .map(|d| {
                if compact {
                    d.flex().flex_col().items_center()
                } else {
                    d.w_full().h(px(BAR_HEIGHT)).flex().items_center()
                }
            })
            .child(pill)
            .children(buttons)
            .children(add)
            .into_any_element()
    }

    // ---- The swipe ----

    /// Catches two-finger swipes over the rail before the lists under it
    /// see them (the capture phase), so a sideways one moves between modes
    /// and a vertical one still scrolls.
    pub(super) fn mode_swipe_catcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = cx.weak_entity();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture || !bounds.contains(&event.position) {
                        return;
                    }
                    let width = bounds.size.width.as_f32();
                    let took = shell
                        .update(cx, |shell, cx| shell.mode_wheel(event, width, window, cx))
                        .unwrap_or(false);
                    if took {
                        cx.stop_propagation();
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// The rail's body for each mode in view: one at rest, two sliding
    /// side by side while it moves between them.
    pub(super) fn mode_slider(
        &mut self,
        width: f32,
        mut body: impl FnMut(&mut Self, AppMode, &mut Context<Self>) -> AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let position = self.mode_position();
        let shown = self.workspace.mode_bar.shown.clone();
        let mut panels = Vec::new();
        for (ix, mode) in shown.into_iter().enumerate() {
            let offset = (ix as f32 - position) * width;
            if offset.abs() >= width - 0.5 {
                continue;
            }
            let content = body(self, mode, cx);
            panels.push(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(offset))
                    .w(px(width))
                    .flex()
                    .flex_col()
                    .child(content),
            );
        }
        div()
            .id("rail-modes")
            .debug_selector(|| "rail-modes".into())
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .children(panels)
            .into_any_element()
    }

    // ---- Browser mode's sidebar ----

    /// The browser's sidebar: a field for a new tab, the favourites, the
    /// user's tabs and each project's pages.
    pub(super) fn browse_sidebar(&mut self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        if compact {
            return self.browse_sidebar_compact(cx);
        }
        let browse = &self.workspace.browse;
        let tabs = |panes: &[i64]| -> Vec<BrowseTab> {
            panes
                .iter()
                .filter_map(|pane| self.browse_tab(*pane, cx))
                .collect()
        };
        let favorites = tabs(&browse.favorite_panes());

        let today = tabs(&browse.today());
        let projects = self.browse_projects();
        self.browse_note_rows(&today, &projects, cx);
        let mut list: Vec<AnyElement> = Vec::new();
        self.browse_pinned_rows(&mut list, cx);
        list.push(self.browse_divider(!today.is_empty(), cx));
        list.push(self.browse_new_tab_row(cx));
        for tab in today {
            let pane = tab.pane;
            let row = self.browse_row(tab, 0, cx);
            list.push(self.browse_row_in(IconTarget::Tab(pane), row));
        }
        for project in projects {
            list.push(self.browse_project_heading(&project, cx));
            if !self.browse_folded(&project.path) {
                for pane in project.panes {
                    if let Some(tab) = self.browse_tab(pane, cx) {
                        let row = self.browse_row(tab, 0, cx);
                        list.push(self.browse_row_in(IconTarget::Tab(pane), row));
                    }
                }
            }
        }
        div()
            .id("browse-sidebar")
            .debug_selector(|| "browse-sidebar".into())
            .role(Role::Navigation)
            .aria_label("Browser tabs")
            .size_full()
            .flex()
            .flex_col()
            .pl(px(6.))
            .pr(px(6.))
            .pt(px(4.))
            .child(self.browse_address_field(cx))
            .child(self.browse_favorites(favorites, cx))
            .child(
                div()
                    .id("browse-tabs")
                    .debug_selector(|| "browse-tabs".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap(px(ROW_GAP))
                    .children(list)
                    // The room under the last row: the sidebar's menu.
                    .child(
                        div()
                            .id("browse-sidebar-room")
                            .flex_1()
                            .min_h(px(40.))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.open_menu(
                                        MenuTarget::BrowseSidebar,
                                        Anchor::Point(event.position),
                                        window,
                                        cx,
                                    );
                                }),
                            )
                            .on_drop(cx.listener(|this, dragged: &DraggedBrowseTab, _, cx| {
                                this.browse_place(dragged.pane, BrowseSection::Today, None, cx)
                            })),
                    ),
            )
            .children(self.browse_media_card(cx))
            .into_any_element()
    }

    /// The fill of the selected row and tile: a light plate over the
    /// sidebar's colour, as Arc's.
    fn browse_selected_fill(&self) -> Hsla {
        if crate::theme::is_light() {
            white(0.82)
        } else {
            mix(white(0.13), self.browse_highlight(), 0.35)
        }
    }

    /// A row's fill under the pointer.
    fn browse_hover_fill() -> Hsla {
        if crate::theme::is_light() {
            black(0.055)
        } else {
            white(0.07)
        }
    }

    /// The address field: the page on screen's site, which a click (or ⌘L)
    /// edits in the page's own field; a new tab's when nothing is open.
    fn browse_address_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.workspace.browse.active;
        let url = (active)
            .and_then(|pane| self.browse_tab(pane, cx))
            .map(|tab| tab.url)
            .unwrap_or_default();
        let host = host_of(&url);
        let light = crate::theme::is_light();
        let has_url = !url.is_empty();
        // The page's toolbar lives here in Browser mode.
        let page = (active)
            .and_then(|pane| self.browser_pages.get(&pane))
            .map(|(page, _)| page.clone());
        let (status, actions, controls_open, field_anchor) = match page {
            Some(page) => page.update(cx, |page, cx| {
                (
                    page.address_status(cx),
                    Some(page.address_actions(cx)),
                    page.site_controls_open(),
                    Some(page.address_field_probe()),
                )
            }),
            None => (None, None, false, None),
        };
        // Opaque enough to hide the host's end under the buttons.
        let strip = if light {
            gpui::hsla(0., 0., 0.97, 1.)
        } else {
            gpui::hsla(0., 0., 0.17, 1.)
        };
        div()
            .id("browse-address")
            .debug_selector(|| "browse-address".into())
            .role(Role::Button)
            .aria_label(if has_url {
                SharedString::from(format!("Address: {host}"))
            } else {
                "Search or enter address".into()
            })
            .group("browse-address")
            .relative()
            .cursor_pointer()
            .h(px(URL_HEIGHT))
            .mb(px(10.))
            .pl(px(12.))
            .pr(px(5.))
            .flex_shrink_0()
            .rounded(px(12.))
            .bg(if light { white(0.5) } else { white(0.07) })
            .hover(|s| s.bg(if light { white(0.7) } else { white(0.1) }))
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(13.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(content(if has_url { 0.72 } else { 0.4 }))
                    .child(if has_url {
                        SharedString::from(host)
                    } else {
                        "Search or enter address".into()
                    }),
            )
            // Where the Site Control Center opens before its button is drawn.
            .children(field_anchor)
            // The agent using the page shows at all times (context.md K47),
            // and "Fill as …", the picker and a download under way.
            .children(status)
            // Under the pointer (and while the Site Control Center is open),
            // over the host's end: Copy URL and Site Controls, as SOCode's;
            // the panel has what the page's toolbar had.
            .when(has_url, |d| {
                d.child(
                    div()
                        .id("browse-address-actions")
                        .debug_selector(|| "browse-address-actions".into())
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(px(4.))
                        .pl(px(6.))
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .rounded(px(10.))
                        .bg(strip)
                        .when(!controls_open, |d| {
                            d.invisible().group_hover("browse-address", |s| s.visible())
                        })
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .id("browse-copy-url")
                                .debug_selector(|| "browse-copy-url".into())
                                .role(Role::Button)
                                .aria_label("Copy URL")
                                .flex_shrink_0()
                                .size(px(24.))
                                .rounded(px(7.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|s| s.bg(content(0.08)))
                                .child(icon("copy", 13.).text_color(content(0.5)))
                                .on_click({
                                    let url = url.clone();
                                    move |_, _, cx| {
                                        cx.stop_propagation();
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            url.clone(),
                                        ));
                                    }
                                }),
                        )
                        .children(actions),
                )
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                let target = match this.workspace.browse.active {
                    Some(pane) => super::command_bar::BarTarget::Current(pane),
                    None => super::command_bar::BarTarget::NewTab,
                };
                this.browse_open_command_bar(target, window, cx);
            }))
    }

    /// How the favourites lie: the most columns up to four, then the fewest
    /// that need no more rows (5 as 3 + 2, 7 as 4 + 3).
    fn favorite_columns(count: usize) -> usize {
        if count == 0 {
            return FAVORITE_COLUMNS;
        }
        let rows = count.div_ceil(FAVORITE_COLUMNS);
        count.div_ceil(rows)
    }

    /// Favourites as a grid of icon tiles. With none, a strip that a tab
    /// dragged over offers to make a favourite.
    fn browse_favorites(&self, favorites: Vec<BrowseTab>, cx: &mut Context<Self>) -> AnyElement {
        if favorites.is_empty() {
            return div()
                .id("browse-favorites-drop")
                .debug_selector(|| "browse-favorites-drop".into())
                .flex_shrink_0()
                .h(px(4.))
                .mb(px(2.))
                .overflow_hidden()
                .rounded(px(FAVORITE_RADIUS))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                // The resting strip is 4px tall, so its label would show as a
                // clipped sliver; it only appears once a tab is dragged over.
                .text_color(gpui::transparent_black())
                .child("Drop to add to Favorites")
                .drag_over::<DraggedBrowseTab>(|style, _, _, _| {
                    style
                        .text_color(content(0.45))
                        .h(px(FAVORITE_HEIGHT))
                        .mb(px(10.))
                        .border_1()
                        .border_color(content(0.2))
                        .bg(content(0.04))
                })
                .on_drop(cx.listener(|this, dragged: &DraggedBrowseTab, _, cx| {
                    this.browse_place(dragged.pane, BrowseSection::Favorites, None, cx)
                }))
                .into_any_element();
        }
        let active = self.workspace.browse.active;
        let selected_fill = self.browse_selected_fill();
        let light = crate::theme::is_light();
        let resting = if light { white(0.42) } else { white(0.06) };
        let columns = Self::favorite_columns(favorites.len());
        let rows: Vec<Vec<BrowseTab>> = favorites
            .chunks(columns)
            .map(<[BrowseTab]>::to_vec)
            .collect();
        div()
            .id("browse-favorites")
            .debug_selector(|| "browse-favorites".into())
            .flex_shrink_0()
            .mb(px(10.))
            .flex()
            .flex_col()
            .gap(px(FAVORITE_GAP))
            .children(rows.into_iter().map(|row| {
                let missing = columns - row.len();
                div()
                    .flex()
                    .gap(px(FAVORITE_GAP))
                    .children(row.into_iter().map(|tab| {
                        let pane = tab.pane;
                        let selected = active == Some(pane);
                        let title: SharedString = tab.title.clone().into();
                        let away = tab.home.is_some();
                        div()
                            .id(("browse-favorite", pane.unsigned_abs() as usize))
                            .debug_selector(move || format!("browse-favorite-{pane}"))
                            .role(Role::Tab)
                            .aria_label(title.clone())
                            .aria_selected(selected)
                            .relative()
                            .cursor_pointer()
                            .flex_1()
                            .min_w_0()
                            .h(px(FAVORITE_HEIGHT))
                            .rounded(px(FAVORITE_RADIUS))
                            .border_1()
                            .border_color(if selected {
                                mix(content(0.1), self.workspace.browse.tint.swatch(), 0.5)
                            } else {
                                content(0.05)
                            })
                            .bg(if selected { selected_fill } else { resting })
                            .when(selected, |d| {
                                d.shadow(vec![
                                    BoxShadow::new(px(0.), px(1.), gpui::rgba(0x0000001f).into())
                                        .blur_radius(px(3.)),
                                ])
                            })
                            .when(!selected, |d| {
                                d.hover(|s| s.bg(if light { white(0.62) } else { white(0.1) }))
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(self.browse_icon(&tab, 20., cx))
                            .children(tab.sound.map(|_| {
                                div()
                                    .absolute()
                                    .top(px(4.))
                                    .right(px(5.))
                                    .size(px(6.))
                                    .rounded_full()
                                    .bg(content(0.6))
                            }))
                            // Away from its address: a dot under the icon.
                            .when(away, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .bottom(px(5.))
                                        .size(px(4.))
                                        .rounded_full()
                                        .bg(content(0.35)),
                                )
                            })
                            .on_click(cx.listener(
                                move |this, event: &gpui::ClickEvent, window, cx| {
                                    // A double click goes back to the favourite's address.
                                    if event.click_count() == 2 {
                                        this.browse_back_home(pane, window, cx);
                                    } else {
                                        this.browse_select(pane, window, cx);
                                    }
                                },
                            ))
                            .on_mouse_up(
                                MouseButton::Middle,
                                cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.browse_close(pane, window, cx);
                                }),
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.open_menu(
                                        MenuTarget::BrowseTab(pane),
                                        Anchor::Point(event.position),
                                        window,
                                        cx,
                                    );
                                }),
                            )
                            .on_drag(DraggedBrowseTab { pane, title }, |dragged, _, _, cx| {
                                cx.new(|_| dragged.clone())
                            })
                            .drag_over::<DraggedBrowseTab>(|style, _, _, _| {
                                style.border_color(content(0.4))
                            })
                            .on_drop(cx.listener(move |this, dragged: &DraggedBrowseTab, _, cx| {
                                cx.stop_propagation();
                                this.browse_place(
                                    dragged.pane,
                                    BrowseSection::Favorites,
                                    Some(pane),
                                    cx,
                                )
                            }))
                            .into_any_element()
                    }))
                    .children((0..missing).map(|_| div().flex_1().into_any_element()))
            }))
            // Dropped past the last tile: at the grid's end.
            .on_drop(cx.listener(|this, dragged: &DraggedBrowseTab, _, cx| {
                this.browse_place(dragged.pane, BrowseSection::Favorites, None, cx)
            }))
            .into_any_element()
    }

    /// The line between the pinned tabs and Today, with Clear at its end
    /// while the pointer is over it. A Today tab dropped on it is pinned;
    /// a pinned one goes back to Today.
    fn browse_divider(&self, has_today: bool, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("browse-divider")
            .debug_selector(|| "browse-divider".into())
            .group("browse-divider")
            .flex_shrink_0()
            .h(px(22.))
            .px(px(10.))
            .relative()
            .flex()
            .items_center()
            // The line runs the whole width, ending before Clear while it
            // shows.
            .child(
                div()
                    .flex_1()
                    .h(px(1.))
                    .bg(content(0.1))
                    .when(has_today, |d| {
                        d.group_hover("browse-divider", |s| s.mr(px(58.)))
                    }),
            )
            .when(has_today, |d| {
                d.child(
                    div()
                        .id("browse-clear-tabs")
                        .debug_selector(|| "browse-clear-tabs".into())
                        .role(Role::Button)
                        .aria_label("Clear: close the tabs under the line")
                        .cursor_pointer()
                        .absolute()
                        .right(px(6.))
                        .invisible()
                        .group_hover("browse-divider", |s| s.visible())
                        .px_1p5()
                        .h(px(18.))
                        .rounded_sm()
                        .flex()
                        .items_center()
                        .gap(px(3.))
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(content(0.45))
                        .hover(|s| s.bg(content(0.08)).text_color(content(0.9)))
                        .child("↓")
                        .child("Clear")
                        .on_click(cx.listener(|this, _, window, cx| this.browse_clear(window, cx))),
                )
            })
            .drag_over::<DraggedBrowseTab>(|style, _, _, _| style.bg(content(0.06)).rounded(px(6.)))
            .drag_over::<DraggedBrowseFolder>(|style, _, _, _| {
                style.bg(content(0.06)).rounded(px(6.))
            })
            .on_drop(cx.listener(|this, dragged: &DraggedBrowseFolder, _, cx| {
                cx.stop_propagation();
                this.browse_move_folder(dragged.id, PinSpot::End, cx)
            }))
            .on_drop(cx.listener(|this, dragged: &DraggedBrowseTab, _, cx| {
                cx.stop_propagation();
                let section = match this.workspace.browse.section(dragged.pane) {
                    Some(BrowseSection::Today) => BrowseSection::Pinned,
                    _ => BrowseSection::Today,
                };
                this.browse_place(dragged.pane, section, None, cx)
            }))
            .into_any_element()
    }

    /// "+ New Tab", the first row under the line: new tabs open under it.
    fn browse_new_tab_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let hover = Self::browse_hover_fill();
        div()
            .id("browse-new-tab")
            .debug_selector(|| "browse-new-tab".into())
            .role(Role::Button)
            .aria_label("New Tab")
            .group("browse-new-tab")
            .cursor_pointer()
            .flex_shrink_0()
            .h(px(ROW_HEIGHT))
            .px(px(10.))
            .rounded(px(ROW_RADIUS))
            .flex()
            .items_center()
            .gap(px(10.))
            .text_size(px(13.5))
            .text_color(content(0.5))
            .hover(move |s| s.bg(hover).text_color(content(0.85)))
            .child(
                div()
                    .size(px(ROW_ICON))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("plus", 15.).text_color(content(0.35))),
            )
            .child(div().flex_1().child("New Tab"))
            .child(
                div()
                    .invisible()
                    .group_hover("browse-new-tab", |s| s.visible())
                    .text_size(px(11.))
                    .text_color(content(0.35))
                    .child(format!("{MOD}T")),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.browse_open_command_bar(super::command_bar::BarTarget::NewTab, window, cx);
            }))
            .drag_over::<DraggedBrowseTab>(|style, _, _, _| style.bg(content(0.06)))
            .on_drop(cx.listener(|this, dragged: &DraggedBrowseTab, _, cx| {
                cx.stop_propagation();
                this.browse_place(dragged.pane, BrowseSection::Today, None, cx)
            }))
            .into_any_element()
    }

    /// A project's heading over its pages, which it folds away.
    fn browse_project_heading(
        &self,
        project: &super::modes::BrowseProject,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = project.path.clone();
        let folded = self.browse_folded(&path);
        let label = self.project_label(&path);
        let count = project.panes.len();
        let key = monocode_core::files::path_key(&path);
        let dragged = DraggedBrowseProject {
            key: key.clone(),
            name: label.clone().into(),
        };
        let target = key.clone();
        let panes = project.panes.clone();
        let hover = Self::browse_hover_fill();
        div()
            .id(SharedString::from(format!("browse-project-{key}")))
            .debug_selector(move || {
                format!("browse-project-{}", monocode_core::files::path_key(&key))
            })
            .role(Role::Button)
            .aria_label(format!("{label}, {count} pages"))
            .aria_expanded(!folded)
            .group("browse-project")
            .cursor_pointer()
            .flex_shrink_0()
            .mt(px(8.))
            .h(px(30.))
            .px(px(10.))
            .rounded(px(ROW_RADIUS))
            .flex()
            .items_center()
            .gap(px(10.))
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(content(0.5))
            .hover(move |s| s.bg(hover).text_color(content(0.85)))
            .child(
                div()
                    .size(px(ROW_ICON))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(self.project_mark(&path, 14.)),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(content(0.3))
                    .child(count.to_string()),
            )
            .child(
                icon(
                    if folded {
                        "chevron-right"
                    } else {
                        "chevron-down"
                    },
                    12.,
                )
                .text_color(content(0.3)),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.browse_toggle_fold(&path, cx)))
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedBrowseProject>(|style, _, _, _| style.bg(content(0.08)))
            .drag_over::<DraggedBrowsePage>(|style, _, _, _| style.bg(content(0.08)))
            .on_drop(
                cx.listener(move |this, dragged: &DraggedBrowseProject, _, cx| {
                    cx.stop_propagation();
                    this.browse_move_project(&dragged.key, &target, cx)
                }),
            )
            // A page dropped on its project's heading goes first.
            .on_drop(
                cx.listener(move |this, dragged: &DraggedBrowsePage, _, cx| {
                    cx.stop_propagation();
                    if panes.contains(&dragged.pane) {
                        this.browse_move_project_page(dragged.pane, None, cx)
                    }
                }),
            )
            .into_any_element()
    }

    /// The Browser colour, faintly: the mode pill's and the selection's hue.
    fn browse_highlight(&self) -> Hsla {
        let light = crate::theme::is_light();
        self.workspace
            .browse
            .tint
            .highlight(light)
            .map_or(content(0.1), |tint| mix(content(0.1), tint, 0.75))
    }

    // No tooltips in Browser mode's sidebar, as SOCode's: GPUI draws them
    // over the page, which then gives way to its picture and blinks.

    /// One tab: its icon, title (and, away from its pinned address, the
    /// site it is on), sound, and a close button under the pointer. A
    /// pinned tab's or favourite's button puts it away (−) instead.
    fn browse_row(&self, tab: BrowseTab, depth: usize, cx: &mut Context<Self>) -> AnyElement {
        if let Some(panes) = self.browse_split_of(tab.pane) {
            return self.browse_split_row(tab.pane, panes, depth, cx);
        }
        let pane = tab.pane;
        let selected = self.workspace.browse.active == Some(pane);
        // Chosen with others by ⌘- or ⇧-click: the secondary fill.
        let chosen = !selected
            && self.workspace.browse.selection.len() > 1
            && self.workspace.browse.selection.contains(&pane);
        let rename = self.browse_rename_field(&RenameTarget::BrowseTab(pane), cx);
        let title: SharedString = tab.title.clone().into();
        let group: SharedString = format!("browse-row-{pane}").into();
        let keeps = matches!(
            tab.section,
            Some(BrowseSection::Pinned | BrowseSection::Favorites)
        );
        let own = tab.section.is_some();
        // Away from its pinned address, on another site: that site.
        let elsewhere = (tab.home.as_deref())
            .map(host_of)
            .filter(|home| *home != host_of(&tab.url))
            .map(|_| host_of(&tab.url));
        let hover = Self::browse_hover_fill();
        let row = div()
            .id(("browse-tab", pane.unsigned_abs() as usize))
            .debug_selector(move || format!("browse-tab-{pane}"))
            .role(Role::Tab)
            .aria_label(title.clone())
            .aria_selected(selected)
            .group(group.clone())
            .relative()
            .flex_shrink_0()
            .cursor_pointer()
            .h(px(ROW_HEIGHT))
            .pl(px(10. + FOLDER_INDENT * depth as f32))
            .pr(px(5.))
            .rounded(px(ROW_RADIUS))
            .flex()
            .items_center()
            .gap(px(10.))
            .when(chosen, |d| d.bg(self.browse_highlight()))
            .when(selected, |d| {
                d.bg(self.browse_selected_fill()).shadow(vec![
                    BoxShadow::new(px(0.), px(1.), gpui::rgba(0x0000001a).into())
                        .blur_radius(px(2.)),
                ])
            })
            .when(!selected, |d| d.hover(move |s| s.bg(hover)))
            .child(self.browse_row_icon(&tab, &group, cx))
            .children(rename)
            .when(
                self.renaming
                    .as_ref()
                    .is_none_or(|r| r.target != RenameTarget::BrowseTab(pane)),
                |d| {
                    d.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.5))
                                    .line_height(px(17.))
                                    .font_weight(if selected {
                                        FontWeight::MEDIUM
                                    } else {
                                        FontWeight::NORMAL
                                    })
                                    .text_color(content(if selected { 1. } else { 0.78 }))
                                    .when(!selected, |d| {
                                        d.group_hover(group.clone(), |s| {
                                            s.text_color(content(0.95))
                                        })
                                    })
                                    .child(title.clone()),
                            )
                            .children(elsewhere.map(|host| {
                                div()
                                    .truncate()
                                    .text_size(px(11.))
                                    .line_height(px(13.))
                                    .text_color(content(0.45))
                                    .child(host)
                            })),
                    )
                },
            )
            .when(tab.agent, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .child(icon("bot", 13.).text_color(content(0.4))),
                )
            })
            .children(
                tab.sound
                    .map(|sound| self.tab_sound_button(pane, sound, cx)),
            )
            // Loading: a dot where the close button comes under the pointer.
            .when(tab.loading, |d| {
                d.child(
                    div()
                        .absolute()
                        .right(px(15.))
                        .size(px(6.))
                        .rounded_full()
                        .bg(content(0.45))
                        .group_hover(group.clone(), |s| s.invisible()),
                )
            })
            .child(
                div()
                    .id(("browse-tab-close", pane.unsigned_abs() as usize))
                    .debug_selector(move || format!("browse-tab-close-{pane}"))
                    .role(Role::Button)
                    .aria_label(if keeps {
                        "Close Tab, keep its place"
                    } else {
                        "Close Tab"
                    })
                    .flex_shrink_0()
                    .size(px(26.))
                    .rounded(px(7.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .invisible()
                    .group_hover(group.clone(), |s| s.visible())
                    .text_color(content(0.5))
                    .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
                    .when(keeps && tab.sleeping, |d| {
                        d.group_hover(group.clone(), |s| s.invisible())
                    })
                    .child(icon(if keeps { "minus-sign" } else { "x" }, 12.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.browse_close(pane, window, cx);
                    })),
            )
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    // A double click names the user's own tab.
                    if event.click_count() == 2 && own {
                        this.browse_rename(RenameTarget::BrowseTab(pane), window, cx);
                    } else {
                        this.browse_click(pane, event.modifiers(), window, cx);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.browse_close(pane, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(
                        MenuTarget::BrowseTab(pane),
                        Anchor::Point(event.position),
                        window,
                        cx,
                    );
                }),
            );
        if !own {
            // A project's page moves among its project's.
            return row
                .on_drag(DraggedBrowsePage { pane, title }, |dragged, _, _, cx| {
                    cx.new(|_| dragged.clone())
                })
                .drag_over::<DraggedBrowsePage>(|style, _, _, _| style.bg(content(0.08)))
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedBrowsePage, _, cx| {
                        cx.stop_propagation();
                        this.browse_move_project_page(dragged.pane, Some(pane), cx)
                    }),
                )
                .into_any_element();
        }
        let section = tab.section.unwrap_or(BrowseSection::Today);
        row.on_drag(DraggedBrowseTab { pane, title }, |dragged, _, _, cx| {
            cx.new(|_| dragged.clone())
        })
        .drag_over::<DraggedBrowseTab>(|style, _, _, _| style.bg(content(0.08)))
        .on_drop(cx.listener(move |this, dragged: &DraggedBrowseTab, _, cx| {
            cx.stop_propagation();
            if section == BrowseSection::Pinned {
                // Before it, in its folder.
                this.browse_pin_at(dragged.pane, PinSpot::Before(pane), cx)
            } else {
                this.browse_place(dragged.pane, section, Some(pane), cx)
            }
        }))
        .when(section == BrowseSection::Pinned, |row| {
            row.drag_over::<DraggedBrowseFolder>(|style, _, _, _| style.bg(content(0.08)))
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedBrowseFolder, _, cx| {
                        cx.stop_propagation();
                        this.browse_move_folder(dragged.id, PinSpot::Before(pane), cx)
                    }),
                )
        })
        .into_any_element()
    }

    /// The pinned tabs' rows: tabs, and folders with their entries under
    /// them while open. A closed folder still shows the tab on screen.
    fn browse_pinned_rows(&self, out: &mut Vec<AnyElement>, cx: &mut Context<Self>) {
        for row in self.workspace.browse.pinned_rows() {
            match row {
                PinnedRow::Tab { pane, depth } => {
                    if let Some(tab) = self.browse_tab(pane, cx) {
                        let row = self.browse_row(tab, depth, cx);
                        out.push(self.browse_row_in(IconTarget::Tab(pane), row));
                    }
                }
                PinnedRow::Folder {
                    id,
                    name,
                    open,
                    depth,
                } => {
                    let row = self.browse_folder_row(id, name, open, depth, cx);
                    out.push(self.browse_row_in(IconTarget::Folder(id), row));
                }
            }
        }
    }

    /// Notes which rows the sidebar shows now: those it did not show in
    /// its last frame (a new tab, a folder's tabs as it opens) fade in.
    fn browse_note_rows(
        &mut self,
        today: &[BrowseTab],
        projects: &[super::modes::BrowseProject],
        cx: &mut Context<Self>,
    ) {
        let mut now: std::collections::HashSet<IconTarget> = (today.iter())
            .map(|tab| IconTarget::Tab(tab.pane))
            .chain(
                projects
                    .iter()
                    .flat_map(|p| p.panes.iter().map(|p| IconTarget::Tab(*p))),
            )
            .collect();
        for row in self.workspace.browse.pinned_rows() {
            now.insert(match row {
                PinnedRow::Tab { pane, .. } => IconTarget::Tab(pane),
                PinnedRow::Folder { id, .. } => IconTarget::Folder(id),
            });
        }
        let live = &mut self.browse_live;
        let at = std::time::Instant::now();
        live.fresh_rows
            .retain(|_, since| since.elapsed() < ROW_IN * 2);
        if !live.rows_seen.is_empty() && !cx.reduce_motion() {
            for key in now.difference(&live.rows_seen) {
                live.fresh_rows.insert(*key, at);
            }
        }
        live.rows_seen = now;
    }

    /// `row`, fading in while it is new to the sidebar.
    fn browse_row_in(&self, key: IconTarget, row: AnyElement) -> AnyElement {
        if !self.browse_live.fresh_rows.contains_key(&key) {
            return row;
        }
        let id = match key {
            IconTarget::Tab(pane) => pane.unsigned_abs() as usize,
            IconTarget::Folder(id) => (id as usize) ^ (1 << 40),
        };
        div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .child(row)
            .with_animation(
                ("browse-row-in", id),
                gpui::Animation::new(ROW_IN).with_easing(gpui::ease_out_quint()),
                |row, t| row.opacity(t),
            )
            .into_any_element()
    }

    /// A folder's row: a press opens or closes it, a double click names it.
    /// Tabs and folders dropped on it go in at its end.
    fn browse_folder_row(
        &self,
        id: u64,
        name: String,
        open: bool,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name: SharedString = name.into();
        let hover = Self::browse_hover_fill();
        let rename = self.browse_rename_field(&RenameTarget::BrowseFolder(id), cx);
        let renaming = rename.is_some();
        div()
            .id(("browse-folder", id as usize))
            .debug_selector(move || format!("browse-folder-{id}"))
            .role(Role::Button)
            .aria_label(name.clone())
            .aria_expanded(open)
            .cursor_pointer()
            .flex_shrink_0()
            .h(px(ROW_HEIGHT))
            .pl(px(10. + FOLDER_INDENT * depth as f32))
            .pr(px(8.))
            .rounded(px(ROW_RADIUS))
            .flex()
            .items_center()
            .gap(px(10.))
            .hover(move |s| s.bg(hover))
            .child(
                div()
                    .relative()
                    .size(px(ROW_ICON))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(super::browse_peek::place_marker(
                        &self.browse_live.icon_anchors,
                        IconTarget::Folder(id),
                    ))
                    .child(match self.browse_custom_icon(IconTarget::Folder(id)) {
                        Some(custom) => {
                            super::browse_organize::custom_icon_element(&custom, ROW_ICON)
                        }
                        None => icon(if open { "folder-open" } else { "folder-01" }, 16.)
                            .text_color(content(0.55))
                            .into_any_element(),
                    }),
            )
            .children(rename)
            .when(!renaming, |d| {
                d.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(13.5))
                        .text_color(content(0.78))
                        .child(name.clone()),
                )
            })
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    if event.click_count() == 2 {
                        this.browse_rename(RenameTarget::BrowseFolder(id), window, cx);
                    } else {
                        this.browse_toggle_folder(id, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(
                        MenuTarget::BrowseFolder(id),
                        Anchor::Point(event.position),
                        window,
                        cx,
                    );
                }),
            )
            .on_drag(DraggedBrowseFolder { id, name }, |dragged, _, _, cx| {
                cx.new(|_| dragged.clone())
            })
            .drag_over::<DraggedBrowseTab>(|style, _, _, _| style.bg(content(0.08)))
            .on_drop(cx.listener(move |this, dragged: &DraggedBrowseTab, _, cx| {
                cx.stop_propagation();
                this.browse_pin_at(dragged.pane, PinSpot::Into(id), cx)
            }))
            .drag_over::<DraggedBrowseFolder>(|style, _, _, _| style.bg(content(0.08)))
            .on_drop(
                cx.listener(move |this, dragged: &DraggedBrowseFolder, _, cx| {
                    cx.stop_propagation();
                    if dragged.id != id {
                        this.browse_move_folder(dragged.id, PinSpot::Into(id), cx)
                    }
                }),
            )
            .into_any_element()
    }

    /// A Split View's row: one segment per pane, its icon and title, as
    /// SOCode's. A press shows the split with its focused pane.
    fn browse_split_row(
        &self,
        anchor: i64,
        panes: Vec<i64>,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.workspace.browse.active;
        let selected = active.is_some_and(|a| panes.contains(&a));
        let browse = &self.workspace.browse;
        // The pane the split shows when picked: its focused one.
        let focus = (browse.tab_of(anchor))
            .and_then(|tab| browse.model.split_for(tab))
            .and_then(|split| browse.pane_of(split.active))
            .unwrap_or(anchor);
        let hover = Self::browse_hover_fill();
        let count = panes.len();
        let segments: Vec<AnyElement> = (panes.iter().enumerate())
            .filter_map(|(ix, pane)| {
                let tab = self.browse_tab(*pane, cx)?;
                Some(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .px(px(7.))
                        .when(ix + 1 < count, |d| {
                            d.border_r_1().border_color(content(0.12))
                        })
                        .child(self.browse_icon(&tab, ROW_ICON, cx))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(13.))
                                .text_color(content(if active == Some(*pane) { 1. } else { 0.75 }))
                                .child(tab.title),
                        )
                        .into_any_element(),
                )
            })
            .collect();
        div()
            .id(("browse-split", anchor.unsigned_abs() as usize))
            .debug_selector(move || format!("browse-split-{anchor}"))
            .role(Role::Tab)
            .aria_label("Split View")
            .aria_selected(selected)
            .flex_shrink_0()
            .cursor_pointer()
            .h(px(ROW_HEIGHT))
            .ml(px(FOLDER_INDENT * depth as f32))
            .px(px(3.))
            .rounded(px(ROW_RADIUS))
            .flex()
            .items_center()
            .when(selected, |d| {
                d.bg(self.browse_selected_fill()).shadow(vec![
                    BoxShadow::new(px(0.), px(1.), gpui::rgba(0x0000001a).into())
                        .blur_radius(px(2.)),
                ])
            })
            .when(!selected, |d| d.hover(move |s| s.bg(hover)))
            .children(segments)
            .on_click(cx.listener(move |this, _, window, cx| this.browse_select(focus, window, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(
                        MenuTarget::BrowseTab(anchor),
                        Anchor::Point(event.position),
                        window,
                        cx,
                    );
                }),
            )
            .into_any_element()
    }

    /// A row's icon. Away from its pinned address, it turns into a back
    /// arrow under the pointer, which goes back there.
    fn browse_row_icon(
        &self,
        tab: &BrowseTab,
        group: &SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pane = tab.pane;
        let favicon = div()
            .relative()
            .size(px(ROW_ICON))
            .flex()
            .items_center()
            .justify_center()
            // Where the icon picker opens for it.
            .child(super::browse_peek::place_marker(
                &self.browse_live.icon_anchors,
                IconTarget::Tab(pane),
            ))
            .child(self.browse_icon(tab, ROW_ICON, cx));
        if tab.home.is_none() {
            return favicon.flex_shrink_0().into_any_element();
        }
        div()
            .id(("browse-tab-home", pane.unsigned_abs() as usize))
            .debug_selector(move || format!("browse-tab-home-{pane}"))
            .role(Role::Button)
            .aria_label("Back to Pinned URL")
            .relative()
            .flex_shrink_0()
            .size(px(ROW_ICON))
            .child(favicon.group_hover(group.clone(), |s| s.invisible()))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .invisible()
                    .group_hover(group.clone(), |s| s.visible())
                    .child(icon("arrow-left-2", 14.).text_color(content(0.75))),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.browse_back_home(pane, window, cx);
            }))
            .into_any_element()
    }

    /// The page's icon, or a monogram of its site until one arrives; dim
    /// while the page sleeps.
    fn browse_icon(&self, tab: &BrowseTab, side: f32, cx: &mut Context<Self>) -> AnyElement {
        let dim = if tab.sleeping { 0.5 } else { 1. };
        // The icon the user gave it (Change Icon…); a split's panes keep
        // their own.
        let in_split = (self.workspace.browse.tab_of(tab.pane))
            .is_some_and(|t| self.workspace.browse.model.split_for(t).is_some());
        if !in_split && let Some(custom) = self.browse_custom_icon(IconTarget::Tab(tab.pane)) {
            return div()
                .flex_shrink_0()
                .opacity(dim)
                .child(super::browse_organize::custom_icon_element(&custom, side))
                .into_any_element();
        }
        if tab.url.is_empty() {
            return icon("globe", side - 2.)
                .text_color(content(0.4))
                .into_any_element();
        }
        if let Some(image) = (tab.favicon.as_deref()).and_then(|url| crate::favicons::icon(url, cx))
        {
            return img(image)
                .size(px(side))
                .flex_shrink_0()
                .rounded(px(3.))
                .opacity(dim)
                .into_any_element();
        }
        let host = host_of(&tab.url);
        let letter = host
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "·".into());
        div()
            .flex_shrink_0()
            .size(px(side))
            .rounded(px(side * 0.28))
            .bg(monogram_color(&host))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(side * 0.6))
            .font_weight(FontWeight::BOLD)
            .text_color(gpui::white())
            .opacity(if tab.loading { 0.6 } else { dim })
            .child(letter)
            .into_any_element()
    }

    /// The compact rail's sidebar: a new tab button, then the favourites,
    /// the pinned tabs, Today and each project's pages as icons.
    fn browse_sidebar_compact(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.workspace.browse.active;
        let selected_fill = self.browse_selected_fill();
        let hover = Self::browse_hover_fill();
        let browse = &self.workspace.browse;
        let tabs = |panes: &[i64]| -> Vec<BrowseTab> {
            panes
                .iter()
                .filter_map(|p| self.browse_tab(*p, cx))
                .collect()
        };
        let mut groups: Vec<Vec<BrowseTab>> = vec![
            tabs(&browse.favorite_panes()),
            tabs(&browse.pinned_tabs()),
            tabs(&browse.today()),
        ];
        for project in self.browse_projects() {
            groups.push(tabs(&project.panes));
        }
        let mut items: Vec<AnyElement> = vec![
            icon_button("browse-new-tab", "New Tab", "plus")
                .size(px(32.))
                .debug_selector(|| "browse-new-tab".into())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.browse_open_command_bar(super::command_bar::BarTarget::NewTab, window, cx);
                }))
                .into_any_element(),
        ];
        for group in groups.into_iter().filter(|g| !g.is_empty()) {
            items.push(
                div()
                    .w(px(20.))
                    .h_px()
                    .my(px(4.))
                    .bg(content(0.1))
                    .into_any_element(),
            );
            for tab in group {
                let pane = tab.pane;
                let selected = active == Some(pane);
                items.push(
                    div()
                        .id(("browse-tab", pane.unsigned_abs() as usize))
                        .debug_selector(move || format!("browse-tab-{pane}"))
                        .role(Role::Tab)
                        .aria_label(tab.title.clone())
                        .aria_selected(selected)
                        .cursor_pointer()
                        .size(px(34.))
                        .rounded(px(9.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(selected, |d| d.bg(selected_fill))
                        .when(!selected, |d| d.hover(move |s| s.bg(hover)))
                        .child(self.browse_icon(&tab, ROW_ICON, cx))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.browse_select(pane, window, cx)
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_menu(
                                    MenuTarget::BrowseTab(pane),
                                    Anchor::Point(event.position),
                                    window,
                                    cx,
                                );
                            }),
                        )
                        .into_any_element(),
                );
            }
        }
        div()
            .id("browse-sidebar")
            .debug_selector(|| "browse-sidebar".into())
            .role(Role::Navigation)
            .aria_label("Browser tabs")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(2.))
            .py_1()
            .children(items)
            .into_any_element()
    }

    // ---- Browser mode's page ----

    /// The Browser colour washed over the window's chrome, `amount` of the
    /// way into the mode.
    pub(super) fn browse_wash(&self, amount: f32) -> Option<AnyElement> {
        self.browse_wash_over(amount, None)
    }

    /// [`Self::browse_wash`] over the leftmost `width` points only.
    pub(super) fn browse_wash_over(&self, amount: f32, width: Option<f32>) -> Option<AnyElement> {
        if amount <= 0.001 {
            return None;
        }
        let (top, bottom) = self
            .workspace
            .browse
            .tint
            .wash(amount, crate::theme::is_light());
        if top.a <= 0. && bottom.a <= 0. {
            return None;
        }
        Some(
            div()
                .debug_selector(|| "browse-wash".into())
                .absolute()
                .top_0()
                .left_0()
                .h_full()
                .map(|d| match width {
                    Some(width) => d.w(px(width)),
                    None => d.w_full(),
                })
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(top, 0.),
                    linear_color_stop(bottom, 1.),
                ))
                .into_any_element(),
        )
    }

    /// The page on screen on an inset card, or a hint to open one.
    pub(super) fn browse_main(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.browse_ensure_active();
        self.sync_browse_peek(window, cx);
        // The panes on screen: the page, or every page of its Split View.
        let (panes, axis) = self.browse_split_panes();
        // A press in a pane gives Chromium the keyboard, and the page takes
        // GPUI's focus with it: that pane becomes the one on screen.
        if panes.len() > 1 {
            let focused = panes.iter().copied().find(|pane| {
                Some(*pane) != self.workspace.browse.active
                    && (self.browser_pages.get(pane))
                        .is_some_and(|(page, _)| page.focus_handle(cx).contains_focused(window, cx))
            });
            if let Some(pane) = focused {
                self.workspace.browse.active = Some(pane);
                if let Some(tab) = self.browse_model_tab(pane) {
                    (self.workspace.browse.model).activate_tab(tab, crate::modes::now());
                }
            }
        }
        let in_sidebar = !self.browse_rail_compact();
        let pages: Vec<(i64, Entity<crate::browser::BrowserPage>)> = (panes.iter())
            .filter_map(|pane| Some((*pane, self.browser_page(*pane, cx)?)))
            .collect();
        // The sidebar's header has the pages' history buttons while it is
        // wide enough to hold them.
        for (pane, page) in &pages {
            // A pinned tab or favourite keeps to its pinned site: links
            // elsewhere open in Peek.
            let peek_host = matches!(
                self.workspace.browse.section(*pane),
                Some(BrowseSection::Pinned | BrowseSection::Favorites)
            )
            .then(|| self.workspace.browse.home(*pane).map(host_of))
            .flatten();
            page.update(cx, |page, cx| {
                page.set_peek_host(peek_host);
                if page.nav_in_sidebar != in_sidebar {
                    page.nav_in_sidebar = in_sidebar;
                    cx.notify();
                }
            });
        }
        let split = pages.len() > 1;
        let active = self.workspace.browse.active;
        let ring = self.workspace.browse.tint.swatch();
        let frames = self.browse_live.pane_frames.clone();
        frames.borrow_mut().clear();
        let card = |id: SharedString, pane: Option<i64>, body: AnyElement| {
            div()
                .id(id.clone())
                .debug_selector(move || id.to_string())
                .relative()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded(px(10.))
                .border_1()
                .border_color(content(0.09))
                // Opaque: the wash stays around the card, not in it.
                .bg(crate::theme::background_base())
                .shadow(vec![
                    BoxShadow::new(px(0.), px(2.), gpui::rgba(0x00000030).into())
                        .blur_radius(px(10.)),
                ])
                // Where a tab held over the page would join it.
                .children(pane.map(|pane| super::browse_peek::place_marker(&frames, pane)))
                .child(body)
                .into_any_element()
        };
        // A Split View's pane sits in a 2-point ring, the Browser colour on
        // the focused one: the same size focused or not. It grows by its
        // weight; the dividers between panes resize them.
        let ringed = |focused: bool, weight: f32, card: AnyElement| {
            let mut pane = div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .flex_col()
                .rounded(px(12.))
                .border_2()
                .border_color(if focused {
                    ring
                } else {
                    gpui::transparent_black()
                })
                .child(card);
            pane.style().flex_grow = Some(weight.max(0.01));
            pane.into_any_element()
        };
        let weights = self.browse_split_weights();
        let vertical = axis == socode_browser::splits::Axis::Vertical;
        let mut cards: Vec<AnyElement> = Vec::new();
        if pages.is_empty() {
            cards.push(card("browse-card".into(), None, self.browse_empty(cx)));
        }
        let count = pages.len();
        for (ix, (pane, page)) in pages.into_iter().enumerate() {
            if !split {
                let body = div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(page)
                    .into_any_element();
                cards.push(card("browse-card".into(), Some(pane), body));
                continue;
            }
            let focused = active == Some(pane);
            let body = div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .flex_col()
                .child(self.browse_pane_header(pane, focused, cx))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(page),
                )
                .into_any_element();
            let weight = weights.get(ix).copied().unwrap_or(1. / count as f32);
            cards.push(ringed(
                focused,
                weight,
                card(format!("browse-split-pane-{pane}").into(), Some(pane), body),
            ));
            if ix + 1 < count {
                cards.push(self.browse_split_divider(ix, !vertical, cx));
            }
        }
        // A narrow rail leaves the traffic lights over the page's corner:
        // the card starts below them, under a title bar of its own.
        let rail_compact = self.browse_rail_compact();
        let top = if rail_compact {
            TITLE_BAR_HEIGHT
        } else {
            CARD_GAP
        };
        self.sync_browse_drop_window(window, cx);
        let drop_preview = self.browse_split_drop_fallback(window, cx);
        // Without a window of its own, Peek stands in for the page: a page
        // under it would cover it (the pinned tab waits, unpainted).
        let cards = match self.browse_peek_card(cx) {
            Some(peek) => vec![
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .px(px(36.))
                    .pt(px(10.))
                    .child(peek)
                    .into_any_element(),
            ],
            None => cards,
        };
        div()
            .id("browse-main")
            .debug_selector(|| "browse-main".into())
            .key_context(super::modes::BROWSER_MODE_CONTEXT)
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .pt(px(top))
            .pr(px(CARD_GAP))
            .pb(px(CARD_GAP))
            // The gap above the card moves the window, as a title bar.
            .child(
                titlebar_drag_area()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(top)),
            )
            .child(
                div()
                    .id("browse-cards")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .when(vertical, |d| d.flex_col())
                    .relative()
                    .children(cards)
                    .on_drag_move::<super::browse_split::DividerDrag>(cx.listener(
                        |this, event: &gpui::DragMoveEvent<_>, _, cx| {
                            this.browse_split_drag_move(event, cx)
                        },
                    ))
                    .on_drop(
                        cx.listener(|this, _: &super::browse_split::DividerDrag, _, cx| {
                            this.persist(cx)
                        }),
                    )
                    // A tab held over the page joins it on the edge nearest
                    // the pointer, as SOCode's.
                    .on_drag_move::<DraggedBrowseTab>(cx.listener(
                        |this, event: &gpui::DragMoveEvent<_>, _, cx| {
                            this.browse_tab_drag_move(event, cx)
                        },
                    ))
                    .on_drop(cx.listener(|this, dragged: &DraggedBrowseTab, window, cx| {
                        if this.browse_live.drop.is_some() {
                            cx.stop_propagation();
                        }
                        this.browse_tab_drop(dragged.pane, window, cx)
                    })),
            )
            .children(drop_preview)
            .into_any_element()
    }

    /// Where Browser mode has no child windows (tests), the drop preview
    /// draws over the window itself; the pages under it give way to their
    /// pictures.
    fn browse_split_drop_fallback(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if super::browse_peek::child_windows() {
            return None;
        }
        let layer = self.browse_split_drop_layer(window, cx)?;
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(px(0.), px(0.)))
                    .child(
                        div()
                            .debug_selector(|| "browse-split-drop".into())
                            .w(window.viewport_size().width)
                            .h(window.viewport_size().height)
                            .child(layer),
                    ),
            )
            .into_any_element(),
        )
    }

    /// The panes Browser mode shows: the page on screen, or its Split
    /// View's pages in their order, with the split's axis.
    pub(super) fn browse_split_panes(&self) -> (Vec<i64>, socode_browser::splits::Axis) {
        let browse = &self.workspace.browse;
        let Some(active) = browse.active else {
            return (Vec::new(), Default::default());
        };
        let split = (browse.tab_of(active)).and_then(|tab| browse.model.split_for(tab));
        match split {
            Some(split) => (
                (split.panes.iter())
                    .filter_map(|tab| browse.pane_of(*tab))
                    .collect(),
                split.axis,
            ),
            None => (vec![active], Default::default()),
        }
    }

    fn browse_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let tint = self.workspace.browse.tint.swatch();
        div()
            .debug_selector(|| "browse-empty".into())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(icon("compass", 40.).text_color(mix(content(0.3), tint, 0.6)))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(content(0.85))
                    .child("No tab open"),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(content(0.45))
                    .child("Browse the web without leaving your agents."),
            )
            .child(
                div()
                    .id("browse-empty-new-tab")
                    .role(Role::Button)
                    .aria_label("New Tab")
                    .cursor_pointer()
                    .mt_2()
                    .h(px(30.))
                    .px_3()
                    .rounded_md()
                    .bg(content(0.08))
                    .hover(|s| s.bg(content(0.14)))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(13.))
                    .text_color(content(0.9))
                    .child(icon("plus", 13.).text_color(content(0.7)))
                    .child("New Tab")
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(content(0.4))
                            .child(format!("{MOD}T")),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.browse_new_tab(None, None, true, window, cx);
                    })),
            )
            .into_any_element()
    }

    /// The rail is the narrow one, of icons.
    pub(super) fn browse_rail_compact(&self) -> bool {
        if self.workspace.classic_layout {
            !self.workspace.project_rail_open
        } else {
            !self.workspace.sidebar_visible
        }
    }

    /// The sidebar's width as drawn.
    pub(super) fn browse_rail_width(&self) -> f32 {
        if self.workspace.classic_layout {
            self.project_rail_drawn_width()
        } else if self.workspace.sidebar_visible {
            self.workspace.sidebar_width
        } else {
            48.
        }
    }

    /// The rail's header in Browser mode: the traffic lights' room, the
    /// sidebar toggle and, at the sidebar's far edge, the page on screen's
    /// Back, Forward and Reload, as Arc's.
    pub(super) fn browse_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let compact = self.browse_rail_compact();
        let page = (self.workspace.browse.active)
            .and_then(|pane| self.browser_pages.get(&pane))
            .map(|(page, _)| page.clone());
        let (back, forward, loading) = page.as_ref().map_or((false, false, false), |page| {
            let page = page.read(cx);
            (page.can_go_back(), page.can_go_forward(), page.loading())
        });
        let has_page = page.is_some();
        let nav = |id: &'static str, label: &'static str, path: &'static str, enabled: bool| {
            icon_button_enabled(id, label, path, enabled)
                .size(px(28.))
                .rounded(px(7.))
                .debug_selector(move || id.into())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        div()
            .absolute()
            .top_0()
            .left_0()
            .h(px(TITLE_BAR_HEIGHT))
            .when(!compact, |d| d.w(px(self.browse_rail_width())).pr(px(8.)))
            .flex()
            .items_center()
            .child(
                titlebar_drag_area()
                    .w(px(self.traffic_lights()))
                    .h_full()
                    .flex_shrink_0(),
            )
            .child(self.sidebar_toggle(cx))
            .when(!compact, |d| {
                d.child(titlebar_drag_area().flex_1().h_full())
                    .child(
                        nav("browse-back", "Back", "arrow-left-2", back).on_click(
                            cx.listener(|this, _, _, cx| this.browse_history_step(true, cx)),
                        ),
                    )
                    .child(
                        nav("browse-forward", "Forward", "arrow-right-2", forward).on_click(
                            cx.listener(|this, _, _, cx| this.browse_history_step(false, cx)),
                        ),
                    )
                    .child(
                        nav(
                            "browse-reload",
                            if loading { "Stop" } else { "Reload" },
                            if loading { "x" } else { "refresh-cw" },
                            has_page,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some((page, _)) = (this.workspace.browse.active)
                                .and_then(|p| this.browser_pages.get(&p))
                            {
                                page.clone().update(cx, |page, _| page.reload_or_stop());
                            }
                        })),
                    )
            })
            .into_any_element()
    }

    /// Browser mode in a layout without the project rail: the browser's
    /// sidebar takes the sessions sidebar's place.
    pub(super) fn browse_standalone_rail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let compact = !self.workspace.sidebar_visible;
        let width = if compact {
            48.
        } else {
            self.workspace.sidebar_width
        };
        let live = self.live_agents_preview(cx);
        div()
            .id("browse-rail")
            .debug_selector(|| "browse-rail".into())
            .relative()
            .w(px(width))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                titlebar_drag_area()
                    .w_full()
                    .h(px(TITLE_BAR_HEIGHT))
                    .flex_shrink_0(),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.browse_sidebar(compact, cx)),
            )
            .children(live)
            .child(
                div()
                    .py_2()
                    .pl(px(6.))
                    .pr_2()
                    .flex_shrink_0()
                    .when(compact, |d| d.flex().justify_center())
                    .child(self.mode_bar(compact, cx)),
            )
            .child(self.mode_swipe_catcher(cx))
            .into_any_element()
    }

    /// Browser mode's whole body: the wash, the sidebar (the project rail
    /// in the original layout) and the page.
    pub(super) fn browse_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wash = self.browse_wash(self.browser_presence().max(0.));
        let rail = if self.workspace.classic_layout {
            self.project_rail_frame(cx)
        } else {
            self.browse_standalone_rail(cx)
        };
        let main = self.browse_main(window, cx);
        div()
            .id("browse-body")
            .debug_selector(|| "browse-body".into())
            .relative()
            .flex_1()
            .min_h_0()
            .size_full()
            .flex()
            .children(wash)
            .child(rail)
            .child(main)
            .child(self.browse_header(cx))
            // In the child window when there is one (`overlay_window.rs`).
            .children(
                (!self.browse_overlay_wanted())
                    .then(|| self.browse_command_bar_layer(cx))
                    .flatten(),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_drop_their_scheme_www_and_credentials() {
        assert_eq!(host_of("https://www.example.com/a?b"), "example.com");
        assert_eq!(host_of("http://user:pw@localhost:3000/"), "localhost:3000");
        assert_eq!(host_of("x.com"), "x.com");
        assert_eq!(monogram_color("x.com"), monogram_color("x.com"));
    }
}
