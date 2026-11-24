//! Rename and code actions at the caret, on the editor's side: asking the
//! providers, the list of actions (⌥↩) and the rename field (⇧F6), and
//! handing the edits they answer with to `refactor::apply`. Compiled as a
//! child of the editor so it can reach the buffer.
use super::*;
use crate::code_editor::{
    ActionGroup, CodeActionItem, CodeActionProvider, FileEdit, Position, RenameProvider,
    RenameRange, TextEdit, refactor,
};
use crate::popover::{
    Anchor, Menu, MenuAction, MenuEvent, MenuItem, MenuStyle, menu_popover, popover,
};
use crate::theme::content;
use gpui::{Subscription, WeakEntity};
use std::time::Duration;

/// Why a rename, code action or "Go To" came to nothing. The file editor
/// shows it above the text for a moment.
pub(crate) struct EditorNotice(pub String);
impl EventEmitter<EditorNotice> for Input {}

pub(in crate::input) struct ActionsMenu {
    menu: Entity<Menu>,
    position: Point<Pixels>,
    _events: Subscription,
}

pub(in crate::input) struct RenameField {
    field: Entity<Input>,
    /// What the server would rename, in bytes of `position.text`.
    range: Range<usize>,
    placeholder: String,
    position: Position,
    at: Point<Pixels>,
    owner: WeakEntity<Input>,
    _events: Subscription,
}

const ACTIONS_WIDTH: f32 = 380.;
const RENAME_WIDTH: f32 = 280.;

impl Input {
    pub(in crate::input) fn rename_provider(&self) -> Option<Rc<dyn RenameProvider>> {
        let ext = self.code.as_ref()?.ext.as_ref()?;
        ext.providers.rename.clone()
    }
    pub(in crate::input) fn actions_provider(&self) -> Option<Rc<dyn CodeActionProvider>> {
        let ext = self.code.as_ref()?.ext.as_ref()?;
        ext.providers.actions.clone()
    }

    /// Under the start of the line `offset` is on, in window coordinates.
    fn below(&self, offset: usize) -> Point<Pixels> {
        self.bounds.map_or_else(Point::default, |bounds| {
            bounds.origin + self.code_position(offset) - point(self.scroll_x, self.scroll_y)
                + point(px(0.), px(self.line_height))
        })
    }

    fn refactor_failed(&mut self, message: String, cx: &mut Context<Self>) {
        cx.emit(EditorNotice(message));
    }

    /// Hands a change to `refactor::apply` once this update is over: it
    /// reaches this editor's buffer too, through the hub.
    fn apply_change(&mut self, change: Vec<FileEdit>, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        window.defer(cx, move |_, cx| {
            let fail = move |message: String, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.refactor_failed(message, cx));
            };
            match refactor::apply(change, cx) {
                Err(message) => fail(message, cx),
                Ok(written) => cx
                    .spawn(async move |cx| {
                        if let Err(message) = written.await {
                            let _ = cx.update(|cx| fail(message, cx));
                        }
                    })
                    .detach(),
            }
        });
    }

    pub(in crate::input) fn show_context_actions(
        &mut self,
        _: &ShowContextActions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self.actions_provider() else {
            cx.propagate();
            return;
        };
        let range = self.selection.clone();
        let task = self.ask(
            self.cursor(),
            Duration::ZERO,
            window,
            cx,
            move |position, cx| {
                let found = provider.actions(position.clone(), range, cx);
                cx.foreground_executor()
                    .spawn(async move { (position, found.await) })
            },
            |this, (position, found), window, cx| match found {
                Ok(items) => this.open_actions(position, items, window, cx),
                Err(message) => this.refactor_failed(message, cx),
            },
        );
        if let Some(code) = &mut self.code {
            code.ext_mut().refactor_task = Some(task);
        }
    }

    /// The code actions of one kind at the caret (`CodeActionsOfKind`): one
    /// is taken at once, several are listed, none is said.
    pub(in crate::input) fn code_actions_of_kind(
        &mut self,
        _: &CodeActionsOfKind,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }

    fn open_actions(
        &mut self,
        position: Position,
        mut items: Vec<CodeActionItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Fixes first and the preferred one leading each group; otherwise as
        // the servers listed them.
        items.sort_by_key(|item| {
            (
                ActionGroup::of(item.kind.as_deref()),
                !item.preferred,
                item.disabled.is_some(),
            )
        });
        let mut rows: Vec<MenuItem> = Vec::new();
        let mut group = None;
        for (n, item) in items.iter().enumerate() {
            let this_group = ActionGroup::of(item.kind.as_deref());
            if group != Some(this_group) {
                rows.push(MenuItem::Header(this_group.title().into()));
                group = Some(this_group);
            }
            let row = MenuAction::new(n.to_string(), item.title.clone())
                .icon(match this_group {
                    ActionGroup::Fix => "ai-idea",
                    ActionGroup::Refactor => "wand",
                    _ => "file-script",
                })
                .disabled(item.disabled.is_some());
            rows.push(match &item.disabled {
                Some(reason) => row.description(reason.clone()).into(),
                None => row.into(),
            });
        }
        if rows.is_empty() {
            rows.push(MenuItem::Note("No actions available here".into()));
        }
        let menu = cx.new(|cx| {
            Menu::new("Context actions", rows, window, cx)
                .style(MenuStyle::Code)
                .width(ACTIONS_WIDTH)
        });
        let events = cx.subscribe_in(
            &menu,
            window,
            move |this, _, event: &MenuEvent, window, cx| {
                if let Some(code) = &mut this.code {
                    code.ext_mut().actions_menu = None;
                }
                if let MenuEvent::Picked(id, _) = event
                    && let Some(item) = id.parse::<usize>().ok().and_then(|n| items.get(n))
                {
                    this.take_action(position.clone(), item.clone(), window, cx);
                }
                cx.notify();
            },
        );
        let at = self.below(self.cursor());
        if let Some(code) = &mut self.code {
            code.ext_mut().actions_menu = Some(ActionsMenu {
                menu,
                position: at,
                _events: events,
            });
        }
        cx.notify();
    }

    fn take_action(
        &mut self,
        position: Position,
        item: CodeActionItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self.actions_provider() else {
            return;
        };
        let text = position.text.clone();
        let change = provider.edits(position, &item, cx);
        let task = cx.spawn_in(window, async move |this, cx| {
            let change = change.await;
            let _ = this.update_in(cx, |this, window, cx| match change {
                // The text moved on while the server worked: its edits
                // would land in the wrong places.
                Ok(_) if this.content != text => {
                    this.refactor_failed("The file changed; try the action again".into(), cx)
                }
                Ok(change) => this.apply_change(change, window, cx),
                Err(message) => this.refactor_failed(message, cx),
            });
        });
        if let Some(code) = &mut self.code {
            code.ext_mut().refactor_task = Some(task);
        }
    }

    pub(in crate::input) fn rename_symbol(
        &mut self,
        _: &RenameSymbol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self.rename_provider() else {
            cx.propagate();
            return;
        };
        if self.read_only {
            return;
        }
        let task = self.ask(
            self.cursor(),
            Duration::ZERO,
            window,
            cx,
            move |position, cx| {
                let found = provider.prepare(position.clone(), cx);
                cx.foreground_executor()
                    .spawn(async move { (position, found.await) })
            },
            |this, (position, found), window, cx| match found {
                Ok(Some(target)) => this.open_rename(position, target, window, cx),
                Ok(None) => this.refactor_failed("There is nothing to rename here".into(), cx),
                Err(message) => this.refactor_failed(message, cx),
            },
        );
        if let Some(code) = &mut self.code {
            code.ext_mut().refactor_task = Some(task);
        }
    }

    fn open_rename(
        &mut self,
        position: Position,
        target: RenameRange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if target.range.end > self.content.len() {
            return;
        }
        let field = cx.new(|cx| {
            let mut field = Input::field("New name", "New name", content(0.35), cx);
            field.set_font_size(13.);
            field.set_text(target.placeholder.clone(), cx);
            field.select_range(0..target.placeholder.len(), cx);
            field
        });
        let events = cx.subscribe_in(&field, window, |this, _, _: &Submitted, window, cx| {
            this.commit_rename(window, cx)
        });
        window.focus(&field.focus_handle(cx), cx);
        let at = self.below(target.range.start);
        let owner = cx.entity().downgrade();
        if let Some(code) = &mut self.code {
            code.ext_mut().rename = Some(RenameField {
                field,
                range: target.range,
                placeholder: target.placeholder,
                position,
                at,
                owner,
                _events: events,
            });
        }
        cx.notify();
    }

    /// Closes the rename field without renaming.
    pub(in crate::input) fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .code
            .as_mut()
            .and_then(|code| code.ext.as_mut())
            .and_then(|ext| ext.rename.take());
        if open.is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self
            .code
            .as_mut()
            .and_then(|code| code.ext.as_mut())
            .and_then(|ext| ext.rename.take())
        else {
            return;
        };
        window.focus(&self.focus, cx);
        cx.notify();
        let name = open.field.read(cx).content.trim().to_string();
        if name.is_empty() || name == open.placeholder {
            return;
        }
        let Some(provider) = self.rename_provider() else {
            return;
        };
        let text = open.position.text.clone();
        if self.content != text {
            return self.refactor_failed("The file changed; rename again".into(), cx);
        }
        let change = provider.rename(open.position, name, cx);
        let task = cx.spawn_in(window, async move |this, cx| {
            let change = change.await;
            let _ = this.update_in(cx, |this, window, cx| match change {
                Ok(_) if this.content != text => {
                    this.refactor_failed("The file changed; rename again".into(), cx)
                }
                Ok(change) => this.apply_change(change, window, cx),
                Err(message) => this.refactor_failed(message, cx),
            });
        });
        if let Some(code) = &mut self.code {
            code.ext_mut().refactor_task = Some(task);
        }
    }

    /// Edits from a rename or code action, in bytes of `base`, as one step
    /// of undo. Refused when the buffer is no longer `base`.
    pub(crate) fn apply_text_edits(
        &mut self,
        base: &str,
        edits: &[TextEdit],
        cx: &mut Context<Self>,
    ) -> bool {
        let (Some(first), Some(last)) = (edits.first(), edits.last()) else {
            return true;
        };
        if self.content.as_ref() != base || self.read_only {
            return false;
        }
        let hull = first.range.start..last.range.end;
        let Some(inner) = base.get(hull.clone()).and_then(|_| {
            let shifted: Vec<TextEdit> = edits
                .iter()
                .map(|edit| TextEdit {
                    range: edit.range.start - hull.start..edit.range.end - hull.start,
                    text: edit.text.clone(),
                })
                .collect();
            refactor::applied(&base[hull.clone()], &shifted)
        }) else {
            return false;
        };
        // The caret keeps its place in the text around the edits, and in a
        // name that was replaced.
        let caret = self.cursor();
        let moved = edits
            .iter()
            .filter(|edit| edit.range.end <= caret)
            .map(|edit| edit.text.len() as isize - edit.range.len() as isize)
            .sum::<isize>();
        let inside = edits
            .iter()
            .find(|edit| edit.range.start < caret && caret < edit.range.end);
        let caret = match inside {
            Some(edit) => {
                let before: isize = edits
                    .iter()
                    .take_while(|other| other.range.end <= edit.range.start)
                    .map(|other| other.text.len() as isize - other.range.len() as isize)
                    .sum();
                // As far into the new text as it was into the old.
                (edit.range.start as isize + before) as usize
                    + (caret - edit.range.start).min(edit.text.len())
            }
            None => (caret as isize + moved).max(0) as usize,
        };
        let before = self.content.clone();
        self.replace_bytes(hull, &inner, 0, cx);
        if self.content == before {
            return false;
        }
        let caret = caret.min(self.content.len());
        let mut caret = caret;
        while !self.content.is_char_boundary(caret) {
            caret -= 1;
        }
        self.select_range(caret..caret, cx);
        true
    }

    pub(in crate::input) fn refactor_popovers(&self, cx: &App) -> Vec<AnyElement> {
        let Some(ext) = self.code.as_ref().and_then(|code| code.ext.as_ref()) else {
            return Vec::new();
        };
        let menu = ext.actions_menu.as_ref().map(|open| {
            menu_popover(&open.menu, Anchor::Point(open.position), cx)
                .without_animation()
                .into_any_element()
        });
        let rename = ext.rename.as_ref().map(|open| {
            let (escape, dismiss) = (open.owner.clone(), open.owner.clone());
            let shown = self
                .content
                .get(open.range.clone())
                .unwrap_or(&open.placeholder)
                .to_string();
            popover("editor-rename", Anchor::Point(open.at))
                .without_animation()
                .width(RENAME_WIDTH)
                .padding(8.)
                .on_dismiss(move |window, cx| {
                    let _ = dismiss.update(cx, |this, cx| this.cancel_rename(window, cx));
                })
                .child(
                    div()
                        .id("editor-rename")
                        .debug_selector(|| "editor-rename".into())
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .capture_key_down(move |event, window, cx| {
                            if event.keystroke.key == "escape" {
                                let _ =
                                    escape.update(cx, |this, cx| this.cancel_rename(window, cx));
                                cx.stop_propagation();
                            }
                        })
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(content(0.5))
                                .truncate()
                                .child(format!("Rename '{shown}' to")),
                        )
                        .child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded(px(6.))
                                .bg(content(0.06))
                                .child(open.field.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(content(0.4))
                                .child("↩ rename everywhere · esc cancel"),
                        ),
                )
                .into_any_element()
        });
        menu.into_iter().chain(rename).collect()
    }

    #[cfg(test)]
    pub(crate) fn rename_open(&self) -> Option<Entity<Input>> {
        let ext = self.code.as_ref()?.ext.as_ref()?;
        ext.rename.as_ref().map(|open| open.field.clone())
    }
    #[cfg(test)]
    pub(crate) fn actions_open(&self) -> bool {
        (self.code.as_ref())
            .and_then(|code| code.ext.as_ref())
            .is_some_and(|ext| ext.actions_menu.is_some())
    }
}
