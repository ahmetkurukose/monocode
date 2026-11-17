//! Tabs a feature opens in a project's terminal dock: one that runs a given
//! process (a run configuration, a test, a debuggee) and one that shows a
//! view of the feature's own (a debug session). Both sit in the dock's strip
//! with the terminals and are selected, reordered, closed and moved with the
//! dock like them; neither is saved, since a restart ends what they show.
//! Ordinary terminals are untouched.
use super::context::Checkout;
use crate::terminal::Terminal;
pub(crate) use crate::terminal::{
    hosted::{DockArea, DockPlacement, PlacementChanged, TabClosed},
    run::{Run, RunEvent, RunExit, RunState},
};
use gpui::{AnyView, App, Entity, FocusHandle, SharedString, WeakEntity, Window};
use monocode_core::code::spawn::Spawn;

/// A process to run in a dock tab.
pub(crate) struct RunRequest {
    /// The label titles the tab; `env` is added to the checkout's terminal
    /// environment, its virtual environment included.
    pub spawn: Spawn,
    /// The checkout it belongs to; the tab opens in its project's dock.
    pub checkout: Checkout,
    /// The label of the agent session that started it, shown in the dock's
    /// header while the tab is selected.
    pub agent: Option<SharedString>,
    /// Show the dock on the new tab. Without it the tab is added and the
    /// dock stays as it is.
    pub reveal: bool,
    /// Give the tab the keyboard.
    pub focus: bool,
}
impl RunRequest {
    /// Revealed, without taking the keyboard.
    pub(crate) fn new(spawn: Spawn, checkout: Checkout) -> Self {
        Self {
            spawn,
            checkout,
            agent: None,
            reveal: true,
            focus: false,
        }
    }
}

/// Starts `request.spawn` as the child of a new tab in its project's dock.
/// The returned `Run` reports the state, the process id and the output, and
/// stops and reruns it; the tab stays, read-only, once the process has
/// ended. An error is a sentence for the user (a remote project has no
/// terminals; the window is not up yet).
pub(crate) fn run(
    request: RunRequest,
    window: &mut Window,
    cx: &mut App,
) -> Result<Entity<Run>, String> {
    super::with_shell(window, cx, |shell, window, cx| {
        shell.run_in_dock(request, window, cx)
    })
    .unwrap_or_else(|| Err("The window is not ready yet.".into()))
}

/// A view to show as a dock tab.
pub(crate) struct DockTab {
    /// The feature's name for the tab, unique in its project's dock, for
    /// `find_tab`.
    pub id: SharedString,
    pub title: SharedString,
    pub icon: &'static str,
    pub view: AnyView,
    /// What takes the keyboard when the tab is focused.
    pub focus: Option<FocusHandle>,
    /// Drawn in the dock's header while the tab is selected: step buttons.
    pub controls: Option<AnyView>,
    pub checkout: Checkout,
    /// Told once the tab is gone, however it was closed.
    pub on_close: Option<TabClosed>,
    /// Told where the dock is and how much room it has, at first and when
    /// either changes, so the view can stack its columns in a narrow dock.
    pub on_placement: Option<PlacementChanged>,
    pub reveal: bool,
}

/// A tab of a dock, a run's or a view's, for acting on it from outside.
#[derive(Clone)]
pub(crate) struct DockTabHandle(pub(crate) WeakEntity<Terminal>);
impl DockTabHandle {
    /// The tab of a run.
    pub(crate) fn of_run(run: &Entity<Run>, cx: &App) -> Self {
        Self(run.read(cx).terminal())
    }
    /// False once the tab is closed.
    pub(crate) fn is_open(&self) -> bool {
        self.0.upgrade().is_some()
    }
    /// Selects the tab, bringing its project to the front and opening the
    /// dock if needed, and gives it the keyboard. False when it is closed.
    pub(crate) fn reveal(&self, window: &mut Window, cx: &mut App) -> bool {
        let Some(tab) = self.0.upgrade() else {
            return false;
        };
        super::with_shell(window, cx, |shell, window, cx| {
            shell.reveal_dock_tab(&tab, window, cx)
        })
        .unwrap_or(false)
    }
    /// Closes the tab without asking. A run still going is hung up.
    pub(crate) fn close(&self, window: &mut Window, cx: &mut App) {
        if let Some(tab) = self.0.upgrade() {
            super::with_shell(window, cx, |shell, window, cx| {
                shell.close_dock_tab(&tab, window, cx)
            });
        }
    }
    pub(crate) fn set_title(&self, title: impl Into<String>, cx: &mut App) {
        let title = title.into();
        if self
            .0
            .update(cx, |tab, cx| tab.set_title(title, cx))
            .is_ok()
        {
            super::refresh(cx);
        }
    }
    /// What closing a view's tab would interrupt ("debug session"): with it
    /// set, the dock asks before closing, as it does for a busy terminal.
    pub(crate) fn set_busy(&self, busy: Option<String>, cx: &mut App) {
        let _ = self.0.update(cx, |tab, cx| tab.set_busy(busy, cx));
    }
    /// Where the tab's dock is; `None` until it has been drawn.
    pub(crate) fn placement(&self, cx: &App) -> Option<DockPlacement> {
        let tab = self.0.upgrade()?;
        tab.read(cx).hosted_view()?.placement
    }
}

/// Adds `tab` to its project's dock.
pub(crate) fn add_tab(
    tab: DockTab,
    window: &mut Window,
    cx: &mut App,
) -> Result<DockTabHandle, String> {
    super::with_shell(window, cx, |shell, window, cx| {
        shell.add_dock_view(tab, window, cx)
    })
    .unwrap_or_else(|| Err("The window is not ready yet.".into()))
}

/// The view tab a feature added to `project`'s dock as `id`, if it is still
/// open. Reads the shell, so not from inside a closure the shell calls while
/// it draws.
pub(crate) fn find_tab(project: &str, id: &str, cx: &App) -> Option<DockTabHandle> {
    let shell = cx.try_global::<super::Host>()?.shell.upgrade()?;
    shell.read(cx).find_dock_view(project, id, cx)
}
