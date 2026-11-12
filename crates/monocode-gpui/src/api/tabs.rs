//! Managing the request tabs as a group: closing several at once, saving
//! them all, reopening what was closed and stepping between them.

use super::*;

const SHORTCUT_CLOSE_ALL: &str = if cfg!(target_os = "macos") {
    "⌘⇧W"
} else {
    "Ctrl+Shift+W"
};
const SHORTCUT_REOPEN: &str = if cfg!(target_os = "macos") {
    "⌘⇧T"
} else {
    "Ctrl+Shift+T"
};
const SHORTCUT_SAVE_ALL: &str = if cfg!(target_os = "macos") {
    "⌘⌥S"
} else {
    "Ctrl+Alt+S"
};

impl ApiView {
    pub(super) fn dirty_ids(&self) -> Vec<u64> {
        self.tabs
            .iter()
            .filter(|t| t.dirty())
            .map(|t| t.id)
            .collect()
    }

    /// Closes `ids`, asking first when any has unsaved changes.
    pub(super) fn close_many(
        &mut self,
        ids: Vec<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids: Vec<u64> = ids
            .into_iter()
            .filter(|id| self.tab(*id).is_some())
            .collect();
        if ids.is_empty() {
            return;
        }
        if ids
            .iter()
            .any(|id| self.tab(*id).is_some_and(RequestTab::dirty))
        {
            self.dialog = Some(DialogKind::CloseMany(ids));
            cx.notify();
            return;
        }
        self.discard_many(ids, window, cx);
    }

    pub(super) fn discard_many(
        &mut self,
        ids: Vec<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for id in ids {
            self.discard_tab(id, window, cx);
        }
    }

    pub(super) fn close_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.tabs.iter().map(|t| t.id).collect();
        self.close_many(ids, window, cx);
    }

    pub(super) fn close_others(&mut self, keep: u64, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self
            .tabs
            .iter()
            .map(|t| t.id)
            .filter(|id| *id != keep)
            .collect();
        self.close_many(ids, window, cx);
    }

    pub(super) fn close_right(&mut self, of: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.tabs.iter().position(|t| t.id == of) else {
            return;
        };
        let ids = self.tabs[at + 1..].iter().map(|t| t.id).collect();
        self.close_many(ids, window, cx);
    }

    pub(super) fn close_left(&mut self, of: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.tabs.iter().position(|t| t.id == of) else {
            return;
        };
        let ids = self.tabs[..at].iter().map(|t| t.id).collect();
        self.close_many(ids, window, cx);
    }

    /// Closes every tab with nothing unsaved, keeping the ones being worked on.
    pub(super) fn close_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self
            .tabs
            .iter()
            .filter(|t| !t.dirty())
            .map(|t| t.id)
            .collect();
        self.discard_many(ids, window, cx);
    }

    /// Saves every tab with changes. Stops at the first that fails.
    pub(super) fn save_all(&mut self, cx: &mut Context<Self>) -> bool {
        let ids = self.dirty_ids();
        let count = ids.len();
        for id in ids {
            if !self.save(id, cx) {
                return false;
            }
        }
        if count > 0 {
            self.notify_user(
                if count == 1 {
                    "Saved 1 request.".to_owned()
                } else {
                    format!("Saved {count} requests.")
                },
                cx,
            );
        }
        true
    }

    /// Brings back the tab closed last whose request is still there.
    pub(super) fn reopen_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some((file, path)) = self.closed.pop() {
            let exists = self
                .collection(&file)
                .is_some_and(|c| c.collection.request(&path).is_some());
            if exists && !self.tabs.iter().any(|t| t.file == file && t.path == path) {
                self.open_request(&file, path, window, cx);
                return;
            }
        }
        self.notify_user("No closed request to reopen.", cx);
    }

    /// Moves the front tab `delta` places along, wrapping round.
    pub(super) fn step_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tabs.len() as isize;
        if count < 2 {
            return;
        }
        let at = self
            .active
            .and_then(|id| self.tabs.iter().position(|t| t.id == id))
            .unwrap_or(0) as isize;
        let next = (at + delta).rem_euclid(count) as usize;
        let id = self.tabs[next].id;
        self.activate(id, window, cx);
    }

    /// The tab at `number` (1 is the first); 9 is always the last.
    pub(super) fn go_to_tab(&mut self, number: usize, window: &mut Window, cx: &mut Context<Self>) {
        let index = if number >= 9 {
            self.tabs.len().checked_sub(1)
        } else {
            (number >= 1 && number <= self.tabs.len()).then(|| number - 1)
        };
        if let Some(id) = index.map(|i| self.tabs[i].id) {
            self.activate(id, window, cx);
        }
    }

    pub(super) fn open_tab_menu(
        &mut self,
        id: u64,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        let count = self.tabs.len();
        let dirty = self.tabs[index].dirty();
        let any_saved = self.tabs.iter().any(|t| !t.dirty());
        let items: Vec<MenuItem> = vec![
            MenuAction::new("close", "Close")
                .shortcut(if cfg!(target_os = "macos") {
                    "⌘W"
                } else {
                    "Ctrl+W"
                })
                .into(),
            MenuAction::new("others", "Close other tabs")
                .disabled(count < 2)
                .into(),
            MenuAction::new("right", "Close tabs to the right")
                .disabled(index + 1 >= count)
                .into(),
            MenuAction::new("left", "Close tabs to the left")
                .disabled(index == 0)
                .into(),
            MenuAction::new("saved", "Close saved tabs")
                .disabled(!any_saved)
                .into(),
            MenuAction::new("all", "Close all tabs")
                .shortcut(SHORTCUT_CLOSE_ALL)
                .into(),
            MenuItem::Separator,
            MenuAction::new("save", "Save")
                .shortcut(if cfg!(target_os = "macos") {
                    "⌘S"
                } else {
                    "Ctrl+S"
                })
                .disabled(!dirty)
                .into(),
            MenuAction::new("curl", "Copy as cURL").into(),
            MenuAction::new("reveal", "Show in the tree").into(),
        ];
        self.open_menu(MenuKind::Tab(id), items, "Tab actions", at, window, cx);
    }

    pub(super) fn run_tab_action(
        &mut self,
        id: u64,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            "close" => self.close_tab(id, window, cx),
            "others" => self.close_others(id, window, cx),
            "right" => self.close_right(id, window, cx),
            "left" => self.close_left(id, window, cx),
            "saved" => self.close_saved(window, cx),
            "all" => self.close_all(window, cx),
            "save" => {
                self.save(id, cx);
            }
            "curl" => self.copy_curl(id, cx),
            "reveal" => {
                if let Some((file, path)) = self.tab(id).map(|t| (t.file.clone(), t.path.clone())) {
                    self.collapsed.remove(&Self::collection_key(&file));
                    for depth in 1..path.len() {
                        self.collapsed
                            .remove(&Self::folder_key(&file, &path[..depth]));
                    }
                    self.save_state();
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    /// The strip's overflow menu: group actions, then every open tab.
    pub(super) fn open_tabs_menu(
        &mut self,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dirty = self.dirty_ids().len();
        let mut items: Vec<MenuItem> = vec![
            MenuAction::new("save-all", "Save all")
                .shortcut(SHORTCUT_SAVE_ALL)
                .disabled(dirty == 0)
                .into(),
            MenuAction::new("reopen", "Reopen closed tab")
                .shortcut(SHORTCUT_REOPEN)
                .disabled(self.closed.is_empty())
                .into(),
            MenuItem::Separator,
            MenuAction::new("saved", "Close saved tabs")
                .disabled(self.tabs.iter().all(|t| t.dirty()))
                .into(),
            MenuAction::new("all", "Close all tabs")
                .shortcut(SHORTCUT_CLOSE_ALL)
                .disabled(self.tabs.is_empty())
                .into(),
        ];
        if !self.tabs.is_empty() {
            items.push(MenuItem::Separator);
        }
        for tab in &self.tabs {
            let mark = if tab.dirty() { " •" } else { "" };
            items.push(
                MenuAction::new(
                    format!("go:{}", tab.id),
                    format!("{}  {}{mark}", tab.draft.method, tab.draft.name),
                )
                .checked(self.active == Some(tab.id))
                .into(),
            );
        }
        self.open_menu(MenuKind::Tabs, items, "Open requests", at, window, cx);
    }

    pub(super) fn run_tabs_action(
        &mut self,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            "save-all" => {
                self.save_all(cx);
            }
            "reopen" => self.reopen_closed(window, cx),
            "saved" => self.close_saved(window, cx),
            "all" => self.close_all(window, cx),
            other => {
                if let Some(id) = other.strip_prefix("go:").and_then(|n| n.parse().ok()) {
                    self.activate(id, window, cx);
                }
            }
        }
    }
}
