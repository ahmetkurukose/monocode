//! What is shown about the symbol under the mouse or at the caret, and the
//! signature of the call being typed. Both are answered asynchronously.
use super::{DocumentInfo, Position};
use gpui::{App, SharedString, Task};
use std::ops::Range;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Hover {
    /// What the text is about; the identifier at the position when `None`.
    /// The popover is anchored here and stays while the mouse is inside it.
    pub range: Option<Range<usize>>,
    pub markdown: String,
}

pub(crate) trait HoverProvider {
    fn hover(&self, at: Position, cx: &mut App) -> Task<Option<Hover>>;
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Signature {
    /// The whole signature, `connect(host: str, port: int = 5432) -> None`.
    pub label: String,
    /// Byte ranges of each parameter inside `label`.
    pub parameters: Vec<Range<usize>>,
    /// Which parameter the caret is on.
    pub active: Option<usize>,
    pub documentation: Option<String>,
}

pub(crate) trait SignatureProvider {
    /// Typing one of these asks for the signature.
    fn trigger_characters(&self, _doc: &DocumentInfo) -> SharedString {
        "(,".into()
    }
    /// `None` once the caret is outside a call, which closes the popover.
    fn signature(&self, at: Position, cx: &mut App) -> Task<Option<Signature>>;
}
