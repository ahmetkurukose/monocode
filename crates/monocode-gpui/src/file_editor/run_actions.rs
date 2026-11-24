//! Run and Debug from the code editor: the file, or the test or entry
//! point at the caret, through the project's run and debug support.
use super::{DebugAtCaret, DebugFile, FileEditor, RunAtCaret, RunFile};
use crate::popover::MenuItem;
use gpui::{App, Context, Window};

impl FileEditor {
    /// The code menu's Run and Debug rows; empty when the file cannot run.
    pub(super) fn run_menu_items(&self, _cx: &App) -> Vec<MenuItem> {
        Vec::new()
    }
    /// Takes a picked row of `run_menu_items`; `false` when it is not one.
    pub(super) fn run_menu_pick(
        &mut self,
        _id: &str,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> bool {
        false
    }
    pub(super) fn run_file(&mut self, _: &RunFile, _: &mut Window, cx: &mut Context<Self>) {
        cx.propagate();
    }
    pub(super) fn debug_file(&mut self, _: &DebugFile, _: &mut Window, cx: &mut Context<Self>) {
        cx.propagate();
    }
    pub(super) fn run_at_caret(&mut self, _: &RunAtCaret, _: &mut Window, cx: &mut Context<Self>) {
        cx.propagate();
    }
    pub(super) fn debug_at_caret(
        &mut self,
        _: &DebugAtCaret,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }
}
