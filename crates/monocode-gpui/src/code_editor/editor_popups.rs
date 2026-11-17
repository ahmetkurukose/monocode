//! What the editor does with its hover, signature and link providers: the
//! pointer and key handling that asks them, the popovers that show their
//! answers, and the list a symbol with several targets opens.
use super::*;
use crate::code_editor::{
    LinkProvider, Location, References, ReferencesProvider, ResolvedTarget, Signature, links,
    markdown::{Document, surface},
};
use crate::popover::{Anchor, Menu, MenuAction, MenuEvent, MenuItem, menu_popover};
use gpui::{HighlightStyle, ModifiersChangedEvent, Size, StyledText, Subscription};
use std::time::Duration;

/// How long the mouse rests on a word before its hover is asked for.
const HOVER_DELAY: Duration = Duration::from_millis(350);
/// Coalesces the signature requests of a burst of typing.
const SIGNATURE_DELAY: Duration = Duration::from_millis(40);

pub(super) struct HoverPopup {
    pub(super) range: Range<usize>,
    pub(super) doc: Document,
}
pub(super) struct SignaturePopup {
    pub(super) signature: Signature,
}
pub(super) struct Picker {
    menu: Entity<Menu>,
    position: Point<Pixels>,
    /// The symbol the list was opened from, where Back returns to.
    origin: usize,
    pub(super) targets: Vec<ResolvedTarget>,
    _events: Subscription,
}
/// The menu a gutter provider asked for, open at the press.
pub(super) struct GutterMenuState {
    menu: Entity<Menu>,
    position: Point<Pixels>,
    _events: Subscription,
}

impl Input {
    /// The buffer offset under a window position, when a character is there
    /// (not the blank space after a line or below the text).
    pub(super) fn text_offset_at(&self, position: Point<Pixels>) -> Option<usize> {
        let bounds = self.bounds.filter(|bounds| bounds.contains(&position))?;
        let code = self.code.as_ref()?;
        // `code_index` records which side of a wrap was clicked for the
        // caret; a passing pointer must not change that.
        let affinity = code.affinity.get();
        let local = position - bounds.origin + point(self.scroll_x, self.scroll_y);
        let at = self.code_index(local);
        let glyph = self.code_position(at);
        code.affinity.set(affinity);
        (local.y >= glyph.y
            && local.y < glyph.y + px(self.line_height)
            && (local.x - glyph.x).abs() <= px(self.font_size * 0.6))
        .then_some(at)
    }
    /// Where a popover anchored at `offset` goes inside the editor: the left
    /// edge of the character and the top of its line, and the editor's size.
    fn popover_anchor(&self, offset: usize) -> Option<(f32, f32, Size<Pixels>)> {
        let code = self.code.as_ref()?;
        let bounds = self.bounds?;
        let at = self.code_position(offset.min(self.content.len()));
        Some((
            code.gutter() + (at.x - self.scroll_x).as_f32(),
            (at.y - self.scroll_y).as_f32(),
            size(bounds.size.width + px(code.gutter()), bounds.size.height),
        ))
    }

    pub(in crate::input) fn code_pointer(
        &mut self,
        e: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(code) = &self.code else {
            return;
        };
        if code.ext.is_none() && code.decor.is_none() {
            return;
        }
        self.gutter_pointer(e, cx);
        if e.pressed_button.is_none() && !self.selecting {
            self.probe_pointer(e.position, e.modifiers.secondary(), window, cx);
        }
    }
    /// Looks at what is under the pointer: a link while the modifier is
    /// held, otherwise something to show a hover for.
    fn probe_pointer(
        &mut self,
        position: Point<Pixels>,
        held: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ext) = self.code.as_mut().and_then(|code| code.ext.as_mut()) else {
            return;
        };
        // The window replays the last mouse move after a redraw. Acting on
        // those would close a hover the key command opened under a mouse
        // that is resting somewhere else.
        if ext.pointer.replace((position, held)) == Some((position, held)) {
            return;
        }
        let links = ext.providers.links.clone();
        let word = self
            .text_offset_at(position)
            .and_then(|at| Some((at, links::word_at(&self.content, at)?)));
        match links.filter(|_| held) {
            Some(provider) => self.probe_link(word, provider, window, cx),
            None => {
                self.clear_link(cx);
                self.probe_hover(word, window, cx);
            }
        }
    }
    /// The pointer left the editor.
    pub(in crate::input) fn code_pointer_left(&mut self, cx: &mut Context<Self>) {
        self.clear_link(cx);
        if let Some(code) = &mut self.code {
            if code
                .decor
                .as_ref()
                .is_some_and(|decor| decor.hover.is_some())
            {
                code.decor_mut().hover = None;
                cx.notify();
            }
            if let Some(ext) = &mut code.ext {
                ext.pointer = None;
                if !ext.hover_pinned {
                    ext.hover_task = None;
                    ext.hover_probe = None;
                }
                if ext.gutter_tip.take().is_some() {
                    cx.notify();
                }
            }
        }
    }
    pub(in crate::input) fn code_modifiers(
        &mut self,
        e: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Pressing or releasing the modifier over a resting mouse changes
        // what is under it from text to a link and back.
        let pointer = self
            .code
            .as_ref()
            .and_then(|code| code.ext.as_ref())
            .and_then(|ext| ext.pointer);
        match pointer {
            Some((position, _)) => {
                self.probe_pointer(position, e.modifiers.secondary(), window, cx)
            }
            None if !e.modifiers.secondary() => self.clear_link(cx),
            None => {}
        }
    }
    pub(in crate::input) fn code_link_hovered(&self) -> bool {
        self.code
            .as_ref()
            .and_then(|code| code.decor.as_ref())
            .is_some_and(|decor| decor.link.is_some())
    }
    fn set_link(&mut self, link: Option<Range<usize>>, cx: &mut Context<Self>) {
        if let Some(code) = &mut self.code
            && code.decor.as_ref().and_then(|decor| decor.link.clone()) != link
        {
            code.decor_mut().link = link;
            cx.notify();
        }
    }
    fn clear_link(&mut self, cx: &mut Context<Self>) {
        if let Some(ext) = self.code.as_mut().and_then(|code| code.ext.as_mut()) {
            ext.link_probe = None;
            ext.link_task = None;
        }
        self.set_link(None, cx);
    }
    fn probe_link(
        &mut self,
        word: Option<(usize, Range<usize>)>,
        provider: Rc<dyn LinkProvider>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = word.as_ref().map(|(_, range)| range.clone());
        let Some(ext) = self.code.as_mut().and_then(|code| code.ext.as_mut()) else {
            return;
        };
        if ext.link_probe == range {
            return;
        }
        ext.link_probe = range;
        ext.link_task = None;
        self.set_link(None, cx);
        let Some((at, range)) = word else {
            return;
        };
        let task = self.ask(
            at,
            Duration::ZERO,
            window,
            cx,
            move |position, cx| provider.navigable(position, cx),
            move |this, navigable, _, cx| {
                let probing = this
                    .code
                    .as_ref()
                    .and_then(|code| code.ext.as_ref())
                    .is_some_and(|ext| ext.link_probe.as_ref() == Some(&range));
                if navigable && probing {
                    this.set_link(Some(range), cx);
                }
            },
        );
        if let Some(code) = &mut self.code {
            code.ext_mut().link_task = Some(task);
        }
    }
    /// The modifier is held over a character: follow the link there.
    pub(in crate::input) fn code_link_down(
        &mut self,
        e: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !e.modifiers.secondary() || self.link_provider().is_none() {
            return false;
        }
        let Some(at) = self
            .text_offset_at(e.position)
            .filter(|at| links::word_at(&self.content, *at).is_some())
        else {
            return false;
        };
        window.focus(&self.focus, cx);
        self.definition_at(at, window, cx);
        true
    }
    /// Who answers "Go to Definition" here, if anyone.
    pub(in crate::input) fn link_provider(&self) -> Option<Rc<dyn LinkProvider>> {
        let ext = self.code.as_ref()?.ext.as_ref()?;
        ext.providers.links.clone()
    }
    /// Who answers "Find Usages" here, if anyone.
    pub(in crate::input) fn references_provider(&self) -> Option<Rc<dyn ReferencesProvider>> {
        let ext = self.code.as_ref()?.ext.as_ref()?;
        ext.providers.references.clone()
    }
    pub(in crate::input) fn go_to_definition(
        &mut self,
        _: &GoToDefinition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.link_provider().is_some() {
            self.definition_at(self.cursor(), window, cx);
        } else {
            cx.propagate();
        }
    }
    pub(in crate::input) fn find_usages(
        &mut self,
        _: &FindUsages,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.references_provider().is_some() {
            self.usages_at(self.cursor(), window, cx);
        } else {
            cx.propagate();
        }
    }
    fn definition_at(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(provider), Some(code)) = (self.link_provider(), &self.code) else {
            return;
        };
        let open = (code.document().path.clone(), self.content.clone());
        let task = self.ask(
            at,
            Duration::ZERO,
            window,
            cx,
            move |position, cx| {
                let targets = provider.targets(position, cx);
                // Finding each target's line reads files.
                cx.background_executor()
                    .spawn(async move { links::resolve(targets.await, open) })
            },
            move |this, targets: Vec<ResolvedTarget>, window, cx| match targets.as_slice() {
                [] => {}
                [only] => this.jump(at, &only.location, window, cx),
                _ => this.open_picker(at, "Definitions", targets, window, cx),
            },
        );
        if let Some(code) = &mut self.code {
            code.ext_mut().navigation = Some(task);
        }
    }
    fn usages_at(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(provider), Some(code)) = (self.references_provider(), &self.code) else {
            return;
        };
        let path = code.document().path.clone();
        let open = (path.clone(), self.content.clone());
        let symbol = links::word_at(&self.content, at)
            .map(|range| self.content[range].to_owned())
            .unwrap_or_default();
        let origin = Location::of(&path, &self.content, at);
        let task = self.ask(
            at,
            Duration::ZERO,
            window,
            cx,
            move |position, cx| {
                let targets = provider.references(position, cx);
                cx.background_executor()
                    .spawn(async move { links::resolve(targets.await, open) })
            },
            move |this, targets: Vec<ResolvedTarget>, window, cx| {
                let references = References {
                    symbol,
                    origin,
                    targets,
                };
                let handler = crate::code_editor::hub(cx).read(cx).references_handler();
                if !handler.is_some_and(|handle| handle(&references, window, cx)) {
                    this.open_picker(at, "Usages", references.targets, window, cx);
                }
            },
        );
        if let Some(code) = &mut self.code {
            code.ext_mut().navigation = Some(task);
        }
    }
    /// Goes to a place and records the move from the symbol at `origin`, in
    /// this file directly and in another through the application's file
    /// route.
    fn jump(&mut self, origin: usize, to: &Location, window: &mut Window, cx: &mut Context<Self>) {
        let Some(code) = &self.code else {
            return;
        };
        let path = code.document().path.clone();
        let from = Location::of(&path, &self.content, origin);
        crate::code_editor::hub(cx).update(cx, |hub, cx| hub.jump(from, to.clone(), cx));
        self.clear_link(cx);
        if to.path == path {
            let at = to.offset_in(&self.content);
            self.select_range(at..at, cx);
            window.focus(&self.focus, cx);
        } else {
            crate::code_editor::open_location(to, window, cx);
        }
    }
    fn open_picker(
        &mut self,
        anchor: usize,
        label: &'static str,
        targets: Vec<ResolvedTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(code) = &self.code else {
            return;
        };
        let doc = code.document();
        let position = self.bounds.map_or_else(Point::default, |bounds| {
            let word = links::word_at(&self.content, anchor).map_or(anchor, |range| range.start);
            bounds.origin + self.code_position(word) - point(self.scroll_x, self.scroll_y)
                + point(px(0.), px(self.line_height))
        });
        let items: Vec<MenuItem> = if targets.is_empty() {
            vec![MenuItem::Note("Nothing found".into())]
        } else {
            targets
                .iter()
                .enumerate()
                .map(|(n, target)| {
                    let path = &target.location.path;
                    let shown = path.strip_prefix(&doc.root).unwrap_or(path);
                    let row = MenuAction::new(
                        n.to_string(),
                        format!("{}:{}", shown.display(), target.location.line),
                    );
                    if target.text.is_empty() {
                        row.into()
                    } else {
                        row.description(target.text.clone()).into()
                    }
                })
                .collect()
        };
        let menu = cx.new(|cx| Menu::new(label, items, window, cx).width(460.));
        let events = cx.subscribe_in(&menu, window, |this, _, event: &MenuEvent, window, cx| {
            let picker = this
                .code
                .as_mut()
                .and_then(|code| code.ext.as_mut())
                .and_then(|ext| ext.picker.take());
            if let (MenuEvent::Picked(id, _), Some(picker)) = (event, picker)
                && let Some(target) = id.parse::<usize>().ok().and_then(|n| picker.targets.get(n))
            {
                this.jump(picker.origin, &target.location, window, cx);
            }
            cx.notify();
        });
        if let Some(code) = &mut self.code {
            code.ext_mut().picker = Some(Picker {
                menu,
                position,
                origin: anchor,
                targets,
                _events: events,
            });
        }
        cx.notify();
    }
    pub(super) fn open_gutter_menu(
        &mut self,
        menu: crate::code_editor::GutterMenu,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on_pick = menu.on_pick;
        let (label, items, width) = (menu.label, menu.items, menu.width);
        let menu = cx.new(|cx| Menu::new(label, items, window, cx).width(width));
        let events = cx.subscribe_in(
            &menu,
            window,
            move |this, _, event: &MenuEvent, window, cx| {
                if let Some(code) = &mut this.code {
                    code.ext_mut().gutter_menu = None;
                }
                if let MenuEvent::Picked(id, _) = event {
                    let (id, on_pick) = (id.clone(), on_pick.clone());
                    // After this update, so the provider may reach the editor.
                    window.defer(cx, move |window, cx| on_pick(&id, window, cx));
                }
                cx.notify();
            },
        );
        if let Some(code) = &mut self.code {
            code.ext_mut().gutter_menu = Some(GutterMenuState {
                menu,
                position,
                _events: events,
            });
        }
        cx.notify();
    }
    #[cfg(test)]
    pub(crate) fn gutter_menu_open(&self) -> bool {
        (self.code.as_ref())
            .and_then(|code| code.ext.as_ref())
            .is_some_and(|ext| ext.gutter_menu.is_some())
    }
    #[cfg(test)]
    pub(crate) fn picker_targets(&self) -> Option<&[ResolvedTarget]> {
        let picker = self.code.as_ref()?.ext.as_ref()?.picker.as_ref()?;
        Some(&picker.targets)
    }

    fn probe_hover(
        &mut self,
        word: Option<(usize, Range<usize>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = word.as_ref().map(|(_, range)| range.clone());
        let Some(ext) = self.code.as_mut().and_then(|code| code.ext.as_mut()) else {
            return;
        };
        // Still on what the popover is about.
        if let (Some(hover), Some(range)) = (&ext.hover, &range)
            && hover.range.start <= range.start
            && range.end <= hover.range.end
        {
            return;
        }
        if ext.hover_probe == range {
            return;
        }
        // A hover opened with the key command belongs to the caret: the
        // mouse passing over blank space does not take it away.
        if ext.hover_pinned && range.is_none() {
            return;
        }
        ext.hover_pinned = false;
        ext.hover_probe = range;
        ext.hover_task = None;
        if ext.hover.take().is_some() {
            cx.notify();
        }
        if let Some((at, range)) = word {
            self.request_hover(at, range, HOVER_DELAY, window, cx);
        }
    }
    fn request_hover(
        &mut self,
        at: usize,
        range: Range<usize>,
        delay: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(code) = &self.code else {
            return;
        };
        let provider = code
            .ext
            .as_ref()
            .and_then(|ext| ext.providers.hover.clone());
        // What outside diagnostics say about this place leads the popover.
        let mut notes = String::new();
        for item in self.external_diagnostics() {
            if item.range.start <= at && at <= item.range.end {
                if !notes.is_empty() {
                    notes.push_str("\n\n");
                }
                notes.push_str(item.label());
            }
        }
        if provider.is_none() && notes.is_empty() {
            return;
        }
        let asked = range.clone();
        let task = self.ask(
            at,
            delay,
            window,
            cx,
            move |position, cx| match provider {
                Some(provider) => provider.hover(position, cx),
                None => Task::ready(None),
            },
            move |this, hover, _, cx| {
                let Some(ext) = this.code.as_mut().and_then(|code| code.ext.as_mut()) else {
                    return;
                };
                if ext.hover_probe.as_ref() != Some(&asked) {
                    return;
                }
                let mut text = notes;
                let mut range = asked;
                if let Some(hover) = hover {
                    if !text.is_empty() && !hover.markdown.trim().is_empty() {
                        text.push_str("\n\n---\n\n");
                    }
                    text.push_str(&hover.markdown);
                    range = hover.range.unwrap_or(range);
                }
                if !text.trim().is_empty() {
                    ext.hover = Some(HoverPopup {
                        range,
                        doc: Document::new(&text),
                    });
                    cx.notify();
                }
            },
        );
        if let Some(code) = &mut self.code {
            code.ext_mut().hover_task = Some(task);
        }
    }
    pub(in crate::input) fn show_hover(
        &mut self,
        _: &ShowHover,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Only an editor something is installed on answers the key.
        let Some(ext) = self.code.as_mut().and_then(|code| code.ext.as_mut()) else {
            cx.propagate();
            return;
        };
        let at = self.selection.end.min(self.content.len());
        let range = links::word_at(&self.content, at).unwrap_or(at..at);
        ext.hover = None;
        ext.hover_pinned = true;
        ext.hover_probe = Some(range.clone());
        self.request_hover(at, range, Duration::ZERO, window, cx);
    }
    #[cfg(test)]
    pub(crate) fn hover_text(&self) -> Option<(Range<usize>, SharedString)> {
        let hover = self.code.as_ref()?.ext.as_ref()?.hover.as_ref()?;
        Some((hover.range.clone(), hover.doc.text.clone()))
    }

    pub(super) fn request_signature(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(provider) = self
            .code
            .as_ref()
            .and_then(|code| code.ext.as_ref())
            .and_then(|ext| ext.providers.signature.clone())
        else {
            return;
        };
        let at = self.cursor();
        let task = self.ask(
            at,
            SIGNATURE_DELAY,
            window,
            cx,
            move |position, cx| provider.signature(position, cx),
            move |this, signature, _, cx| {
                if this.cursor() != at {
                    return;
                }
                if let Some(ext) = this.code.as_mut().and_then(|code| code.ext.as_mut()) {
                    ext.signature_live = signature.is_some();
                    ext.signature = signature.map(|signature| SignaturePopup { signature });
                    cx.notify();
                }
            },
        );
        if let Some(code) = &mut self.code {
            let ext = code.ext_mut();
            ext.signature_live = true;
            ext.signature_task = Some(task);
        }
    }
    pub(in crate::input) fn show_signature(
        &mut self,
        _: &ShowSignature,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let installed = self
            .code
            .as_ref()
            .and_then(|code| code.ext.as_ref())
            .is_some_and(|ext| ext.providers.signature.is_some());
        if installed {
            self.request_signature(window, cx);
        } else {
            cx.propagate();
        }
    }
    #[cfg(test)]
    pub(crate) fn signature_shown(&self) -> Option<&Signature> {
        let popup = self.code.as_ref()?.ext.as_ref()?.signature.as_ref()?;
        Some(&popup.signature)
    }

    /// Escape closes the hover and signature popovers before anything else.
    pub(super) fn dismiss_popovers(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(ext) = self.code.as_mut().and_then(|code| code.ext.as_mut()) else {
            return false;
        };
        let open = ext.hover.is_some() || ext.signature.is_some();
        ext.hover = None;
        ext.hover_pinned = false;
        ext.hover_probe = None;
        ext.hover_task = None;
        ext.signature = None;
        ext.signature_task = None;
        ext.signature_live = false;
        if open {
            cx.notify();
        }
        open
    }

    fn hover_popover(&self) -> Option<AnyElement> {
        let hover = self.code.as_ref()?.ext.as_ref()?.hover.as_ref()?;
        let (x, y, viewport) = self.popover_anchor(hover.range.start)?;
        let longest = hover
            .doc
            .text
            .lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        let width = (longest as f32 * 6.6 + 28.)
            .clamp(160., 480.)
            .min((viewport.width.as_f32() - 8.).max(1.));
        let left = x.clamp(4., (viewport.width.as_f32() - width - 4.).max(4.));
        // Above the line, as the caret's own popovers go below it; under a
        // line too close to the top to fit one.
        let below = y < 120.;
        Some(
            surface()
                .id("editor-hover")
                .occlude()
                .absolute()
                .left(px(left))
                .w(px(width))
                .max_h(px(220.))
                .overflow_y_scroll()
                .px_2()
                .py_1p5()
                .map(|d| {
                    if below {
                        d.top(px(y + self.line_height + 2.))
                    } else {
                        d.bottom(px(viewport.height.as_f32() - y + 2.))
                    }
                })
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(hover.doc.render("editor-hover"))
                .into_any_element(),
        )
    }
    fn signature_popover(&self) -> Option<AnyElement> {
        let popup = self.code.as_ref()?.ext.as_ref()?.signature.as_ref()?;
        let signature = &popup.signature;
        let (x, y, viewport) = self.popover_anchor(self.cursor())?;
        let label = &signature.label;
        let active = signature
            .active
            .and_then(|n| signature.parameters.get(n))
            .filter(|range| {
                range.start < range.end
                    && label.is_char_boundary(range.start)
                    && label.is_char_boundary(range.end)
                    && range.end <= label.len()
            });
        let text = StyledText::new(label.clone()).with_highlights(active.map(|range| {
            (
                range.clone(),
                HighlightStyle {
                    color: Some(crate::theme::accent_color()),
                    font_weight: Some(gpui::FontWeight::BOLD),
                    ..Default::default()
                },
            )
        }));
        let width = (label.chars().count() as f32 * 7.4 + 24.)
            .clamp(120., 520.)
            .min((viewport.width.as_f32() - 8.).max(1.));
        let left = x.clamp(4., (viewport.width.as_f32() - width - 4.).max(4.));
        let below = y < 60.;
        Some(
            surface()
                .id("editor-signature")
                .absolute()
                .left(px(left))
                .w(px(width))
                .px_2()
                .py_1()
                .map(|d| {
                    if below {
                        d.top(px(y + self.line_height + 2.))
                    } else {
                        d.bottom(px(viewport.height.as_f32() - y + 2.))
                    }
                })
                .child(
                    div()
                        .font_family(crate::fonts::mono())
                        .text_color(crate::theme::content(0.75))
                        .child(text),
                )
                .when_some(
                    signature
                        .documentation
                        .as_ref()
                        .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
                        .map(|line| line.trim().to_owned()),
                    |d, line| {
                        d.child(
                            div()
                                .text_size(px(11.))
                                .text_color(crate::theme::content(0.5))
                                .truncate()
                                .child(line),
                        )
                    },
                )
                .into_any_element(),
        )
    }
    /// Everything the extension points draw above the text.
    pub(in crate::input) fn code_popovers(&self, cx: &App) -> Vec<AnyElement> {
        let Some(ext) = self.code.as_ref().and_then(|code| code.ext.as_ref()) else {
            return Vec::new();
        };
        self.gutter_tooltip()
            .into_iter()
            .chain(self.hover_popover())
            .chain(self.signature_popover())
            .chain(ext.picker.as_ref().map(|picker| {
                menu_popover(&picker.menu, Anchor::Point(picker.position), cx)
                    .without_animation()
                    .into_any_element()
            }))
            .chain(ext.gutter_menu.as_ref().map(|open| {
                menu_popover(&open.menu, Anchor::Point(open.position), cx)
                    .without_animation()
                    .into_any_element()
            }))
            .collect()
    }
}
