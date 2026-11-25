//! Browser mode's Peek (docs/native/modes.md §Peek): a pinned tab's or a
//! favourite's link to another site opens over the page in a card, as
//! SOCode's and Arc's, so the pinned tab stays where it is. The card opens
//! as a tab (⌘O), joins the page in a Split View, or closes (⌘W, Esc, its
//! ✕, a press beside it).
//!
//! As SOCode's, the card lives in a transparent child window above the
//! main one (the way menus do, `shell/overlay_window.rs`): Chromium's pages
//! are native views GPUI cannot draw over, so in the main window the card
//! would have to stand in for the pinned tab. In its own window the pinned
//! tab stays live under a veil, and Peek's page is a native view of the
//! child window. Tests, and platforms without child windows, draw the card
//! in the main window instead (`browse_peek_card`).
use super::*;
use gpui::{
    AnyWindowHandle, Bounds, Div, Point, Stateful, WeakEntity, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

actions!(
    workspace,
    [
        /// ⌘O while Peek is open: its page becomes a tab of Today.
        BrowsePeekExpand
    ]
);

/// Opening follows a damped spring this long; closing shrinks the card
/// toward the link it came from.
const OPEN_MOTION: Duration = Duration::from_millis(340);
const CLOSE_MOTION: Duration = Duration::from_millis(180);

pub(super) fn bind(cx: &mut App) {
    let m = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.bind_keys([KeyBinding::new(
        &format!("{m}-o"),
        BrowsePeekExpand,
        Some(super::modes::BROWSER_MODE_CONTEXT),
    )]);
}

/// What Browser mode keeps besides the model while it draws: Peek's
/// child window and motion, the split drop preview and the sidebar's icon
/// places (for the icon picker).
#[derive(Default)]
pub(super) struct BrowseLive {
    pub(super) peek: Option<PeekLive>,
    /// A tab held over the page, where it would join it.
    pub(super) drop: Option<super::browse_split::SplitDrop>,
    /// The passive child window drawing the drop preview over live pages.
    pub(super) drop_window: Option<ChildWindow>,
    pub(super) drop_window_opening: bool,
    /// Where each sidebar row's icon was drawn last, in window coordinates.
    pub(super) icon_anchors: Places<super::browse_organize::IconTarget>,
    /// Where each pane on screen was drawn last, in window coordinates.
    pub(super) pane_frames: Places<i64>,
    /// The sidebar's rows in its last frame, and those new since, fading
    /// in (`Shell::browse_row_in`).
    pub(super) rows_seen: std::collections::HashSet<super::browse_organize::IconTarget>,
    pub(super) fresh_rows: HashMap<super::browse_organize::IconTarget, Instant>,
}

/// Where things were drawn last, written by their elements while painting.
pub(super) type Places<K> = std::rc::Rc<std::cell::RefCell<HashMap<K, Bounds<Pixels>>>>;

/// An invisible element that notes its bounds under `key` in `places`.
pub(super) fn place_marker<K: std::hash::Hash + Eq + Copy + 'static>(
    places: &Places<K>,
    key: K,
) -> impl IntoElement {
    let places = places.clone();
    gpui::canvas(
        move |bounds, _, _| {
            places.borrow_mut().insert(key, bounds);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// Peek's motion and child window.
pub(super) struct PeekLive {
    opened: Instant,
    /// Closing: the card shrinks to `origin`, then the window goes.
    closing: Option<Instant>,
    /// Where the link was pressed, in window coordinates.
    origin: Point<Pixels>,
    window: Option<ChildWindow>,
    opening: bool,
}

/// A child window of Browser mode's, and the main window's size when it
/// opened.
pub(super) struct ChildWindow {
    pub(super) handle: AnyWindowHandle,
    pub(super) size: gpui::Size<Pixels>,
}

/// Whether Browser mode draws over pages in child windows here.
pub(super) fn child_windows() -> bool {
    !cfg!(test) && cfg!(target_os = "macos")
}

/// A transparent, borderless child window over `window`, the same size,
/// drawing `root`. With `passive`, presses go through it to the main
/// window. `None` when the platform has none.
pub(super) fn open_child_window<V: Render + 'static>(
    window: &Window,
    passive: bool,
    focus: bool,
    cx: &mut App,
    root: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> Option<gpui::WindowHandle<V>> {
    let parent = super::overlay_window::native_window(window)?;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(window.bounds())),
        titlebar: None,
        focus,
        show: true,
        kind: WindowKind::Normal,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        super::overlay_window::attach_child(parent, window);
        if passive {
            ignore_mouse(window);
        }
        root(window, cx)
    })
    .ok()
}

/// Presses and moves go through `window` to the one below.
#[cfg(target_os = "macos")]
fn ignore_mouse(window: &Window) {
    use objc2::{msg_send, runtime::AnyObject, runtime::Bool};
    let Some(native) = super::overlay_window::native_window(window) else {
        return;
    };
    // SAFETY: the window was just made and is alive.
    unsafe {
        let _: () = msg_send![native as *mut AnyObject, setIgnoresMouseEvents: Bool::YES];
    }
}

#[cfg(not(target_os = "macos"))]
fn ignore_mouse(_: &Window) {}

/// Closes a child window, after this frame.
pub(super) fn close_child_window(handle: AnyWindowHandle, cx: &mut App) {
    cx.defer(move |cx| {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    });
}

/// Peek's child window: the veil, the card with the page, the controls.
pub(crate) struct PeekRoot {
    shell: WeakEntity<Shell>,
    _observe: Subscription,
}

impl Render for PeekRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let drawn = (self.shell.upgrade())
            .and_then(|shell| shell.update(cx, |shell, cx| shell.browse_peek_layer(window, cx)));
        div()
            .id("browse-peek-root")
            .size_full()
            .relative()
            .text_color(content(1.))
            .font_family(crate::fonts::UI)
            .children(drawn)
            // Peek's page is a view of this window: its sweep places it.
            .child(crate::browser::sweep())
    }
}

/// The spring Peek opens with, 0 to about 1, at `t` seconds.
fn spring(t: f32) -> f32 {
    let t = (t / OPEN_MOTION.as_secs_f32()).min(1.);
    if t >= 1. {
        1.
    } else {
        1. - (-8. * t).exp() * ((12. * t).cos() + (8. / 12.) * (12. * t).sin())
    }
}

/// Where the card goes: left, top, width, height, for a window
/// `extent`, the page area starting at `left`, `amount` of the way open (1
/// settled) and, closing, the press's `origin`.
fn card_frame(
    extent: gpui::Size<Pixels>,
    left: f32,
    amount: f32,
    closing: Option<Point<Pixels>>,
) -> (f32, f32, f32, f32) {
    let (w, h) = (f32::from(extent.width), f32::from(extent.height));
    let available = (w - left).max(0.);
    let target = available * 0.8;
    match closing {
        Some(origin) => {
            let x = f32::from(origin.x).clamp(left, w);
            let y = f32::from(origin.y).clamp(12., (h - 12.).max(12.));
            (
                x + (left + available * 0.1 - x) * amount,
                y + (12. - y) * amount,
                target * amount,
                (h - 24.).max(0.) * amount,
            )
        }
        None => {
            let top = 12. + 40. * (1. - amount);
            (
                left + available * 0.1 - 8. * (1. - amount),
                top,
                target * (0.82 + 0.18 * amount),
                (h - top * 2.).max(0.),
            )
        }
    }
}

impl Shell {
    /// `url`, clicked in the page of `source`, opens in Peek.
    pub(super) fn browse_open_peek(
        &mut self,
        source: i64,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browse_drop_peek();
        let pane = self.create_browser_pane(super::modes::NO_OWNER, url);
        self.workspace.browse.peek = Some((pane, source));
        self.browse_live.peek = Some(PeekLive {
            opened: Instant::now(),
            closing: None,
            origin: window.mouse_position(),
            window: None,
            opening: false,
        });
        if let Some(page) = self.browser_page(pane, cx) {
            page.update(cx, |page, cx| {
                page.nav_in_sidebar = true;
                page.focus(window, cx)
            });
        }
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// Peek closes; its page goes, and the pinned tab under it shows. The
    /// card shrinks toward the link it came from.
    pub(super) fn browse_close_peek(&mut self, cx: &mut Context<Self>) {
        let Some((pane, _)) = self.workspace.browse.peek.take() else {
            return;
        };
        self.workspace.browser_panes.remove(&pane);
        self.browser_pages.remove(&pane);
        self.browse_settle_peek(cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// Peek's page is gone or taken as a tab: its card shrinks away, then
    /// its window closes.
    fn browse_settle_peek(&mut self, cx: &mut Context<Self>) {
        let Some(live) = &mut self.browse_live.peek else {
            return;
        };
        if live.window.is_none() || cx.reduce_motion() {
            self.browse_drop_peek_window(cx);
            return;
        }
        live.closing = Some(Instant::now());
        // The main window takes the keyboard back now, for the page.
        if let Some(main) = self.main_window {
            cx.defer(move |cx| {
                let _ = main.update(cx, |_, window, _| window.activate_window());
            });
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CLOSE_MOTION).await;
            let _ = this.update(cx, |this, cx| {
                if this.workspace.browse.peek.is_none() {
                    this.browse_drop_peek_window(cx);
                }
            });
        })
        .detach();
    }

    /// Peek's window goes now.
    fn browse_drop_peek_window(&mut self, cx: &mut Context<Self>) {
        if let Some(live) = self.browse_live.peek.take()
            && let Some(child) = live.window
        {
            close_child_window(child.handle, cx);
            if let Some(main) = self.main_window {
                cx.defer(move |cx| {
                    let _ = main.update(cx, |_, window, _| window.activate_window());
                });
            }
        }
    }

    /// An earlier Peek's page goes at once, before another opens.
    fn browse_drop_peek(&mut self) {
        if let Some((pane, _)) = self.workspace.browse.peek.take() {
            self.workspace.browser_panes.remove(&pane);
            self.browser_pages.remove(&pane);
        }
        if let Some(live) = &mut self.browse_live.peek {
            live.closing = None;
        }
    }

    /// Peek's page becomes a tab at the top of Today, keeping its page and
    /// history; with `split`, it joins the pinned tab in a Split View.
    pub(super) fn browse_expand_peek(
        &mut self,
        split: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane, source)) = self.workspace.browse.peek.take() else {
            return;
        };
        // The page moves to the main window: no card shrinks after it.
        self.browse_drop_peek_window(cx);
        let url = self
            .browse_tab(pane, cx)
            .map(|tab| tab.url)
            .unwrap_or_default();
        // The model's new tab takes Peek's pane: its page lives on.
        let tab = self
            .workspace
            .browse
            .model
            .new_tab(url, crate::modes::now());
        self.workspace.browse.panes.insert(tab.get(), pane);
        self.browse_reconcile(cx);
        if split {
            self.browse_add_split(source, pane, window, cx);
        } else {
            self.browse_select(pane, window, cx);
        }
    }

    /// At each frame of the main window, from `browse_main`: Peek's child
    /// window opens while Peek is, follows the main window's size, and goes
    /// when the page on screen is no longer Peek's source.
    pub(super) fn sync_browse_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, source)) = self.workspace.browse.peek
            && (self.workspace.browse.active != Some(source) || !self.browser_mode())
        {
            self.browse_close_peek(cx);
        }
        if !child_windows() {
            return;
        }
        let size = window.bounds().size;
        let Some(live) = &mut self.browse_live.peek else {
            return;
        };
        match &mut live.window {
            Some(child) if child.size != size => {
                child.size = size;
                let handle = child.handle;
                cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.resize(size));
                });
            }
            Some(_) => {}
            None if !live.opening && self.workspace.browse.peek.is_some() => {
                live.opening = true;
                let shell = cx.entity();
                let weak = shell.downgrade();
                let page =
                    (self.workspace.browse.peek).and_then(|(pane, _)| self.browser_page(pane, cx));
                window.defer(cx, move |window, cx| {
                    let opened = open_child_window(window, false, true, cx, |window, cx| {
                        if let Some(page) = &page {
                            page.update(cx, |page, cx| page.focus(window, cx));
                        }
                        cx.new(|cx| PeekRoot {
                            shell: weak.clone(),
                            _observe: cx.observe(&shell, |_, _, cx| cx.notify()),
                        })
                    });
                    shell.update(cx, |this, cx| {
                        if let Some(live) = &mut this.browse_live.peek {
                            live.opening = false;
                            live.window = opened.map(|handle| ChildWindow {
                                handle: handle.into(),
                                size,
                            });
                        }
                        cx.notify();
                    });
                });
            }
            None => {}
        }
    }

    /// Whether the main window leaves Peek to its child window.
    pub(super) fn browse_peek_in_child(&self) -> bool {
        child_windows() && self.browse_live.peek.is_some()
    }

    /// What Peek's child window draws: the veil over the page area, which
    /// closes Peek on a press, the card with Peek's page, and its controls
    /// beside it, as SOCode's.
    pub(super) fn browse_peek_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let live = self.browse_live.peek.as_ref()?;
        let reduce = cx.reduce_motion();
        let opened = live.opened.elapsed().as_secs_f32();
        let closing = live
            .closing
            .map(|at| at.elapsed().as_secs_f32() / CLOSE_MOTION.as_secs_f32());
        let progress = if reduce { 1. } else { spring(opened) };
        let amount = closing.map_or(progress.min(1.009), |t| 1. - t.clamp(0., 1.).powi(2));
        if !reduce && (opened < OPEN_MOTION.as_secs_f32() || closing.is_some_and(|t| t < 1.)) {
            window.request_animation_frame();
        }
        let origin = live.origin;
        let left = self.browse_rail_width();
        let extent = window.viewport_size();
        let (x, y, w, h) = card_frame(extent, left, amount, closing.map(|_| origin));
        let fade = amount.clamp(0., 1.);
        let page = (self.workspace.browse.peek).and_then(|(pane, _)| self.browser_page(pane, cx));
        if let Some(page) = &page {
            page.update(cx, |page, cx| {
                if !page.nav_in_sidebar {
                    page.nav_in_sidebar = true;
                    cx.notify();
                }
            });
        }
        let light = crate::theme::is_light();
        let control_fill = crate::theme::floating_fill();
        let control_ink = if light {
            gpui::rgb(0x2f39ff).into()
        } else {
            content(0.95)
        };
        let button = |id: &'static str, label: &'static str, path: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .role(Role::Button)
                .aria_label(label)
                .cursor_pointer()
                .size(px(34.))
                .flex_shrink_0()
                .rounded_full()
                .bg(control_fill)
                .border_1()
                .border_color(content(0.1))
                .text_color(control_ink)
                .shadow_md()
                .flex()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(content(0.16)))
                .child(icon(path, 16.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        let veil = div()
            .id("browse-peek-veil")
            .debug_selector(|| "browse-peek-veil".into())
            .absolute()
            .top(px(19.))
            .bottom(px(19.))
            .left(px(left + 9.))
            .right(px(19.))
            .rounded(px(10.))
            .bg(crate::theme::pane_fill())
            .opacity(0.7 * fade)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.in_main_window(cx, |this, window, cx| {
                        this.browse_close_peek(cx);
                        this.focus_browse_page(window, cx);
                    })
                }),
            );
        let card = div()
            .id("browse-peek")
            .debug_selector(|| "browse-peek".into())
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(w.max(0.)))
            .h(px(h.max(0.)))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(10.))
            .border_1()
            .border_color(content(0.12))
            .bg(crate::theme::background_base())
            .opacity(if closing.is_some() { fade } else { 1. })
            .shadow(vec![
                BoxShadow::new(px(0.), px(2.), gpui::rgba(0x00000038).into()).blur_radius(px(9.)),
            ])
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(page.filter(|_| closing.is_none()).map(|page| {
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(page)
            }));
        let controls = div()
            .absolute()
            .left(px(x + w + 13.))
            .top(px(20.))
            .opacity(fade)
            .flex()
            .flex_col()
            .gap(px(9.5))
            .child(
                button("browse-peek-close", "Close Peek", "x").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.in_main_window(cx, |this, window, cx| {
                            this.browse_close_peek(cx);
                            this.focus_browse_page(window, cx);
                        })
                    },
                )),
            )
            .child(
                button("browse-peek-expand", "Open as Tab (⌘O)", "maximize-2").on_click(
                    cx.listener(|this, _, _, cx| {
                        this.in_main_window(cx, |this, window, cx| {
                            this.browse_expand_peek(false, window, cx)
                        })
                    }),
                ),
            )
            .child(
                button("browse-peek-split", "Open in Split View", "panel-right").on_click(
                    cx.listener(|this, _, _, cx| {
                        this.in_main_window(cx, |this, window, cx| {
                            this.browse_expand_peek(true, window, cx)
                        })
                    }),
                ),
            );
        Some(
            div()
                .id("browse-peek-layer")
                .size_full()
                .relative()
                // The child window has no workspace keys: Esc, ⌘W and ⌘O
                // are read here and run in the main window.
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    let keys = &event.keystroke;
                    let command = keys.modifiers.platform || keys.modifiers.control;
                    let close = keys.key == "escape" || (command && keys.key == "w");
                    let expand = command && keys.key == "o";
                    if close || expand {
                        cx.stop_propagation();
                        this.in_main_window(cx, move |this, window, cx| {
                            if expand {
                                this.browse_expand_peek(false, window, cx);
                            } else {
                                this.browse_close_peek(cx);
                                this.focus_browse_page(window, cx);
                            }
                        });
                    }
                }))
                .child(veil)
                .child(card)
                .child(controls)
                .into_any_element(),
        )
    }

    /// Peek over the page area in the main window, where it has no window
    /// of its own (tests): its card, with its three controls. `None` while
    /// it is closed.
    pub(super) fn browse_peek_card(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.browse_peek_in_child() {
            return None;
        }
        let (pane, _) = self.workspace.browse.peek?;
        let page = self.browser_page(pane, cx)?;
        page.update(cx, |page, cx| {
            if !page.nav_in_sidebar {
                page.nav_in_sidebar = true;
                cx.notify();
            }
        });
        let host = self
            .browse_tab(pane, cx)
            .map(|tab| super::modes_ui::host_of(&tab.url))
            .unwrap_or_default();
        let control = |id: &'static str, label: &'static str, path: &'static str| {
            icon_button(id, label, path)
                .size(px(26.))
                .rounded(px(7.))
                .debug_selector(move || id.into())
        };
        Some(
            div()
                .id("browse-peek")
                .debug_selector(|| "browse-peek".into())
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded(px(12.))
                .border_1()
                .border_color(content(0.14))
                .bg(crate::theme::background_base())
                .shadow(vec![
                    BoxShadow::new(px(0.), px(12.), gpui::rgba(0x00000059).into())
                        .blur_radius(px(36.)),
                ])
                .child(
                    div()
                        .h(px(36.))
                        .flex_shrink_0()
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .border_b_1()
                        .border_color(content(0.08))
                        .child(control("browse-peek-close", "Close Peek", "x").on_click(
                            cx.listener(|this, _, window, cx| {
                                this.browse_close_peek(cx);
                                this.focus_browse_page(window, cx);
                            }),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.5))
                                .text_color(content(0.55))
                                .text_center()
                                .child(host),
                        )
                        .child(
                            control("browse-peek-split", "Open in Split View", "panel-right")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.browse_expand_peek(true, window, cx)
                                })),
                        )
                        .child(
                            control("browse-peek-expand", "Open as Tab (⌘O)", "window-maximize")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.browse_expand_peek(false, window, cx)
                                })),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(page),
                )
                .into_any_element(),
        )
    }

    /// The actions Peek adds to the workspace's element.
    pub(super) fn browse_peek_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        root.on_action(cx.listener(|this, _: &BrowsePeekExpand, window, cx| {
            if this.browser_mode() && this.workspace.browse.peek.is_some() {
                this.browse_expand_peek(false, window, cx);
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_settles_at_four_fifths_of_the_page_area_and_closes_to_the_link() {
        let extent = gpui::size(px(1200.), px(800.));
        let (x, y, w, h) = card_frame(extent, 228., 1., None);
        assert!((w - 777.6).abs() < 0.01, "{w}");
        assert!((x - (228. + 97.2)).abs() < 0.01, "{x}");
        assert_eq!((y, h), (12., 776.));
        // Closed all the way: a point at the press.
        let origin = gpui::point(px(500.), px(300.));
        let (x, y, w, h) = card_frame(extent, 228., 0., Some(origin));
        assert_eq!((x, y, w, h), (500., 300., 0., 0.));
        // The spring starts closed, overshoots a little and settles open.
        assert_eq!(spring(0.), 0.);
        assert_eq!(spring(1.), 1.);
        assert!((0..34).map(|i| spring(i as f32 / 100.)).any(|v| v > 1.));
    }
}
