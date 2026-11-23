//! The browsing history window (docs/native/browser/context.md K55): ⌘Y,
//! the toolbar's clock or the command palette. It lists this project's
//! visits by default and every project's with the switch, searchable, and
//! deletes one visit, the last hour, today or everything (of the shown
//! scope). Picking a row opens it in the focused page, or a new one.

use crate::{
    everywhere::{hint, keycap},
    input::{self, Input},
    theme::*,
};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Render,
    Role, ScrollHandle, SharedString, Subscription, Window, actions, div, prelude::*, px,
};
use monocode_core::browser::history::{Query, Transition, Visit};

actions!(
    browser,
    [
        /// ⌘Y: the browsing history.
        ShowBrowserHistory
    ]
);

/// Visits listed at most; searching narrows them.
const LIMIT: usize = 300;
const HOUR: i64 = 3_600_000;

pub(crate) enum Event {
    Open(String),
    Dismiss,
}

pub(crate) struct HistoryView {
    pub(crate) search: Entity<Input>,
    /// The project in front (its key) and its name; `None` when there is
    /// none, which shows every project.
    project: Option<(String, String)>,
    all: bool,
    rows: Vec<Visit>,
    active: Option<usize>,
    scroll: ScrollHandle,
    /// "Clear all" asks once more before it clears.
    confirm_clear: bool,
    /// The list child of each row, counting day headings (last render).
    children: Vec<usize>,
    _changed: Subscription,
}

impl EventEmitter<Event> for HistoryView {}
impl Focusable for HistoryView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

/// A project key's name: its folder's.
fn project_name(key: &str) -> String {
    key.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(key)
        .to_owned()
}

/// The heading of a day's visits.
fn day_label(at: i64, now: i64) -> String {
    use chrono::{Datelike, Local, TimeZone};
    let (Some(day), Some(today)) = (
        Local.timestamp_millis_opt(at).single(),
        Local.timestamp_millis_opt(now).single(),
    ) else {
        return String::new();
    };
    let days = today.date_naive().num_days_from_ce() - day.date_naive().num_days_from_ce();
    match days {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ if day.year() == today.year() => day.format("%A, %B %-d").to_string(),
        _ => day.format("%B %-d, %Y").to_string(),
    }
}

/// Midnight of today, local time, in ms.
fn today_start(now: i64) -> i64 {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_millis_opt(now)
        .single()
        .and_then(|t| t.date_naive().and_hms_opt(0, 0, 0))
        .and_then(|t| t.and_local_timezone(Local).earliest())
        .map_or(now - 24 * HOUR, |t| t.timestamp_millis())
}

impl HistoryView {
    /// `project` is the key of the project in front, if any.
    pub(crate) fn new(project: Option<String>, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| Input::command_field("Search History", "Search history…", cx));
        let changed = cx.subscribe(&search, |this, _, _: &input::Changed, cx| this.refresh(cx));
        let mut this = Self {
            search,
            project: project.map(|key| {
                let name = project_name(&key);
                (key, name)
            }),
            all: false,
            rows: Vec::new(),
            active: None,
            scroll: ScrollHandle::new(),
            confirm_clear: false,
            children: Vec::new(),
            _changed: changed,
        };
        this.refresh(cx);
        this
    }

    /// The project the list and deletions are limited to; `None` is all.
    fn scope(&self) -> Option<&str> {
        (!self.all)
            .then(|| self.project.as_ref().map(|(key, _)| key.as_str()))
            .flatten()
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let query = Query {
            text: self.search.read(cx).content.trim().to_owned(),
            project: self.scope().map(str::to_owned),
            since: None,
            until: None,
            limit: LIMIT,
        };
        let stores = super::history::stores(cx);
        self.rows = stores.borrow_mut().visits(&query, super::history::now_ms());
        if self.active.is_some_and(|a| a >= self.rows.len()) {
            self.active = self.rows.len().checked_sub(1);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn urls(&self) -> Vec<String> {
        self.rows.iter().map(|v| v.url.clone()).collect()
    }

    pub(crate) fn set_all(&mut self, all: bool, cx: &mut Context<Self>) {
        if self.all != all {
            self.all = all;
            self.active = None;
            self.confirm_clear = false;
            self.refresh(cx);
        }
    }

    fn step(&mut self, step: isize, cx: &mut Context<Self>) {
        self.active = super::new_tab::step_selection(self.active, step, self.rows.len());
        if let Some(child) = self.active.and_then(|a| self.children.get(a)) {
            self.scroll.scroll_to_item(*child);
        }
        cx.notify();
    }

    fn open(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(visit) = self.rows.get(ix) {
            cx.emit(Event::Open(visit.url.clone()));
        }
    }

    pub(crate) fn delete(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(visit) = self.rows.get(ix).cloned() else {
            return;
        };
        let stores = super::history::stores(cx);
        let deleted = stores
            .borrow_mut()
            .delete_visit(&visit, super::history::now_ms());
        if let Err(error) = deleted {
            log::warn!("browser history: {error}");
        }
        self.refresh(cx);
    }

    /// Deletes the scope's visits since `from`.
    pub(crate) fn delete_since(&mut self, from: i64, cx: &mut Context<Self>) {
        let now = super::history::now_ms();
        let scope = self.scope().map(str::to_owned);
        let stores = super::history::stores(cx);
        let deleted = stores
            .borrow_mut()
            .delete_range(scope.as_deref(), from, i64::MAX, now);
        if let Err(error) = deleted {
            log::warn!("browser history: {error}");
        }
        self.refresh(cx);
    }

    /// The first press asks; the second clears the scope's history.
    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        if !self.confirm_clear {
            self.confirm_clear = true;
            cx.notify();
            return;
        }
        self.confirm_clear = false;
        let scope = self.scope().map(str::to_owned);
        let stores = super::history::stores(cx);
        let cleared = stores
            .borrow_mut()
            .clear(scope.as_deref(), super::history::now_ms());
        if let Err(error) = cleared {
            log::warn!("browser history: {error}");
        }
        self.refresh(cx);
    }

    fn row(&self, ix: usize, visit: &Visit, cx: &mut Context<Self>) -> AnyElement {
        use chrono::{Local, TimeZone};
        let time = Local
            .timestamp_millis_opt(visit.visited_at)
            .single()
            .map(|t| t.format("%H:%M").to_string())
            .unwrap_or_default();
        let title = crate::browser::page_title(&visit.url, &visit.title);
        let elsewhere = self
            .project
            .as_ref()
            .is_none_or(|(key, _)| *key != visit.project);
        let note = match visit.transition {
            Transition::Agent => Some("agent".to_owned()),
            _ => None,
        };
        let project = (self.all && elsewhere).then(|| project_name(&visit.project));
        let active = self.active == Some(ix);
        div()
            .id(("browser-history-row", ix))
            .role(Role::ListBoxOption)
            .aria_label(SharedString::from(title.clone()))
            .group("browser-history-row")
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(10.))
            .h(px(34.))
            .flex_shrink_0()
            .rounded(px(8.))
            .cursor_pointer()
            .when(active, |d| d.bg(content(0.08)))
            .hover(|s| s.bg(content(0.06)))
            .child(
                div()
                    .w(px(38.))
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(content(0.45))
                    .child(time),
            )
            .child(icon("globe", 13.).text_color(content(0.4)))
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(px(300.))
                    .truncate()
                    .text_size(px(13.))
                    .text_color(content(0.92))
                    .child(title),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(content(0.45))
                    .child(visit.url.clone()),
            )
            .children(note.into_iter().chain(project).map(|note| {
                div()
                    .flex_shrink_0()
                    .px(px(6.))
                    .rounded(px(5.))
                    .bg(content(0.06))
                    .text_size(px(10.))
                    .text_color(content(0.55))
                    .child(note)
            }))
            .child(
                div()
                    .id(("browser-history-delete", ix))
                    .role(Role::Button)
                    .aria_label("Remove from History")
                    .flex_shrink_0()
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .opacity(if active { 1. } else { 0. })
                    .group_hover("browser-history-row", |s| s.opacity(1.))
                    .hover(|s| s.bg(content(0.1)))
                    .child(icon("x", 12.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.delete(ix, cx);
                    })),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open(ix, cx)))
            .into_any_element()
    }

    fn scope_switch(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let option = |id: &'static str, label: String, on: bool| {
            div()
                .id(id)
                .role(Role::RadioButton)
                .aria_label(SharedString::from(label.clone()))
                .px(px(10.))
                .h(px(24.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(12.))
                .when(on, |d| d.bg(content(0.1)).text_color(content(1.)))
                .when(!on, |d| {
                    d.text_color(content(0.55))
                        .hover(|s| s.text_color(content(0.9)))
                })
                .child(label)
        };
        let here = self
            .project
            .as_ref()
            .map_or("This project".to_owned(), |(_, name)| name.clone());
        div()
            .id("browser-history-scope")
            .role(Role::RadioGroup)
            .aria_label("Whose history")
            .flex()
            .flex_shrink_0()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(8.))
            .bg(content(0.05))
            .when(self.project.is_some(), |d| {
                d.child(
                    option("browser-history-project", here, !self.all)
                        .on_click(cx.listener(|this, _, _, cx| this.set_all(false, cx))),
                )
            })
            .child(
                option(
                    "browser-history-all",
                    "All projects".to_owned(),
                    self.all || self.project.is_none(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_all(true, cx))),
            )
    }
}

fn action_button(
    id: &'static str,
    label: &'static str,
    danger_ink: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .px(px(10.))
        .h(px(24.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(if danger_ink {
            gpui::hsla(0., 0.66, if is_light() { 0.5 } else { 0.47 }, 1.)
        } else {
            content(0.6)
        })
        .hover(|s| s.bg(content(0.07)))
        .child(label)
}

impl Render for HistoryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = super::history::now_ms();
        let rows = std::mem::take(&mut self.rows);
        let mut list: Vec<AnyElement> = Vec::new();
        let mut children = Vec::new();
        let mut last_day = String::new();
        for (ix, visit) in rows.iter().enumerate() {
            let day = day_label(visit.visited_at, now);
            if day != last_day {
                list.push(
                    div()
                        .flex_shrink_0()
                        .px(px(10.))
                        .pt(px(if ix == 0 { 4. } else { 12. }))
                        .pb(px(4.))
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(content(0.42))
                        .child(day.clone())
                        .into_any_element(),
                );
                last_day = day;
            }
            children.push(list.len());
            list.push(self.row(ix, visit, cx));
        }
        self.rows = rows;
        self.children = children;
        let empty = self.rows.is_empty();
        let searching = !self.search.read(cx).content.trim().is_empty();
        let clear_label = if self.confirm_clear {
            "Clear all — click again"
        } else {
            "Clear all"
        };
        div()
            .id("browser-history")
            .debug_selector(|| "browser-history".into())
            .role(Role::Dialog)
            .aria_label("History")
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
                if let Some(active) = this.active {
                    this.open(active, cx);
                }
                cx.stop_propagation();
            }))
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                let k = &event.keystroke;
                // ⌘⌫ (Ctrl⌫ elsewhere) removes the chosen visit.
                if k.key == "backspace"
                    && (k.modifiers.platform || (!cfg!(target_os = "macos") && k.modifiers.control))
                    && let Some(active) = this.active
                {
                    this.delete(active, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|_, _: &crate::shell::DismissMenu, _, cx| {
                cx.emit(Event::Dismiss);
                cx.stop_propagation();
            }))
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
                    .child(icon("clock", 17.).text_color(content(0.45)))
                    .child(div().flex_1().min_w_0().child(self.search.clone()))
                    .child(self.scope_switch(cx)),
            )
            .child(
                div()
                    .h(px(34.))
                    .flex_shrink_0()
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .border_b_1()
                    .border_color(content(0.07))
                    .child(
                        action_button("browser-history-hour", "Delete last hour", false).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.delete_since(super::history::now_ms() - HOUR, cx)
                            }),
                        ),
                    )
                    .child(
                        action_button("browser-history-today", "Delete today", false).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.delete_since(today_start(super::history::now_ms()), cx)
                            }),
                        ),
                    )
                    .child(div().flex_1())
                    .child(
                        action_button("browser-history-clear", clear_label, true)
                            .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
                    ),
            )
            .child(
                div()
                    .id("browser-history-list")
                    .role(Role::ListBox)
                    .aria_label("Visits")
                    .h(px(440.))
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
                                .py(px(40.))
                                .flex()
                                .justify_center()
                                .text_size(px(13.))
                                .text_color(content(0.5))
                                .child(if searching {
                                    "No visit matches."
                                } else {
                                    "No history yet."
                                }),
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
                    .child(hint(&["↵"], "Open"))
                    .child(hint(&["⌘", "⌫"], "Remove"))
                    .child(div().flex_1())
                    .child(keycap("esc")),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_read_as_people_say_them() {
        use chrono::{Local, TimeZone};
        let now = Local
            .with_ymd_and_hms(2026, 11, 23, 15, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(day_label(now - HOUR, now), "Today");
        assert_eq!(day_label(now - 24 * HOUR, now), "Yesterday");
        assert_eq!(day_label(now - 3 * 24 * HOUR, now), "Friday, November 20");
        assert_eq!(day_label(now - 400 * 24 * HOUR, now), "October 19, 2025");
        let start = today_start(now);
        assert_eq!(now - start, 15 * HOUR);
        assert_eq!(project_name("/Users/me/Dev/app/"), "app");
    }

    #[gpui::test]
    fn the_window_lists_scopes_searches_and_deletes(cx: &mut gpui::TestAppContext) {
        let now = super::super::history::now_ms();
        cx.update(|cx| {
            let stores = super::super::history::stores(cx);
            let mut stores = stores.borrow_mut();
            let store = stores.store("default", now).unwrap();
            for (project, url, at) in [
                ("/p", "https://old.test/", now - 5 * HOUR),
                ("/p", "https://a.test/", now - 10),
                ("/q", "https://b.test/", now),
            ] {
                store
                    .record(project, url, "", Transition::Link, at)
                    .unwrap();
            }
        });
        let view = cx.new(|cx| HistoryView::new(Some("/p".into()), cx));
        view.update(cx, |view, cx| {
            assert_eq!(view.urls(), ["https://a.test/", "https://old.test/"]);
            view.set_all(true, cx);
            assert_eq!(view.urls().len(), 3);
            view.search
                .update(cx, |f, cx| f.set_text("B.TEST".into(), cx));
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            assert_eq!(view.urls(), ["https://b.test/"]);
            view.search
                .update(cx, |f, cx| f.set_text(String::new(), cx));
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.set_all(false, cx);
            // The last hour of this project only.
            view.delete_since(now - HOUR, cx);
            assert_eq!(view.urls(), ["https://old.test/"]);
            view.delete(0, cx);
            assert!(view.urls().is_empty());
            view.set_all(true, cx);
            assert_eq!(view.urls(), ["https://b.test/"]);
            view.clear(cx);
            assert_eq!(view.urls().len(), 1, "the first press only asks");
            view.clear(cx);
            assert!(view.urls().is_empty());
        });
    }
}
