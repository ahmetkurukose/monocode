//! The manager's public face: the calls the editor and the agents' tools
//! make. What happens behind them is in the parent module.
use super::*;

/// The language servers of the whole app. Cheap to clone; every clone is the
/// same manager. Dropping the last one kills every server.
#[derive(Clone)]
pub struct Manager {
    shared: Arc<Shared>,
}

impl Manager {
    /// A manager over the built-in registry that starts real processes and
    /// checks for idle servers by itself.
    pub fn new(options: ManagerOptions) -> Self {
        let manager = Self::with_parts(
            options,
            Registry::builtin(),
            Arc::new(ProcessLauncher),
            Arc::new(Instant::now),
        );
        let weak = Arc::downgrade(&manager.shared);
        let period = (manager.shared.options.idle_period / 4)
            .clamp(Duration::from_secs(1), Duration::from_secs(30));
        let _ = std::thread::Builder::new()
            .name("lsp-idle".into())
            .spawn(move || loop {
                std::thread::sleep(period);
                match weak.upgrade() {
                    Some(shared) => shared.sweep(),
                    None => break,
                }
            });
        manager
    }

    /// A manager from its parts, for tests and unusual setups. Nothing calls
    /// `sweep` for it: the owner does.
    pub fn with_parts(
        options: ManagerOptions,
        registry: Registry,
        launcher: Arc<dyn Launcher>,
        clock: Clock,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                options,
                registry,
                launcher,
                clock,
                state: Mutex::new(State::default()),
                changed: Condvar::new(),
            }),
        }
    }

    /// Events from now on. Each subscriber gets every event; drop the
    /// receiver to unsubscribe.
    pub fn subscribe(&self) -> async_channel::Receiver<LspEvent> {
        let (sender, receiver) = async_channel::unbounded();
        self.shared.state().subscribers.push(sender);
        receiver
    }

    /// Opens `path` with `text` as its content, starting its servers if they
    /// are not running. A file of no known language is ignored. Opening a
    /// document that is already open only counts the extra opener; the text
    /// stays what the last `change` made it. Every `open` needs its `close`.
    pub fn open(&self, path: &Path, text: &str, context: &DocumentContext) {
        let Some(language) = Language::of(path) else {
            return;
        };
        let canonical = std::fs::canonicalize(path).ok().filter(|real| real != path);
        let shared = &self.shared;
        let now = (shared.clock)();
        let mut state = shared.state();
        if let Some(document) = state.documents.get_mut(path) {
            document.opens += 1;
            return;
        }
        let specs: Vec<ServerSpec> = shared.registry.for_language(language).cloned().collect();
        let mut servers = Vec::new();
        for spec in &specs {
            let id = shared.attach(&mut state, path, spec, context, now);
            if !servers.contains(&id) {
                servers.push(id);
            }
        }
        let document = Document {
            text: Arc::from(text),
            version: 1,
            language,
            servers,
            opens: 1,
            canonical,
        };
        for id in &document.servers {
            let Some(entry) = state.entries.get_mut(id) else {
                continue;
            };
            entry.documents.insert(path.to_path_buf());
            entry.idle_since = None;
            entry.last_used = now;
            if let Some(client) = entry.running().cloned() {
                send_open(&client, path, &document);
                if client.pull_diagnostics().is_some() {
                    shared.pull(entry, *id, &[path.to_path_buf()]);
                }
            }
        }
        state.documents.insert(path.to_path_buf(), document);
        state.reached.remove(path);
    }

    /// Replaces the document's text. Servers are told with a new version and
    /// their diagnostics follow as events.
    pub fn change(&self, path: &Path, text: &str) {
        let shared = &self.shared;
        let now = (shared.clock)();
        let mut state = shared.state();
        let State {
            entries, documents, ..
        } = &mut *state;
        let Some(document) = documents.get_mut(path) else {
            return;
        };
        if &*document.text == text {
            return;
        }
        document.text = Arc::from(text);
        document.version += 1;
        for id in &document.servers {
            let Some(entry) = entries.get_mut(id) else {
                continue;
            };
            entry.last_used = now;
            let Some(client) = entry.running().cloned() else {
                continue;
            };
            if client.syncs_changes() {
                client.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": path_to_uri(path), "version": document.version },
                        "contentChanges": [{ "text": text }],
                    }),
                );
            }
            match client.pull_diagnostics() {
                Some(pull) if pull.inter_file => shared.pull_all(entry, *id),
                Some(_) => shared.pull(entry, *id, &[path.to_path_buf()]),
                None => {}
            }
        }
    }

    /// Gives up one `open`. When it was the last, servers are told, the
    /// document's diagnostics go, and a server left with no document starts
    /// its idle period.
    pub fn close(&self, path: &Path) {
        let shared = &self.shared;
        let now = (shared.clock)();
        let mut state = shared.state();
        let Some(document) = state.documents.get_mut(path) else {
            return;
        };
        document.opens = document.opens.saturating_sub(1);
        if document.opens > 0 {
            return;
        }
        let servers = state
            .documents
            .remove(path)
            .map(|document| document.servers)
            .unwrap_or_default();
        for id in servers {
            let Some(entry) = state.entries.get_mut(&id) else {
                continue;
            };
            entry.documents.remove(path);
            entry.reported.remove(path);
            if let Some(client) = entry.running() {
                client.notify(
                    "textDocument/didClose",
                    json!({ "textDocument": { "uri": path_to_uri(path) } }),
                );
            }
            let unused = entry.documents.is_empty();
            let live = entry.live();
            if unused && live {
                entry.idle_since = Some(now);
            }
            state.drop_diagnostics(path, id);
            if unused && !live {
                state.forget(id);
            }
        }
        shared.enforce_cap(&mut state, None);
    }

    pub fn is_open(&self, path: &Path) -> bool {
        self.shared.state().documents.contains_key(path)
    }

    /// Everything every server currently says about `path`, in document
    /// order, each tagged with the server it came from. Never waits.
    pub fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        self.shared.state().merged(path)
    }

    /// The diagnostics of an open document once every server serving it has
    /// reported on its current text, or what there is when `timeout` passes.
    /// Blocks for up to `timeout`.
    pub fn diagnostics_settled(
        &self,
        path: &Path,
        timeout: Duration,
    ) -> Result<Vec<Diagnostic>, LspError> {
        let shared = &self.shared;
        let deadline = Instant::now() + timeout;
        let mut state = shared.state();
        loop {
            let document = state.documents.get(path).ok_or(LspError::NotOpen)?;
            let settled = document.servers.iter().all(|id| {
                state.entries.get(id).is_none_or(|entry| match entry.phase {
                    Phase::Starting => false,
                    Phase::Running => entry
                        .reported
                        .get(path)
                        .is_some_and(|version| *version >= document.version),
                    _ => true,
                })
            });
            let left = deadline.saturating_duration_since(Instant::now());
            if settled || left.is_zero() {
                return Ok(state.merged(path));
            }
            state = shared
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    fn locations(
        &self,
        path: &Path,
        offset: usize,
        provider: &str,
        method: &str,
        extra: Option<(&str, Value)>,
        cancel: &CancelToken,
    ) -> Result<Vec<Location>, LspError> {
        let target = self.shared.target(path, provider)?;
        let found = self.shared.ask(
            path,
            &target,
            method,
            |converter| {
                let mut params = json!({
                    "textDocument": { "uri": target.uri },
                    "position": converter.position(path, offset),
                });
                if let Some((name, value)) = extra {
                    params[name] = value;
                }
                params
            },
            cancel,
            protocol::locations,
        )?;
        self.shared.remember_reached(target.id, &found);
        Ok(found)
    }

    /// Where the symbol at byte `offset` of `path` is defined. Blocks.
    pub fn definition(
        &self,
        path: &Path,
        offset: usize,
        cancel: &CancelToken,
    ) -> Result<Vec<Location>, LspError> {
        self.locations(
            path,
            offset,
            "definitionProvider",
            "textDocument/definition",
            None,
            cancel,
        )
    }

    /// Every use of the symbol at byte `offset` of `path`. Blocks.
    pub fn references(
        &self,
        path: &Path,
        offset: usize,
        include_declaration: bool,
        cancel: &CancelToken,
    ) -> Result<Vec<Location>, LspError> {
        self.locations(
            path,
            offset,
            "referencesProvider",
            "textDocument/references",
            Some((
                "context",
                json!({ "includeDeclaration": include_declaration }),
            )),
            cancel,
        )
    }

    /// What the server knows about the symbol at byte `offset`. Blocks.
    pub fn hover(
        &self,
        path: &Path,
        offset: usize,
        cancel: &CancelToken,
    ) -> Result<Option<Hover>, LspError> {
        let target = self.shared.target(path, "hoverProvider")?;
        self.shared.ask(
            path,
            &target,
            "textDocument/hover",
            |converter| {
                json!({
                    "textDocument": { "uri": target.uri },
                    "position": converter.position(path, offset),
                })
            },
            cancel,
            |converter, result| protocol::hover(converter, path, result),
        )
    }

    /// What could be typed at byte `offset`. `trigger` is the character just
    /// typed when that is why the editor asks (see `triggers`). Blocks.
    pub fn completion(
        &self,
        path: &Path,
        offset: usize,
        trigger: Option<char>,
        cancel: &CancelToken,
    ) -> Result<CompletionList, LspError> {
        let target = self.shared.target(path, "completionProvider")?;
        self.shared.ask(
            path,
            &target,
            "textDocument/completion",
            |converter| {
                json!({
                    "textDocument": { "uri": target.uri },
                    "position": converter.position(path, offset),
                    "context": match trigger {
                        Some(character) => json!({
                            "triggerKind": 2,
                            "triggerCharacter": character.to_string(),
                        }),
                        None => json!({ "triggerKind": 1 }),
                    },
                })
            },
            cancel,
            |converter, result| protocol::completions(converter, path, result, target.id),
        )
    }

    /// Fills in what the server left out of a completion item, usually its
    /// documentation. Returns the item as it was when the server has nothing
    /// to add. `path` is the document the item was offered for. Blocks.
    pub fn resolve_completion(
        &self,
        path: &Path,
        item: &CompletionItem,
        cancel: &CancelToken,
    ) -> Result<CompletionItem, LspError> {
        let target = self.shared.target(path, "completionProvider")?;
        let resolves = target.client.capabilities()["completionProvider"]["resolveProvider"]
            .as_bool()
            .unwrap_or(false);
        if !resolves || target.id != item.server {
            return Ok(item.clone());
        }
        self.shared.ask(
            path,
            &target,
            "completionItem/resolve",
            |_| item.raw.clone(),
            cancel,
            |converter, result| {
                protocol::completion_item(converter, path, result, target.id)
                    .unwrap_or_else(|| item.clone())
            },
        )
    }

    /// The signatures of the call being typed at byte `offset`. Blocks.
    pub fn signature_help(
        &self,
        path: &Path,
        offset: usize,
        cancel: &CancelToken,
    ) -> Result<Option<SignatureHelp>, LspError> {
        let target = self.shared.target(path, "signatureHelpProvider")?;
        let encoding = target.client.encoding();
        self.shared.ask(
            path,
            &target,
            "textDocument/signatureHelp",
            |converter| {
                json!({
                    "textDocument": { "uri": target.uri },
                    "position": converter.position(path, offset),
                })
            },
            cancel,
            |_, result| protocol::signature_help(result, encoding),
        )
    }

    /// The document's classes, functions and so on, for an outline. Blocks.
    pub fn document_symbols(
        &self,
        path: &Path,
        cancel: &CancelToken,
    ) -> Result<Vec<DocumentSymbol>, LspError> {
        let target = self.shared.target(path, "documentSymbolProvider")?;
        self.shared.ask(
            path,
            &target,
            "textDocument/documentSymbol",
            |_| json!({ "textDocument": { "uri": target.uri } }),
            cancel,
            |converter, result| protocol::document_symbols(converter, path, result),
        )
    }

    /// The characters that should make the editor ask for completion or
    /// signature help. Empty until the document's server is ready. Never waits.
    pub fn triggers(&self, path: &Path) -> Triggers {
        let state = self.shared.state();
        let mut triggers = Triggers::default();
        let servers = state.documents.get(path).map(|document| &document.servers);
        for id in servers.into_iter().flatten() {
            let Some(entry) = state.entries.get(id) else {
                continue;
            };
            let (Role::Insight, Some(client)) = (entry.spec.role, entry.running()) else {
                continue;
            };
            let read = |provider: &str| -> Vec<String> {
                client.capabilities()[provider]["triggerCharacters"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|character| character.as_str().map(str::to_string))
                    .collect()
            };
            triggers.completion.extend(read("completionProvider"));
            triggers
                .signature_help
                .extend(read("signatureHelpProvider"));
        }
        triggers
    }

    /// For one-off questions about a file nobody has open, as an agent's
    /// tool asks them: opens it from disk (or joins the editor's open copy),
    /// runs `ask`, and closes it again. The servers it started stay for the
    /// idle period, so a run of questions pays for the start once. Blocks
    /// for as long as `ask` does.
    pub fn with_file<T>(
        &self,
        path: &Path,
        context: &DocumentContext,
        ask: impl FnOnce(&Manager) -> T,
    ) -> Result<T, LspError> {
        let text = if self.is_open(path) {
            String::new()
        } else {
            std::fs::read_to_string(path).map_err(|error| {
                LspError::Io(format!("{} could not be read: {error}", path.display()))
            })?
        };
        self.open(path, &text, context);
        let answer = ask(self);
        self.close(path);
        Ok(answer)
    }

    /// Every server known, running or not, oldest first. Never waits.
    pub fn statuses(&self) -> Vec<ServerStatus> {
        let state = self.shared.state();
        let mut statuses: Vec<ServerStatus> = state
            .entries
            .iter()
            .map(|(id, entry)| entry.status(*id))
            .collect();
        statuses.sort_by_key(|status| status.id);
        statuses
    }

    /// Stops a server. With documents still open on it, it stays listed as
    /// stopped until `restart_server` or until another document that needs
    /// it is opened. Returns at once; the goodbye happens in the background.
    pub fn stop_server(&self, id: ServerId) {
        self.shared.stop(&mut self.shared.state(), id);
    }

    /// Stops a server and starts it again with its open documents; also the
    /// way to retry one that failed or whose tool has since been installed.
    pub fn restart_server(&self, id: ServerId) {
        let shared = &self.shared;
        let mut state = shared.state();
        shared.stop(&mut state, id);
        if let Some(entry) = state.entries.get_mut(&id) {
            entry.failures = 0;
            shared.start(&mut state, id);
        }
    }

    /// Stops every server, as when the app quits.
    pub fn stop_all(&self) {
        let mut state = self.shared.state();
        let ids: Vec<ServerId> = state.entries.keys().copied().collect();
        for id in ids {
            self.shared.stop(&mut state, id);
        }
    }

    /// Stops servers whose idle period has run out. `Manager::new` arranges
    /// for this to be called; with `with_parts` the owner calls it.
    pub fn sweep(&self) {
        self.shared.sweep();
    }
}
