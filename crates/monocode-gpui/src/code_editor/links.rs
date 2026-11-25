//! Navigation from a symbol: where it is defined and where it is used.
//! Providers answer with targets in any file; the editor turns them into
//! lines with their text, off the UI thread, and jumps or lists them.
use super::Position;
use gpui::{App, SharedString, Task, Window};
use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetPosition {
    /// Bytes of the target file's text.
    Range(Range<usize>),
    /// One-based line and column, the column counted in characters.
    LineColumn { line: usize, column: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkTarget {
    pub path: PathBuf,
    pub position: TargetPosition,
}

/// A place in a file, one-based, as file links spell it (`path:line:column`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Location {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
}
impl Location {
    pub(crate) fn of(path: &Path, text: &str, offset: usize) -> Self {
        let mut offset = offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        let start = text[..offset].rfind('\n').map_or(0, |n| n + 1);
        Self {
            path: path.to_owned(),
            line: text[..start].bytes().filter(|b| *b == b'\n').count() + 1,
            column: text[start..offset].chars().count() + 1,
        }
    }
    /// The byte offset of this place in `text`, clamped to its line.
    pub(crate) fn offset_in(&self, text: &str) -> usize {
        let mut start = 0;
        for _ in 1..self.line {
            match text[start..].find('\n') {
                Some(n) => start += n + 1,
                None => return text.len(),
            }
        }
        let end = text[start..].find('\n').map_or(text.len(), |n| start + n);
        start
            + text[start..end]
                .char_indices()
                .nth(self.column.saturating_sub(1))
                .map_or(end - start, |(at, _)| at)
    }
}

/// A target with its line looked up, ready to list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedTarget {
    pub location: Location,
    /// The target's line, trimmed.
    pub text: String,
}

pub(crate) trait LinkProvider {
    /// Whether the identifier at the position leads somewhere, asked while
    /// the modifier is held over it. Answering `true` without looking is fine.
    fn navigable(&self, _at: Position, _cx: &mut App) -> Task<bool> {
        Task::ready(true)
    }
    /// Where the symbol is defined. One target is jumped to, several are
    /// listed, none does nothing.
    fn targets(&self, at: Position, cx: &mut App) -> Task<Vec<LinkTarget>>;
}

/// The other places "Go To" leads from a symbol, besides its definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Navigation {
    Declaration,
    TypeDefinition,
    Implementation,
}
impl Navigation {
    /// The title of the list when there are several, and of the note when
    /// there are none.
    pub(crate) fn plural(self) -> &'static str {
        match self {
            Self::Declaration => "Declarations",
            Self::TypeDefinition => "Type Definitions",
            Self::Implementation => "Implementations",
        }
    }
    pub(crate) fn singular(self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::TypeDefinition => "type definition",
            Self::Implementation => "implementation",
        }
    }
}

pub(crate) trait NavigationProvider {
    /// Where `kind` leads from the symbol at the position. An error says
    /// why it cannot be asked, such as a server that does not offer it.
    fn locate(
        &self,
        kind: Navigation,
        at: Position,
        cx: &mut App,
    ) -> Task<Result<Vec<LinkTarget>, String>>;
}

pub(crate) trait ReferencesProvider {
    fn references(&self, at: Position, cx: &mut App) -> Task<Vec<LinkTarget>>;
}

/// The result of "Find usages", for whoever shows the full list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct References {
    pub symbol: String,
    pub origin: Location,
    pub targets: Vec<ResolvedTarget>,
}

/// Takes a usages result off the editor's hands. Returning `false` leaves
/// it to the editor's own list.
pub(crate) type ReferencesHandler = Rc<dyn Fn(&References, &mut Window, &mut App) -> bool>;

/// The identifier around `offset`, if it is on one.
pub(crate) fn word_at(text: &str, offset: usize) -> Option<Range<usize>> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let offset = offset.min(text.len());
    if !text.is_char_boundary(offset) {
        return None;
    }
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| word(*c))
        .last()
        .map_or(offset, |(at, _)| at);
    let end = text[offset..]
        .char_indices()
        .find(|(_, c)| !word(*c))
        .map_or(text.len(), |(at, _)| offset + at);
    (start < end).then_some(start..end)
}

/// Finds each target's line and its text. Reads the target files, so it runs
/// on a background thread; `open` is the asking editor's own unsaved buffer.
pub(crate) fn resolve(
    targets: Vec<LinkTarget>,
    open: (PathBuf, SharedString),
) -> Vec<ResolvedTarget> {
    use monocode_core::files::document::MAX_TEXT_BYTES;
    let mut files: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut resolved: Vec<ResolvedTarget> = Vec::with_capacity(targets.len());
    for target in targets {
        let text: Option<&str> = if target.path == open.0 {
            Some(open.1.as_ref())
        } else {
            files
                .entry(target.path.clone())
                .or_insert_with(|| {
                    std::fs::metadata(&target.path)
                        .ok()
                        .filter(|meta| meta.len() <= MAX_TEXT_BYTES as u64)
                        .and_then(|_| std::fs::read_to_string(&target.path).ok())
                        .map(|text| text.replace("\r\n", "\n"))
                })
                .as_deref()
        };
        let location = match (&target.position, text) {
            (TargetPosition::Range(range), Some(text)) => {
                Location::of(&target.path, text, range.start)
            }
            // Without the file there is no line to count to.
            (TargetPosition::Range(_), None) => Location {
                path: target.path.clone(),
                line: 1,
                column: 1,
            },
            (TargetPosition::LineColumn { line, column }, _) => Location {
                path: target.path.clone(),
                line: (*line).max(1),
                column: (*column).max(1),
            },
        };
        let line = text
            .and_then(|text| text.split('\n').nth(location.line - 1))
            .unwrap_or("")
            .trim();
        let text = line.chars().take(160).collect();
        if !resolved.iter().any(|seen| seen.location == location) {
            resolved.push(ResolvedTarget { location, text });
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_locations_count_characters_not_bytes() {
        let text = "x = 1\nnaïve_name = x\n";
        let at = text.find("_name").unwrap();
        assert_eq!(&text[word_at(text, at).unwrap()], "naïve_name");
        assert_eq!(word_at(text, text.find(" = x").unwrap() + 1), None);
        let location = Location::of(Path::new("/p/a.py"), text, at);
        assert_eq!((location.line, location.column), (2, 6));
        assert_eq!(location.offset_in(text), at);
        let past = Location {
            line: 2,
            column: 99,
            ..location
        };
        assert_eq!(past.offset_in(text), text.len() - 1);
    }

    #[test]
    fn targets_resolve_against_the_open_buffer_and_files_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("other.py");
        std::fs::write(&other, "import os\r\n\r\ndef helper():\r\n    pass\r\n").unwrap();
        let open = dir.path().join("open.py");
        let buffer = "from other import helper\nhelper()\n";
        let resolved = resolve(
            vec![
                LinkTarget {
                    path: other.clone(),
                    position: TargetPosition::Range(15..21),
                },
                LinkTarget {
                    path: open.clone(),
                    position: TargetPosition::LineColumn { line: 2, column: 1 },
                },
                LinkTarget {
                    path: other.clone(),
                    position: TargetPosition::LineColumn { line: 3, column: 5 },
                },
                LinkTarget {
                    path: dir.path().join("missing.py"),
                    position: TargetPosition::Range(40..44),
                },
            ],
            (open.clone(), buffer.into()),
        );
        assert_eq!(resolved.len(), 3, "the same place twice is listed once");
        assert_eq!(resolved[0].location.path, other);
        assert_eq!(
            (resolved[0].location.line, resolved[0].location.column),
            (3, 5)
        );
        assert_eq!(resolved[0].text, "def helper():");
        assert_eq!(resolved[1].text, "helper()");
        assert_eq!(resolved[2].location.line, 1);
        assert_eq!(resolved[2].text, "");
    }
}
