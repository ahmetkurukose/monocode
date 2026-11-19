//! How a file pane takes part in the editor's extension points: it reports
//! its document's life to the hub, mirrors what features stored there for
//! its file into its editor, installs the registered providers, walks the
//! navigation history, and draws the header's arrows, crumbs, problem count
//! and contributed buttons. The features themselves live elsewhere.
use super::{FileEditor, OpenPath};
use crate::{
    code_editor::{
        self, Crumb, DocumentInfo, GutterSlot, HeaderAction, HeaderClick, HeaderProvider, HubEvent,
        Location, Severity,
        symbols::{self, Symbol},
    },
    editor_analysis::Edit,
    popover::{Anchor, Menu, MenuAction, MenuEvent, MenuItem, menu_popover},
    right_panel::icon_action,
    theme::*,
};
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, Focusable, Pixels, Point, Render, SharedString,
    Subscription, Task, Window, div, prelude::*, px, rgb,
};
use std::{rc::Rc, sync::Arc, time::Duration};

/// How long typing rests before the document's subscribers hear of it.
const SYNC_DELAY: Duration = Duration::from_millis(200);
/// A problems list longer than this is cut; the scrollbar has the rest.
const MAX_LISTED_PROBLEMS: usize = 200;

struct HeaderMenu {
    menu: Entity<Menu>,
    position: Point<Pixels>,
    _events: Subscription,
}

pub(super) struct Extensions {
    pub(super) doc: Rc<DocumentInfo>,
    /// Whether the hub has been told this document is open.
    opened: bool,
    /// The text this editor last gave the hub, or loaded. A flush of an
    /// editor whose buffer is still that must not put its older text over
    /// what another editor of the same file has reported since.
    reported: Option<SharedString>,
    sync: Option<Task<()>>,
    header: Vec<Rc<dyn HeaderProvider>>,
    /// The file's symbols as of the last analysis, for the crumbs.
    pub(super) outline: Arc<Vec<Symbol>>,
    menu: Option<HeaderMenu>,
    _hub: Subscription,
}

/// What the extension points add to the header, built per draw.
#[derive(Default)]
pub(super) struct HeaderParts {
    pub(super) leading: Option<AnyElement>,
    pub(super) crumbs: Vec<AnyElement>,
    pub(super) trailing: Vec<AnyElement>,
}

struct Tip(SharedString);
impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(pane_fill())
            .border_1()
            .border_color(content(0.15))
            .text_size(px(12.))
            .text_color(content(1.))
            .child(self.0.clone())
    }
}

impl Extensions {
    pub(super) fn new(
        path: &str,
        root: &str,
        input: &Entity<crate::input::Input>,
        cx: &mut Context<FileEditor>,
    ) -> Self {
        let doc = Rc::new(DocumentInfo::new(path, root));
        let hub = code_editor::hub(cx);
        let providers = code_editor::providers_for(&doc, cx);
        let header = providers.header.clone();
        let editor = cx.weak_entity();
        input.update(cx, |input, cx| {
            input.set_editor_document(
                doc.clone(),
                Some(Rc::new(move |cx| {
                    editor
                        .update(cx, |this, cx| this.sync_document(cx))
                        .unwrap_or(0)
                })),
            );
            input.set_editor_providers(providers, cx);
        });
        cx.on_release(|this, cx| this.close_document(cx)).detach();
        Self {
            doc,
            opened: false,
            reported: None,
            sync: None,
            header,
            outline: Arc::default(),
            menu: None,
            _hub: cx.subscribe(&hub, |this, _, event: &HubEvent, cx| {
                this.hub_event(event, cx)
            }),
        }
    }
}

impl FileEditor {
    /// The file's symbols as of the last analysis pass.
    #[allow(dead_code)] // The outline panel reads it; it lands separately.
    pub(crate) fn outline(&self) -> Arc<Vec<Symbol>> {
        self.extensions.outline.clone()
    }
    /// The text arrived from disk, for the first time or again.
    pub(super) fn document_loaded(&mut self, cx: &mut Context<Self>) {
        if self.extensions.opened {
            self.sync_document(cx);
            return;
        }
        self.extensions.opened = true;
        let doc = self.extensions.doc.clone();
        let text = self.input.read(cx).content.clone();
        let hub = code_editor::hub(cx);
        self.extensions.reported = Some(text.clone());
        hub.update(cx, |hub, cx| hub.document_opened(doc.clone(), text, cx));
        // What features already know about the file shows from the start.
        for source in hub.read(cx).diagnostic_sources(&doc.path) {
            self.mirror_diagnostics(&source, cx);
        }
        for slot in GutterSlot::ALL {
            self.mirror_gutter(slot, cx);
        }
        self.mirror_decorations(cx);
    }
    /// Reports the buffer if it differs from the last report. Returns the
    /// document's version.
    pub(super) fn sync_document(&mut self, cx: &mut Context<Self>) -> u64 {
        if !self.extensions.opened {
            return 0;
        }
        self.extensions.sync = None;
        let path = self.extensions.doc.path.clone();
        let hub = code_editor::hub(cx);
        let text = self.input.read(cx).content.clone();
        if self.extensions.reported.as_ref() == Some(&text) {
            return hub.read(cx).document(&path).map_or(0, |open| open.version);
        }
        self.extensions.reported = Some(text.clone());
        hub.update(cx, |hub, cx| hub.document_changed(&path, text, cx))
    }
    /// Typing reports once it pauses, so a burst is one change.
    pub(super) fn schedule_document_sync(&mut self, cx: &mut Context<Self>) {
        if !self.extensions.opened {
            return;
        }
        self.extensions.sync = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SYNC_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                this.sync_document(cx);
            });
        }));
    }
    pub(super) fn document_saved(&mut self, cx: &mut Context<Self>) {
        if self.extensions.opened {
            self.sync_document(cx);
            let path = self.extensions.doc.path.clone();
            code_editor::hub(cx).update(cx, |hub, cx| hub.document_saved(&path, cx));
        }
    }
    fn close_document(&mut self, cx: &mut App) {
        if std::mem::take(&mut self.extensions.opened) {
            let path = self.extensions.doc.path.clone();
            code_editor::hub(cx).update(cx, |hub, cx| hub.document_closed(&path, cx));
        }
    }

    fn hub_event(&mut self, event: &HubEvent, cx: &mut Context<Self>) {
        let path = &self.extensions.doc.path.clone();
        match event {
            HubEvent::Providers => {
                let providers = code_editor::providers_for(&self.extensions.doc, cx);
                self.extensions.header = providers.header.clone();
                self.input
                    .update(cx, |input, cx| input.set_editor_providers(providers, cx));
                cx.notify();
            }
            HubEvent::History => cx.notify(),
            // Before the text is here there is nothing to place marks on;
            // loading mirrors everything.
            _ if !self.extensions.opened => {}
            HubEvent::Diagnostics { path: file, source } if file == path => {
                self.mirror_diagnostics(source, cx)
            }
            HubEvent::Gutter { path: file, slot } if file == path => self.mirror_gutter(*slot, cx),
            HubEvent::Decorations(file) if file == path => self.mirror_decorations(cx),
            HubEvent::Reveal { path: file, row } if file == path => {
                let row = *row;
                self.input.update(cx, |input, cx| input.reveal_row(row, cx));
            }
            _ => {}
        }
    }
    fn mirror_diagnostics(&mut self, source: &str, cx: &mut Context<Self>) {
        let path = &self.extensions.doc.path;
        let hub = code_editor::hub(cx);
        let hub = hub.read(cx);
        let mut items = hub.diagnostics(path, Some(source));
        // They were computed for the text last reported; typing since then
        // has moved on, so they are carried over the difference.
        let current = self.input.read(cx).content.clone();
        if let Some(reported) = hub.document(path).map(|open| &open.text)
            && let Some(edit) = Edit::between(reported, &current)
        {
            code_editor::remap_diagnostics(&mut items, edit);
        }
        self.input.update(cx, |input, cx| {
            input.set_external_diagnostics(source, items, cx)
        });
    }
    /// Copies one column's marks. The editor shifts both columns itself as
    /// it is typed in; a feature that rewrote one column must not bring back
    /// the hub's older rows of the other.
    fn mirror_gutter(&mut self, slot: GutterSlot, cx: &mut Context<Self>) {
        let path = &self.extensions.doc.path;
        let marks = code_editor::hub(cx)
            .read(cx)
            .gutter_marks(path, slot)
            .to_vec();
        self.input
            .update(cx, |input, cx| input.set_gutter_marks(slot, marks, cx));
    }
    fn mirror_decorations(&mut self, cx: &mut Context<Self>) {
        let path = &self.extensions.doc.path;
        let hub = code_editor::hub(cx);
        let (lines, notes) = {
            let hub = hub.read(cx);
            (hub.line_decorations(path), hub.inline_notes(path))
        };
        self.input.update(cx, |input, cx| {
            input.set_line_decorations(lines, cx);
            input.set_inline_notes(notes, cx);
        });
    }

    fn location(&self, cx: &App) -> Location {
        let input = self.input.read(cx);
        Location::of(&self.extensions.doc.path, &input.content, input.caret())
    }
    /// Steps through the jump history: within this file directly, to
    /// another through the application's file route, which opens it.
    pub(super) fn navigate_history(
        &mut self,
        back: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.document.is_some().then(|| self.location(cx));
        let target = code_editor::hub(cx).update(cx, |hub, cx| {
            if back {
                hub.back(current, cx)
            } else {
                hub.forward(current, cx)
            }
        });
        match target {
            Some(to) if to.path == self.extensions.doc.path => {
                self.navigate(to.line, to.column, cx);
                window.focus(&self.input.focus_handle(cx), cx);
            }
            Some(to) => window.dispatch_action(
                Box::new(OpenPath(format!(
                    "{}:{}:{}",
                    to.path.display(),
                    to.line,
                    to.column
                ))),
                cx,
            ),
            None => {}
        }
    }
    pub(super) fn jump_to(&mut self, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.jump_to_offset(offset, cx));
        window.focus(&self.input.focus_handle(cx), cx);
        cx.notify();
    }

    fn header_click(&self, event: &ClickEvent, cx: &App) -> HeaderClick {
        let input = self.input.read(cx);
        let offset = input.caret();
        HeaderClick {
            doc: self.extensions.doc.clone(),
            position: event.position(),
            offset,
            row: code_editor::row_of(&input.content, offset),
        }
    }
    fn open_header_menu(
        &mut self,
        label: &'static str,
        items: Vec<MenuItem>,
        position: Point<Pixels>,
        on_pick: impl Fn(&mut Self, &str, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let menu = cx.new(|cx| Menu::new(label, items, window, cx).width(420.));
        let events = cx.subscribe_in(
            &menu,
            window,
            move |this, _, event: &MenuEvent, window, cx| {
                this.extensions.menu = None;
                if let MenuEvent::Picked(id, _) = event {
                    on_pick(this, id, window, cx);
                }
                cx.notify();
            },
        );
        self.extensions.menu = Some(HeaderMenu {
            menu,
            position,
            _events: events,
        });
        cx.notify();
    }
    /// A menu at `position` for a feature that has no view of its own to
    /// hang one from: a gutter marker's tasks.
    pub(crate) fn open_menu_at(
        &mut self,
        label: &'static str,
        items: Vec<MenuItem>,
        position: Point<Pixels>,
        on_pick: impl Fn(&str, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on_pick = move |_: &mut Self, id: &str, window: &mut Window, cx: &mut Context<Self>| {
            on_pick(id, window, cx)
        };
        self.open_header_menu(label, items, position, on_pick, window, cx);
    }
    pub(super) fn header_menu(&self, cx: &App) -> Option<AnyElement> {
        self.extensions.menu.as_ref().map(|state| {
            menu_popover(&state.menu, Anchor::Point(state.position), cx)
                .without_animation()
                .into_any_element()
        })
    }
    /// Lists the file's problems under the count; picking one goes there.
    fn open_problems(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.input.read(cx);
        let problems = input.problems();
        let mut offsets = Vec::new();
        let mut items: Vec<MenuItem> = Vec::new();
        for (n, (at, severity, text)) in problems.iter().take(MAX_LISTED_PROBLEMS).enumerate() {
            let line = code_editor::row_of(&input.content, *at) + 1;
            let summary = text.lines().next().unwrap_or("").to_owned();
            offsets.push(*at);
            items.push(
                MenuAction::new(n.to_string(), summary)
                    .icon(match severity {
                        Severity::Error => "circle-x",
                        _ => "circle-alert",
                    })
                    .icon_color(severity.color())
                    .value(format!("Ln {line}"))
                    .into(),
            );
        }
        if problems.len() > MAX_LISTED_PROBLEMS {
            items.push(MenuItem::Note(
                format!("{} more", problems.len() - MAX_LISTED_PROBLEMS).into(),
            ));
        }
        self.open_header_menu(
            "Problems",
            items,
            position,
            move |this, id, window, cx| {
                if let Some(at) = id.parse::<usize>().ok().and_then(|n| offsets.get(n)) {
                    this.jump_to(*at, window, cx);
                }
            },
            window,
            cx,
        );
    }

    pub(super) fn crumbs(&self, cx: &App) -> Vec<Crumb> {
        let caret = self.input.read(cx).caret();
        let doc = &self.extensions.doc;
        self.extensions
            .header
            .iter()
            .find_map(|provider| provider.crumbs(doc, caret, cx))
            .unwrap_or_else(|| {
                symbols::chain_at(&self.extensions.outline, caret)
                    .into_iter()
                    .map(|symbol| Crumb {
                        label: symbol.name.clone().into(),
                        kind: Some(symbol.kind),
                        offset: symbol.name_range.start,
                    })
                    .collect()
            })
    }
    fn header_button(&self, action: HeaderAction, cx: &mut Context<Self>) -> AnyElement {
        let HeaderAction {
            id,
            icon: path,
            tooltip,
            enabled,
            active,
            on_click,
            menu,
            on_pick,
        } = action;
        let tip = tooltip.clone();
        let selector = id.clone();
        let button = icon_action(id.clone(), tooltip, path, enabled)
            .debug_selector(move || format!("file-header-{selector}"))
            .when(active, |d| d.bg(content(0.15)))
            .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
            .when(enabled, |d| {
                d.cursor_pointer().on_click(cx.listener(
                    move |this, event: &ClickEvent, window, cx| {
                        let click = this.header_click(event, cx);
                        on_click(&click, window, cx);
                    },
                ))
            });
        let (Some(menu), Some(on_pick)) = (menu, on_pick) else {
            return button.into_any_element();
        };
        let selector = id.clone();
        let dropdown = div()
            .id(SharedString::from(format!("{id}-menu")))
            .debug_selector(move || format!("file-header-{selector}-menu"))
            .role(gpui::Role::Button)
            .aria_label("More")
            .cursor_pointer()
            .h(px(20.))
            .w(px(12.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .hover(|s| s.bg(content(0.1)))
            .child(icon("chevron-down", 10.))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                let click = this.header_click(event, cx);
                let on_pick = on_pick.clone();
                this.open_header_menu(
                    "Actions",
                    menu(cx),
                    event.position(),
                    move |_, id, window, cx| on_pick(id, &click, window, cx),
                    window,
                    cx,
                );
            }));
        div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .child(button)
            .child(dropdown)
            .into_any_element()
    }
    /// The header's share of the extension points. Everything here is
    /// absent until a feature gives it something to show, so a pane without
    /// features keeps the header it had.
    pub(super) fn header_parts(&self, cx: &mut Context<Self>) -> HeaderParts {
        let mut parts = HeaderParts::default();
        if self.document.is_none() {
            return parts;
        }
        let hub = code_editor::hub(cx);
        let history = hub.read(cx).history();
        if !history.is_empty() {
            let arrow = |id: &'static str, label: &'static str, path, enabled: bool, back: bool| {
                icon_action(id, label, path, enabled)
                    .size(px(18.))
                    .when(enabled, |d| {
                        d.cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.navigate_history(back, window, cx)
                            }))
                    })
            };
            let back = arrow(
                "file-back",
                "Back",
                "chevron-left",
                history.can_back(),
                true,
            );
            let forward = arrow(
                "file-forward",
                "Forward",
                "chevron-right",
                history.can_forward(),
                false,
            );
            parts.leading = Some(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .child(back)
                    .child(forward)
                    .into_any_element(),
            );
        }
        if self.preview {
            return parts;
        }
        for (n, crumb) in self.crumbs(cx).into_iter().enumerate() {
            let offset = crumb.offset;
            parts.crumbs.push(
                icon("chevron-right", 12.)
                    .text_color(content(0.3))
                    .into_any_element(),
            );
            parts.crumbs.push(
                div()
                    .id(("file-crumb", n))
                    .role(gpui::Role::Link)
                    .aria_label(crumb.label.clone())
                    .cursor_pointer()
                    .min_w_0()
                    .flex_shrink(1.)
                    .truncate()
                    .text_color(content(0.6))
                    .hover(|s| s.text_color(content(1.)))
                    .child(crumb.label)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.jump_to(offset, window, cx)),
                    )
                    .into_any_element(),
            );
        }
        let counts = self.input.read(cx).diagnostic_counts();
        if counts.total() > 0 {
            let count = |path: &'static str, color: gpui::Hsla, n: usize| {
                (n > 0).then(|| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(3.))
                        .child(icon(path, 12.).text_color(color))
                        .child(n.to_string())
                })
            };
            parts.trailing.push(
                div()
                    .id("file-problems")
                    .debug_selector(|| "file-problems".into())
                    .role(gpui::Role::Button)
                    .aria_label(format!(
                        "{} errors, {} warnings",
                        counts.errors, counts.warnings
                    ))
                    .cursor_pointer()
                    .flex_shrink_0()
                    .h(px(20.))
                    .px_1p5()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .text_color(content(0.7))
                    .hover(|s| s.bg(content(0.1)))
                    .children(count("circle-x", Severity::Error.color(), counts.errors))
                    .children(count(
                        "circle-alert",
                        Severity::Warning.color(),
                        counts.warnings,
                    ))
                    .children(count(
                        "circle-alert",
                        rgb(0x60a5fa).into(),
                        counts.infos + counts.hints,
                    ))
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        this.open_problems(event.position(), window, cx)
                    }))
                    .into_any_element(),
            );
        }
        let doc = self.extensions.doc.clone();
        let actions: Vec<HeaderAction> = self
            .extensions
            .header
            .iter()
            .flat_map(|provider| provider.actions(&doc, cx))
            .collect();
        for action in actions {
            parts.trailing.push(self.header_button(action, cx));
        }
        parts
    }
}
