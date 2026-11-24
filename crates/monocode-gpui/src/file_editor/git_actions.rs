//! Git from the code editor: the file's history, its changes against the
//! last commit, blame, a line's history and the file on its remote, and
//! comparing the editor's text with the clipboard.
use super::{
    CompareWithClipboard, FileEditor, FileEvent, NextChange, OpenOnRemote, PreviousChange,
    RevertChange, ShowLineHistory, ToggleBlame,
};
use crate::popover::MenuAction;
use gpui::{Action, App, Context, Focusable, Window};

/// A shortcut as this platform writes it.
fn keys(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        mac
    } else {
        other
    }
}

impl FileEditor {
    /// The rows of the code menu's Git submenu.
    pub(super) fn git_menu_items(&self, cx: &App) -> Vec<MenuAction> {
        let input = self.input.read(cx);
        let changes = !input.editor_analysis().changes.is_empty();
        let writable = !input.editing_caps().read_only;
        vec![
            MenuAction::new("history", "Show History"),
            MenuAction::new("previous-change", "Previous Change")
                .shortcut(keys("⌃⌥⇧↑", "Ctrl+Alt+Shift+Up"))
                .disabled(!changes),
            MenuAction::new("next-change", "Next Change")
                .shortcut(keys("⌃⌥⇧↓", "Ctrl+Alt+Shift+Down"))
                .disabled(!changes),
            MenuAction::new("revert-change", "Revert Change").disabled(!changes || !writable),
        ]
    }
    /// Takes a picked Git row; `false` when it is not one.
    pub(super) fn git_menu_pick(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let action: Box<dyn Action> = match id {
            "history" => {
                cx.emit(FileEvent::FileHistory {
                    cwd: self.cwd.clone(),
                    path: self.path.clone(),
                });
                return true;
            }
            "previous-change" => Box::new(PreviousChange),
            "next-change" => Box::new(NextChange),
            "revert-change" => Box::new(RevertChange),
            _ => return false,
        };
        window.focus(&self.input.focus_handle(cx), cx);
        window.dispatch_action(action, cx);
        true
    }
    pub(super) fn toggle_blame(&mut self, _: &ToggleBlame, _: &mut Window, cx: &mut Context<Self>) {
        cx.propagate();
    }
    pub(super) fn show_line_history(
        &mut self,
        _: &ShowLineHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }
    pub(super) fn open_on_remote(
        &mut self,
        _: &OpenOnRemote,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }
    pub(super) fn compare_with_clipboard(
        &mut self,
        _: &CompareWithClipboard,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }
}
