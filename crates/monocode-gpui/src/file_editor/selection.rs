//! editorSelection.ts: compact file/line references for the chat, and the
//! selected text the code menu sends to a data source.
use super::FileEditor;
use gpui::{App, Context};
use std::{ops::Range, path::Path};

/// What the code menu acts on: the selection, or the file when there is none.
#[derive(Clone, Debug)]
pub(super) struct Target {
    /// Empty when nothing is selected.
    pub range: Range<usize>,
    /// `@path (lines a-b)` for a selection, `@path` for the whole file.
    pub reference: String,
    /// The first and last line selected, 1-based.
    pub lines: (usize, usize),
}

/// The colour a data source carries in lists.
pub(super) fn db_dot(color: Option<monocode_core::db::DbColor>) -> gpui::Hsla {
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
/// `@path` when the composer can mention it, else the path in backticks.
fn mention(path: &str) -> String {
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
    if mentionable {
        format!("@{path}")
    } else {
        format!("`{}`", path.replace(['`', '\r', '\n'], "'"))
    }
}
fn format_reference(path: &str, start: usize, end: usize) -> String {
    let file = mention(path);
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
    /// The path the chat knows the file by: relative to the checkout.
    pub(super) fn relative_path(&self) -> String {
        Path::new(&self.path)
            .strip_prefix(&self.cwd)
            .unwrap_or_else(|_| Path::new(&self.path))
            .to_string_lossy()
            .into_owned()
    }
    pub(super) fn menu_target(&self, cx: &App) -> Target {
        let input = self.input.read(cx);
        let range = input.selection_range();
        let path = self.relative_path();
        match reference_for_selection(&path, &input.content, range.clone()) {
            Some(reference) => Target {
                lines: line_span(&input.content, range.clone()),
                range,
                reference,
            },
            None => {
                let at = range.end.min(input.content.len());
                let line = line_span(&input.content, at..at).0;
                Target {
                    range: at..at,
                    reference: mention(&path),
                    lines: (line, line),
                }
            }
        }
    }
    /// The selected text, if the selection still is `range`.
    pub(super) fn selected_text(
        &self,
        range: &Range<usize>,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let input = self.input.read(cx);
        let content = &input.content;
        (!range.is_empty()
            && input.selection_range() == *range
            && range.end <= content.len()
            && content.is_char_boundary(range.start)
            && content.is_char_boundary(range.end))
        .then(|| content[range.clone()].to_string())
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
        assert_eq!(mention("src/value.ts"), "@src/value.ts");
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
}
