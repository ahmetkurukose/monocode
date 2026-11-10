//! EditorSelectionMenu + editorSelection.ts: share a compact file/line reference.
use super::{FileEditor, FileEvent};
use crate::{
    input::Input,
    popover::{Align, Anchor, AnchorSlot, Side, popover},
    theme::*,
};
use gpui::{
    AnyElement, Bounds, Context, Focusable, MouseButton, Pixels, Role, Size, Window, div,
    prelude::*, px,
};
use std::{ops::Range, path::Path};

#[derive(Clone)]
struct Target {
    range: Range<usize>,
    anchor: Bounds<Pixels>,
    reference: String,
    /// The data sources text can be sent to, the last used first.
    sources: Vec<crate::database::Target>,
    /// The first and last line selected, 1-based.
    lines: (usize, usize),
}
#[derive(Default)]
pub(super) struct Selection {
    range: Range<usize>,
    target: Option<Target>,
    pending: bool,
    scheduled: bool,
    viewport: Option<Size<Pixels>>,
    surface: AnchorSlot,
    /// The list of data sources is open under the buttons.
    choosing: bool,
}
impl Selection {
    pub(super) fn dismiss(&mut self) {
        self.target = None;
        self.pending = false;
        self.choosing = false;
    }
    fn observe(&mut self, input: &Input) {
        let range = input.selection_range();
        if self.range != range {
            self.range = range;
            self.target = None;
            self.pending = !self.range.is_empty();
        } else if self
            .target
            .as_ref()
            .is_some_and(|target| Some(target.anchor) != input.selection_anchor())
        {
            // Scrolling invalidates the captured viewport coordinates.
            self.dismiss();
        }
    }
}

/// The colour a data source carries in lists.
fn db_dot(color: Option<monocode_core::db::DbColor>) -> gpui::Hsla {
    use monocode_core::db::DbColor::*;
    let hex = match color {
        Some(Red) => 0xf87171,
        Some(Orange) => 0xfb923c,
        Some(Yellow) => 0xfacc15,
        Some(Green) => 0x4ade80,
        Some(Blue) => 0x60a5fa,
        Some(Purple) => 0xc084fc,
        Some(Gray) | None => 0x9ca3af,
    };
    gpui::rgb(hex).into()
}
fn format_reference(path: &str, start: usize, end: usize) -> String {
    let path = path.replace('\\', "/");
    let mentionable = !path.is_empty()
        && path.encode_utf16().count() <= 120
        && !path.starts_with('/')
        && !path.chars().any(|c| {
            c.is_whitespace()
                || c == '\u{feff}'
                || c == '@'
                || c <= '\u{1f}'
                || ('\u{7f}'..='\u{9f}').contains(&c)
        })
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    let file = if mentionable {
        format!("@{path}")
    } else {
        format!("`{}`", path.replace(['`', '\r', '\n'], "'"))
    };
    if start == end {
        format!("{file} (line {start})")
    } else {
        format!("{file} (lines {start}-{end})")
    }
}
fn reference_for_selection(path: &str, text: &str, range: Range<usize>) -> Option<String> {
    if range.is_empty()
        || range.end > text.len()
        || !text.is_char_boundary(range.start)
        || !text.is_char_boundary(range.end)
        || text[range.clone()].trim().is_empty()
    {
        return None;
    }
    let start = text.as_bytes()[..range.start]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1;
    // Count bytes rather than slicing at end-1, which can split a Unicode scalar.
    // Ending at the next line's column one does not include that next line.
    let end = text.as_bytes()[..range.end - 1]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1;
    Some(format_reference(path, start, end))
}

/// The first and last line (1-based) a selection touches; the last stops at
/// the line holding its final character, not the one after a trailing newline.
fn line_span(text: &str, range: Range<usize>) -> (usize, usize) {
    let newlines = |upto: usize| {
        text.as_bytes()[..upto]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
    };
    let first = newlines(range.start) + 1;
    let last = if range.end > range.start {
        newlines(range.end - 1) + 1
    } else {
        first
    };
    (first, last.max(first))
}

impl FileEditor {
    pub(super) fn observe_code_selection(&mut self, cx: &mut Context<Self>) {
        self.selection.observe(self.input.read(cx));
    }
    pub(super) fn finish_code_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input.focus_handle(cx).is_focused(window) {
            return;
        }
        self.selection.observe(self.input.read(cx));
        // Existing pending work came from an actual range change. An outside
        // dismissal must not reopen on the following mouse-up.
        cx.notify();
    }
    fn capture_code_selection(&mut self, cx: &mut Context<Self>) {
        self.selection.scheduled = false;
        if !self.selection.pending {
            return;
        }
        self.selection.pending = false;
        if !self.visible || self.preview || self.document.is_none() {
            return;
        }
        let input = self.input.read(cx);
        if input.selection_dragging() {
            return;
        }
        let range = input.selection_range();
        let Some(anchor) = input.selection_anchor() else {
            return;
        };
        let path = Path::new(&self.path)
            .strip_prefix(&self.cwd)
            .unwrap_or_else(|_| Path::new(&self.path))
            .to_string_lossy();
        let Some(reference) = reference_for_selection(&path, &input.content, range.clone()) else {
            return;
        };
        let lines = line_span(&input.content, range.clone());
        self.selection.target = Some(Target {
            range,
            anchor,
            reference,
            sources: crate::database::send_targets(&self.database_dir),
            lines,
        });
        cx.notify();
    }
    pub(super) fn code_selection_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let viewport = window.viewport_size();
        if self
            .selection
            .viewport
            .is_some_and(|previous| previous != viewport)
        {
            self.selection.dismiss();
        }
        self.selection.viewport = Some(viewport);
        if !self.visible || self.preview || self.document.is_none() {
            return None;
        }
        if self.selection.pending
            && !self.selection.scheduled
            && !self.input.read(cx).selection_dragging()
        {
            // Layout first, then capture the caret after any scroll-to-selection.
            self.selection.scheduled = true;
            let view = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = view.update(cx, |this, cx| this.capture_code_selection(cx));
            });
        }
        let target = self.selection.target.clone()?;
        let choosing = self.selection.choosing;
        let anchor = AnchorSlot::default();
        anchor.set(Some(target.anchor));
        let weak = cx.entity().downgrade();
        Some(
            popover("editor-selection-menu", Anchor::Slot(anchor))
                .side(Side::Top)
                .align(Align::Center)
                .gap(6.)
                .child(
                    div()
                        .id("editor-selection-actions")
                        .role(Role::Toolbar)
                        .aria_label("Selected code actions")
                        .p_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .id("editor-selection-chat")
                                .role(Role::Button)
                                .aria_label("Add to chat")
                                .tab_index(0)
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .h(px(28.))
                                .px_2()
                                .rounded(px(8.))
                                .text_size(px(13.))
                                .hover(|s| s.bg(content(0.05)))
                                .cursor_pointer()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(icon("comment-add", 14.))
                                .child("Add to chat")
                                .on_click(cx.listener({
                                    let target = target.clone();
                                    move |this, _, _, cx| {
                                        // The menu never mutates the file buffer or copies its contents.
                                        if this.input.read(cx).selection_range() == target.range {
                                            cx.emit(FileEvent::AddToChat(target.reference.clone()));
                                        }
                                        this.selection.dismiss();
                                        cx.notify();
                                    }
                                })),
                        )
                        .when(!target.sources.is_empty(), |d| {
                            d.child(
                                div()
                                    .id("editor-selection-database")
                                    .debug_selector(|| "editor-selection-database".into())
                                    .role(Role::Button)
                                    .aria_label("Send to Database")
                                    .aria_expanded(choosing)
                                    .tab_index(0)
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .h(px(28.))
                                    .px_2()
                                    .rounded(px(8.))
                                    .text_size(px(13.))
                                    .hover(|s| s.bg(content(0.05)))
                                    .cursor_pointer()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_mouse_up(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child(icon("database", 14.))
                                    .child(div().flex_1().child("Send to Database"))
                                    .child(icon(
                                        if choosing {
                                            "chevron-up"
                                        } else {
                                            "chevron-down"
                                        },
                                        11.,
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.selection.choosing = !this.selection.choosing;
                                        cx.notify();
                                    })),
                            )
                        })
                        .when(choosing, |d| {
                            d.children(target.sources.iter().enumerate().map(|(ix, source)| {
                                let id = source.id.clone();
                                let last = ix == 0;
                                let text_target = target.clone();
                                div()
                                    .id(("editor-send-source", ix))
                                    .debug_selector(move || format!("editor-send-source-{ix}"))
                                    .role(Role::Button)
                                    .aria_label(format!("Send to {}", source.name))
                                    .h(px(28.))
                                    .pl(px(26.))
                                    .pr_2()
                                    .rounded(px(8.))
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(px(13.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(content(0.05)))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_mouse_up(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child(
                                        div()
                                            .size(px(8.))
                                            .flex_shrink_0()
                                            .rounded_full()
                                            .bg(db_dot(source.color)),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(source.name.clone()),
                                    )
                                    .when(!source.read_only, |d| {
                                        d.child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(content(0.4))
                                                .child("writable"),
                                        )
                                    })
                                    .when(last, |d| {
                                        d.child(icon("check", 12.).text_color(content(0.5)))
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let content = this.input.read(cx).content.clone();
                                        let range = text_target.range.clone();
                                        if this.input.read(cx).selection_range() == range
                                            && content.is_char_boundary(range.start)
                                            && content.is_char_boundary(range.end)
                                        {
                                            cx.emit(FileEvent::SendToDatabase {
                                                source: id.clone(),
                                                text: content[range].to_string(),
                                                first_line: text_target.lines.0,
                                                last_line: text_target.lines.1,
                                            });
                                        }
                                        this.selection.dismiss();
                                        cx.notify();
                                    }))
                            }))
                        }),
                )
                .on_dismiss(move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.selection.dismiss();
                        cx.notify();
                    });
                })
                .into_any_element(),
        )
    }
    pub(super) fn selection_surface(&self, cx: &Context<Self>) -> impl gpui::IntoElement + use<> {
        let slot = self.selection.surface.clone();
        let weak = cx.entity().downgrade();
        gpui::canvas(
            move |bounds, _, cx| {
                if slot.get().is_some_and(|old| old != bounds) {
                    let _ = weak.update(cx, |this, cx| {
                        let was_open = this.selection.target.is_some();
                        this.selection.dismiss();
                        if was_open {
                            cx.notify();
                        }
                    });
                }
                slot.set(Some(bounds));
            },
            |_, _, _, _| {},
        )
        // Pinned to the corner, not left inside the parent's padding.
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn references_match_editor_selection_ts_for_mentions_and_quoted_paths() {
        assert_eq!(
            format_reference("src/FileEditor.tsx", 12, 13),
            "@src/FileEditor.tsx (lines 12-13)"
        );
        assert_eq!(
            format_reference("src/value.ts", 7, 7),
            "@src/value.ts (line 7)"
        );
        assert_eq!(
            format_reference("docs/read me.md", 3, 5),
            "`docs/read me.md` (lines 3-5)"
        );
        assert_eq!(
            format_reference("src\\value.ts", 1, 1),
            "@src/value.ts (line 1)"
        );
        assert_eq!(
            format_reference("/absolute/`file`\n.rs", 1, 2),
            "`/absolute/'file''.rs` (lines 1-2)"
        );
        assert_eq!(
            format_reference("../escape.rs", 1, 1),
            "`../escape.rs` (line 1)"
        );
        assert_eq!(format_reference("a@b.rs", 1, 1), "`a@b.rs` (line 1)");
    }
    #[test]
    fn exclusive_range_end_unicode_whitespace_and_unsaved_content_are_respected() {
        let text = "first\r\n🙂 selected\nlast\n";
        let start = text.find('🙂').unwrap();
        let end = text.find("last").unwrap();
        assert_eq!(
            reference_for_selection("src/current.rs", text, start..end).unwrap(),
            "@src/current.rs (line 2)"
        );
        assert_eq!(
            reference_for_selection("src/current.rs", text, 0..end + 1).unwrap(),
            "@src/current.rs (lines 1-3)"
        );
        assert!(reference_for_selection("file.rs", text, start..start + 1).is_none());
        assert!(reference_for_selection("file.rs", " \n\t", 0..3).is_none());
        assert!(reference_for_selection("file.rs", "text", 1..1).is_none());
        assert!(reference_for_selection("file.rs", "text", 0..10).is_none());
    }

    #[test]
    fn line_spans_stop_at_the_last_character_not_the_next_line() {
        let text = "one\ntwo\nthree\n";
        assert_eq!(line_span(text, 0..3), (1, 1));
        assert_eq!(
            line_span(text, 0..8),
            (1, 2),
            "up to and including the newline"
        );
        assert_eq!(line_span(text, 4..13), (2, 3));
        assert_eq!(line_span(text, 9..9), (3, 3));
    }

    #[gpui::test]
    fn selected_text_can_be_sent_to_the_data_source_last_used(cx: &mut gpui::TestAppContext) {
        use gpui::VisualTestContext;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("seed.sql");
        std::fs::write(
            &file,
            "insert into t values (1);\ninsert into t values (2);\n",
        )
        .unwrap();
        let db = tempfile::tempdir().unwrap();
        let (a, b) = crate::database::write_sources(db.path(), &["alpha", "beta"]);
        std::fs::write(
            db.path().join("state.json"),
            serde_json::json!({ "send_target": b }).to_string(),
        )
        .unwrap();
        cx.update(|cx| cx.set_global(crate::settings::Preferences::default()));
        let path = file.to_string_lossy().into_owned();
        let database_dir = db.path().to_owned();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                let git = cx.new(|cx| {
                    crate::git_status::ProjectGit::new(
                        dir.path().to_string_lossy().into_owned(),
                        cx,
                    )
                });
                cx.new(|cx| {
                    let mut editor = super::super::FileEditor::new(path, git, cx);
                    editor.database_dir = database_dir;
                    editor
                })
            })
            .unwrap()
        });
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = sent.clone();
        window
            .update(cx, |editor, _, cx| {
                let subscription = cx.subscribe(&cx.entity(), move |_, _, event: &FileEvent, _| {
                    if let FileEvent::SendToDatabase {
                        source,
                        text,
                        first_line,
                        last_line,
                    } = event
                    {
                        log.borrow_mut().push((
                            source.clone(),
                            text.clone(),
                            *first_line,
                            *last_line,
                        ));
                    }
                });
                std::mem::forget(subscription);
                editor.set_visible(true, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |editor, window, cx| {
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(0..25, cx));
                editor.finish_code_selection(window, cx);
                editor.observe_code_selection(cx);
                // What the next frame would do once the layout is known.
                editor.capture_code_selection(cx);
                cx.notify();
            })
            .unwrap();
        for _ in 0..4 {
            cx.refresh().unwrap();
            cx.run_until_parked();
        }
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let button = visual
            .debug_bounds("editor-selection-database")
            .expect("the selection offers Send to Database");
        visual.simulate_click(button.center(), Default::default());
        visual.run_until_parked();
        // The one used last is listed first.
        let first = visual
            .debug_bounds("editor-send-source-0")
            .expect("the sources are listed");
        assert!(visual.debug_bounds("editor-send-source-1").is_some());
        visual.simulate_click(first.center(), Default::default());
        visual.run_until_parked();
        let log = sent.borrow();
        assert_eq!(log.len(), 1, "{log:?}");
        assert_eq!(log[0].0, b, "beta was used last, so it leads");
        assert_ne!(log[0].0, a);
        assert_eq!(log[0].1, "insert into t values (1);");
        assert_eq!((log[0].2, log[0].3), (1, 1));
    }
}
