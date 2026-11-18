//! The "Outline" tab of the right panel: the classes and functions of the
//! file in the pane in front, from the editor's syntax tree, so it needs no
//! language server. It follows the focused file, marks the symbol the caret
//! is in, narrows to what is typed in its filter, and a row jumps to its
//! symbol. (The header's crumbs are the file pane's own; nothing is added
//! to them here.)
use crate::{
    code_editor::symbols::{self, Symbol, SymbolKind},
    code_shell::{
        context::{self, CodeEvent, FocusedFile},
        panel_tabs::{self, PanelTab},
    },
    file_editor::FileEditor,
    input::{self, Input},
    theme::*,
};
use gpui::{
    App, AppContext, Context, Entity, Focusable, Global, KeyDownEvent, Role, ScrollHandle,
    SharedString, Subscription, Window, div, prelude::*, px,
};
use std::sync::Arc;

pub(super) const TAB: &str = "code-outline";

/// The tab's view, where the tests find it.
#[cfg_attr(not(test), allow(dead_code))]
struct Shown(Entity<Outline>);
impl Global for Shown {}

pub(super) fn install(cx: &mut App) {
    let view = cx.new(Outline::new);
    let focus = view.read(cx).filter.focus_handle(cx);
    cx.set_global(Shown(view.clone()));
    panel_tabs::show(
        PanelTab {
            id: TAB,
            title: "Outline",
            icon: "list-bullet",
            view: view.into(),
            focus: Some(focus),
            closable: false,
            on_close: None,
        },
        cx,
    );
}

#[cfg(any(test, feature = "selftest"))]
pub(super) fn shown(cx: &App) -> Option<Entity<Outline>> {
    cx.try_global::<Shown>().map(|shown| shown.0.clone())
}

/// A short mark for a symbol's kind, and the colour the editor gives it.
pub(super) fn kind_mark(kind: SymbolKind) -> (&'static str, &'static str) {
    match kind {
        SymbolKind::Function => ("f", "function"),
        SymbolKind::Method => ("m", "function"),
        SymbolKind::Class => ("C", "type"),
        SymbolKind::Struct => ("S", "type"),
        SymbolKind::Enum => ("E", "type"),
        SymbolKind::Interface => ("I", "type"),
        SymbolKind::Impl => ("i", "keyword"),
        SymbolKind::Module => ("M", "keyword"),
    }
}

pub(super) fn kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        SymbolKind::Class => "class",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Interface => "interface",
        SymbolKind::Impl => "impl",
        SymbolKind::Module => "module",
    }
}

/// The symbols matching `filter` and the ones enclosing them, as indexes.
/// Everything when the filter is empty.
fn matching(symbols: &[Symbol], filter: &str) -> Vec<usize> {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        return (0..symbols.len()).collect();
    }
    let mut keep = vec![false; symbols.len()];
    for (index, symbol) in symbols.iter().enumerate() {
        if !symbol.name.to_lowercase().contains(&filter) {
            continue;
        }
        keep[index] = true;
        // Source order lists a parent before its children: the nearest
        // shallower symbol above is the parent.
        let mut depth = symbol.depth;
        for above in (0..index).rev() {
            if depth == 0 {
                break;
            }
            if symbols[above].depth < depth {
                keep[above] = true;
                depth = symbols[above].depth;
            }
        }
    }
    (0..symbols.len()).filter(|index| keep[*index]).collect()
}

pub(super) struct Outline {
    file: Option<FocusedFile>,
    symbols: Arc<Vec<Symbol>>,
    caret: usize,
    filter: Entity<Input>,
    /// The row the keyboard is on, as an index into `symbols`.
    selected: Option<usize>,
    scroll: ScrollHandle,
    _watching: Vec<Subscription>,
    _events: Vec<Subscription>,
}

impl Outline {
    fn new(cx: &mut Context<Self>) -> Self {
        let filter =
            cx.new(|cx| Input::field("Filter symbols", "Filter symbols", content(0.35), cx));
        let shell = context::events(cx);
        let events = vec![
            cx.subscribe(&shell, |this, _, event: &CodeEvent, cx| {
                if let CodeEvent::FocusedFileChanged(file) = event {
                    this.follow(file.clone(), cx);
                }
            }),
            cx.subscribe(&filter, |this, _, _: &input::Changed, cx| {
                this.selected = None;
                cx.notify();
            }),
            cx.subscribe(&filter, |this, _, _: &input::Submitted, cx| {
                // Enter in the filter goes to the selected row, else to
                // the first match.
                let first = this.selected.or_else(|| this.rows(cx).first().copied());
                if let Some(index) = first {
                    this.jump(index, None, cx);
                }
            }),
        ];
        let mut this = Self {
            file: None,
            symbols: Arc::default(),
            caret: 0,
            filter,
            selected: None,
            scroll: ScrollHandle::new(),
            _watching: Vec::new(),
            _events: events,
        };
        this.follow(context::focused_file(cx), cx);
        this
    }

    fn editor(&self) -> Option<Entity<FileEditor>> {
        self.file.as_ref()?.editor.upgrade()
    }

    /// Shows another file, or none, and watches its editor for new symbols
    /// and caret moves.
    fn follow(&mut self, file: Option<FocusedFile>, cx: &mut Context<Self>) {
        self.file = file;
        self.selected = None;
        self._watching.clear();
        if let Some(editor) = self.editor() {
            let input = editor.read(cx).input.clone();
            self._watching = vec![
                cx.observe(&editor, |this, _, cx| this.refresh(cx)),
                cx.observe(&input, |this, _, cx| this.refresh(cx)),
            ];
        }
        self.symbols = Arc::default();
        self.refresh(cx);
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor() else {
            return;
        };
        let editor = editor.read(cx);
        let symbols = editor.outline();
        let caret = editor.input.read(cx).caret();
        if !Arc::ptr_eq(&symbols, &self.symbols) || caret != self.caret {
            self.symbols = symbols;
            self.caret = caret;
            cx.notify();
        }
    }

    fn rows(&self, cx: &App) -> Vec<usize> {
        matching(&self.symbols, &self.filter.read(cx).content)
    }

    /// The innermost symbol the caret is in.
    fn current(&self) -> Option<usize> {
        let innermost = *symbols::chain_at(&self.symbols, self.caret).last()?;
        (self.symbols.iter()).position(|symbol| std::ptr::eq(symbol, innermost))
    }

    /// Puts the caret on a symbol's name, as a jump Back returns from.
    fn jump(&mut self, index: usize, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let (Some(editor), Some(symbol)) = (self.editor(), self.symbols.get(index)) else {
            return;
        };
        let offset = symbol.name_range.start;
        let input = editor.read(cx).input.clone();
        input.update(cx, |input, cx| input.jump_to_offset(offset, cx));
        let focus = input.focus_handle(cx);
        match window {
            Some(window) => window.focus(&focus, cx),
            // After this update, when the window can be borrowed.
            None => cx.defer(move |cx| {
                crate::code_shell::in_window(cx, |window, cx| window.focus(&focus, cx));
            }),
        }
        self.selected = Some(index);
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        let at = (self.selected.or_else(|| self.current()))
            .and_then(|index| rows.iter().position(|row| *row == index));
        let next = match (event.keystroke.key.as_str(), at) {
            ("down", None) => rows.first().copied(),
            ("down", Some(at)) => rows.get(at + 1).copied(),
            ("up", Some(at)) => at.checked_sub(1).and_then(|at| rows.get(at)).copied(),
            // Enter arrives from the filter field as its submission.
            _ => return,
        };
        if let Some(next) = next {
            self.selected = Some(next);
            if let Some(shown) = rows.iter().position(|row| *row == next) {
                self.scroll.scroll_to_item(shown);
            }
            cx.notify();
        }
        cx.stop_propagation();
    }

    /// The names listed, each marked when it is the caret's symbol.
    #[cfg(any(test, feature = "selftest"))]
    pub(super) fn listed(&self, cx: &App) -> Vec<(String, bool)> {
        let current = self.current();
        (self.rows(cx).into_iter())
            .map(|index| (self.symbols[index].name.clone(), current == Some(index)))
            .collect()
    }

    #[cfg(test)]
    pub(super) fn filter_by(&mut self, text: &str, cx: &mut Context<Self>) {
        (self.filter).update(cx, |filter, cx| filter.set_text(text.to_owned(), cx));
        self.selected = None;
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn jump_to_named(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.symbols.iter().position(|symbol| symbol.name == name) {
            self.jump(index, Some(window), cx);
        }
    }
}

impl Render for Outline {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);
        let current = self.current();
        let selected = self.selected;
        let empty: Option<SharedString> = match (&self.file, self.symbols.is_empty()) {
            (None, _) => Some("Open a file to see its outline.".into()),
            (Some(_), true) => Some("No symbols in this file.".into()),
            (Some(_), false) if rows.is_empty() => Some("Nothing matches the filter.".into()),
            _ => None,
        };
        let list = rows.into_iter().map(|index| {
            let symbol = &self.symbols[index];
            let (mark, colour) = kind_mark(symbol.kind);
            div()
                .id(("code-outline-row", index))
                .role(Role::Link)
                .aria_label(format!("{} {}", kind_name(symbol.kind), symbol.name))
                .cursor_pointer()
                .h(px(22.))
                .pl(px(8. + symbol.depth as f32 * 14.))
                .pr_2()
                .flex()
                .items_center()
                .gap_2()
                .when(current == Some(index), |d| {
                    d.bg(content(0.08)).text_color(content(1.))
                })
                .when(selected == Some(index), |d| d.bg(content(0.14)))
                .hover(|s| s.bg(content(0.06)))
                .child(
                    div()
                        .w(px(12.))
                        .flex_shrink_0()
                        .font_family(crate::fonts::mono())
                        .text_color(syntax_color(colour))
                        .child(mark),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(symbol.name.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(content(0.35))
                        .child((symbol.row + 1).to_string()),
                )
                .on_click(
                    cx.listener(move |this, _, window, cx| this.jump(index, Some(window), cx)),
                )
        });
        div()
            .id("code-outline")
            .debug_selector(|| "code-outline".into())
            .on_key_down(cx.listener(Self::key))
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(12.))
            .text_color(content(0.75))
            .child(
                div().flex_shrink_0().p_2().child(
                    div()
                        .h(px(28.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .rounded_md()
                        .border_1()
                        .border_color(content(0.1))
                        .bg(content(0.04))
                        .child(icon("search", 12.))
                        .child(div().flex_1().min_w_0().child(self.filter.clone())),
                ),
            )
            .child(
                div()
                    .id("code-outline-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(list)
                    .when_some(empty, |d, text| {
                        d.child(div().px_2().py_1().text_color(content(0.45)).child(text))
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_keeps_matches_and_what_encloses_them() {
        let text = "class Store:\n    def load(self):\n        pass\n    def save(self):\n        pass\n\ndef loader():\n    pass\n";
        let symbols = symbols::outline("/p/store.py", text);
        let names = |indexes: Vec<usize>| -> Vec<&str> {
            (indexes.into_iter())
                .map(|index| symbols[index].name.as_str())
                .collect()
        };
        assert_eq!(
            names(matching(&symbols, "")),
            ["Store", "load", "save", "loader"]
        );
        assert_eq!(
            names(matching(&symbols, "LOAD")),
            ["Store", "load", "loader"]
        );
        assert_eq!(names(matching(&symbols, "sav")), ["Store", "save"]);
        assert!(matching(&symbols, "nothing").is_empty());
    }
}
