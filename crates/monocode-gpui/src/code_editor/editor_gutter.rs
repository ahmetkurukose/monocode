//! The gutter columns left of the line number and the line highlights
//! behind the text: where a press lands, what is drawn on a row, and the
//! tooltip of a mark. The columns exist only while something owns or marks
//! them, so an editor without them keeps its narrow gutter. An annotation
//! column (blame) comes first, at the gutter's left edge.
use super::*;
use crate::code_editor::{
    ANNOTATION_TEXT, Annotation, AnnotationClick, AnnotationColumn, Annotations, Breakpoint,
    GutterButton, GutterClick, GutterMarkKind, GutterSlot, LineStyle, NoteTone, SLOT_WIDTH,
};
use std::collections::HashMap;

const BREAKPOINT_RED: u32 = 0xef4444;
const RUN_GREEN: u32 = 0x4ade80;
const EXECUTION_AMBER: u32 = 0xfbbf24;

/// How strongly the most recent commits tint their rows.
const ANNOTATION_TINT: f32 = 0.3;

/// The width of each part's column: the widest of that part in any entry,
/// in the annotation font.
fn part_widths(entries: &[Annotation], cx: &App) -> Vec<f32> {
    let parts = entries.iter().map(|e| e.parts.len()).max().unwrap_or(0);
    let mut widths = vec![0f32; parts];
    if entries.is_empty() {
        return widths;
    }
    let system = cx.text_system();
    let font = system.resolve_font(&annotation_font());
    let size = px(ANNOTATION_TEXT);
    let mut advances: HashMap<char, f32> = HashMap::new();
    for entry in entries {
        for (part, width) in entry.parts.iter().zip(&mut widths) {
            let measured: f32 = part
                .chars()
                .map(|ch| {
                    *advances.entry(ch).or_insert_with(|| {
                        system
                            .advance(font, size, ch)
                            .map_or(ANNOTATION_TEXT * 0.6, |advance| advance.width.as_f32())
                    })
                })
                .sum();
            *width = width.max(measured.ceil());
        }
    }
    widths
}

fn annotation_font() -> gpui::Font {
    gpui::font(crate::fonts::UI)
}

impl Input {
    /// Shows an annotation column at the gutter's left edge, or takes it
    /// away with `None`. Its parts are measured here, once per set of
    /// entries.
    pub(crate) fn set_annotations(
        &mut self,
        annotations: Option<Annotations>,
        cx: &mut Context<Self>,
    ) {
        let Some(code) = &mut self.code else {
            return;
        };
        if code.decor.is_none() && annotations.is_none() {
            return;
        }
        let column = annotations.map(|annotations| {
            let widths = code
                .decor
                .as_ref()
                .and_then(|decor| decor.annotations.as_ref())
                .filter(|old| Rc::ptr_eq(&old.annotations.entries, &annotations.entries))
                .map(|old| old.widths.clone())
                .unwrap_or_else(|| part_widths(&annotations.entries, cx));
            AnnotationColumn {
                annotations,
                widths,
            }
        });
        let decor = code.decor_mut();
        decor.annotations = column;
        if decor.hover.is_some() {
            decor.hover = None;
        }
        if let Some(ext) = &mut code.ext {
            ext.gutter_tip = None;
        }
        cx.notify();
    }
    /// The row of the annotation column under a window position.
    pub(super) fn annotation_hit(&self, position: Point<Pixels>) -> Option<usize> {
        let code = self.code.as_ref()?;
        let width = code.decor.as_ref()?.annotation_width();
        let bounds = self.bounds?;
        if width <= 0. || position.y < bounds.top() || position.y > bounds.bottom() {
            return None;
        }
        let left = bounds.left() - px(code.gutter());
        if position.x < left || position.x >= left + px(width) {
            return None;
        }
        let y = (position.y - bounds.top() + self.scroll_y).as_f32();
        (y >= TOP_PADDING && y < code.total() - BOTTOM_PADDING).then(|| code.row_at_y(y))
    }
    /// A press in the annotation column goes to the column's provider.
    fn annotation_down(
        &mut self,
        row: usize,
        e: &MouseDownEvent,
        right: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ext) = self.code.as_mut().and_then(|code| code.ext.as_mut())
            && ext.gutter_tip.take().is_some()
        {
            cx.notify();
        }
        cx.stop_propagation();
        let Some(code) = &self.code else {
            return;
        };
        let Some(column) = code.decor.as_ref().and_then(|d| d.annotations.as_ref()) else {
            return;
        };
        let Some(provider) = column.annotations.provider.clone() else {
            return;
        };
        let doc = code.document();
        let click = AnnotationClick {
            row,
            entry: column.annotations.entry(row).cloned(),
            button: if right {
                GutterButton::Right
            } else {
                GutterButton::Left
            },
            position: e.position,
            editor: cx.entity_id(),
        };
        // After this update, so the provider is free to reach the editor.
        let this = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            match provider.menu(&doc, &click, window, cx) {
                Some(menu) => {
                    let _ = this.update(cx, |this, cx| {
                        this.open_gutter_menu(menu, click.position, window, cx)
                    });
                }
                None => provider.click(&doc, &click, window, cx),
            }
        });
    }
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
        if let Some(row) = self.annotation_hit(e.position) {
            self.annotation_down(row, e, right, window, cx);
            return true;
        }
        let Some((slot, row)) = self.gutter_hit(e.position) else {
            return false;
        };
        // A press opens something of its own (a menu the provider draws, a
        // toggle): the tip of the mark would only lie under it.
        if let Some(ext) = self.code.as_mut().and_then(|code| code.ext.as_mut())
            && ext.gutter_tip.take().is_some()
        {
            cx.notify();
        }
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
            let this = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                match provider.menu(&doc, &click, window, cx) {
                    Some(menu) => {
                        let _ = this.update(cx, |this, cx| {
                            this.open_gutter_menu(menu, click.position, window, cx)
                        });
                    }
                    None => provider.click(&doc, &click, window, cx),
                }
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
        let annotated = self.annotation_hit(e.position);
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
            })
            .or_else(|| {
                let row = annotated?;
                let entry = decor.annotation(row).filter(|a| !a.tooltip.is_empty())?;
                Some((
                    entry.tooltip.clone(),
                    code.top(row, self.line_height) + self.line_height - self.scroll_y.as_f32(),
                ))
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
        let ext = code.ext.as_ref()?;
        // The mark's menu covers the same place; a tip over it hides its
        // first row.
        if ext.gutter_menu.is_some() {
            return None;
        }
        let (text, top) = ext.gutter_tip.clone()?;
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
    /// The middle of a row's annotation, in window coordinates.
    #[cfg(any(test, feature = "selftest"))]
    pub(crate) fn annotation_center(&self, row: usize) -> Option<Point<Pixels>> {
        let code = self.code.as_ref()?;
        let bounds = self.bounds?;
        let width = code.decor.as_ref()?.annotation_width();
        (width > 0.).then(|| {
            point(
                bounds.left() - px(code.gutter()) + px(width / 2.),
                bounds.top() + px(code.top(row, self.line_height) + self.line_height / 2.)
                    - self.scroll_y,
            )
        })
    }
    /// What the annotation column shows on a row, its parts spaced, while
    /// there is one.
    #[cfg(test)]
    pub(crate) fn annotation_text(&self, row: usize) -> Option<String> {
        let decor = self.code.as_ref()?.decor.as_ref()?;
        decor.annotations.as_ref()?;
        Some(
            decor
                .annotation(row)
                .map_or(String::new(), |a| a.parts.join(" ")),
        )
    }
    /// The annotation column's width, 0 without one.
    #[cfg(test)]
    pub(crate) fn annotation_width(&self) -> f32 {
        (self.code.as_ref())
            .and_then(|code| code.decor.as_ref())
            .map_or(0., |decor| decor.annotation_width())
    }
    #[cfg(test)]
    pub(crate) fn gutter_tip_text(&self) -> Option<SharedString> {
        let ext = self.code.as_ref()?.ext.as_ref()?;
        ext.gutter_tip.as_ref().map(|(text, _)| text.clone())
    }
    #[cfg(any(test, feature = "selftest"))]
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
    /// A window position inside the character at `offset`, for the self-test
    /// that points at text in a real window.
    #[cfg(feature = "selftest")]
    pub(crate) fn glyph_center(&self, offset: usize) -> Option<Point<Pixels>> {
        let origin = self.bounds?.origin;
        let at = self.code_position(offset) - point(self.scroll_x, self.scroll_y);
        Some(origin + at + point(px(3.), px(self.line_height / 2.)))
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

/// Draws one row's annotation: its tint across the column and its parts
/// on the row's first visual line. `bounds` is the row from the gutter's
/// left edge, as `paint_row` has it.
pub(super) fn paint_annotation(
    decor: &Decor,
    row: usize,
    bounds: Bounds<Pixels>,
    lh: Pixels,
    w: &mut Window,
    cx: &mut App,
) {
    let Some(column) = &decor.annotations else {
        return;
    };
    let Some(entry) = column.annotations.entry(row) else {
        return;
    };
    if entry.tint > 0. {
        w.paint_quad(fill(
            Bounds::new(bounds.origin, size(px(column.width()), bounds.size.height)),
            crate::theme::accent_color().opacity(ANNOTATION_TINT * entry.tint.min(1.)),
        ));
    }
    for (part, left) in entry.parts.iter().zip(column.part_lefts()) {
        if part.is_empty() {
            continue;
        }
        let line = w.text_system().shape_line(
            part.clone(),
            px(ANNOTATION_TEXT),
            &[TextRun {
                len: part.len(),
                font: annotation_font(),
                color: crate::theme::content(0.55),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        let _ = line.paint(
            bounds.origin + point(px(left), px(0.)),
            lh,
            TextAlign::Left,
            None,
            w,
            cx,
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_editor::{
        AnnotationProvider, DocumentInfo, GutterProvider, LineAnnotation, Providers,
    };
    use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowBounds, WindowOptions};
    use std::cell::RefCell;

    #[derive(Default)]
    struct Presses {
        annotations: RefCell<Vec<(usize, Option<String>, GutterButton)>>,
        gutter: RefCell<Vec<(GutterSlot, usize)>>,
    }
    impl AnnotationProvider for Presses {
        fn click(&self, _: &DocumentInfo, click: &AnnotationClick, _: &mut Window, _: &mut App) {
            self.annotations.borrow_mut().push((
                click.row,
                click.entry.as_ref().map(|entry| entry.id.to_string()),
                click.button,
            ));
        }
    }
    impl GutterProvider for Presses {
        fn click(&self, _: &DocumentInfo, click: &GutterClick, _: &mut Window, _: &mut App) {
            self.gutter.borrow_mut().push((click.slot, click.row));
        }
    }

    fn entry(id: &str, parts: [&str; 2], tint: f32) -> Annotation {
        Annotation {
            id: id.to_owned().into(),
            parts: parts.map(|part| part.to_owned().into()).to_vec(),
            tooltip: format!("Commit {id}").into(),
            tint,
        }
    }

    #[gpui::test]
    fn the_annotation_column_comes_first_and_the_gutter_still_works_beside_it(
        cx: &mut TestAppContext,
    ) {
        let window = cx.update(|cx| {
            bind(cx);
            crate::popover::bind(cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(500.), px(300.)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(Input::editor),
            )
            .unwrap()
        });
        let source = "function hello() {\n  return 1;\n}\nlet a = 1;\nlet b = 2;\n";
        let presses = Rc::new(Presses::default());
        let plain = window
            .update(cx, |i, _, cx| {
                i.set_text(source.into(), cx);
                i.set_editor_analysis(
                    crate::editor_analysis::analyze("a.js", source, None).unwrap(),
                    cx,
                );
                i.set_editor_providers(
                    Providers {
                        breakpoints: Some(presses.clone()),
                        ..Default::default()
                    },
                    cx,
                );
                let plain = i.code.as_ref().unwrap().gutter();
                i.set_annotations(
                    Some(Annotations {
                        source: "test".into(),
                        entries: Rc::new(vec![
                            entry("a", ["24.02.2026", "Kartal"], 1.),
                            entry("b", ["1.1.2020", "Bob"], 0.),
                        ]),
                        // Row 2 changed since: blank.
                        rows: [(0, 0), (1, 0), (3, 1), (4, 1)]
                            .map(|(row, entry)| LineAnnotation { row, entry })
                            .to_vec(),
                        provider: Some(presses.clone()),
                    }),
                    cx,
                );
                plain
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let (blank, second, breakpoint, fold) = window
            .update(visual, |i, _, _| {
                let code = i.code.as_ref().unwrap();
                let decor = code.decor.as_ref().unwrap();
                let width = decor.annotation_width();
                assert!(width > 50., "as wide as a date and a name: {width}");
                assert_eq!(code.gutter(), plain + width);
                assert_eq!(decor.slot_left(GutterSlot::Breakpoint), Some(width + 4.));
                assert_eq!(i.annotation_text(0).as_deref(), Some("24.02.2026 Kartal"));
                assert_eq!(i.annotation_text(2).as_deref(), Some(""));
                let row0 = i.annotation_center(0).unwrap();
                (
                    i.annotation_center(2).unwrap(),
                    i.annotation_center(3).unwrap(),
                    i.gutter_slot_center(GutterSlot::Breakpoint, 3).unwrap(),
                    point(i.bounds.unwrap().left() - px(7.), row0.y),
                )
            })
            .unwrap();
        visual.simulate_click(second, Modifiers::default());
        visual.simulate_click(blank, Modifiers::default());
        visual.simulate_click(breakpoint, Modifiers::default());
        visual.simulate_mouse_move(second, None, Modifiers::default());
        visual.run_until_parked();
        window
            .update(visual, |i, _, _| {
                assert_eq!(i.gutter_tip_text().as_deref(), Some("Commit b"));
            })
            .unwrap();
        visual.simulate_click(fold, Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            *presses.annotations.borrow(),
            [
                (3, Some("b".into()), GutterButton::Left),
                (2, None, GutterButton::Left),
            ]
        );
        assert_eq!(*presses.gutter.borrow(), [(GutterSlot::Breakpoint, 3)]);
        // Folded, row 3 moves up to the second visual line and keeps its entry.
        let moved = window
            .update(visual, |i, _, _| {
                let code = i.code.as_ref().unwrap();
                assert!(code.fold_at(0).is_some(), "the arrow folded the function");
                assert!(code.hidden(1));
                i.annotation_center(3).unwrap()
            })
            .unwrap();
        assert!(moved.y < second.y);
        visual.simulate_click(moved, Modifiers::default());
        visual.run_until_parked();
        assert_eq!(presses.annotations.borrow().len(), 3);
        assert_eq!(presses.annotations.borrow()[2].0, 3);
        window
            .update(visual, |i, _, cx| {
                i.set_annotations(None, cx);
                assert_eq!(i.code.as_ref().unwrap().gutter(), plain);
                assert_eq!(i.annotation_text(0), None);
            })
            .unwrap();
    }
}
