//! The entry point to code insight: give it documents and it finds, starts,
//! shares and stops the language servers for them. A server is keyed by its
//! root folder, its kind and the toolchain it runs in, starts when its first
//! document opens, and is stopped again once it has had no open document for
//! the idle period, or earlier when too many servers are alive.
//!
//! Threading: `open`, `change`, `close`, `diagnostics`, `statuses`,
//! `triggers`, `stop_server`, `restart_server` and `sweep` only take a short
//! lock and queue work, so they are safe on a UI thread. The questions
//! (`definition`, `hover`, `completion`, ...), `diagnostics_settled` and
//! `with_file` wait for a server, for at most the request timeout, and belong
//! on a background thread.
use super::client::{
    lock, path_to_uri, uri_to_path, CancelToken, Client, ClientEvent, ClientOptions, Launch,
    Launcher, ProcessLauncher,
};
use super::protocol::{self, Converter};
use super::registry::{find_root, Language, Registry, Role, ServerSpec, SettingsContext};
use super::types::{
    CompletionItem, CompletionList, Diagnostic, DocumentSymbol, Hover, Location, LspError,
    LspEvent, ServerId, ServerState, ServerStatus, SignatureHelp, Triggers,
};
use crate::code::toolchain::Toolchain;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

/// After this many failed starts in a row a server is left alone until
/// `restart_server`.
const MAX_FAILURES: u32 = 3;
const LOG_TAIL_LINES: usize = 40;
const PULL_TIMEOUT: Duration = Duration::from_secs(60);
const REACHED_LIMIT: usize = 4096;
/// The server gave up on a request because the document moved on.
const CONTENT_MODIFIED: i64 = -32801;
const SERVER_CANCELLED: i64 = -32802;

/// The time the idle policy runs on; tests pass their own.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Clone, Debug)]
pub struct ManagerOptions {
    /// How long a server may live with no open document.
    pub idle_period: Duration,
    /// How many servers may be alive. Servers with no open document are
    /// stopped, least recently used first, to stay within it; servers that
    /// still have documents are never stopped for it.
    pub max_servers: usize,
    /// How long a question waits for its answer, a starting server included.
    pub request_timeout: Duration,
    /// How long a server may take over its handshake.
    pub start_timeout: Duration,
    /// Executables the user set, by registry id (`basedpyright`, `ruff`, ...).
    pub tool_paths: HashMap<String, PathBuf>,
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            idle_period: Duration::from_secs(5 * 60),
            max_servers: 6,
            request_timeout: Duration::from_secs(10),
            start_timeout: Duration::from_secs(30),
            tool_paths: HashMap::new(),
        }
    }
}

/// What the user set for one project, changeable while the app runs. It
/// takes effect for documents opened and servers started afterwards.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectSettings {
    /// No server is started for the project's documents.
    pub disabled: bool,
    /// Registry ids of servers the project does without.
    pub disabled_servers: HashSet<String>,
    /// Executables by registry id; they win over `ManagerOptions::tool_paths`.
    pub tool_paths: HashMap<String, PathBuf>,
}

/// What one server says about a document, and for which of its versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerDiagnostics {
    /// The registry id, e.g. `ruff`.
    pub server: String,
    /// The document version the report is about; `None` before the first.
    pub version: Option<i64>,
    pub items: Vec<Diagnostic>,
}

/// An open document's diagnostics, kept apart by server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentDiagnostics {
    /// The document's version now: 1 when opened, one more per changed text.
    pub version: i64,
    pub servers: Vec<ServerDiagnostics>,
}

/// Where a document lives, which decides the servers it gets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentContext {
    /// The checkout (worktree) the file was opened from. Roots are looked
    /// for inside it and never above it.
    pub checkout: PathBuf,
    /// The project the checkout belongs to. Carried for callers; roots and
    /// servers are decided by the checkout alone.
    pub project: PathBuf,
    /// The checkout's toolchain: where servers are found and what they run in.
    pub toolchain: Toolchain,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ServerKey {
    root: PathBuf,
    server: &'static str,
    toolchain: Toolchain,
}

enum Phase {
    Starting,
    Running,
    Failed { message: String, log_tail: String },
    Stopped,
    ToolNotFound,
}

struct Entry {
    key: ServerKey,
    /// The project whose settings it is started under.
    project: PathBuf,
    spec: ServerSpec,
    phase: Phase,
    /// Bumped on every start and stop, so late news from an earlier process
    /// is recognised and dropped.
    generation: u64,
    client: Option<Arc<Client>>,
    command: String,
    /// The executable's name: a fallback may be running under the spec's id.
    name: String,
    documents: HashSet<PathBuf>,
    last_used: Instant,
    idle_since: Option<Instant>,
    /// Background work the server has announced and not finished.
    progress: Vec<(String, Option<String>)>,
    failures: u32,
    /// The document version each path's diagnostics were last reported for.
    reported: HashMap<PathBuf, i64>,
    pulls: Option<mpsc::Sender<PathBuf>>,
}

impl Entry {
    fn live(&self) -> bool {
        matches!(self.phase, Phase::Starting | Phase::Running)
    }

    fn running(&self) -> Option<&Arc<Client>> {
        match self.phase {
            Phase::Running => self.client.as_ref(),
            _ => None,
        }
    }

    fn status(&self, id: ServerId) -> ServerStatus {
        let state = match &self.phase {
            Phase::Starting => ServerState::Starting,
            Phase::Running => match self.progress.last() {
                Some((_, title)) => ServerState::Indexing {
                    title: title.clone(),
                },
                None => ServerState::Ready,
            },
            Phase::Failed { message, log_tail } => ServerState::Failed {
                message: message.clone(),
                log_tail: log_tail.clone(),
            },
            Phase::Stopped => ServerState::Stopped,
            Phase::ToolNotFound => ServerState::ToolNotFound {
                looked_for: self.spec.executables(),
                hint: self.spec.install_hint.to_string(),
            },
        };
        ServerStatus {
            id,
            server: self.spec.id.to_string(),
            name: self.name.clone(),
            root: self.key.root.clone(),
            state,
            pid: self.client.as_ref().and_then(|client| client.pid()),
            command: self.command.clone(),
            open_documents: self.documents.len(),
        }
    }
}

struct Document {
    text: Arc<str>,
    version: i64,
    language: Language,
    servers: Vec<ServerId>,
    /// How many callers have it open; the editor and a tool may overlap.
    opens: usize,
    /// The path with links resolved, which is how some servers name it.
    canonical: Option<PathBuf>,
}

#[derive(Default)]
struct State {
    entries: HashMap<ServerId, Entry>,
    by_key: HashMap<ServerKey, ServerId>,
    documents: HashMap<PathBuf, Document>,
    diagnostics: HashMap<PathBuf, Vec<(ServerId, Vec<Diagnostic>)>>,
    /// Files a server pointed at, so opening one reuses that server.
    reached: HashMap<PathBuf, Vec<ServerId>>,
    subscribers: Vec<async_channel::Sender<LspEvent>>,
    next_id: u64,
    projects: HashMap<PathBuf, ProjectSettings>,
    /// The idle period and server cap, once changed from the options.
    limits: Option<(Duration, usize)>,
}

impl State {
    fn emit(&mut self, event: LspEvent) {
        self.subscribers
            .retain(|subscriber| subscriber.try_send(event.clone()).is_ok());
    }

    fn status_changed(&mut self, id: ServerId) {
        if let Some(status) = self.entries.get(&id).map(|entry| entry.status(id)) {
            self.emit(LspEvent::ServerChanged(status));
        }
    }

    /// Replaces what `id` says about `path`; true when that changed anything.
    fn set_diagnostics(&mut self, path: &Path, id: ServerId, found: Vec<Diagnostic>) -> bool {
        let sets = self.diagnostics.entry(path.to_path_buf()).or_default();
        let at = sets.iter().position(|(server, _)| *server == id);
        let changed = match (at, found.is_empty()) {
            (Some(at), true) => {
                sets.remove(at);
                true
            }
            (Some(at), false) => {
                let changed = sets[at].1 != found;
                sets[at].1 = found;
                changed
            }
            (None, true) => false,
            (None, false) => {
                sets.push((id, found));
                true
            }
        };
        if sets.is_empty() {
            self.diagnostics.remove(path);
        }
        changed
    }

    fn drop_diagnostics(&mut self, path: &Path, id: ServerId) {
        if self.set_diagnostics(path, id, Vec::new()) {
            self.emit(LspEvent::DiagnosticsChanged {
                path: path.to_path_buf(),
            });
        }
    }

    fn drop_server_diagnostics(&mut self, id: ServerId) {
        let paths: Vec<PathBuf> = self
            .diagnostics
            .iter()
            .filter(|(_, sets)| sets.iter().any(|(server, _)| *server == id))
            .map(|(path, _)| path.clone())
            .collect();
        for path in paths {
            self.drop_diagnostics(&path, id);
        }
    }

    fn merged(&self, path: &Path) -> Vec<Diagnostic> {
        let mut all: Vec<Diagnostic> = self
            .diagnostics
            .get(path)
            .into_iter()
            .flatten()
            .flat_map(|(_, found)| found.iter().cloned())
            .collect();
        all.sort_by_key(|diagnostic| (diagnostic.range, diagnostic.severity));
        all
    }

    fn forget(&mut self, id: ServerId) {
        if let Some(entry) = self.entries.remove(&id) {
            self.by_key.remove(&entry.key);
        }
    }

    /// The path we know a document by, for a path a server named it by.
    fn known_path(&self, path: PathBuf) -> PathBuf {
        if self.documents.contains_key(&path) {
            return path;
        }
        self.documents
            .iter()
            .find(|(_, document)| document.canonical.as_deref() == Some(path.as_path()))
            .map_or(path, |(known, _)| known.clone())
    }
}

fn send_open(client: &Client, path: &Path, document: &Document) {
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": {
            "uri": path_to_uri(path),
            "languageId": document.language.id(),
            "version": document.version,
            "text": &*document.text,
        } }),
    );
}

struct Shared {
    options: ManagerOptions,
    registry: Registry,
    launcher: Arc<dyn Launcher>,
    clock: Clock,
    state: Mutex<State>,
    /// Signalled whenever a server changes phase or diagnostics arrive.
    changed: Condvar,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // Threads still waiting on a server hold their own reference to its
        // client; killing here is what ends the process for certain.
        for entry in lock(&self.state).entries.values() {
            if let Some(client) = &entry.client {
                client.kill();
            }
        }
    }
}

/// A server and document picked for one question.
struct Target {
    id: ServerId,
    client: Arc<Client>,
    text: Arc<str>,
    uri: String,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// The server of kind `spec` for `path`, started if need be. There is
    /// always an entry, so a missing tool has something to be reported on.
    fn attach(
        self: &Arc<Self>,
        state: &mut State,
        path: &Path,
        spec: &ServerSpec,
        context: &DocumentContext,
        now: Instant,
    ) -> ServerId {
        // A virtual environment inside the checkout holds libraries too.
        let library = !path.starts_with(&context.checkout)
            || context
                .toolchain
                .venv
                .as_ref()
                .is_some_and(|venv| path.starts_with(venv));
        if library {
            // A file reached by navigation (a library, say): the server that
            // pointed at it, else the checkout's most recently used one.
            let same = |id: &ServerId| {
                state
                    .entries
                    .get(id)
                    .is_some_and(|entry| entry.live() && entry.spec.id == spec.id)
            };
            let from = state
                .reached
                .get(path)
                .and_then(|ids| ids.iter().rev().copied().find(same));
            let recent = || {
                state
                    .entries
                    .iter()
                    .filter(|(id, entry)| {
                        same(id)
                            && entry.key.toolchain == context.toolchain
                            && entry.key.root.starts_with(&context.checkout)
                    })
                    .max_by_key(|(_, entry)| entry.last_used)
                    .map(|(id, _)| *id)
            };
            if let Some(id) = from.or_else(recent) {
                return id;
            }
        }
        let key = ServerKey {
            root: find_root(path, &context.checkout, spec.root_markers),
            server: spec.id,
            toolchain: context.toolchain.clone(),
        };
        let id = match state.by_key.get(&key) {
            Some(id) => *id,
            None => {
                state.next_id += 1;
                let id = ServerId(state.next_id);
                state.by_key.insert(key.clone(), id);
                state.entries.insert(
                    id,
                    Entry {
                        key,
                        project: context.project.clone(),
                        spec: spec.clone(),
                        phase: Phase::Stopped,
                        generation: 0,
                        client: None,
                        command: String::new(),
                        name: spec.id.to_string(),
                        documents: HashSet::new(),
                        last_used: now,
                        idle_since: None,
                        progress: Vec::new(),
                        failures: 0,
                        reported: HashMap::new(),
                        pulls: None,
                    },
                );
                id
            }
        };
        let rest = state.entries.get(&id).is_some_and(|entry| {
            entry.live()
                || (matches!(entry.phase, Phase::Failed { .. }) && entry.failures >= MAX_FAILURES)
        });
        if !rest {
            self.start(state, id);
        }
        id
    }

    fn start(self: &Arc<Self>, state: &mut State, id: ServerId) {
        let explicit = state.entries.get(&id).and_then(|entry| {
            state
                .projects
                .get(&entry.project)
                .and_then(|settings| settings.tool_paths.get(entry.spec.id))
                .or_else(|| self.options.tool_paths.get(entry.spec.id))
                .cloned()
        });
        let Some(entry) = state.entries.get_mut(&id) else {
            return;
        };
        let found = entry
            .spec
            .resolve(&entry.key.toolchain, explicit.as_deref());
        let Some((program, args)) = found else {
            entry.phase = Phase::ToolNotFound;
            state.status_changed(id);
            return;
        };
        let launch = Launch {
            program,
            args: args.iter().map(|arg| arg.to_string()).collect(),
            cwd: entry.key.root.clone(),
            env: entry.key.toolchain.environment.clone(),
        };
        let context = SettingsContext {
            root: &entry.key.root,
            toolchain: &entry.key.toolchain,
        };
        let options = ClientOptions {
            root: entry.key.root.clone(),
            initialization_options: (entry.spec.initialization_options)(&context),
            settings: (entry.spec.settings)(&context),
        };
        entry.command = std::iter::once(launch.program.to_string_lossy().into_owned())
            .chain(launch.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(name) = launch.program.file_name() {
            entry.name = name.to_string_lossy().into_owned();
        }
        entry.phase = Phase::Starting;
        entry.generation += 1;
        entry.progress.clear();
        entry.reported.clear();
        let generation = entry.generation;
        self.enforce_cap(state, Some(id));
        state.status_changed(id);

        // Spawning and the handshake happen off the caller's thread. Only a
        // weak reference is held meanwhile, so dropping the manager is not
        // held up by a slow server.
        let weak = Arc::downgrade(self);
        let launcher = self.launcher.clone();
        let timeout = self.options.start_timeout;
        let work = move || {
            let fail = |message: String, log_tail: String| {
                if let Some(shared) = weak.upgrade() {
                    shared.start_failed(id, generation, message, log_tail);
                }
            };
            let name = launch.program.to_string_lossy().into_owned();
            let transport = match launcher.launch(&launch) {
                Ok(transport) => transport,
                Err(error) => {
                    return fail(
                        format!("{name} could not be started: {error}"),
                        String::new(),
                    )
                }
            };
            let events = weak.clone();
            let handler = Box::new(move |event: ClientEvent| {
                if let Some(shared) = events.upgrade() {
                    shared.client_event(id, generation, event);
                }
            });
            let client = match Client::start(transport, options, handler) {
                Ok(client) => Arc::new(client),
                Err(error) => {
                    return fail(
                        format!("{name} could not be started: {error}"),
                        String::new(),
                    )
                }
            };
            match client.initialize(timeout) {
                Ok(()) => match weak.upgrade() {
                    Some(shared) => shared.started(id, generation, client),
                    None => client.shutdown(),
                },
                Err(error) => {
                    let message = match error {
                        LspError::Timeout => format!("{name} did not answer when started."),
                        LspError::Closed => format!("{name} exited while starting."),
                        other => format!("{name} could not start. {other}"),
                    };
                    fail(message, client.log_tail(LOG_TAIL_LINES));
                }
            }
        };
        if std::thread::Builder::new()
            .name("lsp-start".into())
            .spawn(work)
            .is_err()
        {
            self.fail_entry(
                state,
                id,
                "A thread could not be started.".into(),
                String::new(),
            );
        }
    }

    fn fail_entry(&self, state: &mut State, id: ServerId, message: String, log_tail: String) {
        let Some(entry) = state.entries.get_mut(&id) else {
            return;
        };
        entry.phase = Phase::Failed { message, log_tail };
        entry.failures += 1;
        entry.generation += 1;
        entry.pulls = None;
        entry.progress.clear();
        let client = entry.client.take();
        state.drop_server_diagnostics(id);
        state.status_changed(id);
        self.changed.notify_all();
        if let Some(client) = client {
            client.kill();
        }
    }

    fn start_failed(&self, id: ServerId, generation: u64, message: String, log_tail: String) {
        let mut state = self.state();
        let current = state
            .entries
            .get(&id)
            .is_some_and(|entry| entry.generation == generation);
        if current {
            self.fail_entry(&mut state, id, message, log_tail);
        }
    }

    fn started(self: &Arc<Self>, id: ServerId, generation: u64, client: Arc<Client>) {
        let mut state = self.state();
        let State {
            entries, documents, ..
        } = &mut *state;
        let entry = entries.get_mut(&id).filter(|entry| {
            entry.generation == generation && matches!(entry.phase, Phase::Starting)
        });
        let Some(entry) = entry else {
            // Stopped while it was starting.
            drop(state);
            client.shutdown();
            return;
        };
        entry.phase = Phase::Running;
        entry.failures = 0;
        entry.client = Some(client.clone());
        for path in &entry.documents {
            if let Some(document) = documents.get(path) {
                send_open(&client, path, document);
            }
        }
        if client.pull_diagnostics().is_some() {
            self.pull_all(entry, id);
        }
        state.status_changed(id);
        self.changed.notify_all();
    }

    /// Asks for diagnostics of `paths`, starting the thread that pulls for
    /// this server the first time.
    fn pull(self: &Arc<Self>, entry: &mut Entry, id: ServerId, paths: &[PathBuf]) {
        if entry.pulls.is_none() {
            let (sender, queue) = mpsc::channel();
            let weak = Arc::downgrade(self);
            let generation = entry.generation;
            let spawned = std::thread::Builder::new()
                .name("lsp-pull".into())
                .spawn(move || pull_loop(&weak, id, generation, &queue));
            if spawned.is_ok() {
                entry.pulls = Some(sender);
            }
        }
        if let Some(pulls) = &entry.pulls {
            for path in paths {
                let _ = pulls.send(path.clone());
            }
        }
    }

    fn pull_all(self: &Arc<Self>, entry: &mut Entry, id: ServerId) {
        let paths: Vec<PathBuf> = entry.documents.iter().cloned().collect();
        self.pull(entry, id, &paths);
    }

    fn client_event(self: &Arc<Self>, id: ServerId, generation: u64, event: ClientEvent) {
        let event = match event {
            ClientEvent::Diagnostics {
                uri,
                version,
                items,
            } => {
                if let Some(path) = uri_to_path(&uri) {
                    self.report_diagnostics(id, generation, path, version, &items);
                }
                return;
            }
            other => other,
        };
        let mut state = self.state();
        let Some(entry) = state
            .entries
            .get_mut(&id)
            .filter(|entry| entry.generation == generation)
        else {
            return;
        };
        match event {
            ClientEvent::Diagnostics { .. } => {}
            ClientEvent::ProgressBegin { token, title } => {
                entry.progress.retain(|(known, _)| *known != token);
                entry.progress.push((token, title));
                state.status_changed(id);
            }
            ClientEvent::ProgressEnd { token } => {
                entry.progress.retain(|(known, _)| *known != token);
                state.status_changed(id);
            }
            ClientEvent::PullDiagnosticsRegistered | ClientEvent::DiagnosticsRefresh => {
                if entry.running().is_some() {
                    self.pull_all(entry, id);
                }
            }
            ClientEvent::Closed => {
                // While starting, the handshake's failure reports it instead.
                if let Some(client) = entry.running().cloned() {
                    let name = entry.spec.id;
                    self.fail_entry(
                        &mut state,
                        id,
                        format!("{name} stopped unexpectedly."),
                        client.log_tail(LOG_TAIL_LINES),
                    );
                }
            }
        }
    }

    /// A file's text: an open document's, else what is on disk.
    fn text_of(&self, path: &Path) -> Option<Arc<str>> {
        let open = self
            .state()
            .documents
            .get(path)
            .map(|document| document.text.clone());
        open.or_else(|| std::fs::read_to_string(path).ok().map(Arc::from))
    }

    /// Records what server `id` reports for `path`. A report for an older
    /// version of an open document is dropped: a newer one is on its way.
    /// Positions are converted between two short locks, not under one, since
    /// that can mean reading files.
    fn report_diagnostics(
        &self,
        id: ServerId,
        generation: u64,
        path: PathBuf,
        version: Option<i64>,
        items: &[Value],
    ) {
        let current = |state: &State| {
            let entry = state.entries.get(&id)?;
            let client = entry.client.as_ref()?;
            (entry.generation == generation).then(|| (client.encoding(), entry.spec.id))
        };
        let (path, known) = {
            let state = self.state();
            (state.known_path(path), current(&state))
        };
        let Some((encoding, server)) = known else {
            return;
        };
        let found = {
            let mut converter = Converter::new(encoding, |other: &Path| self.text_of(other));
            protocol::diagnostics(&mut converter, &path, items, server)
        };
        let mut state = self.state();
        if current(&state).is_none() {
            return;
        }
        let open = state.documents.get(&path).map(|document| document.version);
        if matches!((open, version), (Some(open), Some(version)) if version < open) {
            return;
        }
        // A report on a newer text is news even when it says the same:
        // its positions are that text's.
        let mut advanced = false;
        if let (Some(open), Some(entry)) = (open, state.entries.get_mut(&id)) {
            advanced = entry.reported.insert(path.clone(), open) != Some(open);
        }
        if state.set_diagnostics(&path, id, found) || advanced {
            state.emit(LspEvent::DiagnosticsChanged { path });
        }
        self.changed.notify_all();
    }

    /// The server said its earlier report still stands.
    fn report_unchanged(&self, id: ServerId, generation: u64, path: &Path, version: i64) {
        let mut state = self.state();
        let open = state.documents.get(path).map(|document| document.version);
        let entry = state
            .entries
            .get_mut(&id)
            .filter(|entry| entry.generation == generation);
        if let (Some(open), Some(entry)) = (open, entry) {
            if version >= open {
                let advanced = entry.reported.insert(path.to_path_buf(), open) != Some(open);
                if advanced {
                    state.emit(LspEvent::DiagnosticsChanged {
                        path: path.to_path_buf(),
                    });
                }
                self.changed.notify_all();
            }
        }
    }

    /// Stops a server on purpose. Its entry stays, as stopped, only while
    /// documents still point at it.
    fn stop(&self, state: &mut State, id: ServerId) {
        let Some(entry) = state.entries.get_mut(&id) else {
            return;
        };
        let client = entry.client.take();
        entry.phase = Phase::Stopped;
        entry.generation += 1;
        entry.pulls = None;
        entry.progress.clear();
        entry.idle_since = None;
        let unused = entry.documents.is_empty();
        state.drop_server_diagnostics(id);
        state.status_changed(id);
        if unused {
            state.forget(id);
        }
        self.changed.notify_all();
        if let Some(client) = client {
            // The polite goodbye waits on the server, so not on this thread.
            let goodbye = client.clone();
            let spawned = std::thread::Builder::new()
                .name("lsp-stop".into())
                .spawn(move || goodbye.shutdown());
            if spawned.is_err() {
                client.kill();
            }
        }
    }

    /// Stops idle servers, least recently used first, while more than the
    /// cap are alive. `keep` is the one being started.
    fn enforce_cap(&self, state: &mut State, keep: Option<ServerId>) {
        loop {
            let live = state.entries.values().filter(|entry| entry.live()).count();
            let cap = state
                .limits
                .map_or(self.options.max_servers, |(_, cap)| cap);
            if live <= cap {
                return;
            }
            let victim = state
                .entries
                .iter()
                .filter(|(id, entry)| {
                    entry.live() && entry.documents.is_empty() && Some(**id) != keep
                })
                .min_by_key(|(id, entry)| (entry.last_used, **id))
                .map(|(id, _)| *id);
            match victim {
                Some(id) => self.stop(state, id),
                None => return,
            }
        }
    }

    fn sweep(&self) {
        let now = (self.clock)();
        let mut state = self.state();
        let period = state
            .limits
            .map_or(self.options.idle_period, |(period, _)| period);
        let idle: Vec<ServerId> = state
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry.live()
                    && entry.documents.is_empty()
                    && entry
                        .idle_since
                        .is_some_and(|since| now.duration_since(since) >= period)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in idle {
            self.stop(&mut state, id);
        }
    }

    /// The insight server to ask about `path`, waiting while it starts.
    fn target(&self, path: &Path, provider: &str) -> Result<Target, LspError> {
        let deadline = Instant::now() + self.options.request_timeout;
        let now = (self.clock)();
        let mut state = self.state();
        loop {
            let Some(document) = state.documents.get(path) else {
                return Err(match Language::of(path) {
                    Some(_) => LspError::NotOpen,
                    None => LspError::NoServer,
                });
            };
            let mut starting = false;
            let mut unsupported = false;
            let mut found = None;
            for id in &document.servers {
                let Some(entry) = state.entries.get(id) else {
                    continue;
                };
                if entry.spec.role != Role::Insight {
                    continue;
                }
                match entry.running() {
                    Some(client) if client.provides(provider) => {
                        found = Some(Target {
                            id: *id,
                            client: client.clone(),
                            text: document.text.clone(),
                            uri: path_to_uri(path),
                        });
                        break;
                    }
                    Some(_) => unsupported = true,
                    None => starting |= matches!(entry.phase, Phase::Starting),
                }
            }
            if let Some(target) = found {
                if let Some(entry) = state.entries.get_mut(&target.id) {
                    entry.last_used = now;
                }
                return Ok(target);
            }
            if !starting {
                return Err(if unsupported {
                    LspError::Unsupported
                } else {
                    LspError::NoServer
                });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(LspError::Timeout);
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// Asks `target`'s server `method` and reads the answer with `read`,
    /// which gets a converter that knows the document's current text.
    fn ask<T>(
        &self,
        path: &Path,
        target: &Target,
        method: &str,
        params: impl FnOnce(&mut Converter) -> Value,
        cancel: &CancelToken,
        read: impl FnOnce(&mut Converter, &Value) -> T,
    ) -> Result<T, LspError> {
        let mut converter = Converter::new(target.client.encoding(), |other: &Path| {
            if other == path {
                return Some(target.text.clone());
            }
            self.text_of(other)
        });
        let params = params(&mut converter);
        let result = target
            .client
            .request(method, params, self.options.request_timeout, cancel)?;
        Ok(read(&mut converter, &result))
    }

    fn remember_reached(&self, id: ServerId, locations: &[Location]) {
        let mut state = self.state();
        if state.reached.len() > REACHED_LIMIT {
            state.reached.clear();
        }
        for location in locations {
            let ids = state.reached.entry(location.path.clone()).or_default();
            ids.retain(|known| *known != id);
            ids.push(id);
        }
    }
}

/// One server's diagnostics on request, for servers that want to be asked.
/// Requests queue up while one is in flight and repeats are folded together.
fn pull_loop(weak: &Weak<Shared>, id: ServerId, generation: u64, queue: &mpsc::Receiver<PathBuf>) {
    while let Ok(first) = queue.recv() {
        let mut paths = vec![first];
        while let Ok(path) = queue.try_recv() {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        for path in paths {
            for attempt in 0..3 {
                // Looked up afresh each time and released before asking, so
                // the manager can be dropped while a server is thinking.
                let asked = weak.upgrade().and_then(|shared| {
                    let state = shared.state();
                    let entry = state
                        .entries
                        .get(&id)
                        .filter(|entry| entry.generation == generation)?;
                    let client = entry.running()?.clone();
                    let version = state.documents.get(&path)?.version;
                    Some((client, version))
                });
                let Some((client, version)) = asked else {
                    break;
                };
                let Some(pull) = client.pull_diagnostics() else {
                    break;
                };
                let mut params = json!({ "textDocument": { "uri": path_to_uri(&path) } });
                if let Some(identifier) = pull.identifier {
                    params["identifier"] = json!(identifier);
                }
                let answer = client.request(
                    "textDocument/diagnostic",
                    params,
                    PULL_TIMEOUT,
                    &CancelToken::new(),
                );
                let Some(shared) = weak.upgrade() else {
                    return;
                };
                match answer {
                    Ok(report) => {
                        match report.get("items").and_then(Value::as_array) {
                            Some(items) => shared.report_diagnostics(
                                id,
                                generation,
                                path.clone(),
                                Some(version),
                                items,
                            ),
                            // "unchanged": what we hold is still right.
                            None => shared.report_unchanged(id, generation, &path, version),
                        }
                        break;
                    }
                    Err(LspError::Server { code, .. })
                        if (code == CONTENT_MODIFIED || code == SERVER_CANCELLED)
                            && attempt < 2 =>
                    {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(LspError::Closed) => return,
                    Err(_) => break,
                }
            }
        }
    }
}

mod api;
mod settings;
pub use api::Manager;
