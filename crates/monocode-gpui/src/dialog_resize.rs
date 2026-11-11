//! Large dialogs (Commit, Update/Merge/Push) can fill the window from a
//! header button or a double click on the header, and grow or shrink by
//! their bottom-right corner. Each dialog kind remembers the size it was
//! last dragged to until the app quits. Inside, the list above the diff and
//! the diff share the height, and the line between them can be dragged.
use crate::theme::*;
use gpui::{
    AnyElement, Context, MouseButton, Pixels, Point, Size, Stateful, canvas, div, prelude::*, px,
};
use std::sync::Mutex;

pub(crate) const MIN_SIZE: (f32, f32) = (480., 360.);
/// Room kept around a dialog that fills the window.
pub(crate) const MARGIN: f32 = 12.;

/// What one dialog kind remembers until the app quits.
#[derive(Clone, Copy, Default)]
pub(crate) struct Memory {
    /// The size the corner was dragged to.
    size: Option<(f32, f32)>,
    /// The height the area above the diff was dragged to.
    top: Option<f32>,
}
pub(crate) type Remembered = Mutex<Memory>;
pub(crate) const fn remembered() -> Remembered {
    Mutex::new(Memory {
        size: None,
        top: None,
    })
}

/// The least either side of the split keeps.
const SPLIT_MIN: f32 = 72.;

/// The size state of one open dialog.
pub(crate) struct DialogResize {
    maximized: bool,
    /// A size chosen by dragging the corner.
    custom: Option<(f32, f32)>,
    /// A corner drag in progress: where it started and the size it started from.
    dragging: Option<(Point<Pixels>, (f32, f32))>,
    /// The height chosen for the area above the diff.
    top: Option<f32>,
    /// A drag of the line above the diff: where it started, the height the
    /// area above had, and the most it may take.
    splitting: Option<(Point<Pixels>, f32, f32)>,
    remembered: &'static Remembered,
    /// The least height of the usual size, before any drag.
    usual_min_height: f32,
}

/// A dialog view that keeps a `DialogResize`.
pub(crate) trait Resizable: Sized + 'static {
    fn dialog_resize(&mut self) -> &mut DialogResize;
}

impl DialogResize {
    pub(crate) fn new(remembered: &'static Remembered) -> Self {
        let memory = *remembered.lock().unwrap_or_else(|e| e.into_inner());
        Self {
            maximized: false,
            custom: memory.size,
            dragging: None,
            top: memory.top,
            splitting: None,
            remembered,
            usual_min_height: 420.,
        }
    }

    /// A dialog that needs more height than most before it is resized.
    pub(crate) fn with_usual_min_height(mut self, height: f32) -> Self {
        self.usual_min_height = height;
        self
    }

    pub(crate) fn maximized(&self) -> bool {
        self.maximized
    }

    pub(crate) fn toggle(&mut self) {
        self.maximized = !self.maximized;
    }

    /// The card's size in `viewport`: the window less a margin when
    /// maximized, else the dragged size, else the dialog's usual size.
    pub(crate) fn size(&self, viewport: Size<Pixels>) -> (f32, f32) {
        let (vw, vh) = (viewport.width.as_f32(), viewport.height.as_f32());
        let room_w = (vw - 2. * MARGIN).max(MIN_SIZE.0.min(vw));
        let room_h = (vh - 2. * MARGIN).max(MIN_SIZE.1.min(vh));
        if self.maximized {
            (room_w, room_h)
        } else if let Some((w, h)) = self.custom {
            (
                w.clamp(MIN_SIZE.0.min(room_w), room_w),
                h.clamp(MIN_SIZE.1.min(room_h), room_h),
            )
        } else {
            (
                (vw - 80.).clamp(560., 1040.).min(room_w),
                (vh - 80.).clamp(self.usual_min_height, 720.).min(room_h),
            )
        }
    }

    /// The height of the area above the diff, of `room` shared by the two:
    /// the dragged height, else `usual`, leaving both sides something.
    pub(crate) fn top_height(&self, room: f32, usual: f32) -> f32 {
        let most = (room - SPLIT_MIN).max(0.);
        self.top.unwrap_or(usual).clamp(SPLIT_MIN.min(most), most)
    }

    fn start_split(&mut self, at: Point<Pixels>, from: f32, room: f32) {
        self.splitting = Some((at, from, room));
    }

    fn start(&mut self, at: Point<Pixels>, from: (f32, f32)) {
        self.dragging = Some((at, from));
        self.maximized = false;
    }

    /// The card stays centred, so it grows by twice the pointer's travel to
    /// keep the corner under the pointer.
    fn drag(&mut self, event: &gpui::MouseMoveEvent) -> bool {
        if event.pressed_button != Some(MouseButton::Left) {
            return self.finish();
        }
        if let Some((start, from, room)) = self.splitting {
            let most = (room - SPLIT_MIN).max(0.);
            let moved = (event.position - start).y.as_f32();
            self.top = Some((from + moved).clamp(SPLIT_MIN.min(most), most));
            return true;
        }
        let Some((start, from)) = self.dragging else {
            return false;
        };
        let moved = event.position - start;
        self.custom = Some((
            from.0 + 2. * moved.x.as_f32(),
            from.1 + 2. * moved.y.as_f32(),
        ));
        true
    }

    fn finish(&mut self) -> bool {
        if self.dragging.take().is_none() & self.splitting.take().is_none() {
            return false;
        }
        *self.remembered.lock().unwrap_or_else(|e| e.into_inner()) = Memory {
            size: self.custom,
            top: self.top,
        };
        true
    }
}

/// The header button that fills the window and restores the size.
pub(crate) fn maximize_button<V: Resizable>(
    id: &'static str,
    maximized: bool,
    cx: &mut Context<V>,
) -> Stateful<gpui::Div> {
    let (label, glyph) = if maximized {
        ("Restore size", "minimize-2")
    } else {
        ("Fill the window", "maximize-2")
    };
    icon_button(id, label, glyph)
        .debug_selector(move || id.into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(|this: &mut V, _, _, cx| {
            cx.stop_propagation();
            this.dialog_resize().toggle();
            cx.notify();
        }))
}

/// The header's click handler: a double click toggles the size, like the
/// button.
pub(crate) fn header_click<V: Resizable>(
    this: &mut V,
    event: &gpui::ClickEvent,
    _: &mut gpui::Window,
    cx: &mut Context<V>,
) {
    if event.click_count() == 2 {
        this.dialog_resize().toggle();
        cx.notify();
    }
}

/// The bottom-right grip, for a card of `size`.
pub(crate) fn corner_grip<V: Resizable>(
    id: &'static str,
    size: (f32, f32),
    cx: &mut Context<V>,
) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(gpui::Role::Splitter)
        .aria_label("Resize dialog")
        .absolute()
        .right_0()
        .bottom_0()
        .size(px(18.))
        .cursor(gpui::CursorStyle::ResizeUpLeftDownRight)
        .child(
            div()
                .absolute()
                .right(px(4.))
                .bottom(px(4.))
                .size(px(8.))
                .border_r_2()
                .border_b_2()
                .border_color(content(0.3)),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this: &mut V, event: &gpui::MouseDownEvent, _, cx| {
                this.dialog_resize().start(event.position, size);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .into_any_element()
}

/// The line between the area above the diff and the diff, dragged up and
/// down. It takes no height: put it between the two in their column. `top`
/// is the height the area above has, of `room` shared by the two.
pub(crate) fn split_handle<V: Resizable>(
    id: &'static str,
    top: f32,
    room: f32,
    cx: &mut Context<V>,
) -> AnyElement {
    div()
        .relative()
        .h_0()
        .w_full()
        .flex_shrink_0()
        .child(
            div()
                .id(id)
                .debug_selector(move || id.into())
                .role(gpui::Role::Splitter)
                .aria_label("Resize the diff")
                .absolute()
                .left_0()
                .right_0()
                // Above the line, where the diff painted after it cannot
                // cover it.
                .top(px(-6.))
                .h(px(7.))
                .cursor(gpui::CursorStyle::ResizeUpDown)
                .hover(|s| s.bg(content(0.12)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this: &mut V, event: &gpui::MouseDownEvent, _, cx| {
                        this.dialog_resize().start_split(event.position, top, room);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                ),
        )
        .into_any_element()
}

/// The modal overlay hides the dialog from hover, so a corner drag listens
/// at the window. Put this anywhere in the dialog's tree.
pub(crate) fn drag_listener<V: Resizable>(cx: &mut Context<V>) -> AnyElement {
    let weak = cx.weak_entity();
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let moves = weak.clone();
            window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                if phase == gpui::DispatchPhase::Capture {
                    let _ = moves.update(cx, |this, cx| {
                        if this.dialog_resize().drag(event) {
                            cx.notify();
                        }
                    });
                }
            });
            let ups = weak.clone();
            window.on_mouse_event(move |_: &gpui::MouseUpEvent, phase, _, cx| {
                if phase == gpui::DispatchPhase::Capture {
                    let _ = ups.update(cx, |this, cx| {
                        if this.dialog_resize().finish() {
                            cx.notify();
                        }
                    });
                }
            });
        },
    )
    .absolute()
    .size_0()
    .into_any_element()
}
