//! The shell's extension points for the code features (run, code insight,
//! debug; `docs/native/code.md`). Each feature lives in its own module and
//! reaches the window only through here: dock tabs that run a process or show
//! a view, right panel tabs, footer chips, owned processes, quick open
//! commands and modes, project settings pages, rail row statuses, and the
//! context and events a feature needs to know where it is. `shell.rs` holds
//! a few one-line hooks into `shell/code_shell.rs` and nothing of a feature.
//!
//! Two rules keep this safe. Functions that take a `Window` borrow the shell,
//! so they are called from a feature's own entities, tasks and click
//! handlers, never from inside a closure the shell calls while it draws
//! (`FooterChip::state`, `FooterChip::items`). Everything the shell calls
//! back with a `Window` (a picked command, a picked chip row, a settings
//! page being built) runs after the shell's own update is over, so those
//! callbacks may call anything here.
// The run, code insight and debug modules are the callers; until they land,
// only the tests reach most of this.
#![allow(dead_code, unused_imports)]
pub(crate) mod chips;
pub(crate) mod commands;
pub(crate) mod context;
pub(crate) mod dock;
pub(crate) mod launch;
pub(crate) mod panel_tabs;
pub(crate) mod processes;
pub(crate) mod rail;
pub(crate) mod settings;

use crate::shell::Shell;
use gpui::{AnyWindowHandle, App, Context, Global, WeakEntity, Window};

/// The window the features draw into, known once the shell has drawn.
struct Host {
    shell: WeakEntity<Shell>,
    window: AnyWindowHandle,
}
impl Global for Host {}

/// Installs the code features. Called once at startup, before the window
/// opens; each feature adds one line here that calls its own `init`.
pub(crate) fn register(cx: &mut App) {
    context::init(cx);
    // Order matters: debug wraps the hover that code insight installs.
    crate::code_insight::init(cx);
    crate::run_ui::init(cx);
    crate::debug_ui::init(cx);
}

/// The shell's hook, once a frame: remembers the window and tells the
/// features what changed around them.
pub(crate) fn frame(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
    let this = cx.weak_entity();
    let known = (cx.try_global::<Host>()).is_some_and(|host| host.shell == this);
    if !known {
        cx.set_global(Host {
            shell: this,
            window: window.window_handle(),
        });
    }
    shell.publish_code_settings_once(cx);
    shell.place_dock_tabs(cx);
    context::track(shell, cx);
}

/// Runs `f` on the shell. `None` before the window has drawn. Must not be
/// called while the shell is itself being updated (see the module's rules).
pub(crate) fn with_shell<R>(
    window: &mut Window,
    cx: &mut App,
    f: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>) -> R,
) -> Option<R> {
    let shell = cx.try_global::<Host>()?.shell.upgrade()?;
    Some(shell.update(cx, |shell, cx| f(shell, window, cx)))
}

/// Runs `f` with the app's window, for a task that has none at hand. `None`
/// before the window has drawn, and from inside one of the window's own
/// callbacks, which already have it.
pub(crate) fn in_window<R>(cx: &mut App, f: impl FnOnce(&mut Window, &mut App) -> R) -> Option<R> {
    let window = cx.try_global::<Host>()?.window;
    window.update(cx, |_, window, cx| f(window, cx)).ok()
}

/// Redraws the chrome that shows what features contribute (chips, rail
/// statuses, dock tab titles) once the current update is over. The registry
/// functions here call it themselves; a feature calls it when what its chip
/// reports has changed.
pub(crate) fn refresh(cx: &mut App) {
    let Some(shell) = cx.try_global::<Host>().map(|host| host.shell.clone()) else {
        return;
    };
    cx.defer(move |cx| {
        let _ = shell.update(cx, |shell, cx| shell.code_refresh(cx));
    });
}
