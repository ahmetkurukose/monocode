//! The gutter columns left of the line number and the line highlights
//! behind the text: where a press lands, what is drawn on a row, and the
//! tooltip of a mark. The columns exist only while something owns or marks
//! them, so an editor without them keeps its narrow gutter.
use super::*;
use crate::code_editor::{
    Breakpoint, GutterButton, GutterClick, GutterMarkKind, GutterSlot, LineStyle, NoteTone,
    SLOT_WIDTH,
};

const BREAKPOINT_RED: u32 = 0xef4444;
const RUN_GREEN: u32 = 0x4ade80;
const EXECUTION_AMBER: u32 = 0xfbbf24;

impl Input {
    /// The column and row under a window position, folds and wrapped lines
    /// included: a wrapped line is one row however tall it is.
    pub(super) fn gutter_hit(&self, position: Point<Pixels>) -> Option<(GutterSlot, usize)> {
        let code = self.code.as_ref()?;
        let decor = code.decor.as_ref()?;
        let bounds = self.bounds?;
        if position.y < bounds.top() || position.y > bounds.bottom() {
            return None;
        }
        let left = bounds.left() - px(code.gutter());
        let slot = GutterSlot::ALL.into_iter().find(|slot| {
            decor.slot_left(*slot).is_some_and(|x| {
                position.x >= left + px(x) && position.x < left + px(x + SLOT_WIDTH)
            })
        })?;
        let y = (position.y - bounds.top() + self.scroll_y).as_f32();
        (y >= TOP_PADDING && y < code.total() - BOTTOM_PADDING).then(|| (slot, code.row_at_y(y)))
    }
    /// A press in a gutter column goes to its provider and nowhere else.
    pub(in crate::input) fn code_gutter_down(
        &mut self,
        e: &MouseDownEvent,
        right: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((slot, row)) = self.gutter_hit(e.position) else {
            return false;
        };
        let Some(code) = &self.code else {
            return false;
        };
        let provider = code
            .ext
            .as_ref()
            .and_then(|ext| ext.providers.gutter(slot).cloned());
        if let Some(provider) = provider {
            let doc = code.document();
            let click = GutterClick {
                slot,
                row,
                mark: code
                    .decor
                    .as_ref()
                    .and_then(|decor| decor.mark(slot, row))
                    .map(|mark| mark.kind),
                button: if right {
                    GutterButton::Right
                } else {
                    GutterButton::Left
                },
                position: e.position,
            };
            // After this update, so the provider is free to reach the editor.
            window.defer(cx, move |window, cx| {
                provider.click(&doc, &click, window, cx)
            });
        }
        cx.stop_propagation();
        true
    }
    pub(in crate::input) fn code_right_down(
        &mut self,
        e: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.code_gutter_down(e, true, window, cx) {
            self.open_context_menu(e, window, cx);
        }
    }
    pub(super) fn gutter_pointer(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        let hit = self.gutter_hit(e.position);
        let Some(code) = &mut self.code else {
            return;
        };
        let Some(decor) = &code.decor else {
            return;
        };
        let mark = hit.and_then(|(slot, row)| decor.mark(slot, row));
        // A faint dot shows where a press would put a breakpoint.
        let hover = hit
            .filter(|(slot, _)| *slot == GutterSlot::Breakpoint && mark.is_none())
            .map(|(_, row)| row);
        let tip = mark
            .filter(|mark| !mark.tooltip.is_empty())
            .map(|mark| mark.tooltip.clone())
            .zip(hit)
            .map(|(text, (_, row))| {
                (
                    text,
                    code.top(row, self.line_height) - self.scroll_y.as_f32(),
                )
            });
        if decor.hover != hover {
            code.decor_mut().hover = hover;
            cx.notify();
        }
        if code.ext.as_ref().and_then(|ext| ext.gutter_tip.clone()) != tip {
            code.ext_mut().gutter_tip = tip;
            cx.notify();
        }
    }
    pub(super) fn gutter_tooltip(&self) -> Option<AnyElement> {
        let code = self.code.as_ref()?;
        let (text, top) = code.ext.as_ref()?.gutter_tip.clone()?;
        Some(
            div()
                .absolute()
                .left(px(code.gutter()))
                .top(px(top.max(0.)))
                .max_w(px(360.))
                .px_2()
                .py_1()
                .rounded_md()
                .bg(crate::theme::pane_fill())
                .border_1()
                .border_color(crate::theme::content(0.14))
                .text_size(px(12.))
                .text_color(crate::theme::content(1.))
                .child(text)
                .into_any_element(),
        )
    }
    #[cfg(test)]
    pub(crate) fn gutter_slot_center(&self, slot: GutterSlot, row: usize) -> Option<Point<Pixels>> {
        let code = self.code.as_ref()?;
        let bounds = self.bounds?;
        let x = code.decor.as_ref()?.slot_left(slot)?;
        Some(point(
            bounds.left() - px(code.gutter()) + px(x + SLOT_WIDTH / 2.),
            bounds.top() + px(code.top(row, self.line_height) + self.line_height / 2.)
                - self.scroll_y,
        ))
    }
}

pub(super) fn note_color(tone: NoteTone) -> Hsla {
    match tone {
        NoteTone::Dim => crate::theme::content(0.4),
        NoteTone::Ok => Hsla::from(rgb(RUN_GREEN)).opacity(0.75),
        NoteTone::Error => Hsla::from(rgb(0xf87171)).opacity(0.9),
    }
}

/// A triangle pointing right, built from pixel rows so painting a gutter
/// shapes no text.
fn triangle(center: Point<Pixels>, color: Hsla, w: &mut Window) {
    const ROWS: usize = 8;
    for n in 0..ROWS {
        let from_middle = (n as f32 + 0.5 - ROWS as f32 / 2.).abs();
        let width = (ROWS as f32 / 2. - from_middle) * 1.75;
        w.paint_quad(fill(
            Bounds::new(
                center + point(px(-3.), px(n as f32 - ROWS as f32 / 2.)),
                size(px(width), px(1.)),
            ),
            color,
        ));
    }
}

fn dot(center: Point<Pixels>, diameter: f32) -> gpui::PaintQuad {
    fill(
        Bounds::new(
            center - point(px(diameter / 2.), px(diameter / 2.)),
            size(px(diameter), px(diameter)),
        ),
        gpui::transparent_black(),
    )
    .corner_radii(px(diameter / 2.))
}

/// Draws one row's highlight and gutter marks. `bounds` is the row from the
/// editor's left edge to the scrollbar; `lh` the height of one visual line.
pub(super) fn paint_row(
    decor: &Decor,
    row: usize,
    bounds: Bounds<Pixels>,
    lh: Pixels,
    w: &mut Window,
) {
    let line = decor.line(row);
    if let Some(line) = line {
        w.paint_quad(fill(
            bounds,
            match line.style {
                LineStyle::Execution => Hsla::from(rgb(EXECUTION_AMBER)).opacity(0.16),
                LineStyle::Frame => Hsla::from(rgb(0x60a5fa)).opacity(0.13),
            },
        ));
    }
    for slot in GutterSlot::ALL {
        let Some(x) = decor.slot_left(slot) else {
            continue;
        };
        let center = bounds.origin + point(px(x + SLOT_WIDTH / 2.), lh / 2.);
        match decor.mark(slot, row).map(|mark| mark.kind) {
            Some(GutterMarkKind::Breakpoint(state)) => {
                let red: Hsla = rgb(BREAKPOINT_RED).into();
                let quad = dot(center, 9.);
                w.paint_quad(match state {
                    Breakpoint::Enabled | Breakpoint::Conditional => quad.background(red),
                    Breakpoint::Disabled => quad.border_widths(px(1.5)).border_color(red),
                    Breakpoint::Unverified => quad.background(crate::theme::content(0.35)),
                });
                if state == Breakpoint::Conditional {
                    w.paint_quad(dot(center, 3.).background(gpui::white()));
                }
            }
            Some(GutterMarkKind::Run) => triangle(center, rgb(RUN_GREEN).into(), w),
            None if slot == GutterSlot::Breakpoint && decor.hover == Some(row) => {
                w.paint_quad(
                    dot(center, 9.).background(Hsla::from(rgb(BREAKPOINT_RED)).opacity(0.3)),
                );
            }
            None => {}
        }
        if slot == GutterSlot::Breakpoint && line.is_some_and(|line| line.arrow) {
            triangle(
                center,
                match line.map(|line| line.style) {
                    Some(LineStyle::Frame) => rgb(0x60a5fa).into(),
                    _ => rgb(EXECUTION_AMBER).into(),
                },
                w,
            );
        }
    }
}
