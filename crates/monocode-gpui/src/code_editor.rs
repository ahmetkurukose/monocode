//! The file editor's extension points. Code insight, run and debug plug into
//! the editor through what is declared here and never edit the editor itself:
//! they subscribe to the [`Hub`] for document events, push diagnostics, gutter
//! marks and line decorations into it by file path, and install providers for
//! completion, hover, signature help, symbol links, the gutter and the header.
//! Providers are called on the UI thread and must return at once, handing
//! back a `Task` for anything that waits on a process.
// The feature modules that call these land separately from the seams.
#![allow(dead_code, unused_imports)]

use gpui::{App, AppContext, Entity, Global, SharedString, Window};
use std::{
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
};

mod completion;
mod diagnostics;
mod gutter;
mod header;
mod history;
mod hover;
mod hub;
pub(crate) mod links;
pub(crate) mod markdown;
pub(crate) mod popup_place;
pub(crate) mod symbols;

pub(crate) use completion::{
    CompletionItem, CompletionProvider, CompletionRequest, CompletionTrigger,
    carry as carry_completions, rank,
};
pub(crate) use diagnostics::{
    DiagnosticCounts, ExternalDiagnostic, Severity, remap as remap_diagnostics,
};
pub(crate) use gutter::{
    Breakpoint, Decor, GutterButton, GutterClick, GutterMark, GutterMarkKind, GutterMenu,
    GutterProvider, GutterSlot, InlineNote, LineDecoration, LineStyle, NoteTone, RowShift,
    SLOT_WIDTH,
};
pub(crate) use header::{Crumb, HeaderAction, HeaderClick, HeaderProvider};
pub(crate) use history::History;
pub(crate) use hover::{Hover, HoverProvider, Signature, SignatureProvider};
pub(crate) use hub::{DocumentEvent, Hub, HubEvent, OpenDocument};
pub(crate) use links::{
    LinkProvider, LinkTarget, Location, References, ReferencesHandler, ReferencesProvider,
    ResolvedTarget, TargetPosition,
};

/// The file an editor shows: its absolute path, the checkout it was opened
/// from and its language server language name, when it has a grammar here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DocumentInfo {
    pub path: PathBuf,
    pub root: PathBuf,
    pub language: Option<&'static str>,
}
impl DocumentInfo {
    pub(crate) fn new(path: impl Into<PathBuf>, root: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            language: crate::editor_syntax::language_id(&path.to_string_lossy()),
            path,
            root: root.into(),
        }
    }
}

/// Where a provider is asked about: the buffer as the editor has it now, the
/// byte offset, and the version the last document event carried for it.
#[derive(Clone)]
pub(crate) struct Position {
    pub doc: Rc<DocumentInfo>,
    pub text: SharedString,
    pub offset: usize,
    pub version: u64,
}

/// One replacement in a buffer, in byte offsets of the text before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextEdit {
    pub range: Range<usize>,
    pub text: String,
}

/// Everything a feature can install on one editor. A slot left `None` keeps
/// the editor as it is without that feature.
#[derive(Clone, Default)]
pub(crate) struct Providers {
    pub completion: Option<Rc<dyn CompletionProvider>>,
    pub hover: Option<Rc<dyn HoverProvider>>,
    pub signature: Option<Rc<dyn SignatureProvider>>,
    pub links: Option<Rc<dyn LinkProvider>>,
    pub references: Option<Rc<dyn ReferencesProvider>>,
    pub breakpoints: Option<Rc<dyn GutterProvider>>,
    pub run_markers: Option<Rc<dyn GutterProvider>>,
    pub header: Vec<Rc<dyn HeaderProvider>>,
}
impl Providers {
    pub(crate) fn gutter(&self, slot: GutterSlot) -> Option<&Rc<dyn GutterProvider>> {
        match slot {
            GutterSlot::Breakpoint => self.breakpoints.as_ref(),
            GutterSlot::Run => self.run_markers.as_ref(),
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.completion.is_none()
            && self.hover.is_none()
            && self.signature.is_none()
            && self.links.is_none()
            && self.references.is_none()
            && self.breakpoints.is_none()
            && self.run_markers.is_none()
            && self.header.is_empty()
    }
}

type Contributor = Rc<dyn Fn(&DocumentInfo, &mut Providers, &mut App)>;

struct Shared {
    hub: Entity<Hub>,
    contributors: Vec<Contributor>,
}
impl Global for Shared {}

fn shared(cx: &mut App) -> &mut Shared {
    if !cx.has_global::<Shared>() {
        let hub = cx.new(|_| Hub::default());
        cx.set_global(Shared {
            hub,
            contributors: Vec::new(),
        });
    }
    cx.global_mut::<Shared>()
}

/// The one hub of the application, created on first use.
pub(crate) fn hub(cx: &mut App) -> Entity<Hub> {
    shared(cx).hub.clone()
}

/// Attaches a feature to every file editor: `contribute` is called for each
/// file as its editor is created (and again for open editors now) and fills
/// the slots it serves, typically after looking at `doc.language`.
pub(crate) fn register(
    cx: &mut App,
    contribute: impl Fn(&DocumentInfo, &mut Providers, &mut App) + 'static,
) {
    let shared = shared(cx);
    shared.contributors.push(Rc::new(contribute));
    let hub = shared.hub.clone();
    hub.update(cx, |_, cx| cx.emit(HubEvent::Providers));
}

/// What the registered features install for `doc`. Later registrations win a
/// slot two features both fill; header providers accumulate.
pub(crate) fn providers_for(doc: &DocumentInfo, cx: &mut App) -> Providers {
    let contributors = shared(cx).contributors.clone();
    let mut providers = Providers::default();
    for contribute in contributors {
        contribute(doc, &mut providers, cx);
    }
    providers
}

/// Opens `path` at a one-based line and column through the application's
/// file route, so it lands in the pane the shell picks for files.
pub(crate) fn open_location(location: &Location, window: &mut Window, cx: &mut App) {
    window.dispatch_action(
        Box::new(crate::file_editor::OpenPath(format!(
            "{}:{}:{}",
            location.path.display(),
            location.line,
            location.column
        ))),
        cx,
    );
}

/// The byte offset of a zero-based line and UTF-16 column, as language
/// servers and debug adapters count them. Past the end clamps to the end.
pub(crate) fn offset_at(text: &str, line: usize, character: usize) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(n) => start += n + 1,
            None => return text.len(),
        }
    }
    let mut units = 0;
    for (at, ch) in text[start..].char_indices() {
        if units >= character || ch == '\n' {
            return start + at;
        }
        units += ch.len_utf16();
    }
    text.len()
}

/// The zero-based line and UTF-16 column of a byte offset.
pub(crate) fn position_at(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let start = text[..offset].rfind('\n').map_or(0, |n| n + 1);
    (
        text[..start].bytes().filter(|b| *b == b'\n').count(),
        text[start..offset].encode_utf16().count(),
    )
}

/// The zero-based row of a byte offset, for row-keyed marks.
pub(crate) fn row_of(text: &str, offset: usize) -> usize {
    position_at(text, offset).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_convert_between_bytes_and_utf16_columns() {
        let text = "a = 1\nname = '🙂é'\nlast";
        let at = text.find('é').unwrap();
        assert_eq!(position_at(text, at), (1, 10));
        assert_eq!(offset_at(text, 1, 10), at);
        assert_eq!(offset_at(text, 1, 99), text.find("\nlast").unwrap());
        assert_eq!(offset_at(text, 9, 0), text.len());
        assert_eq!(position_at(text, text.len()), (2, 4));
        assert_eq!(row_of(text, 0), 0);
    }
}
