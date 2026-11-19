//! Composer.tsx's file mention: the `@` is drawn as the file's icon, with a
//! little air before the path. The editor keeps the original `@` byte.
use super::*;

// An em blank for the icon and a thin one for the gap replace the `@` when
// shaping. Like the command slot, every caret, selection and hit test maps
// through `DisplayLayout`.
const MENTION_SLOT: &str = "\u{2003}\u{2009}";
pub(super) const ICON_SIDE: f32 = 13.;
/// Bytes of the em blank the icon is centered in.
pub(super) const ICON_LEN: usize = '\u{2003}'.len_utf8();

#[derive(Clone)]
pub(super) enum MentionIcon {
    File(SharedString),
    Note,
}
impl MentionIcon {
    /// `@path`, `@"spaced path"`, or `@note/slug`.
    pub fn of(token: &str) -> Option<Self> {
        let path = token.strip_prefix('@')?;
        if path.starts_with("note/") {
            return Some(Self::Note);
        }
        let path = path
            .strip_prefix('"')
            .and_then(|p| p.strip_suffix('"'))
            .map_or_else(|| path.to_owned(), |p| p.replace("\\\"", "\""));
        let name = path.rsplit('/').next().unwrap_or(&path);
        Some(Self::File(
            format!("mi/{}.svg", crate::file_icons::file_icon_name(name)).into(),
        ))
    }
}

/// The shaped text of an `[Image #N]` marker: a pill with an icon slot and a
/// label. Only no-break blanks, so the pill never wraps across lines.
pub(super) const PILL_PAD: &str = "\u{202F}";
const PILL_ICON: &str = "\u{00A0}\u{00A0}\u{00A0}\u{00A0}";
pub(super) const PILL_ICON_SIDE: f32 = 12.;
fn pill_text(number: usize) -> String {
    format!("{PILL_PAD}{PILL_ICON}Image\u{00A0}#{number}{PILL_PAD}")
}

/// A source range shaped as different text. The caret never stops inside it.
#[derive(Clone)]
struct Slot {
    source: Range<usize>,
    display: SharedString,
}

/// Source ↔ shaped-text offsets for the command slot, mention slots and image
/// pills.
#[derive(Clone, Default)]
pub(super) struct DisplayLayout {
    pub command: Option<command::CommandLayout>,
    /// Source offsets of mention `@`s, ascending.
    pub mentions: Vec<(usize, MentionIcon)>,
    /// Source ranges of image markers, ascending.
    pub images: Vec<Range<usize>>,
    slots: Vec<Slot>,
}
impl DisplayLayout {
    pub(super) fn new(
        command: Option<command::CommandLayout>,
        mentions: Vec<(usize, MentionIcon)>,
        images: Vec<(Range<usize>, usize)>,
    ) -> Self {
        let mut slots: Vec<_> = mentions
            .iter()
            .map(|(at, _)| Slot {
                source: *at..at + 1,
                display: MENTION_SLOT.into(),
            })
            .chain(images.iter().map(|(range, number)| Slot {
                source: range.clone(),
                display: pill_text(*number).into(),
            }))
            .collect();
        slots.sort_by_key(|slot| slot.source.start);
        // A mention can never sit inside a marker, but keep slots disjoint.
        let mut end = 0;
        slots.retain(|slot| {
            let keep = slot.source.start >= end;
            if keep {
                end = slot.source.end;
            }
            keep
        });
        let images = images.into_iter().map(|(range, _)| range).collect();
        Self {
            command,
            mentions,
            images,
            slots,
        }
    }
    fn pre(&self, at: usize) -> usize {
        self.command.map_or(at, |command| command.to_display(at))
    }
    pub fn to_display(&self, at: usize) -> usize {
        let mut shift = 0isize;
        for slot in &self.slots {
            if at <= slot.source.start {
                break;
            }
            if at < slot.source.end {
                // Never inside a pill: the nearer edge.
                let start = (self.pre(slot.source.start) as isize + shift) as usize;
                return if at - slot.source.start <= slot.source.end - at {
                    start
                } else {
                    start + slot.display.len()
                };
            }
            shift += slot.display.len() as isize - slot.source.len() as isize;
        }
        (self.pre(at) as isize + shift) as usize
    }
    pub fn to_source(&self, at: usize) -> usize {
        let mut shift = 0isize;
        for slot in &self.slots {
            let start = (self.pre(slot.source.start) as isize + shift) as usize;
            if at <= start {
                break;
            }
            if at < start + slot.display.len() {
                // Inside the slot: snap to its nearer side.
                return if at - start <= slot.display.len() / 2 {
                    slot.source.start
                } else {
                    slot.source.end
                };
            }
            shift += slot.display.len() as isize - slot.source.len() as isize;
        }
        let at = (at as isize - shift).max(0) as usize;
        self.command.map_or(at, |command| command.to_source(at))
    }
    /// Shaped text with each slot's source swapped for its display.
    pub fn apply(&self, display: SharedString) -> SharedString {
        if self.slots.is_empty() {
            return display;
        }
        let mut text = String::with_capacity(display.len() + 24 * self.slots.len());
        let mut cursor = 0;
        for slot in &self.slots {
            let (start, end) = (self.pre(slot.source.start), self.pre(slot.source.end));
            if start < cursor || end > display.len() {
                continue;
            }
            text.push_str(&display[cursor..start]);
            text.push_str(&slot.display);
            cursor = end;
        }
        text.push_str(&display[cursor..]);
        text.into()
    }
    /// The display range of each image pill and of its icon slot.
    pub fn image_pills(&self) -> impl Iterator<Item = (Range<usize>, Range<usize>)> + '_ {
        self.images.iter().map(|range| {
            let start = self.to_display(range.start);
            let end = self.to_display(range.end);
            let icon = start + PILL_PAD.len();
            (start..end, icon..icon + PILL_ICON.len())
        })
    }
}

impl Input {
    pub(super) fn display_layout(&self) -> DisplayLayout {
        let command = self.command_layout();
        if self.secret || self.content.is_empty() {
            // The placeholder still sits after a retained mode's slot.
            return DisplayLayout::new(command, Vec::new(), Vec::new());
        }
        let mut mentions: Vec<_> = self
            .composer_tokens
            .iter()
            .filter(|range| {
                range.end <= self.content.len()
                    && self.content.is_char_boundary(range.start)
                    && self.content.is_char_boundary(range.end)
                    // A leading `/command` owns byte 0.
                    && !(range.start == 0 && command.is_some())
            })
            .filter_map(|range| Some((range.start, MentionIcon::of(&self.content[range.clone()])?)))
            .collect();
        mentions.sort_by_key(|(at, _)| *at);
        let images = self.image_markers();
        DisplayLayout::new(command, mentions, images)
    }
    /// The `[Image #N]` markers naming one of the attached images; a code
    /// editor or a field without images shows them as typed.
    pub(super) fn image_markers(&self) -> Vec<(Range<usize>, usize)> {
        if self.image_count == 0 || self.code.is_some() || self.secret {
            return Vec::new();
        }
        let command = self.command_layout().is_some_and(|c| c.has_source_prefix());
        monocode_core::composer::image_markers::live_markers(&self.content, self.image_count)
            .into_iter()
            .filter(|(range, _)| !(command && range.start == 0))
            .collect()
    }
}
