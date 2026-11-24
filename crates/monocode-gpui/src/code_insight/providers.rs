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
};
use gpui::{App, SharedString, Task};
use monocode_core::{
    code::lsp::{self, CancelToken, LineIndex, Point, ServerState},
    files::document::MAX_TEXT_BYTES,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

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

impl Provider {
    /// Runs a blocking question off the UI thread. Dropping the returned
    /// task cancels the question, which makes the blocked call return.
    fn ask<T: Send + 'static>(
        &self,
        cx: &App,
        work: impl FnOnce(&lsp::Manager, &CancelToken) -> T + Send + 'static,
    ) -> Task<T> {
        let manager = self.manager.clone();
        let token = CancelToken::new();
        let guard = CancelOnDrop(token.clone());
        let executor = cx.background_executor().clone();
        let call = executor.spawn(async move { work(&manager, &token) });
        executor.spawn(async move {
            let _guard = guard;
            call.await
        })
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

    /// A server's change as edits in bytes of each file's text: the
    /// editors' unsaved text for open files, the disk's for the others.
    /// Reads files: for a background thread.
    fn file_edits(&self, change: lsp::WorkspaceEdit) -> Result<Vec<FileEdit>, String> {
        if change.file_operations > 0 {
            return Err(
                "The change also creates, renames or deletes files, which is not supported yet"
                    .into(),
            );
        }
        (change.changes.into_iter())
            .map(|(path, edits)| {
                let spelled = self.spell(path.clone());
                let base: SharedString = match self.open.get(&spelled) {
                    Some(text) => text.clone(),
                    None => std::fs::read_to_string(&path)
                        .map_err(|error| format!("{}: {error}", path.display()))?
                        .into(),
                };
                let index = LineIndex::new(&base);
                let edits = (edits.into_iter())
                    .map(|edit| TextEdit {
                        range: index.offset_of(&base, edit.range.start)
                            ..index.offset_of(&base, edit.range.end),
                        text: edit.text,
                    })
                    .collect();
                Ok(FileEdit {
                    path: spelled,
                    base,
                    edits,
                })
            })
            .collect()
    }
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
    ) -> Task<Result<Vec<FileEdit>, String>> {
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
            places.file_edits(change)
        })
    }
}

impl CodeActionProvider for Provider {
    fn actions(
        &self,
        at: Position,
        range: std::ops::Range<usize>,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeActionItem>, String>> {
        let path = at.doc.path.clone();
        let found = self.ask(cx, move |manager, cancel| {
            manager
                .code_actions(&path, range, cancel)
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
    ) -> Task<Result<Vec<FileEdit>, String>> {
        let Some(action) = item.data.downcast_ref::<lsp::CodeAction>().cloned() else {
            return Task::ready(Err("The action is not one of the language server's".into()));
        };
        let path = at.doc.path.clone();
        let places = Places::new(cx);
        self.ask(cx, move |manager, cancel| {
            let resolved = manager
                .resolve_code_action(&path, &action, cancel)
                .map_err(|error| error.to_string())?;
            match resolved.edit {
                Some(edit) => places.file_edits(edit),
                // The command asks for its edits while it runs.
                None if resolved.command => {
                    let change = manager
                        .run_code_action_command(&path, &resolved, cancel)
                        .map_err(|error| error.to_string())?;
                    places.file_edits(change)
                }
                None => Err("The language server sent no changes for this action".into()),
            }
        })
    }
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
