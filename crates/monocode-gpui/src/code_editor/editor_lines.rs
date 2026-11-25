//! Line and selection commands of the code editor: growing and shrinking
//! the selection by syntax, duplicating and moving lines, and several
//! carets (column selection, a caret added above or below). Compiled as a
//! child of the editor so it can reach the buffer.
use super::*;

/// What Extend Selection grew from. It holds while the buffer and the
/// selection are the ones it left; anything else starts over.
pub(super) struct Expansion {
    text: SharedString,
    at: Range<usize>,
    stack: Vec<Range<usize>>,
}

/// One replacement of the buffer: what it covers and what goes there.
pub(in crate::input) type Splice = (Range<usize>, String);

/// Where `at` lands after `edits` (sorted, in the old offsets). An offset
/// inside or at the edge of a replaced range lands after its new text.
fn map_through(edits: &[Splice], at: usize) -> usize {
    let mut delta = 0isize;
    for (range, text) in edits {
        if at < range.start {
            break;
        }
        if at <= range.end {
            return (range.start as isize + delta) as usize + text.len();
        }
        delta += text.len() as isize - range.len() as isize;
    }
    (at as isize + delta) as usize
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The identifier `range` lies in, when it is inside one.
fn word_around(text: &str, range: &Range<usize>) -> Option<Range<usize>> {
    if !text[range.clone()].chars().all(is_word) {
        return None;
    }
    let start = text[..range.start]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(range.start, |(at, _)| at);
    let end = text[range.end..]
        .char_indices()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(range.end, |(at, c)| range.end + at + c.len_utf8());
    (start < end).then_some(start..end)
}

/// The unmatched opening bracket before `at`, skipping balanced pairs.
fn opener_before(text: &[u8], at: usize) -> Option<usize> {
    let mut depth = 0usize;
    for ix in (0..at).rev() {
        match text[ix] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' if depth == 0 => return Some(ix),
            b'(' | b'[' | b'{' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The bracket closing the one at `open`, searched from `from`.
fn closer_after(text: &[u8], open: usize, from: usize) -> Option<usize> {
    let close = match text[open] {
        b'(' => b')',
        b'[' => b']',
        _ => b'}',
    };
    let mut depth = 0usize;
    for (ix, byte) in text.iter().enumerate().skip(from) {
        match *byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth > 0 => depth -= 1,
            b if b == close => return Some(ix),
            b')' | b']' | b'}' => return None,
            _ => {}
        }
    }
    None
}

fn trimmed(text: &str, range: Range<usize>) -> Range<usize> {
    let body = &text[range.clone()];
    let lead = body.len() - body.trim_start().len();
    let trail = body.len() - body.trim_end().len();
    if lead == body.len() {
        return range;
    }
    range.start + lead..range.end - trail
}

/// What Extend Selection can grow to without a grammar: the line, quoted
/// text on it, and the brackets around, inside and out.
fn text_ranges(text: &str, range: &Range<usize>) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let line_start = text[..range.start].rfind('\n').map_or(0, |n| n + 1);
    let line_end = text[range.end..]
        .find('\n')
        .map_or(text.len(), |n| range.end + n);
    out.push(trimmed(text, line_start..line_end));
    out.push(line_start..line_end);
    // Quotes on the selection's line, paired left to right.
    let mut open: Option<(usize, char)> = None;
    for (at, c) in text[line_start..line_end].char_indices() {
        let at = line_start + at;
        match open {
            Some((from, quote)) if c == quote => {
                if from < range.start && at >= range.end {
                    out.push(from + 1..at);
                    out.push(from..at + 1);
                }
                open = None;
            }
            None if matches!(c, '"' | '\'' | '`') => open = Some((at, c)),
            _ => {}
        }
    }
    let bytes = text.as_bytes();
    let mut around = range.clone();
    while let Some(open) = opener_before(bytes, around.start) {
        let Some(close) = closer_after(bytes, open, around.end) else {
            break;
        };
        out.push(trimmed(text, open + 1..close));
        out.push(open + 1..close);
        out.push(open..close + 1);
        around = open..close + 1;
    }
    out
}

impl Input {
    /// Whether there are carets besides the primary one.
    pub(in crate::input) fn has_carets(&self) -> bool {
        self.code
            .as_ref()
            .is_some_and(|code| !code.carets.is_empty())
    }
    /// Drops every caret but the primary one. Returns whether there were any.
    pub(in crate::input) fn collapse_carets(&mut self) -> bool {
        let Some(code) = &mut self.code else {
            return false;
        };
        code.block = None;
        if code.carets.is_empty() {
            return false;
        }
        code.carets.clear();
        true
    }
    /// A caret move outside column mode ends the rectangle.
    pub(in crate::input) fn forget_block(&mut self) {
        if let Some(code) = &mut self.code {
            code.block = None;
        }
    }
    /// The carets besides the primary one, for the undo history.
    pub(in crate::input) fn secondary_carets(&self) -> Vec<Caret> {
        self.code
            .as_ref()
            .map_or_else(Vec::new, |code| code.carets.clone())
    }
    /// Every caret, the primary one first.
    pub(in crate::input) fn all_carets(&self) -> Vec<Caret> {
        let primary = Caret {
            range: self.selection.clone(),
            reversed: self.reversed,
        };
        std::iter::once(primary)
            .chain(
                self.code
                    .iter()
                    .flat_map(|code| code.carets.iter().cloned()),
            )
            .collect()
    }
    /// Every caret in document order.
    fn ordered_carets(&self) -> Vec<Caret> {
        let mut all = self.all_carets();
        all.sort_by_key(|caret| caret.range.start);
        all
    }
    /// Sets the carets, the first one primary. Carets that touch or
    /// overlap become one, and the primary one is what it joins.
    pub(in crate::input) fn set_carets(&mut self, carets: Vec<Caret>) {
        let mut items: Vec<(Caret, bool)> = carets
            .into_iter()
            .enumerate()
            .map(|(n, mut caret)| {
                caret.range = self.clip_range(caret.range);
                (caret, n == 0)
            })
            .collect();
        items.sort_by_key(|(caret, _)| (caret.range.start, caret.range.end));
        let mut merged: Vec<(Caret, bool)> = Vec::with_capacity(items.len());
        for (caret, primary) in items {
            if let Some((last, last_primary)) = merged.last_mut()
                && (caret.range.start < last.range.end
                    || caret.range.start == last.range.end
                        && (caret.range.is_empty() || last.range.is_empty()))
            {
                last.range.end = last.range.end.max(caret.range.end);
                if primary {
                    last.reversed = caret.reversed;
                }
                *last_primary |= primary;
                continue;
            }
            merged.push((caret, primary));
        }
        let at = merged.iter().position(|(_, p)| *p).unwrap_or(0);
        let (primary, _) = merged.remove(at);
        self.selection = primary.range;
        self.reversed = primary.reversed;
        if let Some(code) = &mut self.code {
            code.carets = merged.into_iter().map(|(caret, _)| caret).collect();
        }
    }
    /// Runs a caret move once for every caret, as if each were the only one.
    pub(in crate::input) fn for_each_caret(
        &mut self,
        cx: &mut Context<Self>,
        mut f: impl FnMut(&mut Self, &mut Context<Self>),
    ) {
        if !self.has_carets() {
            f(self, cx);
            return;
        }
        let mut moved = Vec::new();
        for caret in self.all_carets() {
            self.selection = caret.range;
            self.reversed = caret.reversed;
            f(self, cx);
            moved.push(Caret {
                range: self.selection.clone(),
                reversed: self.reversed,
            });
        }
        self.set_carets(moved);
    }
    /// Replaces each of `edits` at once, as one undo step, and leaves the
    /// carets `after` (in the new offsets, the primary one first).
    pub(in crate::input) fn splice_carets(
        &mut self,
        mut edits: Vec<Splice>,
        after: Vec<Caret>,
        kind: EditKind,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.read_only || self.is_composing() || self.code.is_none() || after.is_empty() {
            return false;
        }
        edits.sort_by_key(|(range, _)| range.start);
        let mut end = 0;
        edits.retain(|(range, _)| {
            let keep = range.start >= end;
            end = end.max(range.end);
            keep
        });
        if edits
            .iter()
            .all(|(range, text)| range.is_empty() && text.is_empty())
        {
            return false;
        }
        let removed: usize = edits.iter().map(|(range, _)| range.len()).sum();
        let added: usize = edits.iter().map(|(_, text)| text.len()).sum();
        let len = self.content.len() - removed + added;
        if len > monocode_core::files::document::MAX_TEXT_BYTES {
            cx.emit(Rejected("Edit exceeds the 2 MB file limit."));
            return false;
        }
        if !edits
            .iter()
            .all(|(range, text)| self.accept_code_edit(range, text, cx))
        {
            return false;
        }
        let head = self.cursor();
        let primary = edits
            .iter()
            .find(|(range, _)| range.start <= head && head <= range.end)
            .unwrap_or(&edits[0])
            .0
            .clone();
        self.record_undo(kind, &primary, after[0].head(), cx);
        for (range, text) in edits.iter().rev() {
            self.map_code_folds(range, text.len());
        }
        let mut out = String::with_capacity(len);
        let mut at = 0;
        for (range, text) in &edits {
            out.push_str(&self.content[at..range.start]);
            out.push_str(text);
            at = range.end;
        }
        out.push_str(&self.content[at..]);
        self.content = out.into();
        if let Some(code) = &mut self.code {
            code.keep_carets = true;
        }
        self.code_changed();
        self.set_carets(after);
        self.manual_scroll = false;
        self.reveal_code();
        if let Some(code) = &mut self.code {
            code.suppress_completion = true;
        }
        cx.emit(Changed);
        cx.notify();
        true
    }
    /// One edit per caret, each from `edit` run with that caret as the
    /// selection; every caret ends after its new text.
    pub(in crate::input) fn edit_each_caret(
        &mut self,
        kind: EditKind,
        cx: &mut Context<Self>,
        mut edit: impl FnMut(&mut Self, &Caret) -> Option<Splice>,
    ) {
        let carets = self.all_carets();
        let mut edits = Vec::with_capacity(carets.len());
        for caret in &carets {
            self.selection = caret.range.clone();
            self.reversed = caret.reversed;
            if let Some(splice) = edit(self, caret) {
                edits.push(splice);
            }
        }
        self.selection = carets[0].range.clone();
        self.reversed = carets[0].reversed;
        edits.sort_by_key(|(range, _)| range.start);
        let after = carets
            .iter()
            .map(|caret| Caret::at(map_through(&edits, caret.head())))
            .collect();
        self.splice_carets(edits, after, kind, cx);
    }
    /// Typing with several carets: the text replaces every selection.
    pub(in crate::input) fn type_at_carets(
        &mut self,
        text: &str,
        kind: Option<EditKind>,
        cx: &mut Context<Self>,
    ) {
        let text = text.to_owned();
        let kind = kind.unwrap_or_else(|| {
            if !text.contains('\n')
                && text.graphemes(true).count() == 1
                && self.all_carets().iter().all(|c| c.range.is_empty())
            {
                EditKind::Typing
            } else {
                EditKind::Other
            }
        });
        self.edit_each_caret(kind, cx, |_, caret| {
            Some((caret.range.clone(), text.clone()))
        });
        // Typed text asks for completion at the primary caret, as with one.
        if kind == EditKind::Typing
            && let Some(code) = &mut self.code
        {
            code.suppress_completion = false;
        }
    }
    /// Removes each selection, or from each caret to where `to` says.
    pub(in crate::input) fn delete_at_carets(
        &mut self,
        kind: EditKind,
        cx: &mut Context<Self>,
        mut to: impl FnMut(&Self) -> usize,
    ) {
        self.edit_each_caret(kind, cx, |input, caret| {
            let range = if caret.range.is_empty() {
                let to = to(input);
                to.min(caret.head())..to.max(caret.head())
            } else {
                caret.range.clone()
            };
            Some((range, String::new()))
        });
    }
    /// Enter with several carets: a new line at each, indented like its own.
    pub(in crate::input) fn newline_at_carets(&mut self, cx: &mut Context<Self>) {
        self.edit_each_caret(EditKind::Other, cx, |input, caret| {
            let at = caret.range.start;
            let start = input.content[..at].rfind('\n').map_or(0, |n| n + 1);
            let indent: String = input.content[start..at]
                .chars()
                .take_while(|c| matches!(c, ' ' | '\t'))
                .collect();
            Some((caret.range.clone(), format!("\n{indent}")))
        });
    }
    /// Paste with several carets: one line each when the clipboard has as
    /// many lines as there are carets, the whole text at each otherwise.
    pub(in crate::input) fn paste_at_carets(&mut self, text: &str, cx: &mut Context<Self>) {
        let ordered = self.ordered_carets();
        let lines: Vec<&str> = text.split('\n').collect();
        let split = lines.len() == ordered.len();
        let pieces: Vec<(usize, String)> = ordered
            .iter()
            .enumerate()
            .map(|(n, caret)| {
                let piece = if split { lines[n] } else { text };
                (caret.range.start, piece.to_owned())
            })
            .collect();
        self.edit_each_caret(EditKind::Other, cx, |_, caret| {
            let piece = pieces
                .iter()
                .find(|(at, _)| *at == caret.range.start)
                .map_or_else(String::new, |(_, piece)| piece.clone());
            Some((caret.range.clone(), piece))
        });
    }
    /// The selected texts of every caret, in document order, one per line.
    pub(in crate::input) fn carets_text(&self) -> Option<String> {
        let parts: Vec<&str> = self
            .ordered_carets()
            .iter()
            .filter(|caret| !caret.range.is_empty())
            .map(|caret| &self.content[self.clip_range(caret.range.clone())])
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n"))
    }
    /// Tab and Shift+Tab with several carets: spaces at each bare caret, or
    /// every line a caret touches shifted when any of them selects text.
    pub(in crate::input) fn indent_carets(&mut self, outdent: bool, cx: &mut Context<Self>) {
        let carets = self.all_carets();
        if !outdent && carets.iter().all(|c| c.range.is_empty()) {
            self.type_at_carets("  ", Some(EditKind::Other), cx);
            return;
        }
        let Some(code) = &self.code else {
            return;
        };
        let mut rows: Vec<usize> = carets
            .iter()
            .flat_map(|caret| {
                let first = code.row(caret.range.start);
                let mut last = code.row(caret.range.end);
                if last > first && code.offsets[last] == caret.range.end {
                    last -= 1;
                }
                first..=last
            })
            .collect();
        rows.sort_unstable();
        rows.dedup();
        let edits: Vec<Splice> = rows
            .iter()
            .filter_map(|row| {
                let start = code.offsets[*row];
                let line = &self.content[start..self.line_end(*row)];
                if !outdent {
                    return Some((start..start, "    ".to_owned()));
                }
                let remove = if line.starts_with('\t') {
                    1
                } else {
                    line.bytes().take(4).take_while(|c| *c == b' ').count()
                };
                (remove > 0).then(|| (start..start + remove, String::new()))
            })
            .collect();
        let after = carets
            .iter()
            .map(|caret| {
                let (a, b) = (
                    map_through(&edits, caret.range.start),
                    map_through(&edits, caret.range.end),
                );
                Caret {
                    range: a.min(b)..a.max(b),
                    reversed: caret.reversed,
                }
            })
            .collect();
        self.splice_carets(edits, after, EditKind::Other, cx);
    }
    /// Where a row's text ends, before its newline.
    fn line_end(&self, row: usize) -> usize {
        self.code
            .as_ref()
            .and_then(|code| code.offsets.get(row + 1))
            .map_or(self.content.len(), |next| next - 1)
    }
    /// The row and column (in characters) of an offset.
    fn row_col(&self, at: usize) -> (usize, usize) {
        let code = self.code.as_ref().unwrap();
        let row = code.row(at);
        let start = code.offsets[row];
        (
            row,
            self.content[start..at.max(start)].graphemes(true).count(),
        )
    }
    /// The offset at a column of a row, or the row's end when it is shorter.
    fn col_offset(&self, row: usize, col: usize) -> usize {
        let start = self.code.as_ref().unwrap().offsets[row];
        let end = self.line_end(row);
        self.content[start..end]
            .grapheme_indices(true)
            .nth(col)
            .map_or(end, |(at, _)| start + at)
    }
    /// Rows `first..=last` grown to whole folded regions, which move and
    /// duplicate as the one line they show as.
    fn unit_rows(&self, mut first: usize, mut last: usize) -> (usize, usize) {
        let code = self.code.as_ref().unwrap();
        first = code.display_row(code.offsets[first]);
        let header = code.display_row(code.offsets[last]);
        if let Some(fold) = code.fold_at(header) {
            last = last.max(code.row(fold.to));
        }
        (first, last)
    }
    /// The rows a selection covers. A selection ending at the start of a
    /// line leaves that line out.
    fn caret_rows(&self, range: &Range<usize>) -> (usize, usize) {
        let code = self.code.as_ref().unwrap();
        let first = code.row(range.start);
        let mut last = code.row(range.end);
        if last > first && code.offsets[last] == range.end {
            last -= 1;
        }
        self.unit_rows(first, last)
    }
    /// The rows every caret covers, as runs of lines. Carets on the same
    /// lines share a run; with `adjacent`, so do runs that touch.
    fn caret_blocks(&self, carets: &[Caret], adjacent: bool) -> Vec<(usize, usize)> {
        let mut rows: Vec<(usize, usize)> =
            carets.iter().map(|c| self.caret_rows(&c.range)).collect();
        rows.sort_unstable();
        let mut blocks: Vec<(usize, usize)> = Vec::with_capacity(rows.len());
        for (first, last) in rows {
            if let Some(block) = blocks.last_mut()
                && first <= block.1 + usize::from(adjacent)
            {
                block.1 = block.1.max(last);
                continue;
            }
            blocks.push((first, last));
        }
        blocks
    }

    pub(in crate::input) fn expand_selection(
        &mut self,
        _: &ExpandSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.code.is_none() {
            cx.propagate();
            return;
        }
        self.collapse_carets();
        let selection = self.selection.clone();
        let next = self
            .selection_steps(&selection)
            .into_iter()
            .find(|r| r.start <= selection.start && r.end >= selection.end && *r != selection);
        let Some(next) = next else {
            return;
        };
        let mut stack = match self.code.as_mut().unwrap().expansion.take() {
            Some(e) if e.at == selection && e.text == self.content => e.stack,
            _ => Vec::new(),
        };
        stack.push(selection);
        self.select_range(next.clone(), cx);
        self.code.as_mut().unwrap().expansion = Some(Expansion {
            text: self.content.clone(),
            at: next,
            stack,
        });
    }
    pub(in crate::input) fn shrink_selection(
        &mut self,
        _: &ShrinkSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(code) = &mut self.code else {
            cx.propagate();
            return;
        };
        let Some(mut expansion) = code.expansion.take() else {
            return;
        };
        if expansion.at != self.selection || expansion.text != self.content {
            return;
        }
        let Some(previous) = expansion.stack.pop() else {
            return;
        };
        self.select_range(previous.clone(), cx);
        expansion.at = previous;
        self.code.as_mut().unwrap().expansion = Some(expansion);
    }
    /// Every range Extend Selection can grow to, smallest first: the word,
    /// the syntax around (or lines and brackets without a grammar), the file.
    fn selection_steps(&mut self, selection: &Range<usize>) -> Vec<Range<usize>> {
        let mut steps = Vec::new();
        steps.extend(word_around(&self.content, selection));
        let text = self.content.clone();
        let code = self.code.as_mut().unwrap();
        let path = code.path.clone();
        if crate::editor_syntax::supports(&path) {
            if code.structure.as_ref().is_none_or(|s| s.path() != path) {
                code.structure = crate::editor_syntax::Structure::new(&path).map(Box::new);
            }
            if let Some(structure) = &mut code.structure {
                steps.extend(structure.enclosing(&text, selection.clone()));
            }
        } else {
            steps.extend(text_ranges(&text, selection));
        }
        steps.push(0..text.len());
        steps.sort_by_key(|r| (r.len(), r.start));
        steps.dedup();
        steps
    }

    /// Duplicate Line for every caret: a selection inside one line is
    /// repeated after itself, otherwise the caret's lines are, once for all
    /// the carets on them. Each caret moves into its copy.
    pub(in crate::input) fn duplicate_line(
        &mut self,
        _: &DuplicateLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.code.is_none() {
            cx.propagate();
            return;
        }
        let carets = self.all_carets();
        let code = self.code.as_ref().unwrap();
        let within_line =
            |c: &Caret| !c.range.is_empty() && code.row(c.range.start) == code.row(c.range.end);
        let whole: Vec<Caret> = carets.iter().filter(|c| !within_line(c)).cloned().collect();
        let blocks = self.caret_blocks(&whole, false);
        let copied = |row: usize| {
            blocks
                .iter()
                .any(|(first, last)| (*first..=*last).contains(&row))
        };
        // The copy goes above, so a caret in the lines lands in the lower one.
        let mut edits: Vec<Splice> = blocks
            .iter()
            .map(|(first, last)| {
                let lines = code.offsets[*first]..self.line_end(*last);
                (
                    lines.start..lines.start,
                    format!("{}\n", &self.content[lines]),
                )
            })
            .collect();
        let inline: Vec<Range<usize>> = carets
            .iter()
            .filter(|c| within_line(c) && !copied(code.row(c.range.start)))
            .map(|c| c.range.clone())
            .collect();
        edits.extend(
            inline
                .iter()
                .map(|r| (r.end..r.end, self.content[r.clone()].to_owned())),
        );
        edits.sort_by_key(|(range, _)| range.start);
        let after = carets
            .iter()
            .map(|caret| {
                let range = if inline.contains(&caret.range) {
                    let end = map_through(&edits, caret.range.end);
                    end - caret.range.len()..end
                } else {
                    map_through(&edits, caret.range.start)..map_through(&edits, caret.range.end)
                };
                Caret {
                    range,
                    reversed: caret.reversed,
                }
            })
            .collect();
        self.splice_carets(edits, after, EditKind::Other, cx);
    }
    pub(in crate::input) fn move_line_up(
        &mut self,
        _: &MoveLineUp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.code.is_none() {
            cx.propagate();
            return;
        }
        self.move_lines(true, cx);
    }
    pub(in crate::input) fn move_line_down(
        &mut self,
        _: &MoveLineDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.code.is_none() {
            cx.propagate();
            return;
        }
        self.move_lines(false, cx);
    }
    /// Swaps the lines of every caret with the line (or folded region)
    /// above or below them; nothing moves when one run is at the edge.
    /// Folded regions on either side stay folded.
    fn move_lines(&mut self, up: bool, cx: &mut Context<Self>) {
        let carets = self.all_carets();
        let blocks = self.caret_blocks(&carets, true);
        let code = self.code.as_ref().unwrap();
        let rows = code.offsets.len();
        let at_edge = if up {
            blocks.first().is_none_or(|(first, _)| *first == 0)
        } else {
            blocks.last().is_none_or(|(_, last)| last + 1 >= rows)
        };
        if at_edge {
            return;
        }
        let shift = |at: usize, by: isize| (at as isize + by) as usize;
        let mut edits: Vec<Splice> = Vec::with_capacity(blocks.len());
        let mut moves: Vec<(Range<usize>, isize)> = Vec::with_capacity(blocks.len());
        let mut folds = Vec::new();
        for (first, last) in blocks {
            let (other_first, other_last) = if up {
                self.unit_rows(first - 1, first - 1)
            } else {
                self.unit_rows(last + 1, last + 1)
            };
            let moved = code.offsets[first]..self.line_end(last);
            let other = code.offsets[other_first]..self.line_end(other_last);
            let (region, text, shift_moved, shift_other) = if up {
                (
                    other.start..moved.end,
                    format!(
                        "{}\n{}",
                        &self.content[moved.clone()],
                        &self.content[other.clone()]
                    ),
                    -(other.len() as isize + 1),
                    moved.len() as isize + 1,
                )
            } else {
                (
                    moved.start..other.end,
                    format!(
                        "{}\n{}",
                        &self.content[other.clone()],
                        &self.content[moved.clone()]
                    ),
                    other.len() as isize + 1,
                    -(moved.len() as isize + 1),
                )
            };
            folds.extend(
                code.folded
                    .iter()
                    .filter(|f| f.from >= region.start && f.to <= region.end)
                    .map(|f| {
                        let by = if moved.contains(&f.from) {
                            shift_moved
                        } else {
                            shift_other
                        };
                        crate::editor_analysis::Fold {
                            from: shift(f.from, by),
                            to: shift(f.to, by),
                        }
                    }),
            );
            moves.push((moved, shift_moved));
            edits.push((region, text));
        }
        // Each swap keeps its length, so a caret only moves with its lines.
        let after = carets
            .iter()
            .map(|caret| {
                let by = moves
                    .iter()
                    .find(|(moved, _)| {
                        moved.start <= caret.range.start && caret.range.start <= moved.end
                    })
                    .map_or(0, |(_, by)| *by);
                Caret {
                    range: shift(caret.range.start, by)..shift(caret.range.end, by),
                    reversed: caret.reversed,
                }
            })
            .collect();
        if self.splice_carets(edits, after, EditKind::Other, cx)
            && let Some(code) = &mut self.code
        {
            code.folded.extend(folds);
            code.layout_width = None;
        }
    }

    pub(in crate::input) fn toggle_column_selection(
        &mut self,
        _: &ToggleColumnSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(code) = &mut self.code else {
            cx.propagate();
            return;
        };
        code.column_mode = !code.column_mode;
        code.block = None;
        cx.notify();
    }
    /// Whether column selection mode is on, for the menu's check.
    pub(crate) fn column_selection(&self) -> bool {
        self.code.as_ref().is_some_and(|code| code.column_mode)
    }
    /// Shift+arrows in column mode grow the rectangle by a row or a column.
    /// Returns false outside column mode.
    pub(in crate::input) fn column_select(
        &mut self,
        rows: isize,
        cols: isize,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.column_selection() {
            return false;
        }
        let block = match self.code.as_ref().unwrap().block {
            Some(block) => block,
            None => {
                let anchor = if self.reversed {
                    self.selection.end
                } else {
                    self.selection.start
                };
                Block {
                    anchor: self.row_col(anchor),
                    head: self.row_col(self.cursor()),
                }
            }
        };
        let code = self.code.as_ref().unwrap();
        let (mut row, col) = block.head;
        if rows != 0 {
            let mut next = row;
            loop {
                let Some(step) = next.checked_add_signed(rows) else {
                    break;
                };
                if step >= code.offsets.len() {
                    break;
                }
                next = step;
                if !code.hidden(next) {
                    row = next;
                    break;
                }
            }
        }
        let col = col.saturating_add_signed(cols);
        self.apply_block(
            Block {
                head: (row, col),
                ..block
            },
            cx,
        );
        true
    }
    /// Selects the rectangle: one caret per row, the head's row primary.
    fn apply_block(&mut self, block: Block, cx: &mut Context<Self>) {
        let code = self.code.as_ref().unwrap();
        let (top, bottom) = (
            block.anchor.0.min(block.head.0),
            block.anchor.0.max(block.head.0),
        );
        let (left, right) = (
            block.anchor.1.min(block.head.1),
            block.anchor.1.max(block.head.1),
        );
        let reversed = block.head.1 < block.anchor.1;
        let mut carets = Vec::new();
        let mut primary = None;
        for row in top..=bottom {
            if code.hidden(row) {
                continue;
            }
            let caret = Caret {
                range: self.col_offset(row, left)..self.col_offset(row, right),
                reversed,
            };
            if row == block.head.0 {
                primary = Some(caret);
            } else {
                carets.push(caret);
            }
        }
        let Some(primary) = primary else {
            return;
        };
        carets.insert(0, primary);
        self.manual_scroll = false;
        self.reveal_code();
        self.clear_completion();
        self.history.break_run();
        self.set_carets(carets);
        self.code.as_mut().unwrap().block = Some(block);
        cx.notify();
    }
    /// The row and column under a point in the window, for column mode.
    fn block_point(&self, p: Point<Pixels>) -> Option<(usize, usize)> {
        let (code, bounds) = (self.code.as_ref()?, self.bounds?);
        let local = p - bounds.origin + point(self.scroll_x, self.scroll_y);
        let row = code.row_at_y(local.y.as_f32());
        let row = code.display_row(code.offsets[row]);
        let col = (local.x.as_f32().max(0.) / code.char_width.max(1.)).round() as usize;
        Some((row, col))
    }
    /// A press in column mode starts a rectangle there, or with Shift grows
    /// the current one. Returns false outside column mode.
    pub(in crate::input) fn column_mouse_down(
        &mut self,
        p: Point<Pixels>,
        extend: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.column_selection() {
            return false;
        }
        let Some(at) = self.block_point(p) else {
            return false;
        };
        let anchor = match self.code.as_ref().unwrap().block {
            Some(block) if extend => block.anchor,
            _ if extend => self.row_col(self.cursor()),
            _ => at,
        };
        self.apply_block(Block { anchor, head: at }, cx);
        true
    }
    /// Dragging in column mode moves the rectangle's corner. Returns false
    /// outside column mode.
    pub(in crate::input) fn column_drag(
        &mut self,
        p: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.column_selection() {
            return false;
        }
        let (Some(block), Some(at)) = (self.code.as_ref().unwrap().block, self.block_point(p))
        else {
            return true;
        };
        if block.head != at {
            self.apply_block(Block { head: at, ..block }, cx);
        }
        true
    }
    /// Alt+Shift+click: a caret there besides the others, or the caret
    /// there removed when there already is one.
    pub(in crate::input) fn toggle_caret_at(&mut self, at: usize, cx: &mut Context<Self>) {
        let mut carets = self.all_carets();
        if carets.len() > 1
            && let Some(n) = carets
                .iter()
                .position(|c| c.range.is_empty() && c.head() == at)
        {
            carets.remove(n);
        } else {
            carets.insert(0, Caret::at(at));
        }
        self.history.break_run();
        self.set_carets(carets);
        self.reveal_code();
        cx.notify();
    }

    pub(in crate::input) fn add_caret_above(
        &mut self,
        _: &AddCaretAbove,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.code.is_none() {
            cx.propagate();
            return;
        }
        self.add_caret(true, cx);
    }
    pub(in crate::input) fn add_caret_below(
        &mut self,
        _: &AddCaretBelow,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.code.is_none() {
            cx.propagate();
            return;
        }
        self.add_caret(false, cx);
    }
    /// A caret on the nearest line above the topmost caret (or below the
    /// lowest) that reaches its column; shorter lines are skipped.
    fn add_caret(&mut self, above: bool, cx: &mut Context<Self>) {
        let carets = self.all_carets();
        let edge = if above {
            carets.iter().map(Caret::head).min()
        } else {
            carets.iter().map(Caret::head).max()
        };
        let Some(edge) = edge else {
            return;
        };
        let (mut row, col) = self.row_col(edge);
        let code = self.code.as_ref().unwrap();
        let target = loop {
            row = match (above, row) {
                (true, 0) => return,
                (true, _) => row - 1,
                (false, _) if row + 1 >= code.offsets.len() => return,
                (false, _) => row + 1,
            };
            if code.hidden(row) {
                continue;
            }
            let start = code.offsets[row];
            if self.content[start..self.line_end(row)]
                .graphemes(true)
                .count()
                >= col
            {
                break self.col_offset(row, col);
            }
        };
        let mut carets = carets;
        carets.insert(0, Caret::at(target));
        self.history.break_run();
        self.set_carets(carets);
        self.manual_scroll = false;
        self.reveal_code();
        cx.notify();
    }

    /// What the occurrence commands look for: the primary selection, or the
    /// word at a bare caret (then selected), and whether to match only
    /// whole words, as when the search started from a bare caret.
    fn occurrence_target(&mut self) -> Option<(String, bool)> {
        if self.selection.is_empty() {
            let at = self.cursor();
            let word = word_around(&self.content, &(at..at))?;
            let text = self.content[word.clone()].to_owned();
            let mut carets = self.all_carets();
            carets[0] = Caret {
                range: word,
                reversed: false,
            };
            self.set_carets(carets);
            self.code.as_mut()?.occurrence_word = Some(text.clone());
            return Some((text, true));
        }
        let text = self.content[self.selection.clone()].to_owned();
        let code = self.code.as_mut()?;
        let whole = code.occurrence_word.as_deref() == Some(text.as_str());
        if !whole {
            code.occurrence_word = None;
        }
        Some((text, whole))
    }
    /// Every place `needle` is in the buffer, in order.
    fn occurrences(&self, needle: &str, whole: bool) -> Vec<Range<usize>> {
        let text = self.content.as_ref();
        text.match_indices(needle)
            .map(|(at, _)| at..at + needle.len())
            .filter(|r| {
                !whole
                    || !text[..r.start].chars().next_back().is_some_and(is_word)
                        && !text[r.end..].chars().next().is_some_and(is_word)
            })
            .collect()
    }
    /// Leaves `carets` (the primary one first) after an occurrence command.
    fn select_carets(&mut self, carets: Vec<Caret>, cx: &mut Context<Self>) {
        for caret in &carets {
            self.unfold_range(&caret.range);
        }
        self.clear_completion();
        self.history.break_run();
        self.set_carets(carets);
        self.manual_scroll = false;
        self.reveal_code();
        cx.notify();
    }
    /// Select Next Occurrence: the word at a bare caret first, then each
    /// press adds the next match of the selection after the newest caret,
    /// wrapping around past the end, as a new primary caret.
    pub(in crate::input) fn select_next_occurrence(
        &mut self,
        _: &SelectNextOccurrence,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.code.is_none() {
            cx.propagate();
            return;
        }
        let bare = self.selection.is_empty();
        let Some((needle, whole)) = self.occurrence_target() else {
            return;
        };
        let carets = self.all_carets();
        if bare {
            self.select_carets(carets, cx);
            return;
        }
        let found = self.occurrences(&needle, whole);
        let from = self.selection.end;
        let free = |r: &Range<usize>| {
            !carets
                .iter()
                .any(|c| c.range.start < r.end && r.start < c.range.end)
        };
        let next = found
            .iter()
            .filter(|r| r.start >= from)
            .chain(found.iter().filter(|r| r.start < from))
            .find(|r| free(r))
            .cloned();
        let Some(next) = next else {
            return;
        };
        let mut carets = carets;
        carets.insert(
            0,
            Caret {
                range: next,
                reversed: false,
            },
        );
        self.select_carets(carets, cx);
    }
    /// Select All Occurrences: a caret on every match of the selection (or
    /// of the word at a bare caret); the primary one stays where it was.
    pub(in crate::input) fn select_all_occurrences(
        &mut self,
        _: &SelectAllOccurrences,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.code.is_none() {
            cx.propagate();
            return;
        }
        let Some((needle, whole)) = self.occurrence_target() else {
            return;
        };
        let primary = self.selection.clone();
        let carets = std::iter::once(Caret {
            range: primary.clone(),
            reversed: false,
        })
        .chain(
            self.occurrences(&needle, whole)
                .into_iter()
                .filter(|r| *r != primary)
                .map(|range| Caret {
                    range,
                    reversed: false,
                }),
        )
        .collect();
        self.select_carets(carets, cx);
    }

    /// Accepting a completion with several carets: the item goes in at
    /// every caret as one undo step. A caret after the same prefix as the
    /// primary one has that prefix replaced (and the rest of the word, when
    /// it matches too); a selection is replaced; elsewhere the text is put
    /// in as it is. The item's other edits (an import) are made once.
    pub(in crate::input) fn complete_at_carets(
        &mut self,
        item: CompletionItem,
        cx: &mut Context<Self>,
    ) {
        let head = self.cursor();
        let replace = self.clip_range(item.replace.clone());
        let (prefix, suffix) = if replace.start <= head && head <= replace.end {
            (
                self.content[replace.start..head].to_owned(),
                self.content[head..replace.end].to_owned(),
            )
        } else {
            (String::new(), String::new())
        };
        let carets = self.all_carets();
        let text = self.content.as_ref();
        let ranges: Vec<Range<usize>> = carets
            .iter()
            .enumerate()
            .map(|(n, caret)| {
                if n == 0 {
                    return replace.clone();
                }
                if !caret.range.is_empty() {
                    return caret.range.clone();
                }
                let at = caret.head();
                let Some(start) = at.checked_sub(prefix.len()) else {
                    return at..at;
                };
                if !text.is_char_boundary(start)
                    || text[start..at] != *prefix
                    || text[..start].chars().next_back().is_some_and(is_word)
                {
                    return at..at;
                }
                let end = if text[at..].starts_with(suffix.as_str()) {
                    at + suffix.len()
                } else {
                    at
                };
                start..end
            })
            .collect();
        let mut edits: Vec<Splice> = item
            .additional_edits
            .iter()
            .map(|edit| (self.clip_range(edit.range.clone()), edit.text.clone()))
            .chain(ranges.iter().map(|r| (r.clone(), item.insert.clone())))
            .collect();
        edits.sort_by_key(|(range, _)| range.start);
        let cursor = item
            .cursor
            .unwrap_or(item.insert.len())
            .min(item.insert.len());
        let after = ranges
            .iter()
            .map(|r| Caret::at(map_through(&edits, r.end) - item.insert.len() + cursor))
            .collect();
        self.clear_completion();
        self.splice_carets(edits, after, EditKind::Other, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_follow_edits_and_land_after_inserted_text() {
        let edits = vec![(2..4, "xyz".to_owned()), (6..6, "!".to_owned())];
        assert_eq!(map_through(&edits, 1), 1);
        assert_eq!(map_through(&edits, 2), 5);
        assert_eq!(map_through(&edits, 4), 5);
        assert_eq!(map_through(&edits, 5), 6);
        assert_eq!(map_through(&edits, 6), 8);
        assert_eq!(map_through(&edits, 9), 11);
    }

    #[test]
    fn plain_text_grows_through_words_quotes_brackets_and_lines() {
        let text = "call(a, \"some text\", [x + 1])\nnext";
        let at = text.find("text").unwrap();
        assert_eq!(word_around(text, &(at..at)), Some(at..at + 4));
        let steps = text_ranges(text, &(at..at + 4));
        let quoted = text.find("some").unwrap();
        assert!(steps.contains(&(quoted..quoted + 9)));
        assert!(steps.contains(&(quoted - 1..quoted + 10)));
        let args = 5..text.find("])").unwrap() + 1;
        assert!(steps.contains(&args), "{steps:?}");
        assert!(steps.contains(&(0..text.find('\n').unwrap())));
        let x = text.find("x +").unwrap();
        let inner = text_ranges(text, &(x..x + 1));
        assert!(inner.contains(&(x..x + 5)), "{inner:?}");
    }
}
