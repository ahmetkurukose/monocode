//! How run hands a process to the debugger without either feature knowing
//! the other: debug installs a launcher, run asks for one.
use super::context::Checkout;
use gpui::{App, Global, Window};
use monocode_core::code::spawn::Spawn;
use std::rc::Rc;

pub(crate) type Launcher = Rc<dyn Fn(Spawn, Checkout, &mut Window, &mut App)>;

struct Debugger(Launcher);
impl Global for Debugger {}

pub(crate) fn set_debugger(launcher: Launcher, cx: &mut App) {
    cx.set_global(Debugger(launcher));
}

pub(crate) fn can_debug(cx: &App) -> bool {
    cx.try_global::<Debugger>().is_some()
}

/// Starts `spawn` under the debugger. False when no debugger is installed.
pub(crate) fn debug(spawn: Spawn, checkout: Checkout, window: &mut Window, cx: &mut App) -> bool {
    let Some(launcher) = cx
        .try_global::<Debugger>()
        .map(|debugger| debugger.0.clone())
    else {
        return false;
    };
    launcher(spawn, checkout, window, cx);
    true
}
