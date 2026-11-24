//! The "Groups" part of the Run configurations page: the project's groups as
//! a list with add, duplicate, remove and move, and a form for the selected
//! one with its name, the three choices that say how it starts and ends, and
//! its ordered members with their waits. Choices and list changes save at
//! once; the name and the numbers are typed and wait for Save.
use super::{RunsPage, boxed, hint, label};
use crate::{
    input::{self, Input},
    project_settings::button,
    theme::*,
};
use gpui::{
    App, Context, Entity, Focusable, SharedString, Stateful, Subscription, Window, div, prelude::*,
    px, rgb,
};
use monocode_core::code::run::{
    DEFAULT_WAIT_TIMEOUT, GroupMember, OnFailedStart, OnMemberExit, RunGroup, StartOrder, Wait,
};

/// The group being edited and the fields of its form.
pub(super) struct GroupForm {
    pub(super) selected: Option<String>,
    pub(super) name: Entity<Input>,
    members: Vec<MemberFields>,
}

/// The typed fields of one member: what its wait is about and how long a
/// port is waited for.
struct MemberFields {
    value: Entity<Input>,
    timeout: Entity<Input>,
    _events: [Subscription; 2],
}

/// The kind of a member's wait, as the choice shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitKind {
    None,
    Seconds,
    Port,
}

impl WaitKind {
    fn of(wait: Wait) -> Self {
        match wait {
            Wait::None => Self::None,
            Wait::Seconds(_) => Self::Seconds,
            Wait::Port(_) => Self::Port,
        }
    }

    /// What a member waits for once the kind is chosen, until a number is
    /// typed.
    fn starting_wait(self) -> Wait {
        match self {
            Self::None => Wait::None,
            Self::Seconds => Wait::Seconds(5),
            Self::Port => Wait::Port(8000),
        }
    }
}

/// The fields of the form, for a test to type into.
#[cfg(any(test, feature = "selftest"))]
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemberField {
    Value,
    Timeout,
}

impl GroupForm {
    pub(super) fn new(cx: &mut Context<RunsPage>) -> (Self, Subscription) {
        let name = cx.new(|cx| Input::field("Name", "Dev environment", content(0.35), cx));
        let typed = cx.subscribe(&name, |this, _, _: &input::Changed, cx| {
            this.group_typed(cx)
        });
        let form = Self {
            selected: None,
            name,
            members: Vec::new(),
        };
        (form, typed)
    }
}

impl MemberFields {
    fn new(member: &GroupMember, cx: &mut Context<RunsPage>) -> Self {
        let number = |placeholder: &str, text: String, cx: &mut Context<RunsPage>| {
            let field =
                cx.new(|cx| Input::field("Value", placeholder.to_owned(), content(0.35), cx));
            field.update(cx, |field, cx| field.set_text(text, cx));
            let typed = cx.subscribe(&field, |this, _, _: &input::Changed, cx| {
                this.group_typed(cx)
            });
            (field, typed)
        };
        let value = match member.wait {
            Wait::None => String::new(),
            Wait::Seconds(seconds) => seconds.to_string(),
            Wait::Port(port) => port.to_string(),
        };
        let (value, typed_value) = number("5", value, cx);
        let (timeout, typed_timeout) = number("60", member.timeout.to_string(), cx);
        Self {
            value,
            timeout,
            _events: [typed_value, typed_timeout],
        }
    }
}

impl RunsPage {
    pub(crate) fn selected_group(&self) -> Option<&RunGroup> {
        self.runs.group(self.group.selected.as_deref()?)
    }

    /// Shows a group in the form.
    pub(crate) fn select_group(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.group.selected = id;
        self.fill_group(cx);
    }

    /// The form follows the selected group as the list has it.
    pub(super) fn fill_group(&mut self, cx: &mut Context<Self>) {
        let group = self.selected_group().cloned().unwrap_or_default();
        self.filling = true;
        self.group
            .name
            .update(cx, |field, cx| field.set_text(group.name.clone(), cx));
        self.group.members = (group.members.iter())
            .map(|member| MemberFields::new(member, cx))
            .collect();
        self.filling = false;
        cx.notify();
    }

    /// Something was typed in the group's form: the list follows it. A number
    /// that is not one yet leaves what the group had.
    pub(super) fn group_typed(&mut self, cx: &mut Context<Self>) {
        if self.filling {
            return;
        }
        let Some(id) = self.group.selected.clone() else {
            return;
        };
        let name = self.group.name.read(cx).content.to_string();
        let texts: Vec<(String, String)> = (self.group.members.iter())
            .map(|fields| {
                (
                    fields.value.read(cx).content.to_string(),
                    fields.timeout.read(cx).content.to_string(),
                )
            })
            .collect();
        let Some(group) = self.runs.group_mut(&id) else {
            return;
        };
        let before = group.clone();
        group.name = name;
        for (member, (value, timeout)) in group.members.iter_mut().zip(texts) {
            match member.wait {
                Wait::Seconds(_) => {
                    if let Ok(seconds) = value.trim().parse() {
                        member.wait = Wait::Seconds(seconds);
                    }
                }
                Wait::Port(_) => {
                    if let Ok(port) = value.trim().parse::<u16>()
                        && port != 0
                    {
                        member.wait = Wait::Port(port);
                    }
                }
                Wait::None => {}
            }
            if let Ok(timeout) = timeout.trim().parse::<u32>()
                && timeout > 0
            {
                member.timeout = timeout;
            }
        }
        if *group != before {
            self.unsaved = true;
        }
        cx.notify();
    }

    /// Types `text` into a field of a member's form, as the keyboard would.
    #[cfg(any(test, feature = "selftest"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn set_member_field(
        &mut self,
        member: usize,
        field: MemberField,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(fields) = self.group.members.get(member) else {
            return;
        };
        let input = match field {
            MemberField::Value => fields.value.clone(),
            MemberField::Timeout => fields.timeout.clone(),
        };
        input.update(cx, |input, cx| input.set_text(text.to_owned(), cx));
        self.group_typed(cx);
    }

    /// Types the group's name, as the keyboard would.
    #[cfg(any(test, feature = "selftest"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn set_group_name(&mut self, text: &str, cx: &mut Context<Self>) {
        let input = self.group.name.clone();
        input.update(cx, |input, cx| input.set_text(text.to_owned(), cx));
        self.group_typed(cx);
    }

    pub(crate) fn add_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let taken = |name: &str| self.runs.groups.iter().any(|group| group.name == name);
        let mut name = "New group".to_owned();
        let mut n = 2;
        while taken(&name) {
            name = format!("New group {n}");
            n += 1;
        }
        let id = self.runs.add_group(RunGroup::named(&name));
        self.select_group(Some(id), cx);
        window.focus(&self.group.name.focus_handle(cx), cx);
        self.save(window, cx);
    }

    pub(crate) fn duplicate_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut copy) = self.selected_group().cloned() else {
            return;
        };
        let at = (self.runs.groups.iter()).position(|group| group.id == copy.id);
        copy.name = format!("{} copy", copy.name);
        let id = self.runs.add_group(copy);
        if let Some(at) = at {
            self.runs.move_group(&id, at + 1);
        }
        self.select_group(Some(id), cx);
        self.save(window, cx);
    }

    pub(crate) fn remove_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.group.selected.clone() else {
            return;
        };
        let at = (self.runs.groups.iter()).position(|group| group.id == id);
        self.runs.remove_group(&id);
        let next = at
            .and_then(|at| {
                let groups = &self.runs.groups;
                groups.get(at).or_else(|| groups.last())
            })
            .map(|group| group.id.clone());
        self.select_group(next, cx);
        self.save(window, cx);
    }

    /// Moves the selected group one place up or down.
    pub(crate) fn shift_group(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.group.selected.clone() else {
            return;
        };
        let Some(at) = (self.runs.groups.iter()).position(|group| group.id == id) else {
            return;
        };
        let to = match down {
            true => at + 1,
            false => at.saturating_sub(1),
        };
        if to != at && to < self.runs.groups.len() {
            self.runs.move_group(&id, to);
            self.save(window, cx);
        }
    }

    /// Changes the selected group and saves.
    fn edit_group(
        &mut self,
        change: impl FnOnce(&mut RunGroup),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.group.selected.clone() else {
            return;
        };
        if let Some(group) = self.runs.group_mut(&id) {
            let before = group.clone();
            change(group);
            if *group == before {
                return;
            }
        }
        self.fill_group(cx);
        self.save(window, cx);
    }

    pub(crate) fn set_order(
        &mut self,
        order: StartOrder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_group(|group| group.order = order, window, cx);
    }

    pub(crate) fn set_on_failed_start(
        &mut self,
        choice: OnFailedStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_group(|group| group.on_failed_start = choice, window, cx);
    }

    pub(crate) fn set_on_member_exit(
        &mut self,
        choice: OnMemberExit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_group(|group| group.on_member_exit = choice, window, cx);
    }

    /// Adds the project's configuration `config` to the end of the group.
    pub(crate) fn add_member(&mut self, config: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.runs.get(config).map(|config| config.label()) else {
            return;
        };
        self.edit_group(|group| _ = group.add_member(config, &name), window, cx);
    }

    pub(crate) fn remove_member(
        &mut self,
        member: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_group(
            |group| {
                if member < group.members.len() {
                    group.members.remove(member);
                }
            },
            window,
            cx,
        );
    }

    pub(crate) fn shift_member(
        &mut self,
        member: usize,
        down: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let to = match down {
            true => member + 1,
            false => member.saturating_sub(1),
        };
        self.edit_group(
            |group| {
                if to < group.members.len() {
                    group.move_member(member, to);
                }
            },
            window,
            cx,
        );
    }

    pub(crate) fn set_wait_kind(
        &mut self,
        member: usize,
        kind: WaitKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_group(
            |group| {
                if let Some(member) = group.members.get_mut(member)
                    && WaitKind::of(member.wait) != kind
                {
                    member.wait = kind.starting_wait();
                    member.timeout = DEFAULT_WAIT_TIMEOUT;
                }
            },
            window,
            cx,
        );
    }

    /// Whether a typed number of the form is not one the group could take.
    fn group_numbers_invalid(&self, cx: &App) -> bool {
        let Some(group) = self.selected_group() else {
            return false;
        };
        let bad = |text: &Entity<Input>, port: bool| {
            let text = text.read(cx).content.to_string();
            let text = text.trim();
            match port {
                true => text.parse::<u16>().is_err_and(|_| true) || text == "0",
                false => text.is_empty() || text.parse::<u32>().is_err(),
            }
        };
        (group.members.iter().zip(&self.group.members)).any(|(member, fields)| {
            let value_bad = match member.wait {
                Wait::None => false,
                Wait::Seconds(_) => bad(&fields.value, false),
                Wait::Port(_) => bad(&fields.value, true),
            };
            let timeout_bad = matches!(member.wait, Wait::Port(_))
                && (bad(&fields.timeout, false) || fields.timeout.read(cx).content.trim() == "0");
            value_bad || timeout_bad
        })
    }
}

fn choice(id: impl Into<gpui::ElementId>, text: &'static str, on: bool) -> Stateful<gpui::Div> {
    button(id, text).when(on, |b| b.bg(content(0.1)).text_color(content(1.)))
}

fn choices(
    title: &'static str,
    note: &'static str,
    buttons: Vec<Stateful<gpui::Div>>,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(label(title))
        .child(div().flex().flex_wrap().gap_2().children(buttons))
        .child(hint(note))
}

impl RunsPage {
    pub(super) fn groups_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(label("Groups"))
                    .child(hint(
                        "A group starts several of the configurations above together, each in its own tab.",
                    )),
            )
            .child(self.group_list(cx))
            .child(
                div().flex().child(
                    button("run-group-add", "Add group")
                        .on_click(cx.listener(|this, _, window, cx| this.add_group(window, cx))),
                ),
            )
            .when(self.selected_group().is_some(), |d| d.child(self.group_form(cx)))
    }

    fn group_list(&self, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let count = self.runs.groups.len();
        let rows = self.runs.groups.iter().enumerate().map(|(ix, group)| {
            let selected = self.group.selected.as_deref() == Some(group.id.as_str());
            let id = group.id.clone();
            let name = match group.name.trim() {
                "" => "Unnamed".to_owned(),
                name => name.to_owned(),
            };
            let members = group.members.len();
            div()
                .id(("run-group", ix))
                .role(gpui::Role::ListItem)
                .aria_label(name.clone())
                .aria_selected(selected)
                .h(px(30.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(2.))
                .rounded_md()
                .cursor_pointer()
                .text_size(px(12.))
                .map(|d| match selected {
                    true => d.bg(content(0.1)).text_color(content(1.)),
                    false => d.text_color(content(0.7)).hover(|s| s.bg(content(0.05))),
                })
                .child(div().flex_1().min_w_0().truncate().child(name))
                .child(hint(match members {
                    1 => "1 member".to_owned(),
                    members => format!("{members} members"),
                }))
                .when(selected, |d| {
                    d.child(
                        icon_button_enabled("run-group-up", "Move up", "chevron-up", ix > 0)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.shift_group(false, window, cx)
                            })),
                    )
                    .child(
                        icon_button_enabled(
                            "run-group-down",
                            "Move down",
                            "chevron-down",
                            ix + 1 < count,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.shift_group(true, window, cx)),
                        ),
                    )
                    .child(
                        icon_button("run-group-copy", "Duplicate", "response-copy").on_click(
                            cx.listener(|this, _, window, cx| this.duplicate_group(window, cx)),
                        ),
                    )
                    .child(
                        icon_button("run-group-remove", "Remove", "trash").on_click(
                            cx.listener(|this, _, window, cx| this.remove_group(window, cx)),
                        ),
                    )
                })
                .on_click(
                    cx.listener(move |this, _, _, cx| this.select_group(Some(id.clone()), cx)),
                )
        });
        div()
            .id("run-groups")
            .debug_selector(|| "run-groups".into())
            .role(gpui::Role::List)
            .aria_label("Run groups")
            .flex()
            .flex_col()
            .gap(px(2.))
            .p(px(4.))
            .rounded_md()
            .border_1()
            .border_color(content(0.1))
            .children(rows)
            .when(count == 0, |d| {
                d.child(div().px(px(8.)).py(px(6.)).child(hint("No groups yet.")))
            })
    }

    fn group_form(&self, cx: &mut Context<Self>) -> gpui::Div {
        let Some(group) = self.selected_group() else {
            return div();
        };
        let (order, on_failed, on_exit) =
            (group.order, group.on_failed_start, group.on_member_exit);
        let order_choice = |id: &'static str, text: &'static str, value: StartOrder| {
            choice(id, text, order == value)
                .on_click(cx.listener(move |this, _, window, cx| this.set_order(value, window, cx)))
        };
        let failed_choice = |id: &'static str, text: &'static str, value: OnFailedStart| {
            choice(id, text, on_failed == value).on_click(
                cx.listener(move |this, _, window, cx| this.set_on_failed_start(value, window, cx)),
            )
        };
        let exit_choice = |id: &'static str, text: &'static str, value: OnMemberExit| {
            choice(id, text, on_exit == value).on_click(
                cx.listener(move |this, _, window, cx| this.set_on_member_exit(value, window, cx)),
            )
        };
        let problem = group.problem();
        let invalid = self.group_numbers_invalid(cx);
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(label("Name"))
                    .child(boxed(&self.group.name, false)),
            )
            .child(choices(
                "Start order",
                "One after another waits for each member's wait before the next one starts. All at once starts every member together and ignores the waits.",
                vec![
                    order_choice("run-group-sequential", "One after another", StartOrder::Sequential),
                    order_choice("run-group-parallel", "All at once", StartOrder::Parallel),
                ],
            ))
            .child(choices(
                "When a member fails to start",
                "A member fails to start when it cannot run, ends before its wait is over, or its wait times out.",
                vec![
                    failed_choice("run-group-failed-stop", "Stop the group", OnFailedStart::StopGroup),
                    failed_choice(
                        "run-group-failed-halt",
                        "Do not start the rest",
                        OnFailedStart::DoNotStartRest,
                    ),
                    failed_choice("run-group-failed-continue", "Skip it and go on", OnFailedStart::Continue),
                ],
            ))
            .child(choices(
                "When a member ends later",
                "A member you stop yourself never stops the group.",
                vec![
                    exit_choice("run-group-exit-stop", "Stop the group", OnMemberExit::StopGroup),
                    exit_choice(
                        "run-group-exit-leave",
                        "Leave the others running",
                        OnMemberExit::LeaveOthers,
                    ),
                ],
            ))
            .child(self.member_list(cx))
            .children(problem.or(invalid.then(|| {
                "Seconds, ports and timeouts need whole numbers, and a port is 1 to 65535.".to_owned()
            })).map(|sentence| hint(sentence).text_color(rgb(0xfbbf24))))
    }

    fn member_list(&self, cx: &mut Context<Self>) -> gpui::Div {
        let Some(group) = self.selected_group() else {
            return div();
        };
        let count = group.members.len();
        let sequential = group.order == StartOrder::Sequential;
        let rows = group.members.iter().enumerate().map(|(ix, member)| {
            let missing = self.runs.get(&member.config).is_none();
            let name = match missing {
                true => format!("{} (missing)", member.label),
                false => member.label.clone(),
            };
            let kind = WaitKind::of(member.wait);
            let wait_choice =
                |id: &'static str, text: &'static str, value: WaitKind| {
                    choice((id, ix), text, kind == value).on_click(cx.listener(
                        move |this, _, window, cx| this.set_wait_kind(ix, value, window, cx),
                    ))
                };
            let fields = self.group.members.get(ix);
            let small = |field: &Entity<Input>| div().w(px(90.)).child(boxed(field, false));
            div()
                .id(("run-group-member", ix))
                .flex()
                .flex_col()
                .gap(px(6.))
                .p(px(6.))
                .rounded_md()
                .border_1()
                .border_color(content(0.1))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .text_size(px(12.))
                        .text_color(match missing {
                            true => rgb(0xfbbf24).into(),
                            false => content(0.9),
                        })
                        .child(div().flex_1().min_w_0().truncate().child(name))
                        .child(
                            icon_button_enabled(
                                "run-group-member-up",
                                "Move up",
                                "chevron-up",
                                ix > 0,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| this.shift_member(ix, false, window, cx),
                            )),
                        )
                        .child(
                            icon_button_enabled(
                                "run-group-member-down",
                                "Move down",
                                "chevron-down",
                                ix + 1 < count,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| this.shift_member(ix, true, window, cx),
                            )),
                        )
                        .child(
                            icon_button(
                                "run-group-member-remove",
                                "Remove from the group",
                                "trash",
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| this.remove_member(ix, window, cx),
                            )),
                        ),
                )
                .when(sequential, |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(hint("Then wait"))
                            .child(wait_choice(
                                "run-group-wait-none",
                                "No wait",
                                WaitKind::None,
                            ))
                            .child(wait_choice(
                                "run-group-wait-seconds",
                                "Seconds",
                                WaitKind::Seconds,
                            ))
                            .child(wait_choice("run-group-wait-port", "Port", WaitKind::Port))
                            .when_some(fields, |d, fields| {
                                d.when(kind != WaitKind::None, |d| d.child(small(&fields.value)))
                                    .when(kind == WaitKind::Port, |d| {
                                        d.child(hint("timeout (seconds)"))
                                            .child(small(&fields.timeout))
                                    })
                            }),
                    )
                })
        });
        let free: Vec<(String, String)> = (self.runs.configs.iter())
            .filter(|config| {
                group
                    .members
                    .iter()
                    .all(|member| member.config != config.id)
            })
            .map(|config| (config.id.clone(), config.label()))
            .collect();
        let none_left = match self.runs.configs.is_empty() {
            true => "Add a run configuration above first.",
            false => "Every run configuration is in the group.",
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(label("Members"))
            .child(hint(
                "Started in this order. Each is an ordinary run configuration with its own tab.",
            ))
            .children(rows)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(hint("Add"))
                    .children(free.iter().enumerate().map(|(ix, (id, name))| {
                        let id = id.clone();
                        button(
                            ("run-group-add-member", ix),
                            SharedString::from(name.clone()),
                        )
                        .on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.add_member(&id, window, cx)
                            }),
                        )
                    }))
                    .when(free.is_empty(), |d| d.child(hint(none_left))),
            )
    }
}
