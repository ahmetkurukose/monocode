//! The "Usages" tab of the right panel: what "Find Usages" found when it is
//! more than the editor's own small list holds. Results are grouped by file,
//! each with its line number and the line's text with the symbol marked;
//! a row opens its place and records the move for Back.
use crate::{
    code_editor::{self, Location, References},
    code_shell::panel_tabs::{self, PanelTab},
    theme::*,
};
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, FontWeight, Global, HighlightStyle,
    KeyDownEvent, Role, ScrollHandle, SharedString, StyledText, Window, div, prelude::*, px,
};
use std::{collections::HashSet, ops::Range, path::PathBuf};

pub(super) const TAB: &str = "code-usages";
/// Up to this many usages stay in the editor's list beside the symbol.
const SMALL: usize = 6;
/// A result longer than this is cut; the count still says how many.
const MAX_ROWS: usize = 2000;

/// The view of the last result, while its tab is there; the tests find it here.
#[derive(Default)]
#[cfg_attr(not(test), allow(dead_code))]
struct Shown(Option<Entity<Usages>>);
impl Global for Shown {}

pub(super) fn install(cx: &mut App) {
    let hub = code_editor::hub(cx);
    hub.update(cx, |hub, _| {
        hub.set_references_handler(Some(std::rc::Rc::new(show)));
    });
}

#[cfg(test)]
pub(super) fn shown(cx: &App) -> Option<Entity<Usages>> {
    cx.try_global::<Shown>()?.0.clone()
}

/// Takes a result the editor's list is too small for. The tab replaces the
/// one of the search before it.
fn show(found: &References, window: &mut Window, cx: &mut App) -> bool {
    if found.targets.len() <= SMALL {
        return false;
    }
    let root = code_editor::hub(cx)
        .read(cx)
        .document(&found.origin.path)
        .map(|open| open.doc.root.clone())
        .unwrap_or_default();
    let view = cx.new(|cx| Usages::new(found, root, cx));
    let focus = view.read(cx).focus.clone();
    cx.set_global(Shown(Some(view.clone())));
    panel_tabs::show(
        PanelTab {
            id: TAB,
            title: "Usages",
            icon: "search",
            view: view.into(),
            focus: Some(focus),
            closable: true,
            on_close: Some(std::rc::Rc::new(|cx| cx.set_global(Shown(None)))),
        },
        cx,
    );
    // The editor that asked is in the middle of its own update.
    window.defer(cx, |window, cx| {
        panel_tabs::reveal(TAB, window, cx);
    });
    true
}

enum Row {
    File {
        group: usize,
        /// The path from the checkout, or whole when it lies outside.
        shown: SharedString,
        count: usize,
    },
    Usage {
        group: usize,
        location: Location,
        text: SharedString,
        /// The symbol within `text`.
        mark: Option<Range<usize>>,
    },
}

/// Where the `nth` whole-word occurrence of `symbol` is in `text`.
fn occurrence(text: &str, symbol: &str, nth: usize) -> Option<Range<usize>> {
    if symbol.is_empty() {
        return None;
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(symbol)
        .map(|(at, _)| at..at + symbol.len())
        .filter(|range| {
            !text[..range.start].chars().next_back().is_some_and(word)
                && !text[range.end..].chars().next().is_some_and(word)
        })
        .nth(nth)
}

pub(super) struct Usages {
    symbol: SharedString,
    total: usize,
    files: usize,
    rows: Vec<Row>,
    collapsed: HashSet<usize>,
    /// Index into `rows` of the usage the keyboard is on.
    selected: Option<usize>,
    /// Where the last opened row left from, for the history.
    last: Location,
    focus: FocusHandle,
    scroll: ScrollHandle,
}

impl Focusable for Usages {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Usages {
    fn new(found: &References, root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut groups: Vec<(PathBuf, Vec<&code_editor::ResolvedTarget>)> = Vec::new();
        for target in found.targets.iter().take(MAX_ROWS) {
            match (groups.iter_mut()).find(|(path, _)| *path == target.location.path) {
                Some((_, targets)) => targets.push(target),
                None => groups.push((target.location.path.clone(), vec![target])),
            }
        }
        let mut rows = Vec::new();
        for (group, (path, targets)) in groups.iter_mut().enumerate() {
            targets.sort_by_key(|target| (target.location.line, target.location.column));
            let shown = path.strip_prefix(&root).unwrap_or(path);
            rows.push(Row::File {
                group,
                shown: shown.to_string_lossy().into_owned().into(),
                count: targets.len(),
            });
            let mut line = (0, 0);
            for target in targets.iter() {
                // Several usages on one line mark one occurrence each.
                line = match line {
                    (at, nth) if at == target.location.line => (at, nth + 1),
                    _ => (target.location.line, 0),
                };
                rows.push(Row::Usage {
                    group,
                    location: target.location.clone(),
                    mark: occurrence(&target.text, &found.symbol, line.1),
                    text: target.text.clone().into(),
                });
            }
        }
        let selected = rows.iter().position(|row| matches!(row, Row::Usage { .. }));
        Self {
            symbol: found.symbol.clone().into(),
            total: found.targets.len(),
            files: groups.len(),
            rows,
            collapsed: HashSet::new(),
            selected,
            last: found.origin.clone(),
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
        }
    }

    fn visible(&self, index: usize) -> bool {
        match &self.rows[index] {
            Row::File { .. } => true,
            Row::Usage { group, .. } => !self.collapsed.contains(group),
        }
    }

    /// Moves the selection to the next usage that shows, in either direction.
    fn step(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let usage =
            |index: &usize| matches!(self.rows[*index], Row::Usage { .. }) && self.visible(*index);
        let next = match (self.selected, backwards) {
            (None, _) => (0..self.rows.len()).find(usage),
            (Some(at), false) => (at + 1..self.rows.len()).find(usage),
            (Some(at), true) => (0..at).rev().find(usage),
        };
        if let Some(next) = next {
            self.selected = Some(next);
            let shown = (0..next).filter(|index| self.visible(*index)).count();
            self.scroll.scroll_to_item(shown);
            cx.notify();
        }
    }

    /// Opens a usage where files open, as a jump Back returns from.
    pub(super) fn open(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Row::Usage { location, .. }) = self.rows.get(index) else {
            return;
        };
        let to = location.clone();
        let from = std::mem::replace(&mut self.last, to.clone());
        self.selected = Some(index);
        code_editor::hub(cx).update(cx, |hub, cx| hub.jump(from, to.clone(), cx));
        code_editor::open_location(&to, window, cx);
        cx.notify();
    }

    fn toggle(&mut self, group: usize, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&group) {
            self.collapsed.insert(group);
        }
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "down" => self.step(false, cx),
            "up" => self.step(true, cx),
            "enter" => {
                if let Some(index) = self.selected {
                    self.open(index, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The symbol, the count, and each listed place as `path:line: text`.
    #[cfg(test)]
    pub(super) fn listed(&self) -> (String, usize, Vec<String>) {
        let lines = (self.rows.iter())
            .filter_map(|row| match row {
                Row::Usage { location, text, .. } => Some(format!(
                    "{}:{}: {text}",
                    location.path.file_name()?.to_string_lossy(),
                    location.line
                )),
                Row::File { .. } => None,
            })
            .collect();
        (self.symbol.to_string(), self.total, lines)
    }

    #[cfg(test)]
    pub(super) fn usage_rows(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|index| matches!(self.rows[*index], Row::Usage { .. }))
            .collect()
    }
}

impl Render for Usages {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let summary = format!(
            "{} {} in {} {}",
            self.total,
            if self.total == 1 { "usage" } else { "usages" },
            self.files,
            if self.files == 1 { "file" } else { "files" },
        );
        let selected = self.selected;
        let rows: Vec<gpui::AnyElement> = (0..self.rows.len())
            .filter(|index| self.visible(*index))
            .map(|index| match &self.rows[index] {
                Row::File {
                    group,
                    shown,
                    count,
                } => {
                    let group = *group;
                    div()
                        .id(("usages-file", index))
                        .role(Role::Button)
                        .aria_label(shown.clone())
                        .cursor_pointer()
                        .h(px(24.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_1()
                        .hover(|s| s.bg(content(0.06)))
                        .child(icon(
                            match self.collapsed.contains(&group) {
                                true => "chevron-right",
                                false => "chevron-down",
                            },
                            12.,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(content(0.85))
                                .font_weight(FontWeight::MEDIUM)
                                .child(shown.clone()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(content(0.4))
                                .child(count.to_string()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle(group, cx)))
                        .into_any_element()
                }
                Row::Usage {
                    location,
                    text,
                    mark,
                    ..
                } => {
                    let line =
                        StyledText::new(text.clone()).with_highlights(mark.clone().map(|range| {
                            (
                                range,
                                HighlightStyle {
                                    color: Some(accent_color()),
                                    font_weight: Some(FontWeight::BOLD),
                                    ..Default::default()
                                },
                            )
                        }));
                    div()
                        .id(("usages-row", index))
                        .role(Role::Link)
                        .aria_label(format!("Line {}: {text}", location.line))
                        .cursor_pointer()
                        .h(px(22.))
                        .pl(px(22.))
                        .pr_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(selected == Some(index), |d| d.bg(content(0.1)))
                        .hover(|s| s.bg(content(0.06)))
                        .child(
                            div()
                                .w(px(34.))
                                .flex_shrink_0()
                                .text_color(content(0.4))
                                .child(location.line.to_string()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(crate::fonts::mono())
                                .text_color(content(0.75))
                                .child(line),
                        )
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.open(index, window, cx)),
                        )
                        .into_any_element()
                }
            })
            .collect();
        div()
            .id("code-usages")
            .debug_selector(|| "code-usages".into())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(12.))
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(30.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(crate::fonts::mono())
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.symbol.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(content(0.5))
                            .child(summary),
                    )
                    .child(
                        icon_button("close-code-usages", "Close usages", "close")
                            .size(px(22.))
                            .on_click(|_, _, cx| panel_tabs::remove(TAB, cx)),
                    ),
            )
            .child(
                div()
                    .id("code-usages-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows)
                    .when(self.total > MAX_ROWS, |d| {
                        d.child(
                            div()
                                .px_2()
                                .py_1()
                                .text_color(content(0.45))
                                .child(format!("{} more are not listed.", self.total - MAX_ROWS)),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_usage_on_a_line_marks_its_own_occurrence() {
        let text = "total = add(total, subtotal)";
        assert_eq!(occurrence(text, "total", 0), Some(0..5));
        // `subtotal` holds the word without being it.
        assert_eq!(occurrence(text, "total", 1), Some(12..17));
        assert_eq!(occurrence(text, "total", 2), None);
        assert_eq!(occurrence(text, "", 0), None);
    }
}
