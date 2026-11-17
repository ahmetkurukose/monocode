//! The editor's completion, hover, signature, definition and usages
//! providers, answered by the language servers. Every request runs on a
//! background thread with a cancel token that is cancelled when the editor
//! drops the task it was handed, so a burst of typing leaves no queue of
//! requests behind it.
use super::{convert, sync::served};
use crate::code_editor::{
    self, CompletionItem, CompletionProvider, CompletionRequest, CompletionTrigger, DocumentInfo,
    Hover, HoverProvider, LinkProvider, LinkTarget, Position, ReferencesProvider, Signature,
    SignatureProvider, TargetPosition,
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

/// Installs the providers on every file a language server is registered for.
pub(super) fn register(manager: lsp::Manager, cx: &mut App) {
    let provider = Rc::new(Provider { manager });
    code_editor::register(cx, move |doc, providers, _| {
        if !served(&doc.path) {
            return;
        }
        providers.completion = Some(provider.clone());
        providers.hover = Some(provider.clone());
        providers.signature = Some(provider.clone());
        providers.links = Some(provider.clone());
        providers.references = Some(provider.clone());
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
