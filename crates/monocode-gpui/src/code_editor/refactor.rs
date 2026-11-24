//! Changes a feature makes to code on the user's request: renaming a symbol
//! across files and the code actions (quick fixes, refactorings, source
//! actions) offered at the caret. Providers answer with edits in byte
//! offsets of the text they were computed on; [`apply`] carries them out,
//! into the buffer of every editor that has the file open (so they can be
//! undone there and saved with it), and on disk for the files none has.
use super::{HubEvent, Position, TextEdit};
use gpui::{App, SharedString, Task};
use std::{any::Any, ops::Range, path::PathBuf, rc::Rc};

/// One file's share of a change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileEdit {
    pub path: PathBuf,
    /// The text the edits' offsets are in. A file whose text is no longer
    /// that is left alone, as the edits would land in the wrong places.
    pub base: SharedString,
    /// In order, none overlapping.
    pub edits: Vec<TextEdit>,
}

/// What a rename at the caret would replace, in bytes of the editor's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenameRange {
    pub range: Range<usize>,
    /// The name the field starts with.
    pub placeholder: String,
}

pub(crate) trait RenameProvider {
    /// What would be renamed at the position; `Ok(None)` when nothing can
    /// be renamed there.
    fn prepare(&self, at: Position, cx: &mut App) -> Task<Result<Option<RenameRange>, String>>;
    /// The edits that rename the symbol at the position to `name`.
    fn rename(
        &self,
        at: Position,
        name: String,
        cx: &mut App,
    ) -> Task<Result<Vec<FileEdit>, String>>;
}

/// One entry of the code actions list.
#[derive(Clone)]
pub(crate) struct CodeActionItem {
    pub title: SharedString,
    /// `quickfix`, `refactor.extract`, `source.organizeImports`, ...
    pub kind: Option<SharedString>,
    pub preferred: bool,
    /// Why it cannot be taken; it is listed greyed with this.
    pub disabled: Option<SharedString>,
    /// The provider's own record of the action, handed back to `edits`.
    pub data: Rc<dyn Any>,
}

/// Where an action is listed, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ActionGroup {
    Fix,
    Refactor,
    Source,
    Other,
}
impl ActionGroup {
    pub(crate) fn of(kind: Option<&str>) -> Self {
        let kind = kind.unwrap_or("");
        let is = |prefix: &str| kind == prefix || kind.starts_with(&format!("{prefix}."));
        if is("quickfix") {
            Self::Fix
        } else if is("refactor") {
            Self::Refactor
        } else if is("source") {
            Self::Source
        } else {
            Self::Other
        }
    }
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Fix => "Quick Fixes",
            Self::Refactor => "Refactor",
            Self::Source => "Source",
            Self::Other => "Other",
        }
    }
}

pub(crate) trait CodeActionProvider {
    /// What can be done over `range` (bytes of `at.text`; empty at the caret).
    fn actions(
        &self,
        at: Position,
        range: Range<usize>,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeActionItem>, String>>;
    /// The edits taking `item` makes.
    fn edits(
        &self,
        at: Position,
        item: &CodeActionItem,
        cx: &mut App,
    ) -> Task<Result<Vec<FileEdit>, String>>;
}

/// `base` with `edits` applied: edits in order, none overlapping, all on
/// character boundaries. `None` when they are not.
pub(crate) fn applied(base: &str, edits: &[TextEdit]) -> Option<String> {
    let mut out = String::with_capacity(base.len());
    let mut at = 0;
    for edit in edits {
        let Range { start, end } = edit.range;
        if start < at
            || end < start
            || end > base.len()
            || !base.is_char_boundary(start)
            || !base.is_char_boundary(end)
        {
            return None;
        }
        out.push_str(&base[at..start]);
        out.push_str(&edit.text);
        at = end;
    }
    out.push_str(&base[at..]);
    Some(out)
}

/// Carries out a change: open files through their editors, the others on
/// disk, off the UI thread. Every file is checked before any is changed, so
/// a change whose edits do not fit is refused whole. The task finishes
/// when the files on disk are written.
pub(crate) fn apply(
    change: Vec<FileEdit>,
    cx: &mut App,
) -> Result<Task<Result<(), String>>, String> {
    let change: Vec<FileEdit> = change
        .into_iter()
        .filter(|file| !file.edits.is_empty())
        .collect();
    for file in &change {
        if applied(&file.base, &file.edits).is_none() {
            return Err(format!(
                "The edits for {} do not fit the file",
                file.path.display()
            ));
        }
    }
    let hub = super::hub(cx);
    let (open, closed): (Vec<_>, Vec<_>) = change
        .into_iter()
        .partition(|file| hub.read(cx).document(&file.path).is_some());
    for file in open {
        hub.update(cx, |_, cx| {
            cx.emit(HubEvent::Apply {
                path: file.path,
                base: file.base,
                edits: file.edits,
            })
        });
    }
    Ok(cx.background_executor().spawn(async move {
        let mut failed = Vec::new();
        for file in closed {
            let now = std::fs::read_to_string(&file.path).map_err(|e| e.to_string());
            let result = now.and_then(|now| {
                if now != file.base.as_ref() {
                    return Err("it changed on disk".into());
                }
                let text = applied(&now, &file.edits).ok_or("the edits do not fit")?;
                std::fs::write(&file.path, text).map_err(|e| e.to_string())
            });
            if let Err(error) = result {
                failed.push(format!("{}: {error}", file.path.display()));
            }
        }
        match failed.is_empty() {
            true => Ok(()),
            false => Err(format!(
                "Some files were not changed: {}",
                failed.join("; ")
            )),
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(range: Range<usize>, text: &str) -> TextEdit {
        TextEdit {
            range,
            text: text.into(),
        }
    }

    #[test]
    fn edits_apply_in_order_and_misfits_are_refused() {
        assert_eq!(
            applied("let ş = ş;", &[edit(4..6, "go"), edit(9..11, "go")]).as_deref(),
            Some("let go = go;")
        );
        assert_eq!(applied("abc", &[]).as_deref(), Some("abc"));
        assert!(applied("abc", &[edit(1..2, "x"), edit(0..1, "y")]).is_none());
        assert!(applied("abc", &[edit(2..9, "x")]).is_none());
        assert!(
            applied("ş", &[edit(1..2, "x")]).is_none(),
            "inside a character"
        );
    }

    #[test]
    fn action_kinds_fall_into_groups() {
        assert_eq!(ActionGroup::of(Some("quickfix")), ActionGroup::Fix);
        assert_eq!(
            ActionGroup::of(Some("refactor.extract")),
            ActionGroup::Refactor
        );
        assert_eq!(
            ActionGroup::of(Some("source.organizeImports")),
            ActionGroup::Source
        );
        assert_eq!(ActionGroup::of(Some("refactoring")), ActionGroup::Other);
        assert_eq!(ActionGroup::of(None), ActionGroup::Other);
    }
}
