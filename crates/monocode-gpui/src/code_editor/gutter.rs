//! What outside code can draw beside and behind the editor's lines: an
//! annotation column at the gutter's left edge (blame), marks in two gutter
//! columns left of the line number (breakpoint, then run marker), line
//! highlights with an optional arrow, and short notes after a line's end.
//! All of it is keyed by zero-based row and follows the rows through edits.
use super::{DocumentInfo, ExternalDiagnostic};
use crate::editor_analysis::Edit;
use gpui::{App, Pixels, Point, SharedString, Window};
use std::ops::Range;

/// Width one gutter column takes once it is in use.
pub(crate) const SLOT_WIDTH: f32 = 14.;
/// Space around and between the parts of an annotation.
pub(crate) const ANNOTATION_PAD: f32 = 6.;
/// The annotation column's text size.
pub(crate) const ANNOTATION_TEXT: f32 = 11.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GutterSlot {
    Breakpoint,
    Run,
}
impl GutterSlot {
    pub(crate) const ALL: [Self; 2] = [Self::Breakpoint, Self::Run];
    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Breakpoint {
    Enabled,
    Disabled,
    Conditional,
    /// Set, but the debugger has not bound it to code yet.
    Unverified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GutterMarkKind {
    Breakpoint(Breakpoint),
    Run,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GutterMark {
    pub row: usize,
    pub kind: GutterMarkKind,
    pub tooltip: SharedString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GutterButton {
    Left,
    Right,
}

/// A press in a gutter column. `mark` is what was drawn on that row, so a
/// left press with `None` in the breakpoint column asks to toggle one there.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GutterClick {
    pub slot: GutterSlot,
    pub row: usize,
    pub mark: Option<GutterMarkKind>,
    pub button: GutterButton,
    /// Window position of the press, to anchor a menu.
    pub position: Point<Pixels>,
}

/// What a gutter menu does with the id of the row that was picked.
pub(crate) type GutterPick = std::rc::Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// A menu a gutter press opens at the pointer, drawn by the editor that was
/// pressed.
pub(crate) struct GutterMenu {
    pub label: SharedString,
    pub items: Vec<crate::popover::MenuItem>,
    pub width: f32,
    pub on_pick: GutterPick,
}

/// Owns one gutter column of the editors it is installed on. The marks
/// themselves are pushed through `Hub::set_gutter_marks`.
pub(crate) trait GutterProvider {
    fn click(&self, doc: &DocumentInfo, click: &GutterClick, window: &mut Window, cx: &mut App);
    /// Asked before `click`: a menu to open for this press in its place.
    fn menu(
        &self,
        _doc: &DocumentInfo,
        _click: &GutterClick,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<GutterMenu> {
        None
    }
    /// An edit moved the column's marks to these rows.
    fn moved(&self, _doc: &DocumentInfo, _marks: &[GutterMark], _cx: &mut App) {}
}

/// What the rows of an annotation column can show; rows point into a list
/// of these, as many lines share one commit.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Annotation {
    /// What the provider knows it by, such as a commit's sha.
    pub id: SharedString,
    /// Drawn left to right, each part in a column as wide as its widest.
    pub parts: Vec<SharedString>,
    pub tooltip: SharedString,
    /// How strongly the row is tinted, from 0 (not at all) to 1.
    pub tint: f32,
}

/// A row of the annotation column and the entry (an index into
/// [`Annotations::entries`]) it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineAnnotation {
    pub row: usize,
    pub entry: usize,
}

/// A press in the annotation column; `entry` is the row's, when it has one.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnnotationClick {
    pub row: usize,
    pub entry: Option<Annotation>,
    pub button: GutterButton,
    pub position: Point<Pixels>,
    /// The editor that was pressed, for a provider that answers only it.
    pub editor: gpui::EntityId,
}

/// Owns an annotation column: presses in it come here.
pub(crate) trait AnnotationProvider {
    fn click(&self, doc: &DocumentInfo, click: &AnnotationClick, window: &mut Window, cx: &mut App);
    /// Asked before `click`: a menu to open for this press in its place.
    fn menu(
        &self,
        _doc: &DocumentInfo,
        _click: &AnnotationClick,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<GutterMenu> {
        None
    }
}

/// One file's annotation column, pushed through `Hub::set_annotations`.
#[derive(Clone)]
pub(crate) struct Annotations {
    pub source: SharedString,
    pub entries: std::rc::Rc<Vec<Annotation>>,
    /// Sorted by row; a row without one is blank.
    pub rows: Vec<LineAnnotation>,
    pub provider: Option<std::rc::Rc<dyn AnnotationProvider>>,
}
impl Annotations {
    pub(crate) fn entry(&self, row: usize) -> Option<&Annotation> {
        let at = self.rows.partition_point(|line| line.row < row);
        let line = self.rows.get(at).filter(|line| line.row == row)?;
        self.entries.get(line.entry)
    }
}
impl std::fmt::Debug for Annotations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Annotations")
            .field("source", &self.source)
            .field("rows", &self.rows.len())
            .finish()
    }
}

/// The annotation column as one editor draws it: the rows and how wide
/// each part's column is, measured when they were set.
#[derive(Clone, Debug)]
pub(crate) struct AnnotationColumn {
    pub annotations: Annotations,
    pub widths: Vec<f32>,
}
impl AnnotationColumn {
    pub(crate) fn width(&self) -> f32 {
        let parts: f32 = self.widths.iter().sum();
        parts + ANNOTATION_PAD * (self.widths.len() as f32 + 1.)
    }
    /// Left edges of the parts, from the column's left.
    pub(crate) fn part_lefts(&self) -> Vec<f32> {
        let mut x = ANNOTATION_PAD;
        self.widths
            .iter()
            .map(|width| {
                let left = x;
                x += width + ANNOTATION_PAD;
                left
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineStyle {
    /// Where the debugged program is stopped.
    Execution,
    /// The line of the stack frame being inspected.
    Frame,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineDecoration {
    pub row: usize,
    pub style: LineStyle,
    /// Draws an arrow in the breakpoint column.
    pub arrow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoteTone {
    Dim,
    Ok,
    Error,
}

/// Dimmed text after the end of a line, such as a variable's value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InlineNote {
    pub row: usize,
    pub text: SharedString,
    pub tone: NoteTone,
}

/// How rows move across one edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowShift {
    /// Rows before this one stay.
    keep_below: usize,
    /// Rows from this one on move by the change in line count; the rows
    /// between were replaced and land on the nearest line that is left.
    shift_from: usize,
    old_last: usize,
    new_last: usize,
    /// The row the edit starts in, when it changed that row's own text.
    first_changed: Option<usize>,
}
impl RowShift {
    /// `old` and `new` are the line start offsets before and after `edit`.
    pub(crate) fn new(edit: Edit, old: &[usize], new: &[usize], new_text: &str) -> Self {
        let row = |offsets: &[usize], at: usize| {
            offsets
                .partition_point(|start| *start <= at)
                .saturating_sub(1)
        };
        let first = row(old, edit.start);
        // Lines typed above the start of a line push that line down with them.
        let pushes = edit.old_end == edit.start
            && old.get(first) == Some(&edit.start)
            && new_text.as_bytes().get(edit.new_end.wrapping_sub(1)) == Some(&b'\n');
        let old_last = row(old, edit.old_end);
        // Starting at a line's end only adds after it.
        let at_end = old
            .get(first + 1)
            .is_some_and(|next| *next == edit.start + 1);
        Self {
            keep_below: if pushes { first } else { first + 1 },
            shift_from: if pushes { first } else { old_last + 1 },
            old_last,
            new_last: row(new, edit.new_end),
            first_changed: (!pushes && !at_end).then_some(first),
        }
    }
    pub(crate) fn map(&self, row: usize) -> usize {
        if row < self.keep_below {
            row
        } else if row >= self.shift_from {
            row - self.old_last + self.new_last
        } else {
            row.min(self.new_last)
        }
    }
    fn unchanged(&self) -> bool {
        self.old_last == self.new_last
    }
    /// Whether the edit replaced this row rather than keeping or moving it.
    fn replaces(&self, row: usize) -> bool {
        (row >= self.keep_below && row < self.shift_from) || self.first_changed == Some(row)
    }
}

/// The outside state one editor paints. It sits behind an `Rc` in the editor
/// so a frame takes a reference instead of copying the lists.
#[derive(Clone, Default)]
pub(crate) struct Decor {
    /// Sorted by start.
    pub diagnostics: Vec<ExternalDiagnostic>,
    /// Per slot, sorted by row.
    pub marks: [Vec<GutterMark>; 2],
    /// Whether a provider owns the column, which gives it width when empty.
    pub slots: [bool; 2],
    pub lines: Vec<LineDecoration>,
    pub notes: Vec<InlineNote>,
    /// The identifier underlined as a link while the modifier is held.
    pub link: Option<Range<usize>>,
    /// Where a breakpoint would go: the hovered row of its empty column.
    pub hover: Option<usize>,
    /// The annotation column left of everything else, while one is set.
    pub annotations: Option<AnnotationColumn>,
}
impl Decor {
    /// Width of the annotation column, 0 without one.
    pub(crate) fn annotation_width(&self) -> f32 {
        self.annotations
            .as_ref()
            .map_or(0., AnnotationColumn::width)
    }
    pub(crate) fn annotation(&self, row: usize) -> Option<&Annotation> {
        self.annotations.as_ref()?.annotations.entry(row)
    }
    pub(crate) fn slot_visible(&self, slot: GutterSlot) -> bool {
        let n = slot.index();
        self.slots[n]
            || !self.marks[n].is_empty()
            || (slot == GutterSlot::Breakpoint && self.lines.iter().any(|line| line.arrow))
    }
    /// Left edge of a column inside the gutter, when it is shown.
    pub(crate) fn slot_left(&self, slot: GutterSlot) -> Option<f32> {
        self.slot_visible(slot).then(|| {
            self.annotation_width()
                + 4.
                + GutterSlot::ALL
                    .iter()
                    .take(slot.index())
                    .filter(|before| self.slot_visible(**before))
                    .count() as f32
                    * SLOT_WIDTH
        })
    }
    pub(crate) fn width(&self) -> f32 {
        self.annotation_width()
            + GutterSlot::ALL
                .iter()
                .filter(|slot| self.slot_visible(**slot))
                .count() as f32
                * SLOT_WIDTH
    }
    pub(crate) fn mark(&self, slot: GutterSlot, row: usize) -> Option<&GutterMark> {
        let marks = &self.marks[slot.index()];
        let at = marks.partition_point(|mark| mark.row < row);
        marks.get(at).filter(|mark| mark.row == row)
    }
    pub(crate) fn line(&self, row: usize) -> Option<&LineDecoration> {
        let at = self.lines.partition_point(|line| line.row < row);
        self.lines.get(at).filter(|line| line.row == row)
    }
    pub(crate) fn notes_on(&self, row: usize) -> &[InlineNote] {
        let from = self.notes.partition_point(|note| note.row < row);
        let to = self.notes.partition_point(|note| note.row <= row);
        &self.notes[from..to]
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
            && self.marks.iter().all(Vec::is_empty)
            && !self.slots.iter().any(|on| *on)
            && self.lines.is_empty()
            && self.notes.is_empty()
            && self.link.is_none()
            && self.annotations.is_none()
    }
    /// Carries everything through an edit. Returns which gutter columns had a
    /// mark change rows, so their providers can be told.
    pub(crate) fn remap(&mut self, edit: Edit, rows: RowShift) -> [bool; 2] {
        super::diagnostics::remap(&mut self.diagnostics, edit);
        self.link = None;
        self.hover = None;
        let mut moved = [false; 2];
        if rows.unchanged() {
            return moved;
        }
        for (marks, moved) in self.marks.iter_mut().zip(&mut moved) {
            for mark in marks.iter_mut() {
                let row = rows.map(mark.row);
                *moved |= row != mark.row;
                mark.row = row;
            }
            // Lines that were joined leave one mark, the first.
            marks.dedup_by_key(|mark| mark.row);
        }
        for line in &mut self.lines {
            line.row = rows.map(line.row);
        }
        for note in &mut self.notes {
            note.row = rows.map(note.row);
        }
        if let Some(column) = &mut self.annotations {
            // Replaced lines are not the committed ones any more.
            let lines = &mut column.annotations.rows;
            lines.retain(|line| !rows.replaces(line.row));
            for line in lines.iter_mut() {
                line.row = rows.map(line.row);
            }
            lines.dedup_by_key(|line| line.row);
        }
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn starts(text: &str) -> Vec<usize> {
        std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .collect()
    }
    fn shift(old: &str, new: &str) -> RowShift {
        RowShift::new(
            Edit::between(old, new).unwrap(),
            &starts(old),
            &starts(new),
            new,
        )
    }

    #[test]
    fn rows_follow_their_lines_through_inserts_deletes_and_joins() {
        let old = "a\nb\nc\nd\n";
        // A line typed above `b` pushes it and everything after down.
        let rows = shift(old, "a\nnew\nb\nc\nd\n");
        assert_eq!([0, 1, 2, 3].map(|row| rows.map(row)), [0, 2, 3, 4]);
        // Typing inside a line moves nothing.
        assert!(shift(old, "a\nbb\nc\nd\n").unchanged());
        // Deleting `b` pulls the rest up; its own mark stays on that row.
        let rows = shift(old, "a\nc\nd\n");
        assert_eq!([0, 2, 3].map(|row| rows.map(row)), [0, 1, 2]);
        // Joining `b` and `c` lands both marks on the joined line.
        let rows = shift(old, "a\nbc\nd\n");
        assert_eq!([1, 2, 3].map(|row| rows.map(row)), [1, 1, 2]);
        // Replacing two lines by four keeps what follows attached.
        let rows = shift(old, "a\n1\n2\n3\n4\nd\n");
        assert_eq!(rows.map(3), 5);
    }

    #[test]
    fn columns_take_width_only_when_owned_or_marked() {
        let mut decor = Decor::default();
        assert_eq!(decor.width(), 0.);
        assert_eq!(decor.slot_left(GutterSlot::Run), None);
        decor.marks[1].push(GutterMark {
            row: 3,
            kind: GutterMarkKind::Run,
            tooltip: "Run test".into(),
        });
        assert_eq!(decor.width(), SLOT_WIDTH);
        assert_eq!(decor.slot_left(GutterSlot::Run), Some(4.));
        decor.slots[0] = true;
        assert_eq!(decor.slot_left(GutterSlot::Run), Some(4. + SLOT_WIDTH));
        assert!(decor.mark(GutterSlot::Run, 3).is_some());
        assert!(decor.mark(GutterSlot::Run, 2).is_none());
        // An annotation column comes first and pushes the slots right.
        decor.annotations = Some(column(&[(0, 0), (2, 1)]));
        assert_eq!(decor.annotation_width(), 30. + 3. * ANNOTATION_PAD);
        assert_eq!(
            decor.slot_left(GutterSlot::Breakpoint),
            Some(decor.annotation_width() + 4.)
        );
        assert_eq!(decor.width(), decor.annotation_width() + 2. * SLOT_WIDTH);
        assert_eq!(decor.annotation(2).map(|a| a.id.as_ref()), Some("b"));
        assert!(decor.annotation(1).is_none());
    }

    fn column(rows: &[(usize, usize)]) -> AnnotationColumn {
        let entry = |id: &str| Annotation {
            id: id.to_owned().into(),
            parts: vec!["24.02.2026".into(), "Ann".into()],
            tooltip: id.to_owned().into(),
            tint: 1.,
        };
        AnnotationColumn {
            annotations: Annotations {
                source: "test".into(),
                entries: std::rc::Rc::new(vec![entry("a"), entry("b")]),
                rows: rows
                    .iter()
                    .map(|(row, entry)| LineAnnotation {
                        row: *row,
                        entry: *entry,
                    })
                    .collect(),
                provider: None,
            },
            widths: vec![20., 10.],
        }
    }

    #[test]
    fn annotations_follow_moved_lines_and_leave_replaced_ones_blank() {
        let old = "a\nb\nc\nd\n";
        let mut decor = Decor {
            annotations: Some(column(&[(0, 0), (1, 0), (2, 1), (3, 1)])),
            ..Default::default()
        };
        let rows = |decor: &Decor| -> Vec<usize> {
            let column = decor.annotations.as_ref().unwrap();
            column
                .annotations
                .rows
                .iter()
                .map(|line| line.row)
                .collect()
        };
        let new = "a\nnew\nb\nc\nd\n";
        decor.remap(Edit::between(old, new).unwrap(), shift(old, new));
        assert_eq!(rows(&decor), [0, 2, 3, 4], "a typed line has none");
        // Replacing `b` and `c` by one line blanks it; `d` moves up.
        let newer = "a\nnew\nX\nd\n";
        decor.remap(Edit::between(new, newer).unwrap(), shift(new, newer));
        assert_eq!(rows(&decor), [0, 3]);
    }
}
