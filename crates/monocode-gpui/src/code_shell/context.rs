//! Where a feature is: which project and checkout a file, a pane or the
//! focused surface belongs to, the focused file editor, the project's
//! terminal setup and toolchain, and the two ways out of a feature's own
//! views (open a file at a line). Also the
//! app-wide events a feature follows: projects and checkouts coming and
//! going, the focused file changing, the app quitting.
//!
//! The lookups read a snapshot the shell refreshes each frame, so they are
//! safe from anywhere, including while the shell draws.
use crate::{file_editor::FileEditor, shell::Shell};
use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, WeakEntity, Window};
use monocode_core::{code::toolchain::Toolchain, files::path_key, terminal_setup::TerminalSetup};
use std::path::Path;

/// One working copy of a project: the project's own folder, or a worktree
/// of it. `project` is the project's path as the shell knows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Checkout {
    pub project: String,
    pub folder: String,
}

#[derive(Clone, Debug)]
pub(crate) struct FocusedFile {
    /// Absolute.
    pub path: String,
    pub checkout: Checkout,
    /// The pane showing it.
    pub pane: i64,
    pub editor: WeakEntity<FileEditor>,
}
impl PartialEq for FocusedFile {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.pane == other.pane && self.editor == other.editor
    }
}

/// What the shell shows, as the features see it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Snapshot {
    /// The project in front and the checkout its panes work in.
    pub focus: Checkout,
    /// Open local projects, in rail order.
    pub projects: Vec<String>,
    /// Every checkout the shell knows of: each project's folder and the
    /// worktrees its sessions and its worktree switcher use.
    pub checkouts: Vec<Checkout>,
    /// The file editor of the pane in front, if it shows a file.
    pub focused: Option<FocusedFile>,
    /// The open projects that are asleep: nothing may run for them.
    pub asleep: Vec<String>,
}

pub(crate) enum CodeEvent {
    ProjectOpened(String),
    ProjectClosed(String),
    /// The project was put to sleep: what the features run for it (language
    /// servers, debug sessions, runs) is to stop and give its memory back.
    ProjectSlept(String),
    /// The project is awake again: what its open files need comes back.
    ProjectWoke(String),
    CheckoutAdded(Checkout),
    CheckoutRemoved(Checkout),
    /// The pane in front shows another file, or none.
    FocusedFileChanged(Option<FocusedFile>),
}

/// Emits `CodeEvent`s; subscribe to `events(cx)`.
pub(crate) struct Events;
impl EventEmitter<CodeEvent> for Events {}

type QuitHook = Box<dyn FnOnce(&mut App)>;

struct State {
    snapshot: Snapshot,
    /// What the snapshot was built from, to skip rebuilding it.
    stamp: u64,
    events: Entity<Events>,
    quit: Vec<QuitHook>,
}
impl Global for State {}

pub(super) fn init(cx: &mut App) {
    if cx.has_global::<State>() {
        return;
    }
    let events = cx.new(|_| Events);
    cx.set_global(State {
        snapshot: Snapshot::default(),
        stamp: 0,
        events,
        quit: Vec::new(),
    });
    cx.on_app_quit(|cx| {
        for quit in std::mem::take(&mut cx.global_mut::<State>().quit) {
            quit(cx);
        }
        async {}
    })
    .detach();
}

/// The emitter of `CodeEvent`s. A feature subscribes in its `init`, before
/// the window opens, and so hears `ProjectOpened` for every restored project.
pub(crate) fn events(cx: &mut App) -> Entity<Events> {
    init(cx);
    cx.global::<State>().events.clone()
}

/// Runs `f` when the app quits, on the UI thread, before the process ends:
/// the place to shut servers and adapters down. It must not block for long.
pub(crate) fn on_quit(cx: &mut App, f: impl FnOnce(&mut App) + 'static) {
    init(cx);
    cx.global_mut::<State>().quit.push(Box::new(f));
}

/// The shell's frame hook: rebuilds the snapshot when what it is built from
/// changed, and emits what differs.
pub(super) fn track(shell: &mut Shell, cx: &mut Context<Shell>) {
    init(cx);
    let stamp = shell.code_stamp(cx);
    if cx.global::<State>().stamp == stamp {
        return;
    }
    let next = shell.code_snapshot(cx);
    let state = cx.global_mut::<State>();
    state.stamp = stamp;
    let before = std::mem::replace(&mut state.snapshot, next.clone());
    let events = state.events.clone();
    let mut changes = Vec::new();
    for project in &next.projects {
        if !before.projects.contains(project) {
            changes.push(CodeEvent::ProjectOpened(project.clone()));
        }
    }
    for checkout in &next.checkouts {
        if !before.checkouts.contains(checkout) {
            changes.push(CodeEvent::CheckoutAdded(checkout.clone()));
        }
    }
    for checkout in &before.checkouts {
        if !next.checkouts.contains(checkout) {
            changes.push(CodeEvent::CheckoutRemoved(checkout.clone()));
        }
    }
    for project in &before.projects {
        if !next.projects.contains(project) {
            changes.push(CodeEvent::ProjectClosed(project.clone()));
        }
    }
    for project in &next.asleep {
        if !before.asleep.contains(project) {
            changes.push(CodeEvent::ProjectSlept(project.clone()));
        }
    }
    for project in &before.asleep {
        if !next.asleep.contains(project) && next.projects.contains(project) {
            changes.push(CodeEvent::ProjectWoke(project.clone()));
        }
    }
    if before.focused != next.focused {
        changes.push(CodeEvent::FocusedFileChanged(next.focused.clone()));
    }
    if !changes.is_empty() {
        events.update(cx, |_, cx| {
            for change in changes {
                cx.emit(change);
            }
        });
    }
}

/// What the shell shows now. Empty before the window has drawn.
pub(crate) fn snapshot(cx: &App) -> Snapshot {
    cx.try_global::<State>()
        .map(|state| state.snapshot.clone())
        .unwrap_or_default()
}

/// The project in front and the checkout its panes work in.
pub(crate) fn focused_checkout(cx: &App) -> Option<Checkout> {
    let focus = cx.try_global::<State>()?.snapshot.focus.clone();
    (!focus.project.is_empty()).then_some(focus)
}

/// The file editor of the pane in front, with its path and checkout.
pub(crate) fn focused_file(cx: &App) -> Option<FocusedFile> {
    cx.try_global::<State>()?.snapshot.focused.clone()
}

fn inside(path: &str, folder: &str) -> bool {
    Path::new(&path_key(path)).starts_with(path_key(folder))
}

/// The checkout holding `path` (absolute): the deepest known folder it is
/// under. `None` for a file outside every open project.
pub(crate) fn checkout_for_path(path: &str, cx: &App) -> Option<Checkout> {
    (cx.try_global::<State>()?.snapshot.checkouts.iter())
        .filter(|checkout| inside(path, &checkout.folder))
        .max_by_key(|checkout| path_key(&checkout.folder).len())
        .cloned()
}

/// The checkout a pane works in: a chat's, or the one a file or terminal
/// pane borrows from its session. Reads the shell, so not from
/// inside a closure the shell calls while it draws.
pub(crate) fn checkout_for_pane(pane: i64, cx: &App) -> Option<Checkout> {
    let shell = cx.try_global::<super::Host>()?.shell.upgrade()?;
    shell.read(cx).code_pane_checkout(pane, cx)
}

/// Quick open's scope for the folder it lists.
pub(crate) fn scope(folder: &str, cx: &App) -> super::commands::Scope {
    let project = checkout_for_path(folder, cx)
        .map(|checkout| checkout.project)
        .or_else(|| focused_checkout(cx).map(|focus| focus.project))
        .unwrap_or_else(|| folder.to_owned());
    super::commands::Scope {
        project,
        folder: folder.to_owned(),
    }
}

/// The project's terminal setup; the default when it has none.
pub(crate) fn terminal_setup(project: &str, cx: &App) -> TerminalSetup {
    cx.try_global::<crate::terminal_setup::Setups>()
        .and_then(|setups| setups.0.get(&path_key(project)).cloned())
        .unwrap_or_default()
}

/// The checkout's toolchain: its virtual environment, interpreter and the
/// environment a child process of it is given. Reads the disk to find the
/// venv, so keep the result rather than asking each frame.
pub(crate) fn toolchain(checkout: &Checkout, cx: &App) -> Toolchain {
    Toolchain::for_folder(
        &terminal_setup(&checkout.project, cx),
        Path::new(&checkout.folder),
        Path::new(&checkout.project),
    )
}

/// Opens `path` (absolute) in a file tab of the pane in front, at `at`
/// (1-based line and column) when given: the route a link in a reply takes.
/// False before the window has drawn.
pub(crate) fn open_file(
    path: &str,
    at: Option<(usize, usize)>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let folder = checkout_for_path(path, cx).map(|checkout| checkout.folder);
    super::with_shell(window, cx, |shell, window, cx| {
        shell.code_open_file(folder, path.to_owned(), at, window, cx)
    })
    .is_some()
}

/// Shows the user a sentence: why something a feature was asked for did not
/// happen. False before the window has drawn.
pub(crate) fn notice(text: impl Into<String>, window: &mut Window, cx: &mut App) -> bool {
    let text = text.into();
    super::with_shell(window, cx, |shell, _, cx| shell.code_notice(text, cx)).is_some()
}

/// An agent session's title, for saying who started something. Reads the
/// shell, so not from inside a closure the shell calls while it draws.
pub(crate) fn session_title(session: i64, cx: &App) -> Option<String> {
    let shell = cx.try_global::<super::Host>()?.shell.upgrade()?;
    shell.read(cx).code_session_title(session)
}

/// Whether the project at `project` is asleep.
pub(crate) fn asleep(project: &str, cx: &App) -> bool {
    let key = path_key(project);
    (snapshot(cx).asleep.iter()).any(|asleep| path_key(asleep) == key)
}
