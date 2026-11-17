//! Completion answered asynchronously, by a language server for instance.
//! The editor asks after a pause in typing or on the provider's trigger
//! characters, drops an answer the caret has moved past, and narrows and
//! orders what comes back by what is typed.
use super::{DocumentInfo, Position, TextEdit};
use gpui::{App, SharedString, Task};
use std::{any::Any, ops::Range, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionTrigger {
    /// Asked for with the key command.
    Invoked,
    /// A word character was typed.
    Typed,
    /// One of the provider's trigger characters was typed.
    Character(char),
}

#[derive(Clone)]
pub(crate) struct CompletionRequest {
    pub at: Position,
    pub trigger: CompletionTrigger,
}

#[derive(Clone, Default)]
pub(crate) struct CompletionItem {
    pub label: String,
    /// "function", "method", "class", "variable", "keyword", "module",
    /// "property", "field", "constant" or anything else for a plain dot.
    pub kind: String,
    pub detail: String,
    /// Markdown shown beside the list. `None` asks the provider's `resolve`
    /// when the item is selected.
    pub documentation: Option<String>,
    pub insert: String,
    /// Bytes of the request's text that `insert` replaces.
    pub replace: Range<usize>,
    /// Caret offset inside `insert` afterwards; its end when `None`.
    pub cursor: Option<usize>,
    /// Matched against what is typed instead of the label.
    pub filter: Option<String>,
    /// Orders items instead of the label.
    pub sort: Option<String>,
    /// Edits elsewhere in the file applied with the item, such as an import.
    /// They must not overlap `replace` or each other.
    pub additional_edits: Vec<TextEdit>,
    /// The provider's own handle on the item, for `resolve`.
    pub data: Option<Arc<dyn Any + Send + Sync>>,
}
impl From<crate::editor_analysis::Completion> for CompletionItem {
    fn from(option: crate::editor_analysis::Completion) -> Self {
        Self {
            label: option.label,
            kind: option.kind,
            detail: option.detail,
            replace: option.from..option.to,
            cursor: Some(option.cursor),
            insert: option.insert,
            ..Self::default()
        }
    }
}

pub(crate) trait CompletionProvider {
    /// Characters besides word characters that ask for completion when typed.
    fn trigger_characters(&self, _doc: &DocumentInfo) -> SharedString {
        SharedString::default()
    }
    fn complete(&self, request: CompletionRequest, cx: &mut App) -> Task<Vec<CompletionItem>>;
    /// The documentation of an item that came without it. `None` when there
    /// is nothing to look up.
    fn resolve(
        &self,
        _doc: &DocumentInfo,
        _item: &CompletionItem,
        _cx: &mut App,
    ) -> Option<Task<Option<String>>> {
        None
    }
}

/// Keeps the items that match what is typed between an item's start and the
/// caret, prefix matches first, then by sort key. Servers answer for a
/// position, not for the word, so the narrowing is the editor's.
pub(crate) fn rank(
    mut items: Vec<CompletionItem>,
    text: &str,
    offset: usize,
) -> Vec<CompletionItem> {
    let typed = |item: &CompletionItem| {
        text.get(item.replace.start..offset)
            .unwrap_or("")
            .to_lowercase()
    };
    let key = |item: &CompletionItem| item.filter.as_deref().unwrap_or(&item.label).to_lowercase();
    items.retain(|item| {
        let key = key(item);
        let mut rest = key.chars();
        typed(item).chars().all(|c| rest.any(|k| k == c))
    });
    items.sort_by_cached_key(|item| {
        (
            !key(item).starts_with(&typed(item)),
            item.sort.clone().unwrap_or_else(|| item.label.clone()),
        )
    });
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, sort: Option<&str>) -> CompletionItem {
        CompletionItem {
            label: label.into(),
            insert: label.into(),
            replace: 3..5,
            sort: sort.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn items_narrow_to_what_is_typed_and_prefer_prefixes() {
        let ranked = rank(
            vec![
                item("split", None),
                item("partition", Some("1")),
                item("path", Some("2")),
                item("Path", Some("0")),
                item("zip", None),
            ],
            "os.pa",
            5,
        );
        let labels: Vec<_> = ranked.iter().map(|item| item.label.as_str()).collect();
        // "split" has no `a` after its `p`; "zip" has no `a` at all.
        assert_eq!(labels, ["Path", "partition", "path"]);
    }
}
