//! `[Image #N]` markers in a composer with attached images. The text keeps the
//! marker, which is what the agent reads; the field draws it as one pill the
//! caret steps over and Backspace removes whole. The owner of the attachments
//! tells the field how many images there are, inserts markers for new ones and
//! renumbers them when one is removed.
use super::*;
use monocode_core::composer::image_markers;

impl Input {
    /// How many images the draft has attached. Markers naming one of them
    /// are drawn as pills; any other `[Image #N]` stays plain text.
    pub(crate) fn set_image_count(&mut self, count: usize, cx: &mut Context<Self>) {
        if self.image_count != count {
            self.image_count = count;
            cx.notify();
        }
    }

    /// Markers for newly attached images, at the caret with a space on
    /// either side. Not over a selection or an IME composition: those stay.
    pub(crate) fn insert_image_markers(&mut self, numbers: &[usize], cx: &mut Context<Self>) {
        if numbers.is_empty() || self.read_only || self.code.is_some() || self.secret {
            return;
        }
        let at = if self.is_composing() {
            self.content.len()
        } else {
            self.clip_offset(self.cursor())
        };
        let at = self.outside_image(at, true);
        let text = image_markers::insertion(&self.content[..at], &self.content[at..], numbers);
        let text = bounded(&text, MAX_BYTES.saturating_sub(self.content.len()));
        if text.is_empty() {
            return;
        }
        let end = at + text.len();
        // Attaching is a step of its own: undo takes the markers back out.
        self.record_undo(super::history::EditKind::Other, &(at..at), end, cx);
        self.content = format!("{}{text}{}", &self.content[..at], &self.content[at..]).into();
        if let Some(marked) = &self.marked
            && marked.start >= at
        {
            self.marked = Some(marked.start + text.len()..marked.end + text.len());
        }
        if !self.is_composing() {
            self.selection = end..end;
            self.reversed = false;
        }
        self.manual_scroll = false;
        self.reveal_pending = self.reveal_in_parent;
        cx.emit(Changed);
        cx.notify();
    }

    /// Image `removed` of `count` left the draft: drop its markers and shift
    /// later images' markers down, keeping the caret where it was.
    pub(crate) fn remove_image_markers(
        &mut self,
        removed: usize,
        count: usize,
        cx: &mut Context<Self>,
    ) {
        if self.code.is_some() {
            return;
        }
        let edits = image_markers::removal_edits(&self.content, removed, count);
        if edits.is_empty() {
            return;
        }
        let mut offsets = [self.selection.start, self.selection.end];
        let text = image_markers::apply(&self.content, &edits, &mut offsets);
        // A composition spanning a rewritten marker cannot be carried over.
        self.marked = None;
        // Earlier steps hold the old numbering, which would bring back
        // markers for an image that is gone or now has another number.
        self.history.clear();
        self.content = text.into();
        self.lines.clear();
        self.selection = self.clip_range(offsets[0]..offsets[1]);
        cx.emit(Changed);
        cx.notify();
    }

    /// The marker pill whose inside `at` falls in.
    pub(super) fn image_around(&self, at: usize) -> Option<Range<usize>> {
        self.image_markers()
            .into_iter()
            .map(|(range, _)| range)
            .find(|range| range.start < at && at < range.end)
    }

    /// `at`, or the edge of the pill it falls inside: after it going forward,
    /// before it going back.
    pub(super) fn outside_image(&self, at: usize, forward: bool) -> usize {
        match self.image_around(at) {
            Some(range) if forward => range.end,
            Some(range) => range.start,
            None => at,
        }
    }

    /// One caret step back over a whole pill, if one ends at `at`.
    pub(super) fn image_step_back(&self, at: usize) -> Option<usize> {
        self.image_markers()
            .into_iter()
            .find(|(range, _)| range.end == at || (range.start < at && at < range.end))
            .map(|(range, _)| range.start)
    }

    /// One caret step forward over a whole pill, if one starts at `at`.
    pub(super) fn image_step_forward(&self, at: usize) -> Option<usize> {
        self.image_markers()
            .into_iter()
            .find(|(range, _)| range.start == at || (range.start < at && at < range.end))
            .map(|(range, _)| range.end)
    }
}

/// One shaped line where it is painted; `base` is its display offset.
pub(super) struct PaintedLine<'a> {
    pub line: &'a WrappedLine,
    pub base: usize,
    pub origin: Point<Pixels>,
    pub line_height: Pixels,
}

/// Paint the pills on one line: before its text, a rounded chip behind each
/// label; after it, the image icon in each icon slot.
pub(super) fn paint_pills(
    layout: &DisplayLayout,
    at: &PaintedLine,
    before_text: bool,
    w: &mut Window,
    cx: &mut App,
) {
    let PaintedLine {
        line,
        base,
        origin,
        line_height,
    } = *at;
    for (pill, icon) in layout.image_pills() {
        if pill.start < base || pill.end > base + line.len() {
            continue;
        }
        let (Some(a), Some(b)) = (
            line.position_for_index(pill.start - base, line_height),
            line.position_for_index(pill.end - base, line_height),
        ) else {
            continue;
        };
        if a.y != b.y {
            continue;
        }
        if before_text {
            let inset = px(1.);
            w.paint_quad(
                fill(
                    Bounds::new(
                        point(origin.x + a.x, origin.y + a.y + inset),
                        size(b.x - a.x, line_height - inset * 2.),
                    ),
                    crate::theme::content(0.08),
                )
                .corner_radii(px(6.))
                .border_widths(px(1.))
                .border_color(crate::theme::content(0.1)),
            );
            continue;
        }
        let (Some(icon_a), Some(icon_b)) = (
            line.position_for_index(icon.start - base, line_height),
            line.position_for_index(icon.end - base, line_height),
        ) else {
            continue;
        };
        let side = px(mention::PILL_ICON_SIDE);
        let gap = px(3.);
        let x = (icon_a.x + icon_b.x - gap) / 2. - side / 2.;
        let _ = w.paint_svg(
            Bounds::new(
                point(origin.x + x, origin.y + a.y + (line_height - side) / 2.),
                size(side, side),
            ),
            "image-01".into(),
            None,
            Default::default(),
            crate::theme::content(0.6),
            cx,
        );
    }
}
