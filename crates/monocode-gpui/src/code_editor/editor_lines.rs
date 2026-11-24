//! Line and selection commands of the code editor: growing and shrinking
//! the selection by syntax, duplicating and moving lines, and several
//! carets (column selection, a caret added above or below). Compiled as a
//! child of the editor so it can reach the buffer.
use super::*;

impl Input {
    pub(in crate::input) fn expand_selection(
        &mut self,
        _: &ExpandSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }
    pub(in crate::input) fn shrink_selection(
        &mut self,
        _: &ShrinkSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }
    pub(in crate::input) fn duplicate_line(
        &mut self,
        _: &DuplicateLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }
    pub(in crate::input) fn move_line_up(
        &mut self,
        _: &MoveLineUp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }
    pub(in crate::input) fn move_line_down(
        &mut self,
        _: &MoveLineDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }
    pub(in crate::input) fn toggle_column_selection(
        &mut self,
        _: &ToggleColumnSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }
    pub(in crate::input) fn add_caret_above(
        &mut self,
        _: &AddCaretAbove,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }
    pub(in crate::input) fn add_caret_below(
        &mut self,
        _: &AddCaretBelow,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }
    /// Whether column selection mode is on, for the menu's check.
    pub(crate) fn column_selection(&self) -> bool {
        false
    }
}
