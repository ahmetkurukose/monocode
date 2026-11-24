//! Changes a feature makes to code on the user's request: renaming a symbol
//! across files and the code actions (quick fixes, refactorings, source
//! actions) offered at the caret. Providers answer with steps in order:
//! edits in byte offsets of the text the file has at that step, and files
//! created, renamed or deleted. [`apply`] carries them out, edits into the
//! buffer of every editor that has the file open (so they can be undone
//! there and saved with it), the rest on disk.
use super::{Hub, HubEvent, Position, TextEdit};
use gpui::{App, EventEmitter, SharedString, Task};
use std::{
    any::Any,
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
};

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

/// One step of a change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileChange {
    Edit(FileEdit),
    /// A new empty file. One that exists is emptied with `overwrite`, left
    /// alone with `ignore_if_exists`, else the change stops there.
    Create {
        path: PathBuf,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    /// A file or folder moved; the flags are about `to`, as for `Create`.
    Rename {
        from: PathBuf,
        to: PathBuf,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    /// A file, or with `recursive` a folder and all it holds.
    Delete {
        path: PathBuf,
        recursive: bool,
        ignore_if_not_exists: bool,
    },
}

/// A file a change moved or deleted on disk, told on the hub once it is
/// done, for the editors that show it to follow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileMoved {
    Renamed { from: PathBuf, to: PathBuf },
    Deleted(PathBuf),
}
impl EventEmitter<FileMoved> for Hub {}

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
    /// The change that renames the symbol at the position to `name`.
    fn rename(
        &self,
        at: Position,
        name: String,
        cx: &mut App,
    ) -> Task<Result<Vec<FileChange>, String>>;
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

/// Asked once an action's change is applied and the editors have reported
/// it: the change of a command the action runs after its edit.
pub(crate) type FollowUp = Box<dyn FnOnce(&mut App) -> Task<Result<Vec<FileChange>, String>>>;

/// What taking an action does.
pub(crate) struct ActionChange {
    pub change: Vec<FileChange>,
    pub then: Option<FollowUp>,
}

pub(crate) trait CodeActionProvider {
    /// What can be done over `range` (bytes of `at.text`; empty at the
    /// caret); with `only`, just the actions of that kind or one under it
    /// (`refactor.extract` takes `refactor.extract.function` too).
    fn actions(
        &self,
        at: Position,
        range: Range<usize>,
        only: Option<SharedString>,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeActionItem>, String>>;
    /// The change taking `item` makes.
    fn edits(
        &self,
        at: Position,
        item: &CodeActionItem,
        cx: &mut App,
    ) -> Task<Result<ActionChange, String>>;
}

/// Whether `kind` is `only` or a kind under it.
pub(crate) fn kind_matches(kind: Option<&str>, only: &str) -> bool {
    kind.is_some_and(|kind| {
        kind == only
            || kind
                .strip_prefix(only)
                .is_some_and(|rest| rest.starts_with('.'))
    })
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

/// The edits taking `base` through `rounds`, each made on the text the one
/// before left: the round itself when there is one, else a single edit over
/// all they changed, so they are one step of undo. `None` when a round was
/// not made on the text the one before left.
fn composed(base: &str, rounds: &[(SharedString, Vec<TextEdit>)]) -> Option<Vec<TextEdit>> {
    let mut text = base.to_string();
    for (round_base, edits) in rounds {
        if round_base.as_ref() != text {
            return None;
        }
        text = applied(&text, edits)?;
    }
    if let [(_, edits)] = rounds {
        return Some(edits.clone());
    }
    let mut start = base
        .bytes()
        .zip(text.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let most = base.len().min(text.len()) - start;
    let mut end = (base.bytes().rev())
        .zip(text.bytes().rev())
        .take(most)
        .take_while(|(a, b)| a == b)
        .count();
    while !base.is_char_boundary(start) || !text.is_char_boundary(start) {
        start -= 1;
    }
    while !base.is_char_boundary(base.len() - end) || !text.is_char_boundary(text.len() - end) {
        end -= 1;
    }
    if start == base.len() && start == text.len() {
        return Some(Vec::new());
    }
    Some(vec![TextEdit {
        range: start..base.len() - end,
        text: text[start..text.len() - end].to_string(),
    }])
}

/// A step that is carried out on disk.
enum DiskStep {
    Write(FileEdit),
    Other(FileChange),
}

/// `path` at the place `from` moved to, when it is `from` or inside it.
fn moved(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(match rest.as_os_str().is_empty() {
        true => to.to_path_buf(),
        false => to.join(rest),
    })
}

/// Carries out a change: edits to open files through their editors, the
/// rest on disk in order, off the UI thread. Every edit is checked before
/// anything is changed, so a change whose edits do not fit is refused whole;
/// on disk the first step that fails stops the ones after it. An open file
/// that is renamed keeps its editor, which gets the edits meant for it at
/// either name. The task finishes when the disk is done and the moves are
/// told on the hub ([`FileMoved`]).
pub(crate) fn apply(
    change: Vec<FileChange>,
    cx: &mut App,
) -> Result<Task<Result<(), String>>, String> {
    for step in &change {
        if let FileChange::Edit(file) = step
            && applied(&file.base, &file.edits).is_none()
        {
            return Err(format!(
                "The edits for {} do not fit the file",
                file.path.display()
            ));
        }
    }
    let hub = super::hub(cx);
    // Which editor holds each path the steps reach, as the renames before
    // a step left them; `None` for a path on disk only.
    let mut holders: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    let holder = |holders: &HashMap<PathBuf, Option<PathBuf>>, path: &Path, cx: &App| match holders
        .get(path)
    {
        Some(held) => held.clone(),
        None => (hub.read(cx).document(path)).map(|_| path.to_path_buf()),
    };
    let mut editors: Vec<(PathBuf, Vec<(SharedString, Vec<TextEdit>)>)> = Vec::new();
    let mut disk = Vec::new();
    for step in change {
        match step {
            FileChange::Edit(file) if file.edits.is_empty() => {}
            FileChange::Edit(file) => match holder(&holders, &file.path, cx) {
                Some(open) => match editors.iter_mut().find(|(known, _)| *known == open) {
                    Some((_, rounds)) => rounds.push((file.base, file.edits)),
                    None => editors.push((open, vec![(file.base, file.edits)])),
                },
                None => disk.push(DiskStep::Write(file)),
            },
            FileChange::Rename {
                ref from, ref to, ..
            } => {
                let open: Vec<PathBuf> = (hub.read(cx).open_documents())
                    .map(|open| open.doc.path.clone())
                    .filter(|path| !holders.contains_key(path))
                    .collect();
                for path in open {
                    // What was at `to` is replaced; what is in `from` moves.
                    if path.starts_with(to) {
                        holders.insert(path, None);
                    } else if path.starts_with(from) {
                        holders.insert(path.clone(), Some(path));
                    }
                }
                let inside: Vec<(PathBuf, Option<PathBuf>)> = (holders.iter())
                    .filter(|(path, _)| path.starts_with(from))
                    .map(|(path, held)| (path.clone(), held.clone()))
                    .collect();
                for (path, held) in inside {
                    holders.insert(path.clone(), None);
                    if let Some(new) = moved(&path, from, to) {
                        holders.insert(new, held);
                    }
                }
                disk.push(DiskStep::Other(step));
            }
            FileChange::Create { ref path, .. } => {
                holders.insert(path.clone(), None);
                disk.push(DiskStep::Other(step));
            }
            FileChange::Delete { ref path, .. } => {
                let open: Vec<PathBuf> = (hub.read(cx).open_documents())
                    .map(|open| open.doc.path.clone())
                    .chain(holders.keys().cloned())
                    .filter(|known| known.starts_with(path))
                    .collect();
                for known in open {
                    holders.insert(known, None);
                }
                disk.push(DiskStep::Other(step));
            }
        }
    }
    let mut events = Vec::with_capacity(editors.len());
    for (path, rounds) in editors {
        let base = rounds[0].0.clone();
        let Some(edits) = composed(&base, &rounds) else {
            return Err(format!(
                "The edits for {} do not follow on each other",
                path.display()
            ));
        };
        events.push(HubEvent::Apply { path, base, edits });
    }
    for event in events {
        hub.update(cx, |_, cx| cx.emit(event));
    }
    let on_disk = cx
        .background_executor()
        .spawn(async move { carry_out(disk) });
    Ok(cx.spawn(async move |cx| {
        let (moves, failed) = on_disk.await;
        let _ = cx.update(|cx| {
            for moved in moves {
                hub.update(cx, |_, cx| cx.emit(moved));
            }
        });
        failed.map_or(Ok(()), Err)
    }))
}

/// The disk's steps in order, up to the first that fails: what moved, and
/// why it stopped.
fn carry_out(steps: Vec<DiskStep>) -> (Vec<FileMoved>, Option<String>) {
    let mut moves = Vec::new();
    let mut failed = Vec::new();
    for step in steps {
        match step {
            // A file the change does not create is left alone if it is no
            // longer the text the edits were made for, and the rest goes on.
            DiskStep::Write(file) => {
                let result = std::fs::read_to_string(&file.path)
                    .map_err(|e| e.to_string())
                    .and_then(|now| {
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
            DiskStep::Other(operation) => match operate(&operation) {
                Ok(Some(moved)) => moves.push(moved),
                Ok(None) => {}
                Err(error) => {
                    failed.push(error);
                    break;
                }
            },
        }
    }
    let failed =
        (!failed.is_empty()).then(|| format!("Some files were not changed: {}", failed.join("; ")));
    (moves, failed)
}

/// Creates, renames or deletes on disk, as the step's flags say.
fn operate(step: &FileChange) -> Result<Option<FileMoved>, String> {
    let exists = |path: &Path| std::fs::symlink_metadata(path).is_ok();
    let remove = |path: &Path| match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        _ => std::fs::remove_file(path),
    };
    let parent = |path: &Path| match path.parent() {
        Some(parent) => std::fs::create_dir_all(parent),
        None => Ok(()),
    };
    let failed = |path: &Path, error: std::io::Error| format!("{}: {error}", path.display());
    match step {
        FileChange::Edit(_) => Ok(None),
        FileChange::Create {
            path,
            overwrite,
            ignore_if_exists,
        } => {
            if exists(path) && !overwrite {
                return match ignore_if_exists {
                    true => Ok(None),
                    false => Err(format!("{} already exists", path.display())),
                };
            }
            parent(path).map_err(|e| failed(path, e))?;
            std::fs::write(path, "").map_err(|e| failed(path, e))?;
            Ok(None)
        }
        FileChange::Rename {
            from,
            to,
            overwrite,
            ignore_if_exists,
        } => {
            if !exists(from) {
                return Err(format!("{} does not exist", from.display()));
            }
            if exists(to) {
                if !overwrite {
                    return match ignore_if_exists {
                        true => Ok(None),
                        false => Err(format!("{} already exists", to.display())),
                    };
                }
                remove(to).map_err(|e| failed(to, e))?;
            }
            parent(to).map_err(|e| failed(to, e))?;
            std::fs::rename(from, to).map_err(|e| failed(from, e))?;
            Ok(Some(FileMoved::Renamed {
                from: from.clone(),
                to: to.clone(),
            }))
        }
        FileChange::Delete {
            path,
            recursive,
            ignore_if_not_exists,
        } => {
            let Ok(meta) = std::fs::symlink_metadata(path) else {
                return match ignore_if_not_exists {
                    true => Ok(None),
                    false => Err(format!("{} does not exist", path.display())),
                };
            };
            let removed = match (meta.is_dir(), recursive) {
                (true, true) => std::fs::remove_dir_all(path),
                (true, false) => std::fs::remove_dir(path),
                (false, _) => std::fs::remove_file(path),
            };
            removed.map_err(|e| failed(path, e))?;
            Ok(Some(FileMoved::Deleted(path.clone())))
        }
    }
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
    fn rounds_on_one_file_compose_into_one_edit() {
        let base: SharedString = "a ş c".into();
        let once = vec![edit(0..1, "b")];
        assert_eq!(
            composed(&base, &[(base.clone(), once.clone())]),
            Some(once),
            "one round is kept as it is"
        );
        let rounds = [
            (base.clone(), vec![edit(0..1, "b")]),
            ("b ş c".into(), vec![edit(2..4, "ğ")]),
        ];
        assert_eq!(composed(&base, &rounds), Some(vec![edit(0..4, "b ğ")]));
        let undone = [
            (base.clone(), vec![edit(0..1, "b")]),
            ("b ş c".into(), vec![edit(0..1, "a")]),
        ];
        assert_eq!(composed(&base, &undone), Some(Vec::new()));
        let astray = [
            (base.clone(), vec![edit(0..1, "b")]),
            (base.clone(), vec![edit(0..1, "c")]),
        ];
        assert_eq!(composed(&base, &astray), None);
    }

    #[test]
    fn a_rename_carries_what_is_inside_along() {
        let (from, to) = (Path::new("/r/old"), Path::new("/r/new"));
        assert_eq!(moved(Path::new("/r/old"), from, to), Some(to.into()));
        assert_eq!(
            moved(Path::new("/r/old/a.py"), from, to),
            Some("/r/new/a.py".into())
        );
        assert_eq!(moved(Path::new("/r/older.py"), from, to), None);
        assert!(kind_matches(
            Some("refactor.extract.function"),
            "refactor.extract"
        ));
        assert!(!kind_matches(
            Some("refactor.extractor"),
            "refactor.extract"
        ));
        assert!(!kind_matches(None, "refactor"));
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
