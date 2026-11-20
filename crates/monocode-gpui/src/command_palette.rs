//! The command palette (⌘P): every action the app offers, by name, with
//! its shortcut. Searching anything else lives in Search Everywhere (⇧⇧).
use crate::{
    everywhere::{ActionItem, hint, keycap, rank_actions, remember_action},
    input::{self, Input},
    theme::*,
};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    HighlightStyle, MouseButton, MouseMoveEvent, Pixels, Point, Render, Role, ScrollHandle,
    SharedString, StyledText, Subscription, Window, div, prelude::*, px,
};
use std::ops::Range;

pub(crate) enum Event {
    Run(crate::code_shell::commands::Action),
    /// Hand the typed text to Search Everywhere.
    SearchEverywhere(String),
    Dismiss,
}

#[derive(Clone)]
struct Row {
    item: ActionItem,
    ranges: Vec<Range<usize>>,
    recent: bool,
}

pub(crate) struct CommandPalette {
    pub(crate) search: Entity<Input>,
    actions: Vec<ActionItem>,
    rows: Vec<Row>,
    /// List child of each row, counting group headings.
    children: Vec<usize>,
    active: usize,
    scroll: ScrollHandle,
    pointer: Option<Point<Pixels>>,
    /// Nothing typed: rows sit under their group's heading.
    grouped: bool,
    _changed: Subscription,
}
impl EventEmitter<Event> for CommandPalette {}
impl Focusable for CommandPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

impl CommandPalette {
    pub(crate) fn new(actions: Vec<ActionItem>, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| Input::command_field("Command Palette", "Run a command…", cx));
        let changed = cx.subscribe(&search, |this, _, _: &input::Changed, cx| {
            this.active = 0;
            this.rank(cx);
            cx.notify();
        });
        let mut this = Self {
            search,
            actions,
            rows: vec![],
            children: vec![],
            active: 0,
            scroll: ScrollHandle::new(),
            pointer: None,
            grouped: true,
            _changed: changed,
        };
        this.rank(cx);
        this
    }

    fn query(&self, cx: &App) -> String {
        let text = self.search.read(cx).content.trim();
        // `>` is how the old palette was reached; it still just means commands.
        text.strip_prefix('>').unwrap_or(text).trim().to_owned()
    }

    fn rank(&mut self, cx: &App) {
        let query = self.query(cx);
        let history = cx
            .try_global::<crate::everywhere::ActionHistory>()
            .map(|h| h.0.clone())
            .unwrap_or_default();
        self.rows = rank_actions(&self.actions, &query, &history)
            .into_iter()
            .map(|(item, ranges, recent)| Row {
                item,
                ranges,
                recent,
            })
            .collect();
        if query.is_empty() {
            // Recently used first, then every action under its group.
            let (recent, mut rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.rows)
                .into_iter()
                .partition(|r| r.recent);
            let groups: Vec<SharedString> = {
                let mut seen = vec![];
                for row in &rest {
                    if !seen.contains(&row.item.group) {
                        seen.push(row.item.group.clone());
                    }
                }
                seen
            };
            rest.sort_by_key(|r| groups.iter().position(|g| *g == r.item.group));
            self.rows = recent.into_iter().chain(rest).collect();
        }
        let grouped = query.is_empty();
        self.grouped = grouped;
        let mut children = Vec::with_capacity(self.rows.len());
        let mut child = 0;
        let mut last: Option<SharedString> = None;
        for row in &self.rows {
            if grouped {
                let heading = heading_of(row);
                if last.as_ref() != Some(&heading) {
                    child += 1;
                    last = Some(heading);
                }
            }
            children.push(child);
            child += 1;
        }
        self.children = children;
        self.active = self.active.min(self.rows.len().saturating_sub(1));
        if let Some(child) = self.children.get(self.active) {
            self.scroll.scroll_to_item(*child);
        }
        self.pointer = None;
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() || self.search.read(cx).is_composing() {
            return;
        }
        self.active = (self.active as isize + delta).rem_euclid(self.rows.len() as isize) as usize;
        if let Some(child) = self.children.get(self.active) {
            self.scroll.scroll_to_item(*child);
        }
        self.pointer = None;
        cx.notify();
    }

    fn pick(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.search.read(cx).is_composing() {
            return;
        }
        if let Some(row) = self.rows.get(ix) {
            remember_action(row.item.id.clone(), cx);
            cx.emit(Event::Run(row.item.run.clone()));
        } else {
            let query = self.query(cx);
            if !query.is_empty() {
                cx.emit(Event::SearchEverywhere(query));
            }
        }
    }

    #[cfg(any(test, feature = "selftest"))]
    pub(crate) fn row_labels(&self) -> Vec<(String, String)> {
        (self.rows.iter())
            .map(|row| {
                (
                    row.item.title.to_string(),
                    row.item.note_or_group().to_string(),
                )
            })
            .collect()
    }
    #[cfg(test)]
    pub(crate) fn pick_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.pick(ix, cx)
    }

    fn row(&self, ix: usize, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active == ix;
        let selection = if is_light() { 0.07 } else { 0.09 };
        let title = StyledText::new(row.item.title.clone()).with_highlights(
            row.ranges
                .iter()
                .filter(|r| r.end <= row.item.title.len())
                .cloned()
                .map(|range| {
                    (
                        range,
                        HighlightStyle {
                            font_weight: Some(FontWeight::SEMIBOLD),
                            color: Some(accent_color()),
                            ..Default::default()
                        },
                    )
                }),
        );
        div()
            .id(("palette-row", ix))
            .debug_selector(move || format!("palette-row-{ix}"))
            .role(Role::ListItem)
            .aria_selected(active)
            .aria_label(row.item.title.clone())
            .h(px(36.))
            .w_full()
            .flex_shrink_0()
            .relative()
            .rounded(px(8.))
            .pl(px(8.))
            .pr(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .when(active, |d| d.bg(content(selection)))
            .when(active, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(0.))
                        .top(px(9.))
                        .w(px(2.5))
                        .h(px(18.))
                        .rounded_full()
                        .bg(accent_color()),
                )
            })
            .child(
                div()
                    .size(px(24.))
                    .flex_shrink_0()
                    .rounded(px(7.))
                    .bg(content(if active { 0.08 } else { 0.045 }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(row.item.glyph, 13.).text_color(content(if active {
                        0.8
                    } else {
                        0.55
                    }))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.5))
                            .text_color(content(if active { 1. } else { 0.86 }))
                            .child(title),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.5))
                            .text_color(content(0.36))
                            .when(!self.grouped || row.item.note.is_some(), |d| {
                                d.child(row.item.note_or_group())
                            }),
                    ),
            )
            .children(row.item.keys.clone().map(keycap))
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if this.pointer.is_some_and(|old| old != event.position) && this.active != ix {
                    this.active = ix;
                    cx.notify();
                }
                this.pointer = Some(event.position);
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| this.pick(ix, cx)))
            .into_any_element()
    }
}

fn heading_of(row: &Row) -> SharedString {
    if row.recent {
        "Recently used".into()
    } else {
        row.item.group.clone()
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.query(cx);
        let grouped = query.is_empty();
        let rows = std::mem::take(&mut self.rows);
        let mut list: Vec<AnyElement> = vec![];
        let mut last: Option<SharedString> = None;
        for (ix, row) in rows.iter().enumerate() {
            if grouped {
                let heading = heading_of(row);
                if last.as_ref() != Some(&heading) {
                    list.push(
                        div()
                            .flex_shrink_0()
                            .px(px(10.))
                            .pt(px(if ix == 0 { 6. } else { 12. }))
                            .pb(px(5.))
                            .text_size(px(11.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(content(0.42))
                            .child(heading.clone())
                            .into_any_element(),
                    );
                    last = Some(heading);
                }
            }
            list.push(self.row(ix, row, cx));
        }
        self.rows = rows;
        let empty = self.rows.is_empty();
        div()
            .id("command-palette")
            .debug_selector(|| "command-palette".into())
            .role(Role::Dialog)
            .aria_label("Command Palette")
            .w_full()
            .flex()
            .flex_col()
            .text_color(content(1.))
            .capture_action(cx.listener(|this, _: &input::Up, _, cx| {
                this.step(-1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &input::Down, _, cx| {
                this.step(1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &input::Submit, _, cx| {
                this.pick(this.active, cx);
                cx.stop_propagation();
            }))
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                let k = &event.keystroke.key;
                if k == "pagedown" || k == "pageup" {
                    this.step(if k == "pagedown" { 8 } else { -8 }, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|_, _: &crate::shell::DismissMenu, _, cx| {
                cx.emit(Event::Dismiss);
                cx.stop_propagation();
            }))
            // Tab would leave the field for the window behind.
            .on_action(cx.listener(|_, _: &crate::shell::FocusNext, _, cx| cx.stop_propagation()))
            .on_action(
                cx.listener(|_, _: &crate::shell::FocusPrevious, _, cx| cx.stop_propagation()),
            )
            .child(
                div()
                    .h(px(54.))
                    .flex_shrink_0()
                    .px(px(18.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .border_b_1()
                    .border_color(content(0.07))
                    .child(icon("keyboard", 17.).text_color(content(0.45)))
                    .child(div().flex_1().min_w_0().child(self.search.clone()))
                    .child(keycap("esc")),
            )
            .child(
                div()
                    .id("command-palette-results")
                    .role(Role::ListBox)
                    .aria_label("Commands")
                    .max_h(px(420.))
                    .px(px(8.))
                    .py(px(6.))
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(list)
                    .when(empty, |d| {
                        d.child(
                            div()
                                .id("command-palette-search-everywhere")
                                .py(px(28.))
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(8.))
                                .text_size(px(13.))
                                .text_color(content(0.5))
                                .child(format!("No command matches “{query}”"))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .text_size(px(12.))
                                        .text_color(content(0.4))
                                        .child(keycap("↵"))
                                        .child("Search Everywhere instead"),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .h(px(34.))
                    .flex_shrink_0()
                    .px(px(14.))
                    .border_t_1()
                    .border_color(content(0.07))
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .text_size(px(11.))
                    .text_color(content(0.45))
                    .child(hint(&["↑", "↓"], "Navigate"))
                    .child(hint(&["↵"], "Run"))
                    .child(div().flex_1())
                    .child(hint(&["⇧", "⇧"], "Search Everywhere")),
            )
    }
}
