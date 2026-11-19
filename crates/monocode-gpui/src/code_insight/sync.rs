//! Keeps the language servers and the editors in step. Documents the editors
//! open, change and close are opened, changed and closed with the manager;
//! what the manager reports about a document goes into the editor hub per
//! server, placed on the text it was reported for; servers are listed as
//! running work; and a checkout or project that goes takes its documents and
//! servers with it.
use super::{convert, settings};
use crate::{
    code_editor::{self, DocumentEvent, DocumentInfo},
    code_shell::{
        self,
        context::{self, CodeEvent},
        processes::{self, ProcessKind, Registration},
    },
};
use gpui::{App, Context, SharedString, Task};
use monocode_core::{
    code::{
        lsp::{
            self, DocumentContext, Language, LineIndex, LspEvent, Registry, ServerId, ServerState,
            ServerStatus,
        },
        services::{CodeContext, services},
    },
    files::path_key,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::LazyLock,
    time::Duration,
};

/// The editor's own limit for linting (`editor_syntax::MAX_LINT_BYTES`):
/// past it a file is a viewer, and no server is told about it.
const MAX_BYTES: usize = 512 * 1024;
/// A line this long is generated or minified; servers choke on those.
const MAX_LINE_BYTES: usize = 20_000;
const MEMORY_PERIOD: Duration = Duration::from_secs(10);

pub(super) static REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::builtin);

/// Whether a language server is registered for the file's language and the
/// project's settings let it run: Python's always, another language's only
/// once the project switched it on.
pub(super) fn served(path: &Path, chosen: &settings::InsightSettings) -> bool {
    Language::of(path)
        .is_some_and(|language| (REGISTRY.for_language(language)).any(|spec| chosen.runs(spec)))
}

/// `served` for an editor's document, under its project's settings.
pub(super) fn serves(info: &DocumentInfo, cx: &App) -> bool {
    // Most files have no server at all: those never ask where they belong.
    let known = Language::of(&info.path)
        .is_some_and(|language| REGISTRY.for_language(language).next().is_some());
    if !known {
        return false;
    }
    let project = services().context(&home_of(info, cx)).project;
    let project = project.to_string_lossy();
    // A sleeping project's files wait for it to wake.
    !context::asleep(&project, cx) && served(&info.path, &settings::load(&project, cx))
}

fn too_big(text: &str) -> bool {
    text.len() > MAX_BYTES || text.split('\n').any(|line| line.len() > MAX_LINE_BYTES)
}

/// The path whose project and checkout a document is opened under: its own
/// when it lies in a checkout, else the checkout its editor was opened
/// from, so a library reached by "Go to Definition" joins the server that
/// pointed at it instead of starting one of its own.
fn home_of(info: &DocumentInfo, cx: &App) -> PathBuf {
    let inside = info.root.as_os_str().is_empty()
        || info.path.starts_with(&info.root)
        || context::checkout_for_path(&info.path.to_string_lossy(), cx).is_some();
    if inside {
        info.path.clone()
    } else {
        info.root.clone()
    }
}

/// What is known about a checkout once its disk has been looked at.
struct Home {
    /// What the context was built from; a changed terminal setup rebuilds it.
    code: CodeContext,
    context: DocumentContext,
    /// The checkout with links resolved, when that is another spelling:
    /// servers name files by it.
    real: Option<PathBuf>,
}

enum Phase {
    /// Not told to the manager: no server for it, too big, or its project
    /// has code insight off.
    Skipped,
    /// Its checkout's context is being built.
    Waiting { checkout: PathBuf },
    /// Open with the manager, exactly once.
    Open {
        checkout: PathBuf,
        /// The manager's version of the text it was last given.
        version: i64,
        /// The servers whose diagnostics are in the hub.
        sources: Vec<SharedString>,
    },
}

struct Doc {
    info: Rc<DocumentInfo>,
    text: SharedString,
    /// The hub's version of `text`.
    version: u64,
    phase: Phase,
}

pub(crate) struct Sync {
    manager: lsp::Manager,
    docs: HashMap<PathBuf, Doc>,
    homes: HashMap<PathBuf, Home>,
    building: HashMap<PathBuf, Task<()>>,
    listed: HashMap<ServerId, Registration>,
    /// Resident bytes by server process, summed over its process group.
    memory: HashMap<u32, u64>,
    memory_task: Option<Task<()>>,
    _events: Task<()>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl Sync {
    pub(super) fn new(manager: lsp::Manager, cx: &mut Context<Self>) -> Self {
        let hub = code_editor::hub(cx);
        let events = manager.subscribe();
        let listen = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| this.server_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let documents = cx.subscribe(&hub, |this, _, event: &DocumentEvent, cx| match event {
            DocumentEvent::Opened { doc, text, version } => {
                this.opened(doc.clone(), text.clone(), *version, cx)
            }
            DocumentEvent::Changed { doc, text, version } => {
                this.changed(&doc.path, text.clone(), *version)
            }
            DocumentEvent::Saved { .. } => {}
            DocumentEvent::Closed { doc } => this.closed(&doc.path, cx),
        });
        let shell = context::events(cx);
        let places = cx.subscribe(&shell, |this, _, event: &CodeEvent, cx| match event {
            CodeEvent::ProjectOpened(project) => this.project_opened(project, cx),
            CodeEvent::ProjectClosed(project) => {
                let key = path_key(project);
                this.release(
                    |home| path_key(&home.context.project.to_string_lossy()) == key,
                    cx,
                );
            }
            CodeEvent::CheckoutRemoved(checkout) => {
                let key = path_key(&checkout.folder);
                this.release(
                    |home| path_key(&home.context.checkout.to_string_lossy()) == key,
                    cx,
                );
            }
            // Its editors stay open; the servers and what they hold do not.
            CodeEvent::ProjectSlept(project) => {
                let key = path_key(project);
                this.release(
                    |home| path_key(&home.context.project.to_string_lossy()) == key,
                    cx,
                );
            }
            CodeEvent::ProjectWoke(_) => this.retry_skipped(cx),
            CodeEvent::CheckoutAdded(_) | CodeEvent::FocusedFileChanged(_) => {}
        });
        let mut this = Self {
            manager,
            docs: HashMap::new(),
            homes: HashMap::new(),
            building: HashMap::new(),
            listed: HashMap::new(),
            memory: HashMap::new(),
            memory_task: None,
            _events: listen,
            _subscriptions: vec![documents, places],
        };
        // Editors that were open before the feature started.
        let open: Vec<_> = (hub.read(cx).open_documents())
            .map(|open| (open.doc.clone(), open.text.clone(), open.version))
            .collect();
        for (doc, text, version) in open {
            this.opened(doc, text, version, cx);
        }
        this
    }

    fn opened(
        &mut self,
        info: Rc<DocumentInfo>,
        text: SharedString,
        version: u64,
        cx: &mut Context<Self>,
    ) {
        let path = info.path.clone();
        if self.docs.contains_key(&path) {
            return self.changed(&path, text, version);
        }
        self.docs.insert(
            path.clone(),
            Doc {
                info,
                text,
                version,
                phase: Phase::Skipped,
            },
        );
        self.begin(&path, cx);
    }

    /// Starts a skipped document on its way to the manager, if it may go.
    fn begin(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        if !matches!(doc.phase, Phase::Skipped) || !serves(&doc.info, cx) || too_big(&doc.text) {
            return;
        }
        let code = services().context(&home_of(&doc.info, cx));
        let checkout = code.checkout.clone();
        doc.phase = Phase::Waiting {
            checkout: checkout.clone(),
        };
        if (self.homes.get(&checkout)).is_some_and(|home| home.code == code) {
            return self.open_waiting(&checkout, cx);
        }
        if self.building.contains_key(&checkout) {
            return;
        }
        // Finding the virtual environment and the real path reads the disk.
        let key = checkout.clone();
        let task = cx.spawn(async move |this, cx| {
            let looked = code.clone();
            let (context, real) = cx
                .background_executor()
                .spawn(async move {
                    let real = std::fs::canonicalize(&looked.checkout)
                        .ok()
                        .filter(|real| *real != looked.checkout);
                    (looked.document(), real)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.building.remove(&key);
                let home = Home {
                    code,
                    context,
                    real,
                };
                this.homes.insert(key.clone(), home);
                this.open_waiting(&key, cx);
            });
        });
        self.building.insert(checkout, task);
    }

    /// Opens the documents that waited for `checkout`'s context.
    fn open_waiting(&mut self, checkout: &Path, cx: &mut Context<Self>) {
        let Some(home) = self.homes.get(checkout) else {
            return;
        };
        let context = home.context.clone();
        let chosen = settings::load(&context.project.to_string_lossy(), cx);
        self.manager
            .set_project_settings(&context.project, chosen.for_manager());
        let waiting: Vec<PathBuf> = (self.docs.iter())
            .filter(|(_, doc)| {
                matches!(&doc.phase, Phase::Waiting { checkout: waited } if waited == checkout)
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in waiting {
            let Some(doc) = self.docs.get_mut(&path) else {
                continue;
            };
            doc.phase = Phase::Skipped;
            if chosen.disabled {
                continue;
            }
            self.manager.open(&path, &doc.text, &context);
            // Someone else (an agent's tool) may have had it open with the
            // text on disk; the editor's is the one that counts.
            self.manager.change(&path, &doc.text);
            let Some(found) = self.manager.document_diagnostics(&path) else {
                continue;
            };
            doc.phase = Phase::Open {
                checkout: checkout.to_owned(),
                version: found.version,
                sources: Vec::new(),
            };
            self.push_diagnostics(&path, cx);
        }
    }

    fn changed(&mut self, path: &Path, text: SharedString, version: u64) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        doc.text = text;
        doc.version = version;
        if let Phase::Open { version, .. } = &mut doc.phase {
            self.manager.change(path, &doc.text);
            // What the servers said about the text before stays where
            // the editor carried it until they report on this one.
            if let Some(found) = self.manager.document_diagnostics(path) {
                *version = found.version;
            }
        }
    }

    fn closed(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(doc) = self.docs.remove(path) {
            self.leave(path, doc.phase, cx);
        }
    }

    /// Gives up the manager's copy of a document and what it put in the hub.
    fn leave(&mut self, path: &Path, phase: Phase, cx: &mut Context<Self>) {
        let Phase::Open { sources, .. } = phase else {
            return;
        };
        self.manager.close(path);
        let hub = code_editor::hub(cx);
        hub.update(cx, |hub, cx| {
            for source in sources {
                hub.set_diagnostics(path, source, None, Vec::new(), cx);
            }
        });
    }

    /// Lays each server's report over the document, when it is about the
    /// text the document has now. A report on an older text is left out:
    /// the editor keeps moving what it showed, and the server reports again.
    fn push_diagnostics(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        let Phase::Open {
            version, sources, ..
        } = &mut doc.phase
        else {
            return;
        };
        let Some(found) = self.manager.document_diagnostics(path) else {
            return;
        };
        if found.version != *version {
            return;
        }
        let current: Vec<_> = (found.servers.into_iter())
            .filter(|server| server.version == Some(found.version))
            .collect();
        if current.is_empty() {
            return;
        }
        let index = LineIndex::new(&doc.text);
        let hub = code_editor::hub(cx);
        let (text, hub_version) = (doc.text.clone(), doc.version);
        hub.update(cx, |hub, cx| {
            for server in current {
                let items: Vec<_> = (server.items.iter())
                    .map(|item| convert::diagnostic(item, &index, &text))
                    .collect();
                let source = SharedString::from(server.server);
                if !sources.contains(&source) {
                    if items.is_empty() {
                        continue;
                    }
                    sources.push(source.clone());
                }
                hub.set_diagnostics(path, source, Some(hub_version), items, cx);
            }
        });
    }

    fn server_event(&mut self, event: LspEvent, cx: &mut Context<Self>) {
        match event {
            LspEvent::DiagnosticsChanged { path } => self.push_diagnostics(&path, cx),
            LspEvent::ServerChanged(status) => {
                self.list_process(&status);
                if self.memory_task.is_none() && status.pid.is_some() {
                    self.watch_memory(cx);
                }
                code_shell::refresh(cx);
                cx.notify();
            }
        }
    }

    /// Lists a running server with the running work, and takes one that
    /// ended off the list.
    fn list_process(&mut self, status: &ServerStatus) {
        let live = matches!(
            status.state,
            ServerState::Starting | ServerState::Ready | ServerState::Indexing { .. }
        );
        match status.pid.filter(|_| live) {
            Some(pid) => {
                if !self.listed.contains_key(&status.id) {
                    let (manager, id) = (self.manager.clone(), status.id);
                    let label = format!("{} in {}", status.name, status.root.display());
                    let listing = processes::register_with_stop(
                        pid,
                        label,
                        ProcessKind::LanguageServer,
                        move || manager.stop_server(id),
                    );
                    self.listed.insert(status.id, listing);
                }
            }
            None => {
                self.listed.remove(&status.id);
            }
        }
    }

    /// Measures the servers now and every few seconds while any runs.
    fn watch_memory(&mut self, cx: &mut Context<Self>) {
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(pids) = this.update(cx, |this, _| -> Vec<u32> {
                    (this.manager.statuses().iter())
                        .filter_map(|status| status.pid)
                        .collect()
                }) else {
                    return;
                };
                if pids.is_empty() {
                    let _ = this.update(cx, |this, _| {
                        this.memory.clear();
                        this.memory_task = None;
                    });
                    return;
                }
                let usage = cx
                    .background_executor()
                    .spawn(async move { group_memory(&pids) })
                    .await;
                if this.update(cx, |this, _| this.memory = usage).is_err() {
                    return;
                }
                cx.background_executor().timer(MEMORY_PERIOD).await;
            }
        }));
    }

    /// Resident bytes of a server's process group, as last measured.
    pub(super) fn memory_of(&self, pid: u32) -> Option<u64> {
        self.memory.get(&pid).copied()
    }

    fn project_opened(&mut self, project: &str, cx: &mut Context<Self>) {
        let chosen = settings::load(project, cx);
        self.manager
            .set_project_settings(Path::new(project), chosen.for_manager());
        self.manager.set_limits(settings::limits(cx));
    }

    /// Closes the documents of the checkouts `belongs` picks, stops their
    /// servers and forgets their contexts. The documents stay known, as
    /// skipped, since their editors may still be open. Returns their paths.
    fn release(&mut self, belongs: impl Fn(&Home) -> bool, cx: &mut Context<Self>) -> Vec<PathBuf> {
        let checkouts: Vec<PathBuf> = (self.homes.iter())
            .filter(|(_, home)| belongs(home))
            .map(|(checkout, _)| checkout.clone())
            .collect();
        if checkouts.is_empty() {
            return Vec::new();
        }
        let mut released = Vec::new();
        for (path, doc) in &mut self.docs {
            let (Phase::Open { checkout, .. } | Phase::Waiting { checkout }) = &doc.phase else {
                continue;
            };
            if checkouts.contains(checkout) {
                let phase = std::mem::replace(&mut doc.phase, Phase::Skipped);
                released.push((path.clone(), phase));
            }
        }
        for (path, phase) in &mut released {
            self.leave(path, std::mem::replace(phase, Phase::Skipped), cx);
        }
        for status in self.manager.statuses() {
            if checkouts.iter().any(|at| status.root.starts_with(at)) {
                self.manager.stop_server(status.id);
            }
        }
        for checkout in &checkouts {
            self.homes.remove(checkout);
            self.building.remove(checkout);
        }
        released.into_iter().map(|(path, _)| path).collect()
    }

    /// The project's settings changed: its documents are closed, its
    /// servers stopped, and everything that may be open is opened again
    /// under the new settings.
    pub(super) fn settings_changed(&mut self, project: &str, cx: &mut Context<Self>) {
        let key = path_key(project);
        self.release(
            |home| path_key(&home.context.project.to_string_lossy()) == key,
            cx,
        );
        self.project_opened(project, cx);
        self.retry_skipped(cx);
        // A language switched on or off changes which editors have the
        // servers' providers.
        code_editor::hub(cx).update(cx, |_, cx| cx.emit(code_editor::HubEvent::Providers));
    }

    /// Looks at the checkout's disk again, as after a tool was installed or
    /// a virtual environment made, and reopens its documents.
    pub(super) fn reload_checkout(&mut self, root: &Path, cx: &mut Context<Self>) {
        self.release(|home| root.starts_with(&home.context.checkout), cx);
        self.retry_skipped(cx);
    }

    fn retry_skipped(&mut self, cx: &mut Context<Self>) {
        let skipped: Vec<PathBuf> = (self.docs.iter())
            .filter(|(_, doc)| matches!(doc.phase, Phase::Skipped))
            .map(|(path, _)| path.clone())
            .collect();
        for path in skipped {
            self.begin(&path, cx);
        }
    }

    /// Pairs of (how servers spell a checkout, how the editors do), for the
    /// checkouts where the two differ.
    pub(super) fn spellings(&self) -> Vec<(PathBuf, PathBuf)> {
        (self.homes.iter())
            .filter_map(|(checkout, home)| Some((home.real.clone()?, checkout.clone())))
            .collect()
    }

    /// The project a server belongs to, by its root.
    pub(super) fn project_of(&self, root: &Path) -> Option<PathBuf> {
        (self.homes.iter())
            .filter(|(checkout, _)| root.starts_with(checkout))
            .max_by_key(|(checkout, _)| checkout.as_os_str().len())
            .map(|(_, home)| home.context.project.clone())
    }

    #[cfg(test)]
    pub(crate) fn is_open(&self, path: &Path) -> bool {
        matches!(
            self.docs.get(path).map(|doc| &doc.phase),
            Some(Phase::Open { .. })
        )
    }
}

/// Resident bytes per process group led by one of `leaders`. A server leads
/// its own group, and the group is where its real work may run (basedpyright
/// is a launcher that runs node). Reads the process table: blocking.
fn group_memory(leaders: &[u32]) -> HashMap<u32, u64> {
    let mut usage = HashMap::new();
    if !cfg!(unix) {
        return usage;
    }
    let Ok(output) = std::process::Command::new("/bin/ps")
        .env("LC_ALL", "C")
        .args(["-axww", "-o", "pid=,pgid=,rss="])
        .output()
    else {
        return usage;
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(group), Some(resident)) = (
            fields.next().and_then(|field| field.parse::<u32>().ok()),
            fields.next().and_then(|field| field.parse::<u32>().ok()),
            fields.next().and_then(|field| field.parse::<u64>().ok()),
        ) else {
            continue;
        };
        // A server that did not get its own group still counts itself.
        let leader = if leaders.contains(&group) { group } else { pid };
        if leaders.contains(&leader) {
            *usage.entry(leader).or_insert(0) += resident.saturating_mul(1024);
        }
    }
    usage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_a_server_knows_and_can_take_are_synced() {
        let saved = settings::InsightSettings::default();
        assert!(served(Path::new("/p/app.py"), &saved));
        assert!(!served(Path::new("/p/notes.md"), &saved));
        // The other languages wait for the project to switch them on.
        let rust = settings::InsightSettings {
            languages: vec!["rust-analyzer".into()],
            ..Default::default()
        };
        for other in ["/p/main.rs", "/p/main.go", "/p/app.ts", "/p/app.jsx"] {
            assert!(!served(Path::new(other), &saved), "{other}");
            assert_eq!(served(Path::new(other), &rust), other.ends_with(".rs"));
        }
        let off = rust.for_manager().disabled_servers;
        assert!(off.contains("gopls") && off.contains("typescript"));
        assert!(!off.contains("rust-analyzer") && !off.contains("ruff"));
        assert!(!too_big("x = 1\n"));
        assert!(too_big(&"x".repeat(MAX_LINE_BYTES + 1)));
        assert!(too_big(&"x = 1\n".repeat(MAX_BYTES / 6 + 1)));
    }

    #[cfg(unix)]
    #[test]
    fn a_process_group_is_measured_by_its_leader() {
        let own = std::process::id();
        let usage = group_memory(&[own]);
        assert!(usage.get(&own).is_some_and(|bytes| *bytes > 0));
        assert!(group_memory(&[]).is_empty());
    }
}
