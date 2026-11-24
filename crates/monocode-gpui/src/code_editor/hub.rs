//! The meeting point between file editors and the features around them. An
//! editor reports its document's life here and features subscribe to it; a
//! feature stores what it knows about a file here (diagnostics, gutter
//! marks, line decorations) and every editor showing that file picks it up,
//! now or when it opens.
use super::{
    DiagnosticCounts, DocumentInfo, ExternalDiagnostic, GutterMark, GutterSlot, History,
    InlineNote, LineDecoration, Location, ReferencesHandler, TextEdit,
};
use gpui::{Context, EventEmitter, SharedString};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

/// The life of an editor's document. Text is always the whole buffer; a
/// change arrives once typing pauses, and always before a save or a
/// provider request that depends on it.
#[derive(Clone)]
pub(crate) enum DocumentEvent {
    Opened {
        doc: Rc<DocumentInfo>,
        text: SharedString,
        version: u64,
    },
    Changed {
        doc: Rc<DocumentInfo>,
        text: SharedString,
        /// One more than the version of the event before it.
        version: u64,
    },
    Saved {
        doc: Rc<DocumentInfo>,
        text: SharedString,
        version: u64,
    },
    Closed {
        doc: Rc<DocumentInfo>,
    },
}

/// What changed in the hub, for the editors that mirror it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HubEvent {
    Diagnostics {
        path: PathBuf,
        source: SharedString,
    },
    /// One column's marks of a file were replaced; the other column's are
    /// not meant by it.
    Gutter {
        path: PathBuf,
        slot: GutterSlot,
    },
    Decorations(PathBuf),
    /// Scroll this row (zero-based) into view in editors showing the file.
    Reveal {
        path: PathBuf,
        row: usize,
    },
    /// A feature registered; editors install their providers again.
    Providers,
    /// Edits for the editors that have the file open, in bytes of `base`:
    /// a rename or code action reaching this file (`refactor::apply`).
    Apply {
        path: PathBuf,
        base: SharedString,
        edits: Vec<TextEdit>,
    },
    History,
}

/// A document some editor has open, as the last event described it.
pub(crate) struct OpenDocument {
    pub doc: Rc<DocumentInfo>,
    pub text: SharedString,
    pub version: u64,
    editors: usize,
}

#[derive(Default)]
struct FileState {
    diagnostics: Vec<(SharedString, Vec<ExternalDiagnostic>)>,
    marks: [Vec<GutterMark>; 2],
    lines: Vec<(SharedString, Vec<LineDecoration>)>,
    notes: Vec<(SharedString, Vec<InlineNote>)>,
}
impl FileState {
    fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
            && self.marks.iter().all(Vec::is_empty)
            && self.lines.is_empty()
            && self.notes.is_empty()
    }
}

/// Replaces one source's list among several, dropping the entry when empty.
fn replace<T>(lists: &mut Vec<(SharedString, Vec<T>)>, source: SharedString, items: Vec<T>) {
    lists.retain(|(name, _)| *name != source);
    if !items.is_empty() {
        lists.push((source, items));
    }
}

#[derive(Default)]
pub(crate) struct Hub {
    open: HashMap<PathBuf, OpenDocument>,
    files: HashMap<PathBuf, FileState>,
    history: History,
    references: Option<ReferencesHandler>,
}
impl EventEmitter<DocumentEvent> for Hub {}
impl EventEmitter<HubEvent> for Hub {}

impl Hub {
    /// An editor loaded the file. Two editors on one file share a document:
    /// only the first opens it, and the last one to edit defines its text.
    /// Loading is not editing, so a later editor's text (from disk, maybe)
    /// does not replace what an earlier one has typed.
    pub(crate) fn document_opened(
        &mut self,
        doc: Rc<DocumentInfo>,
        text: SharedString,
        cx: &mut Context<Self>,
    ) -> u64 {
        if let Some(open) = self.open.get_mut(&doc.path) {
            open.editors += 1;
            return open.version;
        }
        self.open.insert(
            doc.path.clone(),
            OpenDocument {
                doc: doc.clone(),
                text: text.clone(),
                version: 1,
                editors: 1,
            },
        );
        cx.emit(DocumentEvent::Opened {
            doc,
            text,
            version: 1,
        });
        1
    }
    /// Reports the buffer's text. Returns the document's version, which
    /// only moves when the text differs from the last report.
    pub(crate) fn document_changed(
        &mut self,
        path: &Path,
        text: SharedString,
        cx: &mut Context<Self>,
    ) -> u64 {
        let Some(open) = self.open.get_mut(path) else {
            return 0;
        };
        if open.text != text {
            open.text = text.clone();
            open.version += 1;
            cx.emit(DocumentEvent::Changed {
                doc: open.doc.clone(),
                text,
                version: open.version,
            });
        }
        open.version
    }
    pub(crate) fn document_saved(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(open) = self.open.get(path) {
            cx.emit(DocumentEvent::Saved {
                doc: open.doc.clone(),
                text: open.text.clone(),
                version: open.version,
            });
        }
    }
    pub(crate) fn document_closed(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(open) = self.open.get_mut(path) else {
            return;
        };
        open.editors -= 1;
        if open.editors == 0
            && let Some(open) = self.open.remove(path)
        {
            cx.emit(DocumentEvent::Closed { doc: open.doc });
        }
    }
    /// The documents open right now, for a subscriber that starts late.
    pub(crate) fn open_documents(&self) -> impl Iterator<Item = &OpenDocument> {
        self.open.values()
    }
    pub(crate) fn document(&self, path: &Path) -> Option<&OpenDocument> {
        self.open.get(path)
    }

    fn file(&mut self, path: &Path) -> &mut FileState {
        self.files.entry(path.to_owned()).or_default()
    }
    fn forget_if_empty(&mut self, path: &Path) {
        if self.files.get(path).is_some_and(FileState::is_empty) {
            self.files.remove(path);
        }
    }

    /// Replaces everything `source` reported for the file. Ranges are bytes
    /// of the text at `version` (`None`: the text on disk). A list for a
    /// version the document has already left is dropped, since its source
    /// will report again for the newer text.
    pub(crate) fn set_diagnostics(
        &mut self,
        path: &Path,
        source: impl Into<SharedString>,
        version: Option<u64>,
        mut items: Vec<ExternalDiagnostic>,
        cx: &mut Context<Self>,
    ) {
        if let (Some(version), Some(open)) = (version, self.open.get(path))
            && version < open.version
        {
            return;
        }
        let source = source.into();
        items.sort_by_key(|item| item.range.start);
        replace(&mut self.file(path).diagnostics, source.clone(), items);
        self.forget_if_empty(path);
        cx.emit(HubEvent::Diagnostics {
            path: path.to_owned(),
            source,
        });
    }
    /// One source's diagnostics for a file, or all of them with `None`.
    pub(crate) fn diagnostics(&self, path: &Path, source: Option<&str>) -> Vec<ExternalDiagnostic> {
        let mut all: Vec<_> = self
            .files
            .get(path)
            .into_iter()
            .flat_map(|file| &file.diagnostics)
            .filter(|(name, _)| source.is_none_or(|source| name.as_ref() == source))
            .flat_map(|(_, items)| items.iter().cloned())
            .collect();
        all.sort_by_key(|item| item.range.start);
        all
    }
    /// Who has reported diagnostics for a file.
    pub(crate) fn diagnostic_sources(&self, path: &Path) -> Vec<SharedString> {
        self.files
            .get(path)
            .into_iter()
            .flat_map(|file| &file.diagnostics)
            .map(|(source, _)| source.clone())
            .collect()
    }
    /// Counts for a file whether or not it is open, for lists of files.
    pub(crate) fn diagnostic_counts(&self, path: &Path) -> DiagnosticCounts {
        let mut counts = DiagnosticCounts::default();
        for (_, items) in self
            .files
            .get(path)
            .into_iter()
            .flat_map(|f| &f.diagnostics)
        {
            for item in items {
                counts.add(item.severity);
            }
        }
        counts
    }

    /// Replaces a gutter column's marks for a file. Rows are zero-based.
    pub(crate) fn set_gutter_marks(
        &mut self,
        path: &Path,
        slot: GutterSlot,
        mut marks: Vec<GutterMark>,
        cx: &mut Context<Self>,
    ) {
        marks.sort_by_key(|mark| mark.row);
        marks.dedup_by_key(|mark| mark.row);
        self.file(path).marks[slot.index()] = marks;
        self.forget_if_empty(path);
        cx.emit(HubEvent::Gutter {
            path: path.to_owned(),
            slot,
        });
    }
    pub(crate) fn gutter_marks(&self, path: &Path, slot: GutterSlot) -> &[GutterMark] {
        self.files
            .get(path)
            .map_or(&[], |file| &file.marks[slot.index()])
    }

    /// Replaces the line highlights `source` set on a file.
    pub(crate) fn set_line_decorations(
        &mut self,
        path: &Path,
        source: impl Into<SharedString>,
        lines: Vec<LineDecoration>,
        cx: &mut Context<Self>,
    ) {
        replace(&mut self.file(path).lines, source.into(), lines);
        self.forget_if_empty(path);
        cx.emit(HubEvent::Decorations(path.to_owned()));
    }
    pub(crate) fn line_decorations(&self, path: &Path) -> Vec<LineDecoration> {
        let mut all: Vec<_> = self
            .files
            .get(path)
            .into_iter()
            .flat_map(|file| &file.lines)
            .flat_map(|(_, lines)| lines.iter().copied())
            .collect();
        // The execution line wins a row it shares with a selected frame.
        all.sort_by_key(|line| (line.row, line.style != super::LineStyle::Execution));
        all.dedup_by_key(|line| line.row);
        all
    }
    /// Replaces the notes `source` shows after line ends in a file.
    pub(crate) fn set_inline_notes(
        &mut self,
        path: &Path,
        source: impl Into<SharedString>,
        notes: Vec<InlineNote>,
        cx: &mut Context<Self>,
    ) {
        replace(&mut self.file(path).notes, source.into(), notes);
        self.forget_if_empty(path);
        cx.emit(HubEvent::Decorations(path.to_owned()));
    }
    pub(crate) fn inline_notes(&self, path: &Path) -> Vec<InlineNote> {
        let mut all: Vec<_> = self
            .files
            .get(path)
            .into_iter()
            .flat_map(|file| &file.notes)
            .flat_map(|(_, notes)| notes.iter().cloned())
            .collect();
        all.sort_by_key(|note| note.row);
        all
    }
    /// Scrolls a row into view in the editors showing the file, without
    /// moving their carets. A file nobody shows is opened with
    /// `code_editor::open_location` instead.
    pub(crate) fn reveal(&mut self, path: &Path, row: usize, cx: &mut Context<Self>) {
        cx.emit(HubEvent::Reveal {
            path: path.to_owned(),
            row,
        });
    }

    /// Records a jump so Back returns to `from`.
    pub(crate) fn jump(&mut self, from: Location, to: Location, cx: &mut Context<Self>) {
        self.history.jump(from, to);
        cx.emit(HubEvent::History);
    }
    /// Records where the caret of the focused editor is; see `History::visit`.
    pub(crate) fn visit(&mut self, place: Location, cx: &mut Context<Self>) {
        if self.history.visit(place) {
            cx.emit(HubEvent::History);
        }
    }
    pub(crate) fn back(
        &mut self,
        current: Option<Location>,
        cx: &mut Context<Self>,
    ) -> Option<Location> {
        cx.emit(HubEvent::History);
        self.history.back(current)
    }
    pub(crate) fn forward(
        &mut self,
        current: Option<Location>,
        cx: &mut Context<Self>,
    ) -> Option<Location> {
        cx.emit(HubEvent::History);
        self.history.forward(current)
    }
    pub(crate) fn history(&self) -> &History {
        &self.history
    }

    /// Who shows the full "Find usages" list instead of the editor's picker.
    pub(crate) fn set_references_handler(&mut self, handler: Option<ReferencesHandler>) {
        self.references = handler;
    }
    pub(crate) fn references_handler(&self) -> Option<ReferencesHandler> {
        self.references.clone()
    }
}
