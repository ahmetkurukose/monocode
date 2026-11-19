//! The three columns of the debug tab: the call stack over the project's
//! breakpoints, the variables of the selected frame as a tree, and the
//! console with its input. The long lists are drawn by row range from the
//! rows the model keeps, so a deep stack or a large list costs what is seen.
use super::{
    breakpoints,
    model::{self, ConsoleLine, StackRow, Tone, VarKind, VarRow},
    tab::{DebugTab, Editing},
};
use crate::{
    input::{self, Input},
    right_panel::icon_action,
    theme::*,
};
use gpui::{
    AnyElement, ClipboardItem, Context, Div, ElementId, Focusable, FontWeight, Hsla, KeyDownEvent,
    Role, SharedString, Stateful, Window, div, prelude::*, px, relative, rgb, uniform_list,
};
use monocode_core::code::dap::{Breakpoint, ExceptionFilters, SessionState};
use std::path::{Path, PathBuf};

const ROW: f32 = 22.;
const INDENT: f32 = 14.;

fn heading(title: &'static str) -> gpui::Div {
    div()
        .flex_shrink_0()
        .h(px(24.))
        .px_2()
        .flex()
        .items_center()
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(content(0.45))
        .child(title)
}

fn hint(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .px_2()
        .py_1()
        .text_color(content(0.4))
        .child(text.into())
}

fn row(id: impl Into<ElementId>, selected: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(ROW))
        .w_full()
        .px_2()
        .flex()
        .items_center()
        .gap(px(6.))
        .whitespace_nowrap()
        .cursor_pointer()
        .map(|row| match selected {
            true => row.bg(content(0.1)),
            false => row.hover(|style| style.bg(content(0.05))),
        })
}

fn checkbox(id: impl Into<ElementId>, label: &'static str, on: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::CheckBox)
        .aria_label(label)
        .size(px(14.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .border_1()
        .cursor_pointer()
        .map(|mark| match on {
            true => mark
                .border_color(accent_color())
                .bg(accent_color())
                .child(icon("check", 10.).text_color(white(1.))),
            false => mark.border_color(content(0.3)),
        })
}

/// Half of a divider line in the console.
fn rule() -> Div {
    div().flex_1().h(px(1.)).bg(content(0.15))
}

fn tone_color(tone: Tone) -> Hsla {
    match tone {
        Tone::Stdout => content(0.85),
        Tone::Stderr | Tone::Error => rgb(0xf87171).into(),
        Tone::Info | Tone::Divider => content(0.45),
        Tone::Input => accent_color(),
        Tone::Result => content(1.),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
        .into_owned()
}

impl DebugTab {
    // The stack and the breakpoints.

    pub(super) fn stack_column(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (rows, state) = {
            let model = self.model.read(cx);
            (model.stack_rows.clone(), model.selected().state())
        };
        let entity = cx.entity();
        div()
            .id("debug-stack")
            .debug_selector(|| "debug-stack".into())
            .size_full()
            .flex()
            .flex_col()
            .child(heading("Call stack"))
            .child(if rows.is_empty() {
                hint(match state {
                    SessionState::Starting => "The program is starting.",
                    SessionState::Running => "The program is running.",
                    SessionState::Stopped(_) => "The debugger gave no stack.",
                    SessionState::Terminated { .. } => "The program has ended.",
                })
                .flex_1()
                .into_any_element()
            } else {
                uniform_list("debug-stack-rows", rows.len(), move |range, _, cx| {
                    entity.update(cx, |this, cx| {
                        range
                            .filter_map(|ix| rows.get(ix).map(|row| this.stack_row(ix, row, cx)))
                            .collect::<Vec<_>>()
                    })
                })
                .track_scroll(&self.stack_scroll)
                .flex_1()
                .min_h_0()
                .into_any_element()
            })
            .child(self.breakpoint_list(cx))
            .into_any_element()
    }

    fn stack_row(&self, ix: usize, row_data: &StackRow, cx: &mut Context<Self>) -> AnyElement {
        match row_data.clone() {
            StackRow::Session {
                id,
                label,
                state,
                selected,
            } => row(("debug-session-row", ix), selected)
                .child(icon("bug", 12.))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(label),
                )
                .child(div().text_color(content(0.45)).child(state))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.model
                        .update(cx, |model, cx| model.select_session(id, cx));
                }))
                .into_any_element(),
            StackRow::Thread {
                session,
                id,
                name,
                selected,
            } => row(("debug-thread-row", ix), selected)
                .pl(px(8. + INDENT))
                .text_color(content(0.7))
                .child(div().min_w_0().truncate().child(name))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.model
                        .update(cx, |model, cx| model.select_thread(session, id, cx));
                }))
                .into_any_element(),
            StackRow::Frame {
                session,
                thread,
                frame,
                selected,
                dim,
            } => {
                let place = (frame.path.as_deref())
                    .map(|path| format!("{}:{}", file_name(path), frame.line));
                let (id, target) = (frame.id, frame.path.clone().map(|path| (path, frame.line)));
                row(("debug-frame-row", ix), selected)
                    .pl(px(8. + INDENT))
                    .when(dim, |row| row.text_color(content(0.45)))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family("monospace")
                            .child(SharedString::from(frame.name)),
                    )
                    .child(div().flex_1())
                    .children(place.map(|place| {
                        div()
                            .flex_shrink_0()
                            .text_color(content(0.45))
                            .child(SharedString::from(place))
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.model
                            .update(cx, |model, cx| model.select_frame(session, thread, id, cx));
                        if let Some((path, line)) = &target {
                            model::open_at(path, *line, window, cx);
                        }
                    }))
                    .into_any_element()
            }
        }
    }

    /// The breakpoints of the tab's project, and the two exception filters.
    fn breakpoint_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let checkout = self.model.read(cx).checkout.clone();
        let store = breakpoints::store();
        let listed: Vec<(PathBuf, Breakpoint)> = (store.files().into_iter())
            .filter(|path| {
                path.starts_with(&checkout.folder) || path.starts_with(&checkout.project)
            })
            .flat_map(|path| {
                let breakpoints = store.in_file(&path);
                breakpoints
                    .into_iter()
                    .map(move |breakpoint| (path.clone(), breakpoint))
            })
            .collect();
        let filters = store.exceptions();
        let filter =
            |id: &'static str, label: &'static str, on: bool, flip: fn(&mut ExceptionFilters)| {
                row(id, false)
                    .debug_selector(move || id.into())
                    .child(checkbox((id, 1usize), label, on))
                    .child(div().text_color(content(0.75)).child(label))
                    .on_click(move |_, _, _| {
                        let mut filters = breakpoints::store().exceptions();
                        flip(&mut filters);
                        breakpoints::store().set_exceptions(filters);
                    })
            };
        div()
            .id("debug-breakpoints")
            .debug_selector(|| "debug-breakpoints".into())
            .flex_shrink_0()
            .max_h(relative(0.5))
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(content(0.08))
            .overflow_y_scroll()
            .child(heading("Breakpoints"))
            .children(
                listed
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (path, breakpoint))| {
                        let line = breakpoint.line;
                        let detail = (breakpoint.condition.clone())
                            .or_else(|| breakpoint.log_message.clone())
                            .or_else(|| breakpoint.hit_condition.clone());
                        let (toggled, removed, opened) = (path.clone(), path.clone(), path.clone());
                        row(("debug-breakpoint-row", ix), false)
                            .group("debug-breakpoint-row")
                            .child(
                                checkbox(
                                    ("debug-breakpoint-on", ix),
                                    "Enabled",
                                    breakpoint.enabled,
                                )
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    breakpoints::update(&toggled, line, |breakpoint| {
                                        breakpoint.enabled = !breakpoint.enabled
                                    });
                                }),
                            )
                            .child(
                                div().flex_shrink_0().child(SharedString::from(format!(
                                    "{}:{line}",
                                    file_name(&path)
                                ))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(content(0.45))
                                    .children(detail.map(SharedString::from)),
                            )
                            .child(
                                icon_action(
                                    ("debug-breakpoint-remove", ix),
                                    "Remove",
                                    "close",
                                    true,
                                )
                                .invisible()
                                .group_hover("debug-breakpoint-row", |style| style.visible())
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    breakpoints::store().remove(&removed, line);
                                }),
                            )
                            .on_click(move |_, window, cx| {
                                model::open_at(&opened, line, window, cx)
                            })
                    }),
            )
            .child(filter(
                "debug-filter-raised",
                "Raised exceptions",
                filters.raised,
                |filters| filters.raised = !filters.raised,
            ))
            .child(filter(
                "debug-filter-uncaught",
                "Uncaught exceptions",
                filters.uncaught,
                |filters| filters.uncaught = !filters.uncaught,
            ))
            .into_any_element()
    }

    // Variables.

    pub(super) fn variable_column(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (rows, paused) = {
            let model = self.model.read(cx);
            (
                model.var_rows.clone(),
                model.selected().state().stop().is_some(),
            )
        };
        let entity = cx.entity();
        div()
            .id("debug-variables")
            .debug_selector(|| "debug-variables".into())
            .size_full()
            .flex()
            .flex_col()
            .child(heading("Variables"))
            .child(if rows.is_empty() {
                hint(if paused {
                    "Select a frame to see its variables."
                } else {
                    "Variables show while the program is paused."
                })
                .into_any_element()
            } else {
                uniform_list("debug-variable-rows", rows.len(), move |range, _, cx| {
                    entity.update(cx, |this, cx| {
                        range
                            .filter_map(|ix| rows.get(ix).map(|row| this.variable_row(ix, row, cx)))
                            .collect::<Vec<_>>()
                    })
                })
                .track_scroll(&self.variable_scroll)
                .flex_1()
                .min_h_0()
                .into_any_element()
            })
            .into_any_element()
    }

    fn variable_row(&self, ix: usize, data: &VarRow, cx: &mut Context<Self>) -> AnyElement {
        let path = data.path.clone();
        let indent = px(8. + data.depth as f32 * INDENT);
        let arrow = |open: bool| {
            icon(
                if open {
                    "chevron-down"
                } else {
                    "chevron-right"
                },
                12.,
            )
        };
        match data.kind {
            VarKind::Loading => row(("debug-variable-row", ix), false)
                .pl(indent + px(18.))
                .text_color(content(0.4))
                .child(data.name.clone())
                .into_any_element(),
            VarKind::More => row(("debug-variable-row", ix), false)
                .pl(indent + px(18.))
                .text_color(accent_color())
                .child(data.name.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.model
                        .update(cx, |model, cx| model.show_more(&path, cx));
                }))
                .into_any_element(),
            VarKind::Scope => row(("debug-variable-row", ix), false)
                .pl(indent)
                .child(arrow(data.expanded))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(content(0.7))
                        .child(data.name.clone()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.model.update(cx, |model, cx| model.toggle(&path, cx));
                }))
                .into_any_element(),
            VarKind::Variable => {
                let editing = (self.editing.as_ref())
                    .filter(|editing| editing.path == data.path)
                    .map(|editing| editing.field.clone());
                let can_edit = self.model.read(cx).can_set_variables();
                let (value, name) = (data.value.clone(), data.name.to_string());
                let (parent, edited, expandable) =
                    (data.parent, data.path.clone(), data.expandable);
                let copied = value.clone();
                row(("debug-variable-row", ix), false)
                    .group("debug-variable-row")
                    .pl(indent)
                    .font_family("monospace")
                    .child(match expandable {
                        true => arrow(data.expanded).into_any_element(),
                        false => div().w(px(12.)).flex_shrink_0().into_any_element(),
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(rgb(0x7dd3fc))
                            .child(data.name.clone()),
                    )
                    .child(match editing {
                        Some(field) => div()
                            .flex_1()
                            .min_w_0()
                            .h(px(18.))
                            .px_1()
                            .rounded(px(3.))
                            .bg(content(0.1))
                            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                if event.keystroke.key == "escape" {
                                    this.editing = None;
                                    cx.stop_propagation();
                                    cx.notify();
                                }
                            }))
                            .child(field)
                            .into_any_element(),
                        None => div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(content(0.85))
                            .child(value)
                            .into_any_element(),
                    })
                    .children(data.type_name.clone().map(|type_name| {
                        div()
                            .flex_shrink_0()
                            .max_w(px(120.))
                            .truncate()
                            .text_color(content(0.4))
                            .child(type_name)
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .invisible()
                            .group_hover("debug-variable-row", |style| style.visible())
                            .child(
                                icon_action(
                                    ("debug-variable-copy", ix),
                                    "Copy value",
                                    "copy",
                                    true,
                                )
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        copied.to_string(),
                                    ));
                                }),
                            )
                            .when(can_edit, |buttons| {
                                buttons.child(
                                    icon_action(
                                        ("debug-variable-edit", ix),
                                        "Edit value",
                                        "pencil",
                                        true,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.edit(
                                                edited.clone(),
                                                parent,
                                                name.clone(),
                                                window,
                                                cx,
                                            );
                                        },
                                    )),
                                )
                            }),
                    )
                    .when(expandable, |row| {
                        row.on_click(cx.listener(move |this, _, _, cx| {
                            this.model.update(cx, |model, cx| model.toggle(&path, cx));
                        }))
                    })
                    .into_any_element()
            }
        }
    }

    /// Puts a field in the variable's row with its value, to change it.
    pub(super) fn edit(
        &mut self,
        path: model::VarPath,
        parent: i64,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.model.read(cx).var_rows.clone();
        let Some(current) = rows.iter().find(|row| row.path == path) else {
            return;
        };
        let value = current.value.to_string();
        let field = cx.new(|cx| {
            let mut field = Input::field("New value", "", content(0.35), cx);
            field.set_text(value, cx);
            field
        });
        window.focus(&field.focus_handle(cx), cx);
        let events = cx.subscribe(&field, |this, field, _: &input::Submitted, cx| {
            let value = field.read(cx).content.to_string();
            if let Some(editing) = this.editing.take() {
                this.model.update(cx, |model, cx| {
                    model.set_variable(editing.parent, editing.name, value, cx)
                });
            }
            cx.notify();
        });
        self.editing = Some(Editing {
            path,
            parent,
            name,
            field,
            _events: events,
        });
        cx.notify();
    }

    // The console.

    pub(super) fn console_column(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (count, can_type, live) = {
            let model = self.model.read(cx);
            (
                model.console.len(),
                model.user_drives() && model.live(),
                model.live(),
            )
        };
        let entity = cx.entity();
        div()
            .id("debug-console")
            .debug_selector(|| "debug-console".into())
            .size_full()
            .flex()
            .flex_col()
            .child(heading("Console"))
            .child(
                uniform_list("debug-console-lines", count, move |range, _, cx| {
                    let lines: Vec<ConsoleLine> = {
                        let console = &entity.read(cx).model.read(cx).console;
                        range.filter_map(|ix| console.get(ix).cloned()).collect()
                    };
                    lines
                        .into_iter()
                        .map(|line| {
                            div()
                                .h(px(18.))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .whitespace_nowrap()
                                .font_family("monospace")
                                .text_color(tone_color(line.tone))
                                .when(line.tone == Tone::Input, |row| row.child("›"))
                                .when(line.tone == Tone::Divider, |row| row.w_full().child(rule()))
                                .child(line.text)
                                .when(line.tone == Tone::Divider, |row| row.child(rule()))
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.console_scroll)
                .flex_1()
                .min_h_0(),
            )
            .child(
                div()
                    .id("debug-console-input")
                    .debug_selector(|| "debug-console-input".into())
                    .flex_shrink_0()
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .border_t_1()
                    .border_color(content(0.08))
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        let older = match event.keystroke.key.as_str() {
                            "up" => true,
                            "down" => false,
                            _ => return,
                        };
                        this.recall(older, cx);
                        cx.stop_propagation();
                    }))
                    .child(div().text_color(accent_color()).child("›"))
                    .child(match can_type {
                        true => div()
                            .flex_1()
                            .min_w_0()
                            .child(self.input.clone())
                            .into_any_element(),
                        false => div()
                            .flex_1()
                            .text_color(content(0.35))
                            .child(if live {
                                "Take over the session to evaluate here."
                            } else {
                                "The session has ended."
                            })
                            .into_any_element(),
                    }),
            )
            .into_any_element()
    }
}
