//! Converting between our byte offsets into UTF-8 text and the positions a
//! language server speaks: a line and a count of code units in that line, in
//! UTF-16 unless the server agreed to something else. Lines end where the
//! protocol says they do (`\n`, `\r\n` or a lone `\r`), whatever the editor
//! itself thinks a line is, so a position always lands on the byte the server
//! meant.

/// The unit a server counts the `character` of a position in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    #[default]
    Utf16,
    Utf32,
}

impl Encoding {
    /// The encoding a server announced in its capabilities; UTF-16 is the
    /// protocol's default and what an unknown name falls back to.
    pub fn from_name(name: Option<&str>) -> Self {
        match name {
            Some("utf-8") => Self::Utf8,
            Some("utf-32") => Self::Utf32,
            _ => Self::Utf16,
        }
    }

    fn units(self, character: char) -> u32 {
        match self {
            Self::Utf8 => character.len_utf8() as u32,
            Self::Utf16 => character.len_utf16() as u32,
            Self::Utf32 => 1,
        }
    }
}

/// A place in a text as results report it: a zero-based line (protocol line
/// rules) and a byte offset within that line. `LineIndex::offset_of` turns it
/// into a byte offset into the whole text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub line: u32,
    pub column: u32,
}

/// From `start` up to, not including, `end`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub start: Point,
    pub end: Point,
}

/// A position as the protocol writes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// Where each line of one text starts. Built once per text and asked many
/// times; it does not hold the text, so pass the same text to every call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineIndex {
    /// Byte offset of the first byte of each line; never empty.
    starts: Vec<usize>,
    len: usize,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut starts = vec![0];
        let mut at = 0;
        while at < bytes.len() {
            match bytes[at] {
                b'\n' => starts.push(at + 1),
                b'\r' => {
                    if bytes.get(at + 1) == Some(&b'\n') {
                        at += 1;
                    }
                    starts.push(at + 1);
                }
                _ => {}
            }
            at += 1;
        }
        Self {
            starts,
            len: bytes.len(),
        }
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// The bytes of `line` without its line ending, as a range of the text.
    fn line_range(&self, text: &str, line: usize) -> (usize, usize) {
        let start = self.starts[line];
        let mut end = self.starts.get(line + 1).copied().unwrap_or(self.len);
        let bytes = text.as_bytes();
        if line + 1 < self.starts.len() {
            // Step back over the ending: one byte, or two for `\r\n`.
            if end > start && bytes.get(end - 1) == Some(&b'\n') {
                end -= 1;
                if end > start && bytes.get(end - 1) == Some(&b'\r') {
                    end -= 1;
                }
            } else if end > start && bytes.get(end - 1) == Some(&b'\r') {
                end -= 1;
            }
        }
        (start, end.min(text.len()))
    }

    fn line_of(&self, offset: usize) -> usize {
        self.starts.partition_point(|start| *start <= offset) - 1
    }

    /// The point at byte `offset`. An offset past the end is the end; one
    /// inside a character or inside a line ending moves back to its start.
    pub fn point_at(&self, text: &str, offset: usize) -> Point {
        let mut offset = offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = self.line_of(offset);
        let (start, end) = self.line_range(text, line);
        Point {
            line: line as u32,
            column: (offset.min(end) - start) as u32,
        }
    }

    /// The byte offset of `point`. A line past the last is the end of the
    /// text; a column past the end of its line is the end of that line.
    pub fn offset_of(&self, text: &str, point: Point) -> usize {
        let line = point.line as usize;
        if line >= self.starts.len() {
            return text.len();
        }
        let (start, end) = self.line_range(text, line);
        let mut offset = (start + point.column as usize).min(end);
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    /// The protocol position of byte `offset`, clamped as `point_at` clamps.
    pub fn position_at(&self, text: &str, offset: usize, encoding: Encoding) -> Position {
        let point = self.point_at(text, offset);
        let start = self.starts[point.line as usize];
        let before = &text[start..start + point.column as usize];
        let character = match encoding {
            Encoding::Utf8 => point.column,
            _ => before.chars().map(|c| encoding.units(c)).sum(),
        };
        Position {
            line: point.line,
            character,
        }
    }

    /// The point a protocol position names. As the protocol asks, a character
    /// past the end of the line is the end of the line; one that falls inside
    /// a character (the second half of a surrogate pair) is that character's
    /// start.
    pub fn point_of(&self, text: &str, position: Position, encoding: Encoding) -> Point {
        let line = position.line as usize;
        if line >= self.starts.len() {
            return self.point_at(text, text.len());
        }
        let (start, end) = self.line_range(text, line);
        let mut units = 0u32;
        let mut column = 0usize;
        for character in text[start..end].chars() {
            let next = units + encoding.units(character);
            if next > position.character {
                break;
            }
            units = next;
            column += character.len_utf8();
        }
        Point {
            line: position.line,
            column: column as u32,
        }
    }

    /// The byte offset a protocol position names.
    pub fn offset_of_position(&self, text: &str, position: Position, encoding: Encoding) -> usize {
        let point = self.point_of(text, position, encoding);
        self.starts[point.line as usize] + point.column as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn ascii_round_trips_in_every_encoding() {
        let text = "def f():\n    return 1\n";
        let index = LineIndex::new(text);
        assert_eq!(index.line_count(), 3);
        for encoding in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            for offset in 0..=text.len() {
                let at = index.position_at(text, offset, encoding);
                assert_eq!(index.offset_of_position(text, at, encoding), offset);
            }
        }
        assert_eq!(index.position_at(text, 13, Encoding::Utf16), position(1, 4));
    }

    #[test]
    fn astral_and_wide_characters_count_per_encoding() {
        // "ş" is 2 bytes and 1 UTF-16 unit; "😀" is 4 bytes and 2 units.
        let text = "aş😀b\nx";
        let index = LineIndex::new(text);
        let after_emoji = "aş😀".len();
        assert_eq!(
            index.position_at(text, after_emoji, Encoding::Utf16),
            position(0, 4)
        );
        assert_eq!(
            index.position_at(text, after_emoji, Encoding::Utf8),
            position(0, 7)
        );
        assert_eq!(
            index.position_at(text, after_emoji, Encoding::Utf32),
            position(0, 3)
        );
        assert_eq!(
            index.offset_of_position(text, position(0, 4), Encoding::Utf16),
            after_emoji
        );
        assert_eq!(
            index.offset_of_position(text, position(0, 3), Encoding::Utf32),
            after_emoji
        );
        // The middle of the surrogate pair is the emoji's start.
        assert_eq!(
            index.offset_of_position(text, position(0, 3), Encoding::Utf16),
            "aş".len()
        );
        // A byte offset inside a character moves back to its start.
        assert_eq!(
            index.position_at(text, "aş".len() + 2, Encoding::Utf16),
            position(0, 2)
        );
        assert_eq!(
            index.position_at(text, text.len(), Encoding::Utf16),
            position(1, 1)
        );
    }

    #[test]
    fn every_line_ending_starts_a_line() {
        let text = "a\r\nbc\rd\n\ne";
        let index = LineIndex::new(text);
        assert_eq!(index.line_count(), 5);
        let offsets = [
            (0, 0, 0),
            (1, 0, 3),
            (1, 2, 5),
            (2, 0, 6),
            (3, 0, 8),
            (4, 1, 10),
        ];
        for (line, character, offset) in offsets {
            assert_eq!(
                index.offset_of_position(text, position(line, character), Encoding::Utf16),
                offset,
                "{line}:{character}"
            );
            assert_eq!(
                index.position_at(text, offset, Encoding::Utf16),
                position(line, character)
            );
        }
        // Between `\r` and `\n` is still the end of the first line.
        assert_eq!(index.position_at(text, 2, Encoding::Utf16), position(0, 1));
    }

    #[test]
    fn positions_past_the_end_are_clamped() {
        let text = "ab\r\ncd";
        let index = LineIndex::new(text);
        // Past the end of a line: the end of that line, before its ending.
        assert_eq!(
            index.offset_of_position(text, position(0, 99), Encoding::Utf16),
            2
        );
        assert_eq!(
            index.offset_of_position(text, position(1, 99), Encoding::Utf16),
            6
        );
        // Past the last line: the end of the text.
        assert_eq!(
            index.offset_of_position(text, position(9, 0), Encoding::Utf16),
            6
        );
        assert_eq!(
            index.position_at(text, 999, Encoding::Utf16),
            position(1, 2)
        );
        assert_eq!(
            index.offset_of(
                text,
                Point {
                    line: 0,
                    column: 50
                }
            ),
            2
        );
    }

    #[test]
    fn empty_text_and_trailing_newline() {
        let index = LineIndex::new("");
        assert_eq!(index.line_count(), 1);
        assert_eq!(index.position_at("", 0, Encoding::Utf16), position(0, 0));
        assert_eq!(
            index.offset_of_position("", position(3, 3), Encoding::Utf16),
            0
        );
        let text = "a\n";
        let index = LineIndex::new(text);
        assert_eq!(index.position_at(text, 2, Encoding::Utf16), position(1, 0));
        assert_eq!(
            index.offset_of_position(text, position(1, 0), Encoding::Utf16),
            2
        );
    }

    #[test]
    fn points_are_bytes_within_the_line() {
        let text = "x\nş😀z";
        let index = LineIndex::new(text);
        let point = index.point_of(text, position(1, 3), Encoding::Utf16);
        assert_eq!(point, Point { line: 1, column: 6 });
        assert_eq!(index.offset_of(text, point), 8);
        assert_eq!(index.point_at(text, 8), point);
        // A column inside a character moves back to its start.
        assert_eq!(index.offset_of(text, Point { line: 1, column: 4 }), 4);
    }

    #[test]
    fn encoding_names() {
        assert_eq!(Encoding::from_name(Some("utf-8")), Encoding::Utf8);
        assert_eq!(Encoding::from_name(Some("utf-32")), Encoding::Utf32);
        assert_eq!(Encoding::from_name(Some("utf-16")), Encoding::Utf16);
        assert_eq!(Encoding::from_name(None), Encoding::Utf16);
    }
}
