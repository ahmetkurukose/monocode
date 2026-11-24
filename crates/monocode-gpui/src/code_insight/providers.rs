//! The editor's completion, hover, signature, definition, usages, rename
//! and code action providers, answered by the language servers. Every request runs on a
//! background thread with a cancel token that is cancelled when the editor
//! drops the task it was handed, so a burst of typing leaves no queue of
//! requests behind it.
use super::{convert, sync::serves};
use crate::code_editor::{
    self, CodeActionItem, CodeActionProvider, CompletionItem, CompletionProvider,
    CompletionRequest, CompletionTrigger, DocumentInfo, FileEdit, Hover, HoverProvider,
    LinkProvider, LinkTarget, Navigation, NavigationProvider, Position, ReferencesProvider,
    RenameProvider, RenameRange, Signature, SignatureProvider, TargetPosition, TextEdit,
    refactor::{self, ActionChange, FileChange, FollowUp},
};
use gpui::{App, SharedString, Task};
use monocode_core::{
    code::lsp::{self, CancelToken, DocumentChange, LineIndex, Point, ServerState},
    files::document::MAX_TEXT_BYTES,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

/// How long a command that follows an action's edit waits for the server
/// to have the edited text; the editors report edits a moment after.
const EDIT_REACHES_SERVER: Duration = Duration::from_secs(3);

/// Installs the providers on every file a language server may run for.
pub(super) fn register(manager: lsp::Manager, cx: &mut App) {
    let provider = Rc::new(Provider { manager });
    code_editor::register(cx, move |doc, providers, cx| {
        if !serves(doc, cx) {
            return;
        }
        providers.completion = Some(provider.clone());
        providers.hover = Some(provider.clone());
        providers.signature = Some(provider.clone());
        providers.links = Some(provider.clone());
        providers.references = Some(provider.clone());
        providers.navigation = Some(provider.clone());
        providers.rename = Some(provider.clone());
        providers.actions = Some(provider.clone());
    });
}

struct Provider {
    manager: lsp::Manager,
}

/// Cancels the request when the task waiting for it is dropped.
struct CancelOnDrop(CancelToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Runs a blocking question off the UI thread. Dropping the returned task
/// cancels the question, which makes the blocked call return.
fn ask<T: Send + 'static>(
    manager: &lsp::Manager,
    cx: &App,
    work: impl FnOnce(&lsp::Manager, &CancelToken) -> T + Send + 'static,
) -> Task<T> {
    let manager = manager.clone();
    let token = CancelToken::new();
    let guard = CancelOnDrop(token.clone());
    let executor = cx.background_executor().clone();
    let call = executor.spawn(async move { work(&manager, &token) });
    executor.spawn(async move {
        let _guard = guard;
        call.await
    })
}

impl Provider {
    fn ask<T: Send + 'static>(
        &self,
        cx: &App,
        work: impl FnOnce(&lsp::Manager, &CancelToken) -> T + Send + 'static,
    ) -> Task<T> {
        ask(&self.manager, cx, work)
    }

    /// False while the file's server is still starting. A question would
    /// wait for it, so the ones the editor asks unprompted are skipped
    /// meanwhile instead of holding a thread each.
    fn settled(&self, path: &Path) -> bool {
        !self.manager.statuses().iter().any(|status| {
            matches!(status.state, ServerState::Starting) && path.starts_with(&status.root)
        })
    }
}

/// What turning server locations into editor targets needs from the UI
/// thread: the unsaved texts, and how the editors spell the checkouts.
struct Places {
    open: HashMap<PathBuf, SharedString>,
    spellings: Vec<(PathBuf, PathBuf)>,
}

impl Places {
    fn new(cx: &mut App) -> Self {
        let hub = code_editor::hub(cx);
        let open = (hub.read(cx).open_documents())
            .map(|open| (open.doc.path.clone(), open.text.clone()))
            .collect();
        let spellings = super::sync(cx)
            .map(|sync| sync.read(cx).spellings())
            .unwrap_or_default();
        Self { open, spellings }
    }

    /// A path as the editors know it. Servers resolve links (`/private/var`
    /// for `/var`), and a file opened under that other spelling would be a
    /// second document.
    fn spell(&self, path: PathBuf) -> PathBuf {
        if self.open.contains_key(&path) {
            return path;
        }
        for (real, known) in &self.spellings {
            if let Ok(rest) = path.strip_prefix(real) {
                return known.join(rest);
            }
        }
        path
    }

    /// Targets with one-based lines and character columns. The column is
    /// counted on the file's text, the unsaved one when an editor has it.
    /// Reads files: for a background thread.
    fn targets(&self, found: Vec<lsp::Location>) -> Vec<LinkTarget> {
        let mut read: HashMap<PathBuf, Option<(String, LineIndex)>> = HashMap::new();
        let mut targets = Vec::with_capacity(found.len());
        for location in found {
            let start = location.range.start;
            let path = self.spell(location.path.clone());
            let column_in = |text: &str, index: &LineIndex| {
                let line = index.offset_of(text, Point { column: 0, ..start });
                let at = index.offset_of(text, start).clamp(line, text.len());
                text.get(line..at).map(|before| before.chars().count())
            };
            let column = match self.open.get(&path) {
                Some(text) => column_in(text, &LineIndex::new(text)),
                None => read
                    .entry(location.path.clone())
                    .or_insert_with(|| {
                        let text = std::fs::metadata(&location.path)
                            .ok()
                            .filter(|meta| meta.len() <= MAX_TEXT_BYTES as u64)
                            .and_then(|_| std::fs::read_to_string(&location.path).ok())?;
                        let index = LineIndex::new(&text);
                        Some((text, index))
                    })
                    .as_ref()
                    .and_then(|(text, index)| column_in(text, index)),
            };
            targets.push(LinkTarget {
                path,
                position: TargetPosition::LineColumn {
                    line: start.line as usize + 1,
                    column: column.unwrap_or(start.column as usize) + 1,
                },
            });
        }
        targets
    }

    /// A server's change as steps for `refactor::apply`, edits in bytes of
    /// each file's text at that step: the editors' unsaved text for open
    /// files, the disk's for the others, then what the steps before made
    /// of it. Steps the server's flags skip are left out, and one that
    /// cannot be taken refuses the change. Reads files: for a background
    /// thread.
    fn changes(&self, change: lsp::WorkspaceEdit) -> Result<Vec<FileChange>, String> {
        let mut known: HashMap<PathBuf, Known> = HashMap::new();
        // The renames and deletions so far, to find where a path not met
        // yet was before them.
        let mut done: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
        let mut steps = Vec::new();
        for step in change.changes {
            match step {
                DocumentChange::Edit { path, edits } => {
                    let path = self.spell(path);
                    let base = match self.state(&mut known, &done, &path) {
                        Known::Text(text) => text,
                        Known::Missing => {
                            return Err(format!("{} does not exist", path.display()));
                        }
                        Known::Other => {
                            return Err(format!("{} is not a text file", path.display()));
                        }
                    };
                    if edits.is_empty() {
                        continue;
                    }
                    let index = LineIndex::new(&base);
                    let edits: Vec<TextEdit> = (edits.into_iter())
                        .map(|edit| TextEdit {
                            range: index.offset_of(&base, edit.range.start)
                                ..index.offset_of(&base, edit.range.end),
                            text: edit.text,
                        })
                        .collect();
                    let Some(after) = refactor::applied(&base, &edits) else {
                        return Err(format!(
                            "The edits for {} do not fit the file",
                            path.display()
                        ));
                    };
                    known.insert(path.clone(), Known::Text(after.into()));
                    steps.push(FileChange::Edit(FileEdit { path, base, edits }));
                }
                DocumentChange::Create {
                    path,
                    overwrite,
                    ignore_if_exists,
                } => {
                    let path = self.spell(path);
                    let exists = !matches!(self.state(&mut known, &done, &path), Known::Missing);
                    if exists && !overwrite {
                        if ignore_if_exists {
                            continue;
                        }
                        return Err(format!("{} already exists", path.display()));
                    }
                    known.insert(path.clone(), Known::Text(SharedString::default()));
                    steps.push(FileChange::Create {
                        path,
                        overwrite,
                        ignore_if_exists,
                    });
                }
                DocumentChange::Rename {
                    from,
                    to,
                    overwrite,
                    ignore_if_exists,
                } => {
                    let (from, to) = (self.spell(from), self.spell(to));
                    if matches!(self.state(&mut known, &done, &from), Known::Missing) {
                        return Err(format!("{} does not exist", from.display()));
                    }
                    let taken = !matches!(self.state(&mut known, &done, &to), Known::Missing);
                    if taken && !overwrite {
                        if ignore_if_exists {
                            continue;
                        }
                        return Err(format!("{} already exists", to.display()));
                    }
                    let inside: Vec<PathBuf> = (known.keys())
                        .filter(|path| path.starts_with(&from) || path.starts_with(&to))
                        .cloned()
                        .collect();
                    let mut arrived = Vec::new();
                    for path in inside {
                        let state = known.insert(path.clone(), Known::Missing);
                        if let Some(state) = state
                            && let Some(new) = moved_to(&path, &from, &to)
                        {
                            arrived.push((new, state));
                        }
                    }
                    known.extend(arrived);
                    done.push((from.clone(), Some(to.clone())));
                    steps.push(FileChange::Rename {
                        from,
                        to,
                        overwrite,
                        ignore_if_exists,
                    });
                }
                DocumentChange::Delete {
                    path,
                    recursive,
                    ignore_if_not_exists,
                } => {
                    let path = self.spell(path);
                    if matches!(self.state(&mut known, &done, &path), Known::Missing) {
                        if ignore_if_not_exists {
                            continue;
                        }
                        return Err(format!("{} does not exist", path.display()));
                    }
                    for (known_path, state) in known.iter_mut() {
                        if known_path.starts_with(&path) {
                            *state = Known::Missing;
                        }
                    }
                    done.push((path.clone(), None));
                    steps.push(FileChange::Delete {
                        path,
                        recursive,
                        ignore_if_not_exists,
                    });
                }
            }
        }
        Ok(steps)
    }

    /// What `path` holds at this step of a change: what a step before made
    /// of it, else what it had before the change, found at the place the
    /// renames so far moved it from.
    fn state(
        &self,
        known: &mut HashMap<PathBuf, Known>,
        done: &[(PathBuf, Option<PathBuf>)],
        path: &Path,
    ) -> Known {
        if let Some(state) = known.get(path) {
            return state.clone();
        }
        let mut origin = path.to_path_buf();
        for (from, to) in done.iter().rev() {
            match to {
                Some(to) => match moved_to(&origin, to, from) {
                    Some(before) => origin = before,
                    None if origin.starts_with(from) => return Known::Missing,
                    None => {}
                },
                None if origin.starts_with(from) => return Known::Missing,
                None => {}
            }
        }
        let state = match self.open.get(&origin) {
            Some(text) => Known::Text(text.clone()),
            None => match std::fs::read_to_string(&origin) {
                Ok(text) => Known::Text(text.into()),
                Err(_) if std::fs::symlink_metadata(&origin).is_ok() => Known::Other,
                Err(_) => Known::Missing,
            },
        };
        known.insert(path.to_path_buf(), state.clone());
        state
    }

    /// These texts in place of what the editors last reported for them.
    fn with_texts(mut self, texts: &[(PathBuf, SharedString)]) -> Self {
        for (path, text) in texts {
            if self.open.contains_key(path) {
                self.open.insert(path.clone(), text.clone());
            }
        }
        self
    }
}

/// What a path holds partway through a change.
#[derive(Clone)]
enum Known {
    Missing,
    Text(SharedString),
    /// A folder, or a file that is not text.
    Other,
}

/// `path` at the place `from` moved to, when it is `from` or inside it.
fn moved_to(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(match rest.as_os_str().is_empty() {
        true => to.to_path_buf(),
        false => to.join(rest),
    })
}

/// The text each edited file has once `change` is applied.
fn edited_texts(change: &[FileChange]) -> Vec<(PathBuf, SharedString)> {
    let mut texts: Vec<(PathBuf, SharedString)> = Vec::new();
    for step in change {
        let FileChange::Edit(file) = step else {
            continue;
        };
        let Some(after) = refactor::applied(&file.base, &file.edits) else {
            continue;
        };
        match texts.iter_mut().find(|(path, _)| *path == file.path) {
            Some((_, text)) => *text = after.into(),
            None => texts.push((file.path.clone(), after.into())),
        }
    }
    texts
}

impl CompletionProvider for Provider {
    fn trigger_characters(&self, doc: &DocumentInfo) -> SharedString {
        self.manager.triggers(&doc.path).completion.concat().into()
    }

    fn complete(&self, request: CompletionRequest, cx: &mut App) -> Task<Vec<CompletionItem>> {
        let path = request.at.doc.path.clone();
        let (text, offset) = (request.at.text, request.at.offset);
        let trigger = match request.trigger {
            CompletionTrigger::Character(typed) => Some(typed),
            CompletionTrigger::Invoked | CompletionTrigger::Typed => None,
        };
        if request.trigger != CompletionTrigger::Invoked && !self.settled(&path) {
            return Task::ready(Vec::new());
        }
        self.ask(cx, move |manager, cancel| {
            let Ok(list) = manager.completion(&path, offset, trigger, cancel) else {
                return Vec::new();
            };
            let index = LineIndex::new(&text);
            (list.items.into_iter())
                .map(|item| convert::completion(item, &index, &text, offset))
                .collect()
        })
    }

    fn resolve(
        &self,
        doc: &DocumentInfo,
        item: &CompletionItem,
        cx: &mut App,
    ) -> Option<Task<Option<String>>> {
        let found = (item.data.clone()?)
            .downcast::<lsp::CompletionItem>()
            .ok()?;
        let path = doc.path.clone();
        Some(self.ask(cx, move |manager, cancel| {
            let resolved = manager.resolve_completion(&path, &found, cancel).ok()?;
            resolved.documentation
        }))
    }
}

impl HoverProvider for Provider {
    fn hover(&self, at: Position, cx: &mut App) -> Task<Option<Hover>> {
        let path = at.doc.path.clone();
        if !self.settled(&path) {
            return Task::ready(None);
        }
        let (text, offset) = (at.text, at.offset);
        self.ask(cx, move |manager, cancel| {
            let found = manager.hover(&path, offset, cancel).ok()??;
            let range =
                (found.range).map(|span| convert::range(&LineIndex::new(&text), &text, span));
            Some(Hover {
                range,
                markdown: found.contents,
            })
        })
    }
}

impl SignatureProvider for Provider {
    fn trigger_characters(&self, doc: &DocumentInfo) -> SharedString {
        let triggers = self.manager.triggers(&doc.path).signature_help;
        match triggers.is_empty() {
            true => "(,".into(),
            false => triggers.concat().into(),
        }
    }

    fn signature(&self, at: Position, cx: &mut App) -> Task<Option<Signature>> {
        let path = at.doc.path.clone();
        if !self.settled(&path) {
            return Task::ready(None);
        }
        let offset = at.offset;
        self.ask(cx, move |manager, cancel| {
            convert::signature(manager.signature_help(&path, offset, cancel).ok()??)
        })
    }
}

impl LinkProvider for Provider {
    fn navigable(&self, at: Position, cx: &mut App) -> Task<bool> {
        let path = at.doc.path.clone();
        // While the server starts, the underline promises what the click
        // will then wait for.
        if !self.settled(&path) {
            return Task::ready(true);
        }
        let offset = at.offset;
        self.ask(cx, move |manager, cancel| {
            (manager.definition(&path, offset, cancel)).is_ok_and(|found| !found.is_empty())
        })
    }

    fn targets(&self, at: Position, cx: &mut App) -> Task<Vec<LinkTarget>> {
        let path = at.doc.path.clone();
        let offset = at.offset;
        let places = Places::new(cx);
        self.ask(cx, move |manager, cancel| {
            let found = manager.definition(&path, offset, cancel);
            places.targets(found.unwrap_or_default())
        })
    }
}

impl ReferencesProvider for Provider {
    fn references(&self, at: Position, cx: &mut App) -> Task<Vec<LinkTarget>> {
        let path = at.doc.path.clone();
        let offset = at.offset;
        let places = Places::new(cx);
        self.ask(cx, move |manager, cancel| {
            let found = manager.references(&path, offset, true, cancel);
            places.targets(found.unwrap_or_default())
        })
    }
}

impl RenameProvider for Provider {
    fn prepare(&self, at: Position, cx: &mut App) -> Task<Result<Option<RenameRange>, String>> {
        let path = at.doc.path.clone();
        let (text, offset) = (at.text, at.offset);
        self.ask(cx, move |manager, cancel| {
            let Some(target) = manager
                .prepare_rename(&path, offset, cancel)
                .map_err(|error| error.to_string())?
            else {
                return Ok(None);
            };
            let range = match target.range {
                Some(span) => {
                    let index = LineIndex::new(&text);
                    index.offset_of(&text, span.start)..index.offset_of(&text, span.end)
                }
                None => match code_editor::links::word_at(&text, offset) {
                    Some(word) => word,
                    None => return Ok(None),
                },
            };
            let placeholder = target
                .placeholder
                .or_else(|| text.get(range.clone()).map(str::to_string))
                .unwrap_or_default();
            Ok(Some(RenameRange { range, placeholder }))
        })
    }

    fn rename(
        &self,
        at: Position,
        name: String,
        cx: &mut App,
    ) -> Task<Result<Vec<FileChange>, String>> {
        let path = at.doc.path.clone();
        let offset = at.offset;
        let places = Places::new(cx);
        self.ask(cx, move |manager, cancel| {
            let change = manager
                .rename(&path, offset, &name, cancel)
                .map_err(|error| error.to_string())?;
            if change.is_empty() {
                return Err("The language server found nothing to rename".into());
            }
            places.changes(change)
        })
    }
}

impl CodeActionProvider for Provider {
    fn actions(
        &self,
        at: Position,
        range: std::ops::Range<usize>,
        only: Option<SharedString>,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeActionItem>, String>> {
        let path = at.doc.path.clone();
        let found = self.ask(cx, move |manager, cancel| {
            manager
                .code_actions(&path, range, only.as_deref(), cancel)
                .map_err(|error| error.to_string())
        });
        // The items hold `Rc`s, so they are made back on the UI thread.
        cx.foreground_executor().spawn(async move {
            Ok(found
                .await?
                .into_iter()
                .map(|action| CodeActionItem {
                    title: action.title.clone().into(),
                    kind: action.kind.clone().map(Into::into),
                    preferred: action.preferred,
                    disabled: action.disabled.clone().map(Into::into),
                    data: Rc::new(action),
                })
                .collect())
        })
    }

    fn edits(
        &self,
        at: Position,
        item: &CodeActionItem,
        cx: &mut App,
    ) -> Task<Result<ActionChange, String>> {
        let Some(action) = item.data.downcast_ref::<lsp::CodeAction>().cloned() else {
            return Task::ready(Err("The action is not one of the language server's".into()));
        };
        let path = at.doc.path.clone();
        let places = Places::new(cx);
        let asked = self.ask(cx, move |manager, cancel| -> Result<_, String> {
            let resolved = manager
                .resolve_code_action(&path, &action, cancel)
                .map_err(|error| error.to_string())?;
            match resolved.edit.clone() {
                // Its command, if any, runs once the edit is in place.
                Some(edit) => Ok((places.changes(edit)?, resolved.command.then_some(resolved))),
                // The command asks for its edits while it runs.
                None if resolved.command => {
                    let change = manager
                        .run_code_action_command(&path, &resolved, cancel)
                        .map_err(|error| error.to_string())?;
                    Ok((places.changes(change)?, None))
                }
                None => Err("The language server sent no changes for this action".into()),
            }
        });
        let (manager, path) = (self.manager.clone(), at.doc.path.clone());
        cx.foreground_executor().spawn(async move {
            let (change, command) = asked.await?;
            let then = command.map(|action| follow_up(manager, path, action, &change));
            Ok(ActionChange { change, then })
        })
    }
}

/// Runs `action`'s command once `change`, its own edit, is applied and the
/// servers have the files' new text, and reads what the command asks for
/// on that text.
fn follow_up(
    manager: lsp::Manager,
    path: PathBuf,
    action: lsp::CodeAction,
    change: &[FileChange],
) -> FollowUp {
    let texts = edited_texts(change);
    Box::new(move |cx| {
        let places = Places::new(cx).with_texts(&texts);
        ask(&manager, cx, move |manager, cancel| {
            for (file, text) in &texts {
                if !manager.await_document_text(file, text, EDIT_REACHES_SERVER, cancel) {
                    return Err(format!(
                        "{} changed before the action's command could run",
                        file.display()
                    ));
                }
            }
            let change = manager
                .run_code_action_command(&path, &action, cancel)
                .map_err(|error| error.to_string())?;
            places.changes(change)
        })
    })
}

impl NavigationProvider for Provider {
    fn locate(
        &self,
        kind: Navigation,
        at: Position,
        cx: &mut App,
    ) -> Task<Result<Vec<LinkTarget>, String>> {
        let path = at.doc.path.clone();
        let offset = at.offset;
        let places = Places::new(cx);
        self.ask(cx, move |manager, cancel| {
            let found = match kind {
                Navigation::Declaration => manager.declaration(&path, offset, cancel),
                Navigation::TypeDefinition => manager.type_definition(&path, offset, cancel),
                Navigation::Implementation => manager.implementation(&path, offset, cancel),
            };
            found
                .map(|found| places.targets(found))
                .map_err(|error| error.to_string())
        })
    }
}
