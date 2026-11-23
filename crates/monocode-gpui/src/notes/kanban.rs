//! A kanban note: Blitzit's four columns of task cards. Cards move by drag
//! and drop or their arrows, carry an estimate and the time taken, and open
//! in a dialog for their notes. The board lives in the note's body as JSON.
use super::monday::DraggedWorkItem;
use crate::{
    input::{self, Input},
    popover::{Anchor, AnchorSlot, popover},
    theme::*,
};
use gpui::{
    AnyElement, App, ClickEvent, Context, CursorStyle, DragMoveEvent, Entity, EventEmitter,
    Focusable, FontWeight, Hsla, MouseButton, Pixels, Point, Role, SharedString, Subscription,
    Window, div, prelude::*, px, relative, rgb,
};
use monocode_core::{
    integrations::WorkItem,
    kanban::{self, Board, Column, Periods, Task, TaskSource},
};
use std::collections::HashMap;

/// The board changed and wants saving.
pub(crate) struct Changed;

/// The gradient Blitzit marks Today and finished work with.
fn teal() -> Hsla {
    rgb(0x3cc9c4).into()
}
fn olive() -> Hsla {
    rgb(0xb3c065).into()
}
fn warn() -> Hsla {
    rgb(0xf59e0b).into()
}
fn danger() -> Hsla {
    rgb(0xf87171).into()
}
fn gradient() -> gpui::Background {
    gpui::linear_gradient(
        90.,
        gpui::linear_color_stop(teal(), 0.),
        gpui::linear_color_stop(olive(), 1.),
    )
}

/// What follows the pointer while a card is dragged.
#[derive(Clone)]
pub(crate) struct DraggedTask {
    id: String,
    title: String,
    width: Pixels,
}

impl gpui::Render for DraggedTask {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(self.width)
            .px_3()
            .py(px(10.))
            .rounded(px(10.))
            .border_1()
            .border_color(content(0.16))
            .bg(pane_fill())
            .shadow_lg()
            .text_size(px(13.))
            .text_color(content(1.))
            .child(self.title.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeField {
    Estimate,
    Taken,
}

/// Where a dragged card would land: a column, and its place among the
/// column's other cards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    column: Column,
    index: usize,
}

pub(crate) struct KanbanBoard {
    board: Board,
    adding: Option<Column>,
    add_title: Entity<Input>,
    add_estimate: Entity<Input>,
    focus_add: bool,
    time_edit: Option<(String, TimeField)>,
    time_input: Entity<Input>,
    focus_time: bool,
    dialog: Option<String>,
    dialog_title: Entity<Input>,
    dialog_estimate: Entity<Input>,
    dialog_taken: Entity<Input>,
    dialog_notes: Entity<Input>,
    menu: Option<(String, Point<Pixels>)>,
    confirm_delete: bool,
    hovered: Option<String>,
    dragging: Option<String>,
    target: Option<Target>,
    /// The column a dragged monday card is over.
    item_over: Option<Column>,
    cards: HashMap<String, AnchorSlot>,
    blur_bound: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Changed> for KanbanBoard {}

/// Keeps a time field to `##:##` as it is typed.
fn masked(input: &Entity<Input>, cx: &mut App) {
    let text = input.read(cx).content.to_string();
    let mask = kanban::mask_hhmm(&text);
    if mask != text {
        input.update(cx, |input, cx| input.set_text(mask, cx));
    }
}

fn time_field(label: &str, cx: &mut App) -> Entity<Input> {
    let label = label.to_owned();
    cx.new(|cx| {
        let mut input = Input::field(label, "00:00", content(0.35), cx);
        input.set_font_size(12.);
        input.set_centered(true);
        input.set_max_chars(5);
        input
    })
}

impl KanbanBoard {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let add_title = cx.new(|cx| {
            let mut input = Input::field("Task title", "Enter task title*", content(0.35), cx);
            input.set_max_chars(kanban::MAX_TASK_TITLE);
            input
        });
        let add_estimate = time_field("Estimated time", cx);
        let time_input = time_field("Time", cx);
        let dialog_title = cx.new(|cx| {
            let mut input = Input::field("Task title", "Task title", content(0.35), cx);
            input.set_font_size(18.);
            input.set_max_chars(kanban::MAX_TASK_TITLE);
            input
        });
        let dialog_estimate = time_field("Estimated time", cx);
        let dialog_taken = time_field("Time taken", cx);
        let dialog_notes = cx.new(|cx| {
            let mut input = Input::multiline_field("Task notes", "Write your notes here…", cx);
            input.set_font_size(13.);
            input.set_min_height(160.);
            input.set_max_height(320.);
            input.set_max_chars(kanban::MAX_TASK_NOTES);
            input
        });
        let subscriptions = vec![
            cx.subscribe(&add_title, |this, _, _: &input::Submitted, cx| {
                this.submit_add(cx)
            }),
            cx.subscribe(&add_estimate, |this, _, _: &input::Submitted, cx| {
                this.submit_add(cx)
            }),
            cx.subscribe(&add_estimate, |_, input, _: &input::Changed, cx| {
                masked(&input, cx)
            }),
            cx.subscribe(&time_input, |_, input, _: &input::Changed, cx| {
                masked(&input, cx)
            }),
            cx.subscribe(&time_input, |this, _, _: &input::Submitted, cx| {
                this.commit_time(cx)
            }),
            cx.subscribe(&dialog_estimate, |_, input, _: &input::Changed, cx| {
                masked(&input, cx)
            }),
            cx.subscribe(&dialog_taken, |_, input, _: &input::Changed, cx| {
                masked(&input, cx)
            }),
            cx.subscribe(&dialog_title, |this, _, _: &input::Submitted, cx| {
                this.save_dialog(cx)
            }),
            cx.subscribe(&dialog_notes, |this, _, _: &input::Submitted, cx| {
                this.save_dialog(cx)
            }),
        ];
        Self {
            board: Board::default(),
            adding: None,
            add_title,
            add_estimate,
            focus_add: false,
            time_edit: None,
            time_input,
            focus_time: false,
            dialog: None,
            dialog_title,
            dialog_estimate,
            dialog_taken,
            dialog_notes,
            menu: None,
            confirm_delete: false,
            hovered: None,
            dragging: None,
            target: None,
            item_over: None,
            cards: HashMap::new(),
            blur_bound: false,
            _subscriptions: subscriptions,
        }
    }

    /// Shows the board a note holds, closing whatever was open on another.
    pub(crate) fn load(&mut self, body: &str, cx: &mut Context<Self>) {
        self.board = Board::parse(body);
        self.adding = None;
        self.time_edit = None;
        self.dialog = None;
        self.menu = None;
        self.dragging = None;
        self.target = None;
        self.cards.clear();
        cx.notify();
    }

    pub(crate) fn board(&self) -> &Board {
        &self.board
    }

    /// Puts a tracker item on the board, once: false when it already was.
    pub(crate) fn add_item(
        &mut self,
        item: &WorkItem,
        column: Column,
        cx: &mut Context<Self>,
    ) -> bool {
        let before = self.board.tasks.len();
        self.board.add_from_source(
            &item.title,
            TaskSource::from_item(item),
            column,
            Self::now(),
        );
        let added = self.board.tasks.len() > before;
        if added {
            self.changed(cx);
        }
        added
    }

    /// Closes the innermost open thing; false when nothing was open, so
    /// Escape can go on to close the view.
    pub(crate) fn dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        let open = if self.menu.is_some() {
            self.menu = None;
            true
        } else if self.dialog.is_some() {
            self.dialog = None;
            true
        } else if self.time_edit.is_some() {
            self.time_edit = None;
            true
        } else if self.adding.is_some() {
            self.adding = None;
            true
        } else {
            false
        };
        cx.notify();
        open
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(Changed);
        cx.notify();
    }

    fn now() -> i64 {
        monocode_core::time::now_ms() as i64
    }

    fn open_add(&mut self, column: Column, cx: &mut Context<Self>) {
        if self.adding == Some(column) {
            self.adding = None;
        } else {
            self.adding = Some(column);
            self.add_title
                .update(cx, |input, cx| input.set_text(String::new(), cx));
            self.add_estimate
                .update(cx, |input, cx| input.set_text(String::new(), cx));
            self.focus_add = true;
        }
        cx.notify();
    }

    fn submit_add(&mut self, cx: &mut Context<Self>) {
        let Some(column) = self.adding else { return };
        let title = self.add_title.read(cx).content.to_string();
        let estimate = self.add_estimate.read(cx).content.to_string();
        if self
            .board
            .quick_add(&title, &estimate, column, Self::now())
            .is_none()
        {
            self.focus_add = true;
            cx.notify();
            return;
        }
        // The form stays open for the next task, as Blitzit's does.
        self.add_title
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        self.add_estimate
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        self.focus_add = true;
        self.changed(cx);
    }

    fn start_time_edit(&mut self, id: String, field: TimeField, cx: &mut Context<Self>) {
        self.commit_time(cx);
        let Some(task) = self.board.task(&id) else {
            return;
        };
        let minutes = match field {
            TimeField::Estimate => task.estimate_min,
            TimeField::Taken => task.taken_min,
        };
        self.time_input
            .update(cx, |input, cx| input.set_text(kanban::hhmm(minutes), cx));
        self.time_edit = Some((id, field));
        self.focus_time = true;
        cx.notify();
    }

    fn commit_time(&mut self, cx: &mut Context<Self>) {
        let Some((id, field)) = self.time_edit.take() else {
            return;
        };
        let minutes = kanban::parse_hhmm(&self.time_input.read(cx).content);
        let (estimate, taken) = match field {
            TimeField::Estimate => (Some(minutes), None),
            TimeField::Taken => (None, Some(minutes)),
        };
        self.board.update(&id, None, None, estimate, taken);
        self.changed(cx);
    }

    fn open_dialog(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.board.task(&id).cloned() else {
            return;
        };
        self.menu = None;
        self.commit_time(cx);
        self.dialog_title
            .update(cx, |input, cx| input.set_text(task.title.clone(), cx));
        self.dialog_estimate.update(cx, |input, cx| {
            input.set_text(kanban::hhmm(task.estimate_min), cx)
        });
        self.dialog_taken.update(cx, |input, cx| {
            input.set_text(kanban::hhmm(task.taken_min), cx)
        });
        self.dialog_notes
            .update(cx, |input, cx| input.set_text(task.notes.clone(), cx));
        self.dialog = Some(id);
        window.focus(&self.dialog_title.focus_handle(cx), cx);
        cx.notify();
    }

    fn save_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.dialog.take() else { return };
        let title = self.dialog_title.read(cx).content.to_string();
        let notes = self.dialog_notes.read(cx).content.to_string();
        let estimate = kanban::parse_hhmm(&self.dialog_estimate.read(cx).content);
        let taken = kanban::parse_hhmm(&self.dialog_taken.read(cx).content);
        self.board
            .update(&id, Some(&title), Some(&notes), Some(estimate), Some(taken));
        self.changed(cx);
    }

    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        self.menu = None;
        self.dialog = None;
        self.confirm_delete = false;
        self.board.remove(id);
        self.changed(cx);
    }

    /// Where the pointer at `y` would drop a card in `column`: past every
    /// card whose middle it is below.
    fn retarget(&mut self, column: Column, y: Pixels, cx: &mut Context<Self>) {
        let Some(dragged) = self.dragging.clone() else {
            return;
        };
        let index = self
            .board
            .column(column)
            .filter(|task| task.id != dragged)
            .filter_map(|task| self.cards.get(&task.id).and_then(AnchorSlot::get))
            .filter(|bounds| y > bounds.center().y)
            .count();
        let target = Some(Target { column, index });
        if self.target != target {
            self.target = target;
            cx.notify();
        }
    }

    fn drop_task(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(target) = self.target.take() {
            self.board
                .move_task(id, target.column, target.index, Self::now());
            self.dragging = None;
            self.changed(cx);
        }
        self.dragging = None;
        cx.notify();
    }

    fn checkbox(&self, task: &Task, cx: &mut Context<Self>) -> AnyElement {
        let id = task.id.clone();
        let done = task.done();
        div()
            .id("kanban-check")
            .debug_selector({
                let id = id.clone();
                move || format!("kanban-check-{id}")
            })
            .role(Role::CheckBox)
            .aria_label("Mark as done")
            .aria_toggled(if done {
                gpui::Toggled::True
            } else {
                gpui::Toggled::False
            })
            .size(px(16.))
            .mt(px(2.))
            .flex_shrink_0()
            .rounded(px(5.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .map(|d| {
                if done {
                    d.bg(gradient())
                        .child(icon("check", 12.).text_color(rgb(0x111111)))
                } else {
                    d.border_1()
                        .border_color(content(0.45))
                        .hover(|s| s.border_color(content(0.9)))
                }
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.board.toggle_done(&id, Self::now());
                this.changed(cx);
            }))
            .into_any_element()
    }

    fn time_button(&self, task: &Task, field: TimeField, cx: &mut Context<Self>) -> AnyElement {
        let editing = self
            .time_edit
            .as_ref()
            .is_some_and(|(id, f)| id == &task.id && *f == field);
        let (minutes, label, name) = match field {
            TimeField::Estimate => (task.estimate_min, "Est.", "kanban-estimate"),
            TimeField::Taken => (task.taken_min, "Taken", "kanban-taken"),
        };
        if editing {
            return div()
                .w(px(56.))
                .h(px(24.))
                .px_1()
                .flex()
                .items_center()
                .rounded_md()
                .border_1()
                .border_color(content(0.2))
                .bg(content(0.05))
                .child(self.time_input.clone())
                .into_any_element();
        }
        let over = field == TimeField::Taken && task.over_estimate();
        let id = task.id.clone();
        div()
            .id(name)
            .debug_selector({
                let id = id.clone();
                move || format!("{name}-{id}")
            })
            .role(Role::Button)
            .aria_label(format!("{label} {}", kanban::hhmm(minutes)))
            .w(px(56.))
            .h(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if over { warn() } else { content(0.55) })
            .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
            .cursor_pointer()
            .child(kanban::hhmm(minutes))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.start_time_edit(id.clone(), field, cx)
            }))
            .into_any_element()
    }

    fn card(
        &mut self,
        task: &Task,
        index: usize,
        numbered: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = task.id.clone();
        let done = task.done();
        let editing_time = self.time_edit.as_ref().is_some_and(|(t, _)| t == &id);
        let hovered = self.hovered.as_ref() == Some(&id) || editing_time;
        let slot = self.cards.entry(id.clone()).or_default().clone();
        let column = task.column.index();
        let dragged = DraggedTask {
            id: id.clone(),
            title: task.title.clone(),
            width: slot.get().map_or(px(260.), |b| b.size.width),
        };
        let hover_id = id.clone();
        let open_id = id.clone();
        let action = |name: &'static str, label: &'static str, glyph: &'static str| {
            icon_button(name, label, glyph)
                .size(px(22.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        let actions =
            div()
                .absolute()
                .top(px(8.))
                .right(px(8.))
                .flex()
                .items_center()
                .gap(px(2.))
                .rounded_md()
                .bg(pane_fill())
                .pl_1()
                .child(
                    action("kanban-notes", "Notes", "note").on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_dialog(id.clone(), window, cx)
                        }
                    })),
                )
                .when(column > 0, |d| {
                    let id = id.clone();
                    d.child(
                        action("kanban-left", "Move to the previous column", "arrow-left-2")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.board.shift(&id, -1, Self::now());
                                this.changed(cx);
                            })),
                    )
                })
                .when(column < Column::ALL.len() - 1, |d| {
                    let id = id.clone();
                    d.child(
                        action("kanban-right", "Move to the next column", "arrow-right-2")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.board.shift(&id, 1, Self::now());
                                this.changed(cx);
                            })),
                    )
                })
                .child(
                    action("kanban-more", "More", "more-horizontal").on_click(cx.listener({
                        let id = id.clone();
                        move |this, event: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.confirm_delete = false;
                            this.menu = Some((id.clone(), event.position()));
                            cx.notify();
                        }
                    })),
                );
        div()
            .id(SharedString::from(format!("kanban-card-{id}")))
            .debug_selector({
                let id = id.clone();
                move || format!("kanban-card-{id}")
            })
            .role(Role::ListItem)
            .aria_label(task.title.clone())
            .relative()
            .flex_shrink_0()
            .px_3()
            .py(px(10.))
            .rounded(px(10.))
            .border_1()
            .border_color(content(0.08))
            .bg(content(if hovered { 0.08 } else { 0.05 }))
            .cursor(CursorStyle::OpenHand)
            .child(slot.probe())
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.hovered = Some(hover_id.clone());
                } else if this.hovered.as_ref() == Some(&hover_id) {
                    this.hovered = None;
                }
                cx.notify();
            }))
            .on_drag(dragged, |task, _, _, cx| cx.new(|_| task.clone()))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.click_count() >= 2 {
                    this.open_dialog(open_id.clone(), window, cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .map(|d| {
                        if numbered && !done && !hovered {
                            d.child(
                                div()
                                    .w(px(16.))
                                    .flex_shrink_0()
                                    .text_size(px(12.))
                                    .line_height(px(20.))
                                    .text_color(content(0.4))
                                    .child((index + 1).to_string()),
                            )
                        } else if done || hovered {
                            d.child(self.checkbox(task, cx))
                        } else {
                            d
                        }
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .text_color(content(if done { 0.5 } else { 1. }))
                            .when(done, |d| d.line_through())
                            .child(task.title.clone()),
                    )
                    .when(done && task.taken_min > 0 && !hovered, |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(12.))
                                .line_height(px(20.))
                                .text_color(content(0.45))
                                .child(kanban::hhmm(task.taken_min)),
                        )
                    }),
            )
            .when_some(task.source.clone(), |d, source| {
                d.child(source_badge(&id, source))
            })
            .when(hovered && !done, |d| {
                d.child(
                    div()
                        .mt_1()
                        .flex()
                        .justify_between()
                        .child(self.time_button(task, TimeField::Estimate, cx))
                        .child(self.time_button(task, TimeField::Taken, cx)),
                )
            })
            .when_some(
                task.notes_preview().filter(|_| !done).map(str::to_owned),
                |d, preview| {
                    d.child(
                        div()
                            .mt(px(6.))
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_size(px(12.))
                            .text_color(content(if hovered { 0.8 } else { 0.45 }))
                            .child(icon("note", 12.))
                            .child(div().flex_1().min_w_0().truncate().child(preview)),
                    )
                },
            )
            .when(hovered && !editing_time, |d| d.child(actions))
            .into_any_element()
    }

    fn add_form(&self, column: Column, cx: &mut Context<Self>) -> AnyElement {
        if self.adding != Some(column) {
            return div()
                .id("kanban-add-task")
                .debug_selector(move || format!("kanban-add-{}", column.id()))
                .role(Role::Button)
                .aria_label(format!("Add task to {}", column.label()))
                .flex_shrink_0()
                .h(px(32.))
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(content(0.5))
                .hover(|s| s.text_color(content(0.9)))
                .cursor_pointer()
                .child(icon("plus", 14.))
                .child("ADD TASK")
                .on_click(cx.listener(move |this, _, _, cx| this.open_add(column, cx)))
                .into_any_element();
        }
        let field = |input: Entity<Input>| {
            div()
                .h(px(32.))
                .px_2()
                .flex()
                .items_center()
                .rounded_md()
                .border_1()
                .border_color(content(0.12))
                .bg(content(0.05))
                .child(input)
        };
        let label = |text: &'static str| {
            div()
                .text_size(px(11.))
                .text_color(content(0.5))
                .mb_1()
                .child(text)
        };
        let mut ink = shell_fill();
        ink.a = 1.;
        div()
            .debug_selector(move || format!("kanban-add-form-{}", column.id()))
            .flex_shrink_0()
            .pt_2()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(label("Title"))
                            .child(field(self.add_title.clone())),
                    )
                    .child(
                        div()
                            .w(px(68.))
                            .flex_shrink_0()
                            .child(label("Est time"))
                            .child(field(self.add_estimate.clone())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(px(11.))
                    .text_color(content(0.45))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child("Try “Report 1h30m @today”"),
                    )
                    .child(
                        div()
                            .id("kanban-add-cancel")
                            .debug_selector(|| "kanban-add-cancel".into())
                            .flex_shrink_0()
                            .role(Role::Button)
                            .aria_label("Cancel")
                            .h(px(26.))
                            .px_3()
                            .rounded_full()
                            .flex()
                            .items_center()
                            .text_size(px(12.))
                            .text_color(content(0.6))
                            .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                            .cursor_pointer()
                            .child("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.adding = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("kanban-add-confirm")
                            .flex_shrink_0()
                            .debug_selector(|| "kanban-add-confirm".into())
                            .role(Role::Button)
                            .aria_label("Confirm")
                            .h(px(26.))
                            .px_3()
                            .rounded_full()
                            .flex()
                            .items_center()
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .bg(content(1.))
                            .text_color(ink)
                            .hover(move |s| s.bg(content(0.85)).text_color(ink))
                            .cursor_pointer()
                            .child("Confirm")
                            .on_click(cx.listener(|this, _, _, cx| this.submit_add(cx))),
                    ),
            )
            .into_any_element()
    }

    fn column(&mut self, column: Column, periods: Periods, cx: &mut Context<Self>) -> AnyElement {
        let tasks: Vec<Task> = self
            .board
            .column(column)
            .filter(|task| self.dragging.as_ref() != Some(&task.id))
            .cloned()
            .collect();
        let today = column == Column::Today;
        let mut items: Vec<AnyElement> = tasks
            .iter()
            .enumerate()
            .map(|(index, task)| self.card(task, index, today, cx))
            .collect();
        if let Some(target) = self.target.filter(|t| t.column == column)
            && self.dragging.is_some()
        {
            let height = (self.dragging.as_ref())
                .and_then(|id| self.cards.get(id))
                .and_then(AnchorSlot::get)
                .map_or(px(44.), |bounds| bounds.size.height);
            items.insert(
                target.index.min(items.len()),
                div()
                    .debug_selector(|| "kanban-placeholder".into())
                    .flex_shrink_0()
                    .h(height)
                    .rounded(px(10.))
                    .border_1()
                    .border_dashed()
                    .border_color(content(0.2))
                    .bg(content(0.03))
                    .into_any_element(),
            );
        }
        let empty = items.is_empty();
        let over = (self.dragging.is_some() && self.target.is_some_and(|t| t.column == column))
            || self.item_over == Some(column);
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .min_h(px(28.))
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(FontWeight::BOLD)
                    .child(column.label()),
            )
            .child(div().flex_1())
            .map(|d| {
                if column == Column::Done {
                    d.child(
                        div()
                            .text_size(px(12.))
                            .text_color(content(0.5))
                            .child(format!(
                                "{} this month 🎉",
                                match self.board.done_since(periods.month) {
                                    1 => "1 task".to_owned(),
                                    n => format!("{n} tasks"),
                                }
                            )),
                    )
                } else {
                    let remaining = self.board.remaining(column);
                    let adding = self.adding == Some(column);
                    d.when(remaining > 0, |d| {
                        d.child(
                            div()
                                .text_size(px(12.))
                                .text_color(content(0.5))
                                .child(kanban::est_label(remaining, true)),
                        )
                    })
                    .child(
                        icon_button(
                            "kanban-column-add",
                            if adding { "Close" } else { "Add task" },
                            if adding { "close" } else { "plus" },
                        )
                        .debug_selector(move || format!("kanban-column-add-{}", column.id()))
                        .on_click(cx.listener(move |this, _, _, cx| this.open_add(column, cx))),
                    )
                }
            });
        let progress = self.board.progress(column, periods).map(|progress| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(content(0.5))
                .child(
                    div()
                        .flex_1()
                        .h(px(6.))
                        .rounded_full()
                        .bg(content(0.1))
                        .overflow_hidden()
                        .child(
                            div()
                                .h_full()
                                .w(relative(progress.fraction()))
                                .rounded_full()
                                .map(|d| {
                                    if today {
                                        d.bg(gradient())
                                    } else {
                                        d.bg(content(0.5))
                                    }
                                }),
                        ),
                )
                .child(format!("{}/{} Done", progress.done, progress.total))
        });
        div()
            .id(("kanban-column", column.index()))
            .debug_selector(move || format!("kanban-column-{}", column.id()))
            .role(Role::List)
            .aria_label(column.label())
            .relative()
            .flex_1()
            .min_w(px(250.))
            .h_full()
            .flex()
            .flex_col()
            .gap_3()
            .px_3()
            .pt_3()
            .pb_2()
            .rounded(px(16.))
            .border_1()
            .border_color(if over { content(0.25) } else { content(0.08) })
            .bg(content(if over { 0.06 } else { 0.03 }))
            .overflow_hidden()
            // Today wears Blitzit's gradient along its top edge.
            .when(today, |d| {
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(2.))
                        .bg(gradient()),
                )
            })
            .on_drag_move::<DraggedTask>(cx.listener(
                move |this, event: &DragMoveEvent<DraggedTask>, _, cx| {
                    if this.dragging.is_none() {
                        this.dragging = Some(event.drag(cx).id.clone());
                        this.hovered = None;
                        this.menu = None;
                    }
                    if event.bounds.contains(&event.event.position) {
                        this.retarget(column, event.event.position.y, cx);
                    }
                },
            ))
            .on_drop(cx.listener(|this, task: &DraggedTask, _, cx| this.drop_task(&task.id, cx)))
            // A monday card from a board beside this one; Done takes none.
            .when(column != Column::Done, |d| {
                d.on_drag_move::<DraggedWorkItem>(cx.listener(
                    move |this, event: &DragMoveEvent<DraggedWorkItem>, _, cx| {
                        let inside = event.bounds.contains(&event.event.position);
                        if inside && this.item_over != Some(column) {
                            this.item_over = Some(column);
                            cx.notify();
                        } else if !inside && this.item_over == Some(column) {
                            this.item_over = None;
                            cx.notify();
                        }
                    },
                ))
                .on_drop(cx.listener(
                    move |this, dragged: &DraggedWorkItem, _, cx| {
                        this.item_over = None;
                        this.add_item(&dragged.item, column, cx);
                        cx.notify();
                    },
                ))
            })
            .child(header)
            .children(progress)
            .child(
                div()
                    .id(("kanban-cards", column.index()))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(items)
                    .when(empty, |d| {
                        d.child(
                            div()
                                .py(px(28.))
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap_2()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(content(0.4))
                                .child(icon("check-circle", 24.).text_color(content(0.3)))
                                .child("All Clear"),
                        )
                    })
                    .when(column != Column::Done, |d| {
                        d.child(self.add_form(column, cx))
                    }),
            )
            .into_any_element()
    }

    fn task_menu(&self, id: &str, at: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.weak_entity();
        let row = |name: &'static str, label: &'static str, glyph: &'static str, color: Hsla| {
            div()
                .id(name)
                .debug_selector(move || name.into())
                .role(Role::MenuItem)
                .aria_label(label)
                .h(px(28.))
                .px_2()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(13.))
                .text_color(color)
                .hover(|s| s.bg(content(0.08)))
                .cursor_pointer()
                .child(icon(glyph, 14.).text_color(color))
                .child(label)
        };
        let (edit, copy, delete, yes) =
            (id.to_owned(), id.to_owned(), id.to_owned(), id.to_owned());
        popover("kanban-task-menu", Anchor::Point(at))
            .width(180.)
            .on_dismiss(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.menu = None;
                    cx.notify();
                });
            })
            .child(
                div()
                    .p_1()
                    .flex()
                    .flex_col()
                    .child(
                        row("kanban-menu-edit", "Edit", "pencil-edit", content(0.9)).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.open_dialog(edit.clone(), window, cx)
                            }),
                        ),
                    )
                    .child(
                        row(
                            "kanban-menu-duplicate",
                            "Duplicate",
                            "response-copy",
                            content(0.9),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            this.board.duplicate(&copy, Self::now());
                            this.changed(cx);
                        })),
                    )
                    .child(div().my_1().h(px(1.)).bg(content(0.08)))
                    .map(|d| {
                        if self.confirm_delete {
                            d.child(
                                div()
                                    .h(px(28.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_size(px(12.))
                                    .text_color(content(0.7))
                                    .child(div().flex_1().child("Sure?"))
                                    .child(
                                        icon_button("kanban-menu-delete-yes", "Delete", "trash")
                                            .debug_selector(|| "kanban-menu-delete-yes".into())
                                            .text_color(danger())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.remove(&yes, cx)
                                            })),
                                    )
                                    .child(
                                        icon_button("kanban-menu-delete-no", "Keep", "close")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.confirm_delete = false;
                                                cx.notify();
                                            })),
                                    ),
                            )
                        } else {
                            let _ = &delete;
                            d.child(
                                row("kanban-menu-delete", "Delete", "trash", danger()).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.confirm_delete = true;
                                        cx.notify();
                                    }),
                                ),
                            )
                        }
                    }),
            )
            .into_any_element()
    }

    fn task_dialog(&self, id: &str, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.weak_entity();
        let width = 520f32.min(window.viewport_size().width.as_f32() - 24.);
        let field = |input: Entity<Input>, multiline: bool| {
            div()
                .when(!multiline, |d| d.h(px(34.)).flex().items_center())
                .when(multiline, |d| d.py_2())
                .px(px(10.))
                .rounded_md()
                .border_1()
                .border_color(content(0.1))
                .bg(content(0.05))
                .text_size(px(13.))
                .child(input)
        };
        let label = |text: &'static str| {
            div()
                .mb_1()
                .text_size(px(11.))
                .text_color(content(0.5))
                .child(text)
        };
        let mut ink = shell_fill();
        ink.a = 1.;
        let remove = id.to_owned();
        let button = |name: &'static str, label: &'static str| {
            div()
                .id(name)
                .debug_selector(move || name.into())
                .role(Role::Button)
                .aria_label(label)
                .h(px(30.))
                .px_3()
                .rounded_full()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
        };
        div()
            .absolute()
            .size_full()
            .child(crate::modal::scrim("kanban-task-scrim"))
            .child(
                popover("kanban-task-dialog", Anchor::Center)
                    .modal_surface(false)
                    .width(width)
                    .gap(0.)
                    .priority(60)
                    .on_dismiss(move |_, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.dialog = None;
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .id("kanban-task-dialog-body")
                            .debug_selector(|| "kanban-task-dialog".into())
                            .role(Role::Dialog)
                            .aria_label("Edit task")
                            .p_5()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .text_color(content(1.))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(content(0.5))
                                    .child("Task"),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::BOLD)
                                    .child(self.dialog_title.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_3()
                                    .child(
                                        div()
                                            .w(px(110.))
                                            .child(label("Est time"))
                                            .child(field(self.dialog_estimate.clone(), false)),
                                    )
                                    .child(
                                        div()
                                            .w(px(110.))
                                            .child(label("Time taken"))
                                            .child(field(self.dialog_taken.clone(), false)),
                                    ),
                            )
                            .child(
                                div()
                                    .child(label("Notes"))
                                    .child(field(self.dialog_notes.clone(), true)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        button("kanban-dialog-delete", "Delete")
                                            .text_color(danger())
                                            .hover(|s| s.bg(content(0.08)))
                                            .child(icon("trash", 14.).text_color(danger()))
                                            .child("Delete")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.remove(&remove, cx)
                                            })),
                                    )
                                    .child(div().flex_1())
                                    .child(
                                        button("kanban-dialog-cancel", "Cancel")
                                            .text_color(content(0.7))
                                            .hover(|s| s.bg(content(0.08)))
                                            .child("Cancel")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.dialog = None;
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        button("kanban-dialog-save", "Save")
                                            .bg(content(1.))
                                            .text_color(ink)
                                            .hover(move |s| s.bg(content(0.85)).text_color(ink))
                                            .child("Save")
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.save_dialog(cx)),
                                            ),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// The mark of a card made from a monday item; clicking it opens the item.
fn source_badge(task: &str, source: TaskSource) -> AnyElement {
    let task = task.to_owned();
    let url = source.url.clone();
    let badge = div()
        .id("kanban-source")
        .debug_selector(move || format!("kanban-source-{task}"))
        .role(Role::Link)
        .aria_label("Open in monday.com")
        .h(px(18.))
        .px_1p5()
        .rounded_sm()
        .flex()
        .items_center()
        .gap_1()
        .bg(content(0.06))
        .text_size(px(10.))
        .text_color(content(0.6))
        .hover(|s| s.bg(content(0.12)).text_color(content(1.)))
        .cursor_pointer()
        .child(icon("inbox-monday", 10.).text_color(content(0.7)))
        .child(if source.provider == "monday" {
            "monday".to_owned()
        } else {
            source.provider.clone()
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(!url.is_empty(), |d| {
            d.on_click(move |_, _, cx| {
                cx.stop_propagation();
                cx.open_url(&url)
            })
        });
    div().mt(px(6.)).flex().child(badge).into_any_element()
}

impl gpui::Render for KanbanBoard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.blur_bound {
            self.blur_bound = true;
            self._subscriptions.push(cx.on_blur(
                &self.time_input.focus_handle(cx),
                window,
                |this, _, cx| this.commit_time(cx),
            ));
        }
        if !cx.has_active_drag() {
            self.item_over = None;
        }
        if !cx.has_active_drag() && self.dragging.is_some() {
            // A drop outside every column leaves the card where it was.
            self.dragging = None;
            self.target = None;
        }
        if std::mem::take(&mut self.focus_add) {
            window.focus(&self.add_title.focus_handle(cx), cx);
        }
        if std::mem::take(&mut self.focus_time) {
            let input = self.time_input.clone();
            window.focus(&input.focus_handle(cx), cx);
            input.update(cx, |input, cx| {
                let length = input.content.len();
                input.select_range(0..length, cx)
            });
        }
        let known: std::collections::HashSet<_> = self
            .board
            .tasks
            .iter()
            .map(|task| task.id.clone())
            .collect();
        self.cards.retain(|id, _| known.contains(id));
        let periods = Periods::now();
        let pending = self.board.pending().count();
        let remaining = self.board.pending_remaining();
        let subtitle = if pending == 0 {
            "This board has no pending tasks".to_owned()
        } else {
            format!(
                "This board has {pending} pending task{}{}",
                if pending == 1 { "" } else { "s" },
                if remaining > 0 {
                    format!(", Est: {}", kanban::est_label(remaining, true))
                } else {
                    String::new()
                }
            )
        };
        let columns: Vec<AnyElement> = Column::ALL
            .into_iter()
            .map(|column| self.column(column, periods, cx))
            .collect();
        div()
            .id("kanban-board")
            .debug_selector(|| "kanban-board".into())
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(13.))
                    .text_color(content(0.5))
                    .child(subtitle),
            )
            .child(
                div()
                    .id("kanban-columns")
                    .flex_1()
                    .min_h_0()
                    .overflow_x_scroll()
                    .child(
                        div()
                            .h_full()
                            .min_w(px(4. * 250. + 3. * 14.))
                            .flex()
                            .gap(px(14.))
                            .children(columns),
                    ),
            )
            .when_some(self.menu.clone(), |d, (id, at)| {
                d.child(self.task_menu(&id, at, cx))
            })
            .when_some(self.dialog.clone(), |d, id| {
                d.child(self.task_dialog(&id, window, cx))
            })
    }
}
