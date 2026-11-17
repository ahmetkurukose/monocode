//! The dock tab of one debug session: the step controls drawn in the dock's
//! header, and the body with its three resizable columns (stack and
//! breakpoints, variables, console), stacked when the dock is narrow. The
//! tab reads the session's model and draws; `columns.rs` holds the columns.
use super::{
    Tip,
    model::{Control, Model, VarPath},
};
use crate::{
    code_shell::dock::{self, DockPlacement, DockTab, DockTabHandle, TabClosed},
    input::{self, Input},
    popover::AnchorSlot,
    right_panel::icon_action,
    theme::*,
};
use gpui::{
    AnyElement, App, Context, CursorStyle, Entity, Focusable, MouseButton, MouseMoveEvent, Role,
    ScrollStrategy, SharedString, Subscription, Task, UniformListScrollHandle, Window, div,
    prelude::*, px, relative, rgb,
};
use monocode_core::code::dap::SessionState;
use std::rc::Rc;

/// A column is never dragged narrower than this share of the tab.
const MIN_SHARE: f32 = 0.12;

/// Adds the tab of `model`'s session to its project's dock.
pub(super) fn open(
    model: &Entity<Model>,
    reveal: bool,
    on_close: TabClosed,
    window: &mut Window,
    cx: &mut App,
) -> Result<DockTabHandle, String> {
    let view = cx.new(|cx| DebugTab::new(model.clone(), cx));
    let controls = cx.new(|cx| Controls::new(model.clone(), cx));
    let placed = view.downgrade();
    let (title, checkout) = {
        let model = model.read(cx);
        (model.title(), model.checkout.clone())
    };
    let focus = view.read(cx).input.read(cx).focus_handle(cx);
    model.update(cx, |model, _| model.console_focus = Some(focus.clone()));
    dock::add_tab(
        DockTab {
            id: format!("debug-{}", model.entity_id().as_u64()).into(),
            title: title.into(),
            icon: "bug",
            view: view.into(),
            focus: Some(focus),
            controls: Some(controls.into()),
            checkout,
            on_close: Some(on_close),
            on_placement: Some(Rc::new(move |placement, cx| {
                let _ = placed.update(cx, |view, cx| view.place(placement, cx));
            })),
            reveal,
        },
        window,
        cx,
    )
}

/// The step buttons, and who drives when it is not the user.
pub(super) struct Controls {
    model: Entity<Model>,
    _model: Subscription,
}
impl Controls {
    pub(super) fn new(model: Entity<Model>, cx: &mut Context<Self>) -> Self {
        Self {
            _model: cx.observe(&model, |_, _, cx| cx.notify()),
            model,
        }
    }
    fn button(
        &self,
        control: Control,
        id: &'static str,
        label: &'static str,
        icon: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let enabled = self.model.read(cx).can(control);
        let tip: SharedString = label.into();
        icon_action(id, label, icon, enabled)
            .debug_selector(move || id.into())
            .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
            .when(enabled, |button| {
                button.on_click(cx.listener(move |this, _, _, cx| {
                    this.model.update(cx, |model, cx| model.act(control, cx));
                }))
            })
            .into_any_element()
    }
}
impl Render for Controls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (running, driver) = {
            let model = self.model.read(cx);
            (
                model.selected().state() == SessionState::Running,
                model.driven_by(),
            )
        };
        let restart = if cfg!(target_os = "macos") {
            "Restart (Cmd+Shift+F5)"
        } else {
            "Restart (Ctrl+Shift+F5)"
        };
        div()
            .id("debug-controls")
            .debug_selector(|| "debug-controls".into())
            .flex()
            .items_center()
            .gap(px(2.))
            .children(driver.map(|agent| {
                div()
                    .id("debug-driver")
                    .debug_selector(|| "debug-driver".into())
                    .role(Role::Status)
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .mr(px(6.))
                    .text_size(px(11.))
                    .text_color(content(0.6))
                    .child(icon("bot", 12.))
                    .child(
                        div()
                            .max_w(px(200.))
                            .truncate()
                            .child(SharedString::from(format!("driven by {agent}"))),
                    )
                    .child(
                        div()
                            .id("debug-take-over")
                            .debug_selector(|| "debug-take-over".into())
                            .role(Role::Button)
                            .aria_label("Take over")
                            .h(px(20.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded_md()
                            .bg(content(0.1))
                            .text_color(content(0.9))
                            .cursor_pointer()
                            .hover(|style| style.bg(content(0.16)))
                            .child("Take over")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.update(cx, |model, cx| model.take_over(cx));
                            })),
                    )
            }))
            .child(if running {
                self.button(
                    Control::Pause,
                    "debug-pause",
                    "Pause (F5)",
                    "queue-pause",
                    cx,
                )
            } else {
                self.button(
                    Control::Continue,
                    "debug-continue",
                    "Continue (F5)",
                    "play",
                    cx,
                )
            })
            .child(self.button(
                Control::StepOver,
                "debug-step-over",
                "Step over (F10)",
                "arrow-right-2",
                cx,
            ))
            .child(self.button(
                Control::StepInto,
                "debug-step-into",
                "Step into (F11)",
                "arrow-down",
                cx,
            ))
            .child(self.button(
                Control::StepOut,
                "debug-step-out",
                "Step out (Shift+F11)",
                "arrow-up",
                cx,
            ))
            .child(self.button(Control::Restart, "debug-restart", restart, "rotate-ccw", cx))
            .child(self.button(Control::Stop, "debug-stop", "Stop (Shift+F5)", "square", cx))
    }
}

/// A variable's value being edited in place.
pub(super) struct Editing {
    pub(super) path: VarPath,
    pub(super) parent: i64,
    pub(super) name: String,
    pub(super) field: Entity<Input>,
    pub(super) _events: Subscription,
}

pub(super) struct DebugTab {
    pub(super) model: Entity<Model>,
    /// The dock is tall and narrow: the columns are stacked.
    narrow: bool,
    /// Where the two dividers are, as shares of the tab's width (or of its
    /// height when stacked).
    split: (f32, f32),
    drag: Option<usize>,
    bounds: AnchorSlot,
    pub(super) stack_scroll: UniformListScrollHandle,
    pub(super) variable_scroll: UniformListScrollHandle,
    pub(super) console_scroll: UniformListScrollHandle,
    /// How many console lines were there at the last draw, to follow new ones.
    console_seen: usize,
    pub(super) input: Entity<Input>,
    /// Which earlier expression the arrow keys have brought back.
    pub(super) recalled: Option<usize>,
    pub(super) editing: Option<Editing>,
    _breakpoints: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl DebugTab {
    pub(super) fn new(model: Entity<Model>, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            Input::field(
                "Evaluate",
                "Evaluate in the selected frame",
                content(0.35),
                cx,
            )
        });
        // The breakpoint list is the store's; it is redrawn when that changes.
        let changes = super::breakpoints::store().subscribe();
        let breakpoints = cx.spawn(async move |this, cx| {
            while changes.recv().await.is_ok() {
                while changes.try_recv().is_ok() {}
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        let subscriptions = vec![
            cx.observe(&model, |this, model, cx| {
                let lines = model.read(cx).console.len();
                if lines != this.console_seen {
                    this.console_seen = lines;
                    if lines > 0 {
                        (this.console_scroll).scroll_to_item(lines - 1, ScrollStrategy::Nearest);
                    }
                }
                cx.notify();
            }),
            cx.subscribe(&input, |this, _, _: &input::Submitted, cx| this.submit(cx)),
        ];
        Self {
            model,
            narrow: false,
            split: (0.3, 0.64),
            drag: None,
            bounds: AnchorSlot::default(),
            stack_scroll: UniformListScrollHandle::new(),
            variable_scroll: UniformListScrollHandle::new(),
            console_scroll: UniformListScrollHandle::new(),
            console_seen: 0,
            input,
            recalled: None,
            editing: None,
            _breakpoints: breakpoints,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn place(&mut self, placement: DockPlacement, cx: &mut Context<Self>) {
        if self.narrow != placement.narrow() {
            self.narrow = placement.narrow();
            cx.notify();
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let expression = self.input.read(cx).content.to_string();
        self.input
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        self.recalled = None;
        self.model
            .update(cx, |model, cx| model.evaluate(expression, cx));
    }

    /// Walks the expressions evaluated before, newest first.
    pub(super) fn recall(&mut self, older: bool, cx: &mut Context<Self>) {
        let history = self.model.read(cx).history.clone();
        let at = match (self.recalled, older) {
            (None, true) => history.len().checked_sub(1),
            (Some(at), true) => Some(at.saturating_sub(1)),
            (Some(at), false) if at + 1 < history.len() => Some(at + 1),
            _ => None,
        };
        self.recalled = at;
        let text = at
            .and_then(|at| history.get(at).cloned())
            .unwrap_or_default();
        self.input.update(cx, |input, cx| input.set_text(text, cx));
    }

    fn drag_to(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let (Some(divider), Some(bounds)) = (self.drag, self.bounds.get()) else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let share = if self.narrow {
            (event.position.y - bounds.top()) / bounds.size.height
        } else {
            (event.position.x - bounds.left()) / bounds.size.width
        };
        let (first, second) = self.split;
        self.split = if divider == 0 {
            (share.clamp(MIN_SHARE, second - MIN_SHARE), second)
        } else {
            (first, share.clamp(first + MIN_SHARE, 1. - MIN_SHARE))
        };
        cx.notify();
    }

    fn pane(&self, index: usize, column: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        let (first, second) = self.split;
        let share = [first, second - first, 1. - second][index];
        let narrow = self.narrow;
        div()
            .relative()
            .min_w_0()
            .min_h_0()
            .flex_shrink_0()
            .overflow_hidden()
            .map(|pane| match narrow {
                true => pane.w_full().h(relative(share)),
                false => pane.h_full().w(relative(share)),
            })
            .when(index > 0, |pane| match narrow {
                true => pane.border_t_1().border_color(content(0.08)),
                false => pane.border_l_1().border_color(content(0.08)),
            })
            .child(column)
            .when(index < 2, |pane| {
                pane.child(
                    div()
                        .id(("debug-divider", index))
                        .role(Role::Splitter)
                        .aria_label("Resize the columns")
                        .absolute()
                        .map(|handle| match narrow {
                            true => handle
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(px(5.))
                                .cursor(CursorStyle::ResizeUpDown),
                            false => handle
                                .top_0()
                                .bottom_0()
                                .right_0()
                                .w(px(5.))
                                .cursor(CursorStyle::ResizeLeftRight),
                        })
                        .hover(|style| style.bg(content(0.1)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.drag = Some(index);
                                cx.stop_propagation();
                            }),
                        ),
                )
            })
            .into_any_element()
    }
}

impl Render for DebugTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let notice = self.model.read(cx).notice.clone();
        let stack = self.stack_column(cx);
        let variables = self.variable_column(cx);
        let console = self.console_column(cx);
        div()
            .id("debug-tab")
            .debug_selector(|| "debug-tab".into())
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .text_size(px(12.))
            .text_color(content(0.9))
            .children(notice.map(|text| {
                div()
                    .id("debug-notice")
                    .debug_selector(|| "debug-notice".into())
                    .role(Role::Alert)
                    .flex_shrink_0()
                    .px_3()
                    .py(px(5.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(gpui::Hsla::from(rgb(0xef4444)).opacity(0.14))
                    .text_color(rgb(0xfca5a5))
                    .child(icon("circle-alert", 14.).text_color(rgb(0xf87171)))
                    .child(div().min_w_0().flex_1().child(text))
            }))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(self.narrow, |columns| columns.flex_col())
                    .child(self.bounds.probe())
                    .on_mouse_move(
                        cx.listener(|this, event: &MouseMoveEvent, _, cx| this.drag_to(event, cx)),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| this.drag = None),
                    )
                    .child(self.pane(0, stack, cx))
                    .child(self.pane(1, variables, cx))
                    .child(self.pane(2, console, cx)),
            )
    }
}
