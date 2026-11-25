//! Browser mode's Split View (docs/native/modes.md §Split View): up to four
//! pages side by side or stacked, one sidebar row, as SOCode's. The model
//! (`socode_browser::splits`) keeps them; these are the shell's verbs.
use super::browse_peek::{ChildWindow, child_windows, close_child_window, open_child_window};
use super::*;
use gpui::{Bounds, Div, DragMoveEvent, Point, Stateful, WeakEntity};
pub(super) use socode_browser::splits::{Axis, Placement};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The gap between a split's panes, which is also its divider.
pub(super) const SPLIT_GAP: f32 = 10.;
/// A pane is never dragged narrower (or lower) than this, as SOCode's.
pub(super) const MIN_PANE: f32 = 220.;
/// A divider dragged this close to the even place snaps to it.
const SNAP: f32 = 12.;
/// A pane's header above its page (SOCode's 32 with the 2-point ring).
pub(super) const PANE_HEADER: f32 = 30.;
/// The drop preview eases between targets this long.
const DROP_MOTION: Duration = Duration::from_millis(180);

/// A divider of a Split View being dragged.
#[derive(Clone)]
pub(super) struct DividerDrag {
    anchor: socode_browser::TabId,
    horizontal: bool,
    index: usize,
    weights: Vec<f32>,
    /// Where in the divider the pointer took it.
    offset: Rc<Cell<f32>>,
}

impl Render for DividerDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_0()
    }
}

/// The weights after the divider after pane `index` is dragged so that its
/// leading edge is `at` points into a container `extent` long, with `gap`
/// between panes. The two panes beside it share their room; neither gets
/// below [`MIN_PANE`]; near the even place they snap to it.
pub(super) fn dragged_weights(
    weights: &[f32],
    index: usize,
    extent: f32,
    gap: f32,
    at: f32,
) -> Option<Vec<f32>> {
    let count = weights.len();
    if index + 1 >= count {
        return None;
    }
    let total: f32 = weights.iter().sum();
    let available = extent - gap * (count - 1) as f32;
    if available <= 0. || total <= 0. {
        return None;
    }
    let share: Vec<f32> = weights.iter().map(|w| w / total).collect();
    let before: f32 = share[..index].iter().sum();
    let pair = available * (share[index] + share[index + 1]);
    let minimum = MIN_PANE.min(pair / 2.);
    let mut first = (at - before * available - gap * index as f32).clamp(minimum, pair - minimum);
    if (first - pair / 2.).abs() < SNAP {
        first = pair / 2.;
    }
    let mut out = share;
    out[index] = first / available;
    out[index + 1] = (pair - first) / available;
    Some(out)
}

/// A tab held over the page: the pane it is over and the side it would
/// join on (`None` in the middle, where it does nothing), as SOCode's.
#[derive(Clone, Debug)]
pub(super) struct SplitDrop {
    pub(super) source: i64,
    pub(super) anchor: i64,
    pub(super) placement: Option<Placement>,
    /// The pane's frame and the pointer, in window coordinates.
    pub(super) frame: Bounds<Pixels>,
    pub(super) pointer: Point<Pixels>,
    pub(super) title: SharedString,
    pub(super) favicon: Option<String>,
    /// The cover and outline painted when this target took over, and when.
    from_cover: Bounds<Pixels>,
    from_outline: Bounds<Pixels>,
    since: Instant,
}

/// The side of `frame` that takes a tab at `pointer`: the outer quarters
/// left and right, and, without a side-by-side split, top and bottom (a
/// stacked split takes only those).
pub(super) fn drop_placement(
    frame: Bounds<Pixels>,
    pointer: Point<Pixels>,
    split: Option<Axis>,
) -> Option<Placement> {
    let w = f32::from(frame.size.width).max(1.);
    let h = f32::from(frame.size.height).max(1.);
    let x = f32::from(pointer.x - frame.origin.x) / w;
    let y = f32::from(pointer.y - frame.origin.y) / h;
    match split {
        Some(Axis::Vertical) if y < 0.25 => Some(Placement::Above),
        Some(Axis::Vertical) if y > 0.75 => Some(Placement::Below),
        Some(Axis::Vertical) => None,
        _ if x < 0.25 => Some(Placement::Left),
        _ if x > 0.75 => Some(Placement::Right),
        None if y < 0.25 => Some(Placement::Above),
        None if y > 0.75 => Some(Placement::Below),
        _ => None,
    }
}

/// The half of `frame` the new pane would take (the cover, in the
/// Browser colour) and the outline drawn in it; without a side, the whole
/// pane's outline.
pub(super) fn drop_geometry(
    frame: Bounds<Pixels>,
    placement: Option<Placement>,
) -> (Bounds<Pixels>, Bounds<Pixels>) {
    let Some(placement) = placement else {
        return (frame, frame);
    };
    let horizontal = matches!(placement, Placement::Left | Placement::Right);
    let before = matches!(placement, Placement::Left | Placement::Above);
    let length = if horizontal {
        frame.size.width
    } else {
        frame.size.height
    };
    let half = length / 2.;
    let gap = px(SPLIT_GAP / 2.);
    let along = |start: Pixels, len: Pixels| {
        if horizontal {
            Bounds::new(
                gpui::point(frame.origin.x + start, frame.origin.y),
                gpui::size(len, frame.size.height),
            )
        } else {
            Bounds::new(
                gpui::point(frame.origin.x, frame.origin.y + start),
                gpui::size(frame.size.width, len),
            )
        }
    };
    if before {
        (along(px(0.), half + gap), along(px(0.), half - gap))
    } else {
        (along(half - gap, half + gap), along(half + gap, half - gap))
    }
}

/// A zero-thickness cover on the edge a new pane slides in from.
fn collapsed(cover: Bounds<Pixels>, placement: Option<Placement>) -> Bounds<Pixels> {
    let (o, s) = (cover.origin, cover.size);
    match placement {
        Some(Placement::Left) => Bounds::new(o, gpui::size(px(0.), s.height)),
        Some(Placement::Right) => Bounds::new(
            gpui::point(o.x + s.width, o.y),
            gpui::size(px(0.), s.height),
        ),
        Some(Placement::Above) => Bounds::new(o, gpui::size(s.width, px(0.))),
        Some(Placement::Below) => Bounds::new(
            gpui::point(o.x, o.y + s.height),
            gpui::size(s.width, px(0.)),
        ),
        None => cover,
    }
}

fn lerp(a: Bounds<Pixels>, b: Bounds<Pixels>, t: f32) -> Bounds<Pixels> {
    let mix = |x: Pixels, y: Pixels| x + (y - x) * t;
    Bounds::new(
        gpui::point(mix(a.origin.x, b.origin.x), mix(a.origin.y, b.origin.y)),
        gpui::size(
            mix(a.size.width, b.size.width),
            mix(a.size.height, b.size.height),
        ),
    )
}

impl SplitDrop {
    fn progress(&self, reduce: bool) -> f32 {
        if reduce {
            return 1.;
        }
        let t = (self.since.elapsed().as_secs_f32() / DROP_MOTION.as_secs_f32()).clamp(0., 1.);
        1. - (1. - t).powi(3)
    }

    /// The cover and outline painted now, mid-motion included.
    fn painted(&self, reduce: bool) -> (Bounds<Pixels>, Bounds<Pixels>) {
        let (cover, outline) = drop_geometry(self.frame, self.placement);
        let t = self.progress(reduce);
        (
            lerp(self.from_cover, cover, t),
            lerp(self.from_outline, outline, t),
        )
    }
}

/// The child window drawing the drop preview over live pages; presses go
/// through it.
pub(crate) struct DropRoot {
    shell: WeakEntity<Shell>,
    _observe: Subscription,
}

impl Render for DropRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let drawn = (self.shell.upgrade()).and_then(|shell| {
            shell.update(cx, |shell, cx| shell.browse_split_drop_layer(window, cx))
        });
        div()
            .size_full()
            .relative()
            .font_family(crate::fonts::UI)
            .children(drawn)
    }
}

actions!(
    workspace,
    [
        /// ⌃⇧= in Browser mode: a page joins the one on screen, picked in
        /// the Command Bar.
        BrowseAddSplit,
        /// ⌃⇧- in Browser mode: the focused pane of a Split View closes.
        BrowseCloseSplitPane
    ]
);

pub(super) fn bind(cx: &mut App) {
    let browse = Some(super::modes::BROWSER_MODE_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-=", BrowseAddSplit, browse),
        KeyBinding::new("ctrl-shift--", BrowseCloseSplitPane, browse),
    ]);
}

impl Shell {
    /// The page of `other` joins the page of `anchor` in a Split View, on
    /// its right; it becomes the page on screen.
    pub(super) fn browse_add_split(
        &mut self,
        anchor: i64,
        other: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browse_add_split_at(anchor, other, Placement::Right, window, cx);
    }

    /// The page of `other` joins the page of `anchor` at `placement` (a tab
    /// dropped on the page's edge).
    pub(super) fn browse_add_split_at(
        &mut self,
        anchor: i64,
        other: i64,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(a), Some(b)) = (self.browse_model_tab(anchor), self.browse_model_tab(other))
        else {
            return;
        };
        let added = self.browse_edit(cx, |model, now| model.add_split(a, b, placement, now));
        if added.is_ok() {
            self.browse_select(other, window, cx);
        }
    }

    /// `url` opens in a new tab that joins the page of `anchor`.
    pub(super) fn browse_add_split_url(
        &mut self,
        anchor: i64,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let other = self.browse_new_tab(Some(url), None, false, window, cx);
        self.browse_add_split(anchor, other, window, cx);
    }

    /// The pane of `pane` leaves its Split View, as a tab of its own.
    pub(super) fn browse_separate_pane(&mut self, pane: i64, cx: &mut Context<Self>) {
        if let Some(tab) = self.browse_model_tab(pane) {
            self.browse_edit(cx, |model, _| model.separate_split_pane(tab));
        }
    }

    /// Every pane of the Split View of `pane` becomes a tab of its own.
    pub(super) fn browse_separate_all(&mut self, pane: i64, cx: &mut Context<Self>) {
        if let Some(tab) = self.browse_model_tab(pane) {
            self.browse_edit(cx, |model, _| model.separate_split(tab));
        }
    }

    /// The Split View of `pane` turns from side by side to stacked, or back.
    pub(super) fn browse_flip_split(&mut self, pane: i64, cx: &mut Context<Self>) {
        let Some(tab) = self.browse_model_tab(pane) else {
            return;
        };
        let axis = match self.workspace.browse.model.split_for(tab).map(|s| s.axis) {
            Some(Axis::Horizontal) => Axis::Vertical,
            Some(Axis::Vertical) => Axis::Horizontal,
            None => return,
        };
        self.browse_edit(cx, |model, _| model.set_split_axis(tab, axis));
    }

    /// The pane of `pane` closes; the rest of its Split View stays.
    pub(super) fn browse_close_split_pane(
        &mut self,
        pane: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.browse_model_tab(pane) else {
            return;
        };
        self.browse_edit(cx, |model, now| model.close_split_pane(tab, now));
        if self.browser_mode() {
            self.focus_browse_page(window, cx);
        }
    }

    /// The panes of the Split View `pane` is in, more than one.
    pub(super) fn browse_split_of(&self, pane: i64) -> Option<Vec<i64>> {
        let browse = &self.workspace.browse;
        let split = browse.model.split_for(browse.tab_of(pane)?)?;
        let panes: Vec<i64> = (split.panes.iter())
            .filter_map(|tab| browse.pane_of(*tab))
            .collect();
        (panes.len() > 1).then_some(panes)
    }

    /// The weights of the Split View on screen, in its panes' order.
    pub(super) fn browse_split_weights(&self) -> Vec<f32> {
        let browse = &self.workspace.browse;
        (browse.active)
            .and_then(|pane| browse.tab_of(pane))
            .and_then(|tab| browse.model.split_for(tab))
            .map(|split| split.weights.clone())
            .unwrap_or_default()
    }

    /// The divider after pane `index` of the split on screen: the gap
    /// between two panes. Dragged, it resizes them; a double click shares
    /// the room evenly again. Its thumb shows under the pointer.
    pub(super) fn browse_split_divider(
        &self,
        index: usize,
        horizontal: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let browse = &self.workspace.browse;
        let split = (browse.active)
            .and_then(|pane| browse.tab_of(pane))
            .and_then(|tab| browse.model.split_for(tab));
        let Some(split) = split else {
            return div().into_any_element();
        };
        let drag = DividerDrag {
            anchor: split.anchor,
            horizontal,
            index,
            weights: split.weights.clone(),
            offset: Rc::default(),
        };
        let group: SharedString = format!("browse-split-divider-{index}").into();
        div()
            .id(("browse-split-divider", index))
            .debug_selector(move || format!("browse-split-divider-{index}"))
            .group(group.clone())
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .map(|d| {
                if horizontal {
                    d.w(px(SPLIT_GAP))
                        .h_full()
                        .cursor(gpui::CursorStyle::ResizeLeftRight)
                } else {
                    d.h(px(SPLIT_GAP))
                        .w_full()
                        .cursor(gpui::CursorStyle::ResizeUpDown)
                }
            })
            .child(
                div()
                    .rounded_full()
                    .bg(content(0.45))
                    .invisible()
                    .group_hover(group, |s| s.visible())
                    .map(|d| {
                        if horizontal {
                            d.w(px(2.)).h(px(24.))
                        } else {
                            d.w(px(64.)).h(px(2.))
                        }
                    }),
            )
            .on_drag(drag, move |drag, offset, _, cx| {
                drag.offset
                    .set(f32::from(if horizontal { offset.x } else { offset.y }));
                cx.new(|_| drag.clone())
            })
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.browse_even_split(cx);
                }
            }))
            .into_any_element()
    }

    /// A divider moves: the panes beside it follow the pointer, over the
    /// cards' container (`event.bounds`).
    pub(super) fn browse_split_drag_move(
        &mut self,
        event: &DragMoveEvent<DividerDrag>,
        cx: &mut Context<Self>,
    ) {
        let drag = event.drag(cx).clone();
        let (extent, at) = if drag.horizontal {
            (
                event.bounds.size.width,
                event.event.position.x - event.bounds.origin.x,
            )
        } else {
            (
                event.bounds.size.height,
                event.event.position.y - event.bounds.origin.y,
            )
        };
        let at = f32::from(at) - drag.offset.get();
        let model = &mut self.workspace.browse.model;
        // The split may have changed under the drag.
        let current = model.split_for(drag.anchor).map(|s| s.weights.len());
        if current != Some(drag.weights.len()) {
            return;
        }
        if let Some(weights) =
            dragged_weights(&drag.weights, drag.index, f32::from(extent), SPLIT_GAP, at)
            && model.set_split_weights(drag.anchor, &weights)
        {
            cx.notify();
        }
    }

    /// The panes of the split on screen share its room evenly.
    pub(super) fn browse_even_split(&mut self, cx: &mut Context<Self>) {
        let browse = &mut self.workspace.browse;
        let Some(tab) = (browse.active).and_then(|pane| browse.tab_of(pane)) else {
            return;
        };
        let Some(count) = browse.model.split_for(tab).map(|s| s.panes.len()) else {
            return;
        };
        let even = vec![1. / count as f32; count];
        if browse.model.set_split_weights(tab, &even) {
            self.persist(cx);
            cx.notify();
        }
    }

    /// A Split View pane's header, as SOCode's: Back, Forward and (focused
    /// or under the pointer) Reload; the site in the middle, which opens
    /// the Command Bar on the pane; the pane's menu and its close button.
    /// A press focuses the pane.
    pub(super) fn browse_pane_header(
        &self,
        pane: i64,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let page = self.browser_pages.get(&pane).map(|(page, _)| page.clone());
        let (back, forward, loading) = page.as_ref().map_or((false, false, false), |page| {
            let page = page.read(cx);
            (page.can_go_back(), page.can_go_forward(), page.loading())
        });
        let host = self
            .browse_tab(pane, cx)
            .map(|tab| super::modes_ui::host_of(&tab.url))
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| "New Tab".into());
        let light = crate::theme::is_light();
        let fill = if light {
            gpui::rgb(0xf5f5f5).into()
        } else {
            white(if focused { 0.1 } else { 0.06 })
        };
        let ink = content(0.7);
        let group: SharedString = format!("browse-pane-header-{pane}").into();
        let tool = |id: &'static str, label: &'static str, path: &'static str, enabled: bool| {
            div()
                .id(SharedString::from(format!("{id}-{pane}")))
                .debug_selector(move || format!("{id}-{pane}"))
                .role(Role::Button)
                .aria_label(label)
                .flex_shrink_0()
                .w(px(28.))
                .h(px(24.))
                .rounded(px(5.))
                .flex()
                .items_center()
                .justify_center()
                .when(enabled, |d| {
                    d.cursor_pointer().hover(|s| s.bg(content(0.1)))
                })
                .text_color(if enabled { ink } else { content(0.3) })
                .child(icon(path, 14.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        let shown_on_hover = |d: Stateful<Div>| {
            if focused {
                d
            } else {
                d.invisible().group_hover(group.clone(), |s| s.visible())
            }
        };
        let inset = if focused { 111. } else { 56. };
        div()
            .id(("browse-pane-header", pane.unsigned_abs() as usize))
            .debug_selector(move || format!("browse-pane-header-{pane}"))
            .group(group.clone())
            .relative()
            .flex_shrink_0()
            .h(px(PANE_HEADER))
            .px(px(6.))
            .flex()
            .items_center()
            .gap(px(2.))
            .bg(fill)
            .border_b_1()
            .border_color(content(0.06))
            .text_color(ink)
            .child(
                tool("browse-pane-back", "Back", "arrow-left-2", back).on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.stop_propagation();
                        if let Some((page, _)) = this.browser_pages.get(&pane) {
                            page.clone().update(cx, |page, _| page.go_back());
                        }
                    },
                )),
            )
            .child(
                tool("browse-pane-forward", "Forward", "arrow-right-2", forward).on_click(
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if let Some((page, _)) = this.browser_pages.get(&pane) {
                            page.clone().update(cx, |page, _| page.go_forward());
                        }
                    }),
                ),
            )
            .child(shown_on_hover(
                tool(
                    "browse-pane-reload",
                    if loading { "Stop" } else { "Reload" },
                    if loading { "x" } else { "refresh-cw" },
                    page.is_some(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if let Some((page, _)) = this.browser_pages.get(&pane) {
                        page.clone().update(cx, |page, _| page.reload_or_stop());
                    }
                })),
            ))
            .child(
                div()
                    .id(("browse-pane-address", pane.unsigned_abs() as usize))
                    .debug_selector(move || format!("browse-pane-address-{pane}"))
                    .absolute()
                    .top_0()
                    .h_full()
                    .left(px(inset))
                    .right(px(inset))
                    .when(!focused, |d| {
                        d.group_hover(group.clone(), |s| s.left(px(94.)).right(px(94.)))
                    })
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(icon("globe", 12.).text_color(content(0.5)))
                    .child(div().min_w_0().truncate().child(host))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.browse_select(pane, window, cx);
                        this.browse_open_command_bar(
                            super::command_bar::BarTarget::Current(pane),
                            window,
                            cx,
                        );
                    })),
            )
            .child(div().flex_1())
            .child(shown_on_hover(
                tool(
                    "browse-pane-menu",
                    "Split View Options",
                    "panel-right",
                    true,
                )
                .on_click(cx.listener(
                    move |this, event: &gpui::ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.open_menu(
                            MenuTarget::BrowseTab(pane),
                            Anchor::Point(event.position()),
                            window,
                            cx,
                        );
                    },
                )),
            ))
            .child(
                tool("browse-pane-close", "Close Pane", "x", true).on_click(cx.listener(
                    move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.browse_close_split_pane(pane, window, cx);
                    },
                )),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.workspace.browse.active != Some(pane) {
                    this.browse_select(pane, window, cx);
                }
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
            .into_any_element()
    }

    // ---- A tab dragged onto the page ----

    /// A tab moves over the page: the pane under the pointer and its edge
    /// nearest the pointer become the drop target, eased toward from what
    /// is painted now. In the middle an armed edge stays, as SOCode's.
    pub(super) fn browse_tab_drag_move(
        &mut self,
        event: &DragMoveEvent<super::modes_ui::DraggedBrowseTab>,
        cx: &mut Context<Self>,
    ) {
        let dragged = event.drag(cx).clone();
        let pointer = event.event.position;
        let next = self.browse_drop_target(dragged.pane, dragged.title, pointer, cx);
        let reduce = cx.reduce_motion();
        let live = &mut self.browse_live;
        let Some(mut next) = next else {
            if live.drop.take().is_some() {
                cx.notify();
            }
            return;
        };
        if let Some(old) = &live.drop
            && old.source == next.source
            && old.anchor == next.anchor
        {
            // In the middle, an armed edge stays.
            if next.placement.is_none() && old.placement.is_some() {
                next.placement = old.placement;
            }
            if old.placement == next.placement {
                next.from_cover = old.from_cover;
                next.from_outline = old.from_outline;
                next.since = old.since;
            } else {
                let (cover, outline) = old.painted(reduce);
                next.from_outline = outline;
                if old.placement.is_some() && next.placement.is_some() {
                    next.from_cover = cover;
                }
            }
        }
        live.drop = Some(next);
        cx.notify();
    }

    /// Where `source`, held at `pointer`, would join the page on screen.
    fn browse_drop_target(
        &self,
        source: i64,
        title: SharedString,
        pointer: Point<Pixels>,
        cx: &App,
    ) -> Option<SplitDrop> {
        let browse = &self.workspace.browse;
        if !self.browser_mode()
            || browse.peek.is_some()
            || self.command_bar.is_some()
            || !browse.owns(source)
            || self.browse_split_of(source).is_some()
        {
            return None;
        }
        let (panes, axis) = self.browse_split_panes();
        let split = panes.len() > 1;
        if panes.contains(&source)
            || panes.len() >= socode_browser::splits::MAX_SPLIT_PANES
            || !panes.iter().all(|pane| browse.owns(*pane))
        {
            return None;
        }
        let frames = self.browse_live.pane_frames.borrow();
        let (anchor, frame) = panes
            .iter()
            .filter_map(|pane| Some((*pane, *frames.get(pane)?)))
            .find(|(_, frame)| frame.contains(&pointer))?;
        let placement = drop_placement(frame, pointer, split.then_some(axis));
        let (cover, outline) = drop_geometry(frame, placement);
        let favicon = self.browse_tab(source, cx).and_then(|tab| tab.favicon);
        Some(SplitDrop {
            source,
            anchor,
            placement,
            frame,
            pointer,
            title,
            favicon,
            from_cover: collapsed(cover, placement),
            from_outline: outline,
            since: Instant::now(),
        })
    }

    /// The tab is let go over the page: on an edge it joins the pane there.
    pub(super) fn browse_tab_drop(
        &mut self,
        dragged: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drop) = self.browse_live.drop.take() else {
            return;
        };
        cx.notify();
        if drop.source != dragged {
            return;
        }
        if let Some(placement) = drop.placement {
            self.browse_add_split_at(drop.anchor, dragged, placement, window, cx);
        }
    }

    /// At each frame, from `browse_main`: the drop preview's window opens
    /// while a tab is held over the page and goes after.
    pub(super) fn sync_browse_drop_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.has_active_drag() && self.browse_live.drop.take().is_some() {
            cx.notify();
        }
        if !child_windows() {
            return;
        }
        let wanted = self.browse_live.drop.is_some();
        let live = &mut self.browse_live;
        match (&live.drop_window, wanted) {
            (Some(child), false) => {
                close_child_window(child.handle, cx);
                live.drop_window = None;
            }
            (None, true) if !live.drop_window_opening => {
                live.drop_window_opening = true;
                let size = window.bounds().size;
                let shell = cx.entity();
                let weak = shell.downgrade();
                window.defer(cx, move |window, cx| {
                    let opened = open_child_window(window, true, false, cx, |_, cx| {
                        cx.new(|cx| DropRoot {
                            shell: weak.clone(),
                            _observe: cx.observe(&shell, |_, _, cx| cx.notify()),
                        })
                    });
                    shell.update(cx, |this, cx| {
                        let live = &mut this.browse_live;
                        live.drop_window_opening = false;
                        let child = opened.map(|handle| ChildWindow {
                            handle: handle.into(),
                            size,
                        });
                        // Let go while it opened: it goes again.
                        match (child, live.drop.is_some()) {
                            (Some(child), true) => live.drop_window = Some(child),
                            (Some(child), false) => close_child_window(child.handle, cx),
                            (None, _) => {}
                        }
                        cx.notify();
                    });
                });
            }
            _ => {}
        }
    }

    /// The drop preview, in window coordinates: the half the new pane
    /// would take in the Browser colour with its outline, the whole pane's
    /// outline in the middle, and the held tab's card at the pointer.
    pub(super) fn browse_split_drop_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let drop = self.browse_live.drop.as_ref()?;
        let reduce = cx.reduce_motion();
        if drop.progress(reduce) < 1. {
            window.request_animation_frame();
        }
        let (cover, outline) = drop.painted(reduce);
        let tint = self.workspace.browse.tint.swatch();
        let armed = drop.placement.is_some();
        let at = |d: Div, b: Bounds<Pixels>| {
            d.absolute()
                .left(b.origin.x)
                .top(b.origin.y)
                .w(b.size.width)
                .h(b.size.height)
        };
        let favicon = (drop.favicon.as_deref()).and_then(|url| crate::favicons::icon(url, cx));
        let card_at = drop.pointer - gpui::point(px(64.), px(28.));
        Some(
            div()
                .size_full()
                .relative()
                .when(armed, |d| {
                    d.child(
                        at(div(), cover)
                            .debug_selector(|| "browse-split-drop-cover".into())
                            .rounded(px(10.))
                            .overflow_hidden()
                            .bg(crate::theme::pane_fill())
                            .child(div().size_full().bg(tint.opacity(0.22))),
                    )
                })
                .child(
                    at(div(), outline)
                        .debug_selector(|| "browse-split-drop-outline".into())
                        .rounded(px(10.))
                        .border_2()
                        .border_color(if armed { white(0.55) } else { tint }),
                )
                .child(
                    div()
                        .debug_selector(|| "browse-split-drag-card".into())
                        .absolute()
                        .left(card_at.x)
                        .top(card_at.y)
                        .w(px(128.))
                        .h(px(250.))
                        .rounded(px(20.))
                        .bg(gpui::rgba(0x000000e6))
                        .shadow(vec![
                            BoxShadow::new(px(0.), px(6.), gpui::rgba(0x0000004d).into())
                                .blur_radius(px(16.)),
                        ])
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(24.))
                        .text_color(gpui::white())
                        .text_size(px(13.))
                        .font_weight(FontWeight::BOLD)
                        .line_height(px(16.))
                        .child(match favicon {
                            Some(image) => gpui::img(image).size(px(24.)).into_any_element(),
                            None => icon("globe", 22.).text_color(white(0.8)).into_any_element(),
                        })
                        .child(
                            div()
                                .w(px(104.))
                                .max_h(px(48.))
                                .overflow_hidden()
                                .text_center()
                                .child(drop.title.clone()),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The actions Split View adds to the workspace's element.
    pub(super) fn browse_split_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        root.on_action(cx.listener(|this, _: &BrowseAddSplit, window, cx| {
            if let Some(pane) = this.workspace.browse.active.filter(|_| this.browser_mode()) {
                this.browse_open_command_bar(
                    super::command_bar::BarTarget::Split(pane),
                    window,
                    cx,
                );
            }
        }))
        .on_action(cx.listener(|this, _: &BrowseCloseSplitPane, window, cx| {
            if let Some(pane) = this.workspace.browse.active.filter(|_| this.browser_mode())
                && this.browse_split_of(pane).is_some()
            {
                this.browse_close_split_pane(pane, window, cx);
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::AppMode;
    use gpui::{TestAppContext, VisualTestContext};

    fn frame() -> Bounds<Pixels> {
        Bounds::new(
            gpui::point(px(100.), px(0.)),
            gpui::size(px(800.), px(600.)),
        )
    }

    #[test]
    fn a_dragged_divider_resizes_its_two_panes_within_their_room() {
        // 1010 points, a 10-point gap: 500 each.
        let even = [0.5, 0.5];
        let w = dragged_weights(&even, 0, 1010., 10., 300.).unwrap();
        assert!((w[0] - 300. / 1000.).abs() < 1e-4, "{w:?}");
        // Never below the least a pane may be.
        let w = dragged_weights(&even, 0, 1010., 10., 20.).unwrap();
        assert!((w[0] * 1000. - MIN_PANE).abs() < 0.01, "{w:?}");
        let w = dragged_weights(&even, 0, 1010., 10., 990.).unwrap();
        assert!((w[1] * 1000. - MIN_PANE).abs() < 0.01, "{w:?}");
        // Near the even place it snaps there.
        let w = dragged_weights(&[0.3, 0.7], 0, 1010., 10., 506.).unwrap();
        assert_eq!(w, vec![0.5, 0.5]);
        // Three panes: the first keeps its share; the two beside the
        // second divider share theirs.
        let thirds = [1. / 3., 1. / 3., 1. / 3.];
        let w = dragged_weights(&thirds, 1, 1020., 10., 600.).unwrap();
        assert!((w[0] - 1. / 3.).abs() < 1e-4, "{w:?}");
        assert!((w[1] + w[2] - 2. / 3.).abs() < 1e-4, "{w:?}");
        assert!(
            (w[1] * 1000. - (600. - 1000. / 3. - 10.)).abs() < 0.01,
            "{w:?}"
        );
        assert!(dragged_weights(&thirds, 2, 1020., 10., 600.).is_none());
    }

    #[test]
    fn a_held_tab_takes_the_edge_nearest_the_pointer() {
        let at = |x: f32, y: f32| gpui::point(px(100. + x), px(y));
        assert_eq!(
            drop_placement(frame(), at(50., 300.), None),
            Some(Placement::Left)
        );
        assert_eq!(
            drop_placement(frame(), at(750., 300.), None),
            Some(Placement::Right)
        );
        assert_eq!(
            drop_placement(frame(), at(400., 40.), None),
            Some(Placement::Above)
        );
        assert_eq!(
            drop_placement(frame(), at(400., 560.), None),
            Some(Placement::Below)
        );
        assert_eq!(drop_placement(frame(), at(400., 300.), None), None);
        // Side by side: only the sides; stacked: only top and bottom.
        let side = Some(Axis::Horizontal);
        assert_eq!(drop_placement(frame(), at(400., 40.), side), None);
        let stacked = Some(Axis::Vertical);
        assert_eq!(drop_placement(frame(), at(50., 300.), stacked), None);
        assert_eq!(
            drop_placement(frame(), at(50., 40.), stacked),
            Some(Placement::Above)
        );
        // The cover is the new pane's half, its outline inside it.
        let (cover, outline) = drop_geometry(frame(), Some(Placement::Right));
        assert_eq!(cover.origin.x, px(100. + 400. - 5.));
        assert_eq!(cover.size.width, px(405.));
        assert_eq!(outline.origin.x, px(100. + 405.));
        let (cover, outline) = drop_geometry(frame(), None);
        assert_eq!((cover, outline), (frame(), frame()));
    }

    #[gpui::test]
    fn dividers_resize_panes_and_a_tab_dropped_on_an_edge_joins_the_page(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = super::super::tests::window(cx);
        let (a, b, c) = window
            .update(cx, |shell, window, cx| {
                shell.workspace.classic_layout = true;
                shell.set_mode(AppMode::Browser, window, cx);
                let a =
                    shell.browse_new_tab(Some("https://a.test/".into()), None, true, window, cx);
                let b =
                    shell.browse_new_tab(Some("https://b.test/".into()), None, true, window, cx);
                let c =
                    shell.browse_new_tab(Some("https://c.test/".into()), None, true, window, cx);
                (a, b, c)
            })
            .unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        cx.simulate_resize(gpui::size(px(1400.), px(900.)));
        cx.run_until_parked();
        // A tab held over the page's right edge: the preview takes that side.
        let row = cx
            .debug_bounds(Box::leak(format!("browse-tab-{a}").into_boxed_str()))
            .expect("a's row");
        let card = cx.debug_bounds("browse-card").expect("the page's card");
        cx.simulate_mouse_down(row.center(), MouseButton::Left, Default::default());
        cx.simulate_mouse_move(
            row.center() + gpui::point(px(30.), px(0.)),
            Some(MouseButton::Left),
            Default::default(),
        );
        let edge = gpui::point(card.right() - px(40.), card.center().y);
        cx.simulate_mouse_move(edge, Some(MouseButton::Left), Default::default());
        cx.run_until_parked();
        let drop = window
            .update(cx, |shell, _, _| shell.browse_live.drop.clone())
            .unwrap()
            .expect("a drop target");
        assert_eq!((drop.source, drop.anchor), (a, c));
        assert_eq!(drop.placement, Some(Placement::Right));
        assert!(cx.debug_bounds("browse-split-drop-cover").is_some());
        assert!(cx.debug_bounds("browse-split-drag-card").is_some());
        // Back in the middle the armed side stays.
        cx.simulate_mouse_move(card.center(), Some(MouseButton::Left), Default::default());
        cx.run_until_parked();
        let placement = window
            .update(cx, |shell, _, _| {
                shell.browse_live.drop.as_ref().and_then(|d| d.placement)
            })
            .unwrap();
        assert_eq!(placement, Some(Placement::Right));
        cx.simulate_mouse_up(card.center(), MouseButton::Left, Default::default());
        cx.run_until_parked();
        let (split, dropped) = window
            .update(cx, |shell, _, _| {
                (shell.browse_split_of(c), shell.browse_live.drop.is_none())
            })
            .unwrap();
        assert_eq!(split, Some(vec![c, a]));
        assert!(dropped, "the preview goes with the drop");
        let _ = b;

        // Each pane has its header; the divider between them resizes them.
        assert!(
            cx.debug_bounds(Box::leak(
                format!("browse-pane-header-{c}").into_boxed_str()
            ))
            .is_some()
        );
        let divider = cx
            .debug_bounds("browse-split-divider-0")
            .expect("a divider");
        let left = cx
            .debug_bounds(Box::leak(format!("browse-split-pane-{c}").into_boxed_str()))
            .unwrap();
        cx.simulate_mouse_down(divider.center(), MouseButton::Left, Default::default());
        cx.simulate_mouse_move(
            divider.center() - gpui::point(px(10.), px(0.)),
            Some(MouseButton::Left),
            Default::default(),
        );
        cx.simulate_mouse_move(
            divider.center() - gpui::point(px(150.), px(0.)),
            Some(MouseButton::Left),
            Default::default(),
        );
        cx.simulate_mouse_up(
            divider.center() - gpui::point(px(150.), px(0.)),
            MouseButton::Left,
            Default::default(),
        );
        cx.run_until_parked();
        let weights = window
            .update(cx, |shell, _, _| shell.browse_split_weights())
            .unwrap();
        assert!(weights[0] < 0.45, "{weights:?}");
        let narrower = cx
            .debug_bounds(Box::leak(format!("browse-split-pane-{c}").into_boxed_str()))
            .unwrap();
        assert!(
            narrower.size.width < left.size.width - px(100.),
            "{narrower:?} {left:?}"
        );
        // A double click on it shares the room evenly again.
        window
            .update(cx, |shell, _, cx| shell.browse_even_split(cx))
            .unwrap();
        let weights = window
            .update(cx, |shell, _, _| shell.browse_split_weights())
            .unwrap();
        assert_eq!(weights, vec![0.5, 0.5]);
        // The pane's menu has its own items.
        window
            .update(cx, |shell, window, cx| {
                let (_, items, _, _) = shell.browse_tab_menu(a, cx);
                let names: Vec<String> = items
                    .iter()
                    .filter_map(|item| match item {
                        MenuItem::Action(action) => Some(action.id.to_string()),
                        _ => None,
                    })
                    .collect();
                for id in ["split-separate", "split-close", "split-separate-all"] {
                    assert!(names.contains(&id.to_string()), "{id} in {names:?}");
                }
                shell.run_browse_tab_menu(a, "split-close", window, cx);
                assert_eq!(shell.browse_split_of(c), None);
            })
            .unwrap();
    }

    fn ids(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action(action) => Some(action.id.to_string()),
                _ => None,
            })
            .collect()
    }

    #[gpui::test]
    fn split_view_joins_pages_and_peek_opens_a_pinned_tabs_links(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.workspace.classic_layout = true;
                shell.set_mode(AppMode::Browser, window, cx);
                let a =
                    shell.browse_new_tab(Some("https://a.test/".into()), None, true, window, cx);
                let b =
                    shell.browse_new_tab(Some("https://b.test/".into()), None, true, window, cx);
                // The tab menu, as SOCode's: Open in Split View for another tab.
                let (_, items, _, style) = shell.browse_tab_menu(a, cx);
                assert_eq!(style, MenuStyle::Native);
                let names = ids(&items);
                for id in [
                    "copy-link",
                    "share",
                    "rename",
                    "mute",
                    "split-add",
                    "duplicate",
                    "move-to",
                    "send-to-chat",
                    "close",
                    "close-below",
                ] {
                    assert!(names.contains(&id.to_string()), "{id} in {names:?}");
                }
                shell.run_browse_tab_menu(a, "split-add", window, cx);
                assert_eq!(shell.browse_split_of(b), Some(vec![b, a]));
                assert_eq!(shell.workspace.browse.active, Some(a));
                assert_eq!(shell.browse_split_panes().0, vec![b, a]);
                // One sidebar row for the split.
                assert_eq!(
                    shell.workspace.browse.today().len(),
                    2,
                    "{:?}",
                    shell.workspace.browse.today()
                );
                let (_, items, _, _) = shell.browse_tab_menu(b, cx);
                assert!(ids(&items).contains(&"split-separate-all".to_string()));
                shell.browse_flip_split(b, cx);
                assert_eq!(shell.browse_split_panes().1, Axis::Vertical);
                shell.browse_separate_all(b, cx);
                assert_eq!(shell.browse_split_of(b), None);
                // Peek: a pinned tab's link elsewhere opens over it, then as a tab.
                shell.browse_toggle_pin(a, cx);
                shell.browse_select(a, window, cx);
                shell.browse_open_peek(a, "https://elsewhere.test/".into(), window, cx);
                let (peek, source) = shell.workspace.browse.peek.unwrap();
                assert_eq!(source, a);
                assert!(shell.workspace.browse.owns(peek), "kept while open");
                assert!(shell.browse_tab(peek, cx).unwrap().section.is_none());
                shell.browse_expand_peek(false, window, cx);
                assert!(shell.workspace.browse.peek.is_none());
                assert_eq!(
                    shell.workspace.browse.today()[0],
                    peek,
                    "the same page, now a tab"
                );
                assert_eq!(shell.workspace.browse.active, Some(peek));
                // Closed, a Peek's page goes.
                shell.browse_open_peek(a, "https://other.test/".into(), window, cx);
                let (gone, _) = shell.workspace.browse.peek.unwrap();
                shell.browse_close_peek(cx);
                assert!(!shell.workspace.browser_panes.contains_key(&gone));
                // A folder's and a selection's menus are native too.
                let (_, _, _, style) = shell.browse_sidebar_menu();
                assert_eq!(style, MenuStyle::Native);
            })
            .unwrap();
    }
}
