//! `[Image #N]` markers: how a prompt names its Nth attached image, the way the
//! Claude and Codex TUIs write them. The composer keeps them as plain text and
//! draws them as pills; the number is the image's position among the message's
//! image attachments (other files are not counted). Removing an image drops its
//! markers and shifts later ones down, so text and thumbnails always agree.
use std::{
    ops::Range,
    path::{Path, PathBuf},
};

const OPEN: &str = "[Image #";

pub fn marker(number: usize) -> String {
    format!("{OPEN}{number}]")
}

/// Every well-formed marker in `text` with its number, in order.
pub fn markers(text: &str) -> Vec<(Range<usize>, usize)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(OPEN) {
        let start = from + at;
        let digits_at = start + OPEN.len();
        let digits = text[digits_at..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        from = digits_at;
        if !(1..=4).contains(&digits) || text.as_bytes().get(digits_at + digits) != Some(&b']') {
            continue;
        }
        let number: usize = text[digits_at..digits_at + digits].parse().unwrap_or(0);
        if number == 0 || text.as_bytes()[digits_at] == b'0' {
            continue;
        }
        let end = digits_at + digits + 1;
        found.push((start..end, number));
        from = end;
    }
    found
}

/// Markers that name one of `count` attached images.
pub fn live_markers(text: &str, count: usize) -> Vec<(Range<usize>, usize)> {
    let mut found = markers(text);
    found.retain(|(_, number)| *number <= count);
    found
}

pub fn is_image(path: &Path) -> bool {
    super::image_mime(path).is_some()
}

pub fn image_count(paths: &[PathBuf]) -> usize {
    paths.iter().filter(|path| is_image(path)).count()
}

/// Each attachment's image number, `None` for files that are not images.
pub fn image_numbers(paths: &[PathBuf]) -> Vec<Option<usize>> {
    let mut next = 0;
    paths
        .iter()
        .map(|path| {
            is_image(path).then(|| {
                next += 1;
                next
            })
        })
        .collect()
}

/// The text to insert between `before` and `after` for markers `numbers`.
/// Spaces keep the markers apart from the words on either side of the caret.
pub fn insertion(before: &str, after: &str, numbers: &[usize]) -> String {
    if numbers.is_empty() {
        return String::new();
    }
    let markers = numbers
        .iter()
        .map(|number| marker(*number))
        .collect::<Vec<_>>()
        .join(" ");
    let lead = if before.is_empty() || before.ends_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    let trail = if after.starts_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    format!("{lead}{markers}{trail}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
}

/// Edits that remove image `removed` (1-based) of `count`: each of its markers
/// goes with the space beside it, and later images' markers shift down by one.
pub fn removal_edits(text: &str, removed: usize, count: usize) -> Vec<Edit> {
    let bytes = text.as_bytes();
    let mut edits: Vec<Edit> = Vec::new();
    for (range, number) in live_markers(text, count) {
        if number == removed {
            let mut range = range;
            if bytes.get(range.end) == Some(&b' ') {
                range.end += 1;
            }
            // At the end of a line, the space before it goes too, so a draft
            // reads as it did before the image was attached.
            let line_end = matches!(bytes.get(range.end), None | Some(b'\n' | b'\r'));
            if line_end
                && range.start > 0
                && bytes[range.start - 1] == b' '
                && edits.last().is_none_or(|edit| edit.range.end < range.start)
            {
                range.start -= 1;
            }
            edits.push(Edit {
                range,
                text: String::new(),
            });
        } else if number > removed {
            edits.push(Edit {
                range,
                text: marker(number - 1),
            });
        }
    }
    edits
}

/// Apply ascending, non-overlapping `edits`, carrying each offset in
/// `offsets` (a caret or selection end) to the same place in the result.
pub fn apply(text: &str, edits: &[Edit], offsets: &mut [usize]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for edit in edits {
        out.push_str(&text[last..edit.range.start]);
        out.push_str(&edit.text);
        last = edit.range.end;
    }
    out.push_str(&text[last..]);
    for offset in offsets.iter_mut() {
        let mut shift: isize = 0;
        let mut mapped = None;
        for edit in edits {
            if *offset >= edit.range.end {
                shift += edit.text.len() as isize - edit.range.len() as isize;
            } else if *offset > edit.range.start {
                // Inside a replaced marker: keep it after the replacement.
                mapped = Some((edit.range.start as isize + shift) as usize + edit.text.len());
                break;
            } else {
                break;
            }
        }
        *offset = mapped.unwrap_or((*offset as isize + shift) as usize);
    }
    out
}

/// Whether `text` names any image by number.
pub fn has_markers(text: &str) -> bool {
    !markers(text).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_only_well_formed_markers() {
        let text =
            "a [Image #1] b [Image #12][Image #0] [Image #] [Image #01] [image #3] [Image #2";
        assert_eq!(markers(text), vec![(2..12, 1), (15..26, 12)]);
        assert_eq!(live_markers(text, 2), vec![(2..12, 1)]);
        assert_eq!(markers("[Image [Image #3]"), vec![(7..17, 3)]);
        assert_eq!(markers("ışık [Image #1]"), vec![(8..18, 1)]);
    }

    #[test]
    fn numbers_count_images_only() {
        let paths: Vec<PathBuf> = vec!["a.png".into(), "notes.txt".into(), "b.JPG".into()];
        assert_eq!(image_numbers(&paths), vec![Some(1), None, Some(2)]);
        assert_eq!(image_count(&paths), 2);
    }

    #[test]
    fn insertion_keeps_words_apart() {
        assert_eq!(insertion("", "", &[1]), "[Image #1] ");
        assert_eq!(insertion("look", "", &[1, 2]), " [Image #1] [Image #2] ");
        assert_eq!(insertion("look ", " here", &[3]), "[Image #3]");
        assert_eq!(insertion("look\n", "here", &[1]), "[Image #1] ");
        assert!(insertion("x", "y", &[]).is_empty());
    }

    #[test]
    fn removing_an_image_drops_its_markers_and_renumbers_later_ones() {
        let text = "[Image #1] compare [Image #2] with [Image #3] and [Image #2]";
        let edits = removal_edits(text, 2, 3);
        let mut caret = [text.len()];
        let out = apply(text, &edits, &mut caret);
        assert_eq!(out, "[Image #1] compare with [Image #2] and");
        assert_eq!(caret[0], out.len());
    }

    #[test]
    fn removal_restores_the_draft_around_the_marker() {
        let remove = |text: &str| apply(text, &removal_edits(text, 1, 1), &mut []);
        assert_eq!(remove("Keep this draft [Image #1] "), "Keep this draft");
        assert_eq!(remove("a [Image #1] b"), "a b");
        assert_eq!(remove("[Image #1] b"), "b");
        assert_eq!(remove("a [Image #1]\nb"), "a\nb");
        assert_eq!(remove("[Image #1] [Image #1] "), "");
    }

    #[test]
    fn removal_leaves_markers_beyond_the_count_as_typed() {
        let text = "[Image #1] [Image #5]";
        let out = apply(text, &removal_edits(text, 1, 2), &mut []);
        assert_eq!(out, "[Image #5]");
    }

    #[test]
    fn apply_moves_offsets_with_the_text() {
        let text = "[Image #1] x [Image #2] y";
        let edits = removal_edits(text, 1, 2);
        // Before x, inside the second marker, and at the end.
        let mut offsets = [11, 15, text.len()];
        let out = apply(text, &edits, &mut offsets);
        assert_eq!(out, "x [Image #1] y");
        assert_eq!(offsets, [0, 12, out.len()]);
    }
}
