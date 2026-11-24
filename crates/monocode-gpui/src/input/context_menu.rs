//! Shared by composers, message editing, search fields and the file editor.
//! GPUI draws these fields itself, so there is no webview editing menu.
use super::*;
use crate::popover::{Anchor, Menu, MenuAction, MenuEvent, MenuItem, menu_popover};

/// Opens an owner's own menu at a right-click. The caret has already moved
/// there, unless it landed inside the selection. Called after the input's
/// update ends, so the owner can read it.
pub(crate) type MenuDelegate = std::rc::Rc<dyn Fn(Point<Pixels>, &mut Window, &mut App)>;

/// What the editing commands can do right now, for an owner's own menu.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct EditingCaps {
    pub definition: bool,
    pub usages: bool,
    pub navigation: bool,
    pub comment: bool,
    pub actions: bool,
    pub rename: bool,
    pub read_only: bool,
    pub folds: bool,
}

pub(super) struct EditingMenu {
    menu: Entity<Menu>,
    position: Point<Pixels>,
    _events: gpui::Subscription,
}

impl Input {
    /// Right-clicks call `open` instead of opening the shared text menu:
    /// the file editor builds a menu of its own.
    pub(crate) fn delegate_context_menu(&mut self, open: MenuDelegate) {
        self.delegate_menu = Some(open);
    }
    pub(crate) fn editing_caps(&self) -> EditingCaps {
        EditingCaps {
            definition: self.link_provider().is_some(),
            usages: self.references_provider().is_some(),
            navigation: self.navigation_provider().is_some(),
            comment: self.comment_syntax().is_some(),
            actions: self.actions_provider().is_some(),
            rename: self.rename_provider().is_some(),
            read_only: self.read_only,
            folds: self.code.is_some() && !self.editor_analysis().folds.is_empty(),
        }
    }
    fn editing_menu_items(&self, clipboard: bool) -> Vec<MenuItem> {
        let modifier = if cfg!(target_os = "macos") {
            "⌘"
        } else {
            "Ctrl+"
        };
        let selected = !self.secret && !self.selection.is_empty();
        let action = |id, label, key, enabled: bool| {
            MenuAction::new(id, label)
                .shortcut(format!("{modifier}{key}"))
                .disabled(!enabled)
                .into()
        };
        let mut items = Vec::new();
        // Shown only in an editor a code insight provider is installed on.
        if self.link_provider().is_some() {
            items.push(
                MenuAction::new("definition", "Go to Definition")
                    .shortcut("F12")
                    .into(),
            );
        }
        if self.references_provider().is_some() {
            items.push(
                MenuAction::new("usages", "Find Usages")
                    .shortcut(if cfg!(target_os = "macos") {
                        "⇧F12"
                    } else {
                        "Shift+F12"
                    })
                    .into(),
            );
        }
        if !items.is_empty() {
            items.push(MenuItem::Separator);
        }
        if !self.read_only && !self.secret {
            items.extend([
                action("undo", "Undo", "Z", self.history_available(false)),
                action(
                    "redo",
                    "Redo",
                    if cfg!(target_os = "macos") {
                        "⇧Z"
                    } else {
                        "Shift+Z"
                    },
                    self.history_available(true),
                ),
                MenuItem::Separator,
            ]);
        }
        if self.comment_syntax().is_some() {
            items.extend([
                action("comment", "Toggle Comment", "/", true),
                MenuItem::Separator,
            ]);
        }
        items.extend([
            action("cut", "Cut", "X", selected && !self.read_only),
            action("copy", "Copy", "C", selected),
            // Dispatch Paste through the focused field's ancestors, just as
            // the shortcut does: composers also accept clipboard images.
            action("paste", "Paste", "V", clipboard && !self.read_only),
            MenuItem::Separator,
            action("select-all", "Select All", "A", !self.content.is_empty()),
        ]);
        items
    }

    pub(super) fn open_context_menu(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(target_os = "macos")]
        crate::traffic_lights::focus_content(window);
        self.end_drag();
        window.focus(&self.focus, cx);
        let index = self.mouse_index(event.position);
        // Preserve a selection clicked inside; otherwise move the insertion
        // point so Paste applies where the user opened the menu.
        if self.selection.is_empty() || !self.selection.contains(&index) {
            self.move_to(index, cx);
        }
        if let Some(open) = self.delegate_menu.clone() {
            let at = event.position;
            window.defer(cx, move |window, cx| open(at, window, cx));
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let items = self.editing_menu_items(cx.read_from_clipboard().is_some());
        let menu = cx.new(|cx| Menu::new("Text editing", items, window, cx));
        let events = cx.subscribe_in(&menu, window, |this, _, event, window, cx| {
            this.context_menu = None;
            if let MenuEvent::Picked(id, _) = event {
                let action: Box<dyn gpui::Action> = match id.as_ref() {
                    "undo" => Box::new(Undo),
                    "redo" => Box::new(Redo),
                    "cut" => Box::new(Cut),
                    "copy" => Box::new(Copy),
                    "paste" => Box::new(Paste),
                    "select-all" => Box::new(SelectAll),
                    "comment" => Box::new(ToggleComment),
                    "definition" => Box::new(GoToDefinition),
                    "usages" => Box::new(FindUsages),
                    _ => return,
                };
                window.focus(&this.focus, cx);
                window.dispatch_action(action, cx);
            }
            cx.notify();
        });
        self.context_menu = Some(EditingMenu {
            menu,
            position: event.position,
            _events: events,
        });
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn editing_menu(&self, cx: &App) -> Option<impl IntoElement> {
        self.context_menu.as_ref().map(|state| {
            menu_popover(&state.menu, Anchor::Point(state.position), cx).without_animation()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn editing_actions_respect_selection_clipboard_read_only_and_secrets(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            let input = cx.new(Input::new);
            input.update(cx, |input, _| {
                let enabled = |input: &Input, clipboard| {
                    input
                        .editing_menu_items(clipboard)
                        .iter()
                        .filter_map(|item| match item {
                            MenuItem::Action(action) if !action.disabled => {
                                Some(action.id.to_string())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                };
                assert!(enabled(input, false).is_empty());
                input.content = "hello".into();
                input.selection = 0..5;
                assert_eq!(enabled(input, true), ["cut", "copy", "paste", "select-all"]);
                input.read_only = true;
                assert_eq!(enabled(input, true), ["copy", "select-all"]);
                input.secret = true;
                input.read_only = false;
                assert_eq!(enabled(input, true), ["paste", "select-all"]);
                input.secret = false;
                input.selection = 5..5;
                assert_eq!(enabled(input, false), ["select-all"]);
            });
        });
    }
}
