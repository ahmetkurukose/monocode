//! The editor's half of the extension points: where an `Input` in editor mode
//! keeps the providers installed on it and the state they drive, how it asks
//! them, and the setters outside code pushes diagnostics, gutter marks and
//! line decorations through. Compiled as a child of the editor so it can
//! reach the buffer; nothing here runs for an editor without extensions.
use super::*;
use crate::code_editor::{
    CompletionProvider, CompletionRequest, CompletionTrigger, DiagnosticCounts, DocumentInfo,
    ExternalDiagnostic, GutterMark, GutterSlot, InlineNote, LineDecoration, Location, Position,
    Providers, RowShift, TextEdit, markdown::Document, rank,
};
use crate::editor_analysis::Edit;
use gpui::AsyncApp;
use std::time::Duration;

/// Reports the buffer to the hub if it changed and returns its version.
pub(crate) type Flush = Rc<dyn Fn(&mut App) -> u64>;

#[derive(Default)]
pub(super) struct Ext {
    pub(super) providers: Providers,
    pub(super) doc: Option<Rc<DocumentInfo>>,
    pub(super) flush: Option<Flush>,
    /// Caret, buffer length and scroll as of the last draw; a difference
    /// closes what was anchored to the old place.
    seen: Option<(usize, usize, Pixels)>,
    completion_doc: Option<(usize, Document)>,
    completion_resolve: Option<Task<()>>,
    /// The list an edit just closed, with the text and caret it was for,
    /// until the provider is asked again and it can be carried over.
    completion_kept: Option<(SharedString, usize, Vec<CompletionItem>)>,
    pub(super) hover: Option<popups::HoverPopup>,
    pub(super) hover_probe: Option<Range<usize>>,
    /// The hover was asked for at the caret, not by resting the mouse.
    pub(super) hover_pinned: bool,
    pub(super) hover_task: Option<Task<()>>,
    pub(super) signature: Option<popups::SignaturePopup>,
    /// A signature is showing or on its way, so caret moves ask again.
    pub(super) signature_live: bool,
    pub(super) signature_task: Option<Task<()>>,
    pub(super) link_probe: Option<Range<usize>>,
    pub(super) link_task: Option<Task<()>>,
    pub(super) navigation: Option<Task<()>>,
    pub(super) picker: Option<popups::Picker>,
    pub(super) gutter_menu: Option<popups::GutterMenuState>,
    pub(super) gutter_tip: Option<(SharedString, f32)>,
    /// Where the mouse last was over the text and whether the link
    /// modifier was held, to tell a real move from a replayed one.
    pub(super) pointer: Option<(Point<Pixels>, bool)>,
    marks_moved: [bool; 2],
}

impl Code {
    pub(super) fn ext_mut(&mut self) -> &mut Ext {
        self.ext.get_or_insert_with(Default::default)
    }
    pub(super) fn decor_mut(&mut self) -> &mut Decor {
        Rc::make_mut(self.decor.get_or_insert_with(Default::default))
    }
    /// The buffer is changing: sets the provider's list aside before it
    /// is cleared.
    pub(super) fn keep_completions(&mut self) {
        let Some(ext) = &mut self.ext else {
            return;
        };
        ext.completion_kept = None;
        if ext.providers.completion.is_some()
            && !self.completions.is_empty()
            && let Some((source, at)) = self.completion_source.take()
        {
            ext.completion_kept = Some((source, at, std::mem::take(&mut self.completions)));
        }
    }
    /// A request for `(source, at)` is on its way: when a provider answers
    /// it, what the list before it still has to offer shows meanwhile.
    pub(super) fn carry_completions(&mut self, provider: bool, source: &str, at: usize) {
        let Some(ext) = &mut self.ext else {
            return;
        };
        let Some((old, old_at, items)) = ext.completion_kept.take().filter(|_| provider) else {
            return;
        };
        ext.completion_doc = None;
        ext.completion_resolve = None;
        self.completions = crate::code_editor::carry_completions(items, &old, old_at, source, at);
        self.completion_index = 0;
    }
    pub(super) fn document(&self) -> Rc<DocumentInfo> {
        self.ext
            .as_ref()
            .and_then(|ext| ext.doc.clone())
            .unwrap_or_else(|| Rc::new(DocumentInfo::new(&self.path, "")))
    }
    /// Called with the line starts from before an edit, while `offsets`
    /// already holds the new ones.
    pub(super) fn remap_decor(&mut self, edit: Edit, old_offsets: &[usize], text: &str) {
        let Some(decor) = &mut self.decor else {
            return;
        };
        let rows = RowShift::new(edit, old_offsets, &self.offsets, text);
        let moved = Rc::make_mut(decor).remap(edit, rows);
        if let Some(ext) = &mut self.ext {
            ext.marks_moved[0] |= moved[0];
            ext.marks_moved[1] |= moved[1];
        }
    }
}

/// A completion provider and what it is asked with, taken before the pause
/// so the request itself needs nothing from the editor.
pub(super) struct Asked {
    provider: Rc<dyn CompletionProvider>,
    doc: Rc<DocumentInfo>,
    flush: Option<Flush>,
    trigger: CompletionTrigger,
}
impl Asked {
    pub(super) async fn run(
        self,
        source: &SharedString,
        at: usize,
        cx: &mut AsyncApp,
    ) -> Vec<CompletionItem> {
        // Two updates on purpose: the first one's document event reaches
        // its subscribers before the provider is called in the second.
        let version = cx.update(|cx| self.flush.as_ref().map_or(0, |flush| flush(cx)));
        let request = CompletionRequest {
            at: Position {
                doc: self.doc,
                text: source.clone(),
                offset: at,
                version,
            },
            trigger: self.trigger,
        };
        let task = cx.update(|cx| self.provider.complete(request, cx));
        rank(task.await, source, at)
    }
}

impl Input {
    /// Says which file this editor shows. `flush` lets a request bring the
    /// document's subscribers up to date first.
    pub(crate) fn set_editor_document(&mut self, doc: Rc<DocumentInfo>, flush: Option<Flush>) {
        if let Some(code) = &mut self.code {
            let ext = code.ext_mut();
            ext.doc = Some(doc);
            ext.flush = flush;
        }
    }
    /// Installs (or replaces) the providers of this editor.
    pub(crate) fn set_editor_providers(&mut self, providers: Providers, cx: &mut Context<Self>) {
        let Some(code) = &mut self.code else {
            return;
        };
        if code.ext.is_none() && providers.is_empty() {
            return;
        }
        let slots = [
            providers.breakpoints.is_some(),
            providers.run_markers.is_some(),
        ];
        code.ext_mut().providers = providers;
        if code.decor.as_ref().map_or([false; 2], |decor| decor.slots) != slots {
            code.decor_mut().slots = slots;
        }
        cx.notify();
    }
    #[allow(dead_code)] // For the feature modules, which land separately.
    pub(crate) fn editor_providers(&self) -> Option<&Providers> {
        Some(&self.code.as_ref()?.ext.as_ref()?.providers)
    }
    /// Replaces what `source` reported. Ranges are bytes of the current text.
    pub(crate) fn set_external_diagnostics(
        &mut self,
        source: &str,
        items: Vec<ExternalDiagnostic>,
        cx: &mut Context<Self>,
    ) {
        let Some(code) = &mut self.code else {
            return;
        };
        if code.decor.is_none() && items.is_empty() {
            return;
        }
        let len = self.content.len();
        let decor = code.decor_mut();
        decor.diagnostics.retain(|item| item.source != source);
        decor.diagnostics.extend(items.into_iter().map(|mut item| {
            item.range = item.range.start.min(len)..item.range.end.min(len);
            item
        }));
        decor.diagnostics.sort_by_key(|item| item.range.start);
        cx.notify();
    }
    pub(crate) fn external_diagnostics(&self) -> &[ExternalDiagnostic] {
        self.code
            .as_ref()
            .and_then(|code| code.decor.as_ref())
            .map_or(&[], |decor| &decor.diagnostics)
    }
    /// The editor's own syntax diagnostics and the outside ones together.
    pub(crate) fn diagnostic_counts(&self) -> DiagnosticCounts {
        let mut counts = DiagnosticCounts::default();
        for (_, severity) in self.code.iter().flat_map(|code| code.diagnostic_ranges()) {
            counts.add(severity);
        }
        counts
    }
    /// Every diagnostic as (offset, severity, text), in file order.
    pub(crate) fn problems(&self) -> Vec<(usize, Severity, SharedString)> {
        let Some(code) = &self.code else {
            return Vec::new();
        };
        let mut problems: Vec<_> = code
            .analysis
            .diagnostics
            .iter()
            .map(|d| {
                (
                    d.from,
                    Severity::parse(&d.severity),
                    SharedString::from(d.message.clone()),
                )
            })
            .chain(
                self.external_diagnostics()
                    .iter()
                    .map(|d| (d.range.start, d.severity, d.label().clone())),
            )
            .collect();
        problems.sort_by_key(|(at, ..)| *at);
        problems
    }
    pub(crate) fn set_gutter_marks(
        &mut self,
        slot: GutterSlot,
        marks: Vec<GutterMark>,
        cx: &mut Context<Self>,
    ) {
        if let Some(code) = &mut self.code
            && (code.decor.is_some() || !marks.is_empty())
        {
            code.decor_mut().marks[slot.index()] = marks;
            cx.notify();
        }
    }
    #[allow(dead_code)] // For the feature modules, which land separately.
    pub(crate) fn gutter_marks(&self, slot: GutterSlot) -> &[GutterMark] {
        self.code
            .as_ref()
            .and_then(|code| code.decor.as_ref())
            .map_or(&[], |decor| &decor.marks[slot.index()])
    }
    pub(crate) fn set_line_decorations(
        &mut self,
        lines: Vec<LineDecoration>,
        cx: &mut Context<Self>,
    ) {
        if let Some(code) = &mut self.code
            && (code.decor.is_some() || !lines.is_empty())
        {
            code.decor_mut().lines = lines;
            cx.notify();
        }
    }
    pub(crate) fn set_inline_notes(&mut self, notes: Vec<InlineNote>, cx: &mut Context<Self>) {
        if let Some(code) = &mut self.code
            && (code.decor.is_some() || !notes.is_empty())
        {
            code.decor_mut().notes = notes;
            cx.notify();
        }
    }
    /// Scrolls a zero-based row into view, centred, leaving the caret alone.
    pub(crate) fn reveal_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(code) = &mut self.code else {
            return;
        };
        let row = row.min(code.offsets.len() - 1);
        let top = code.top(row, self.line_height);
        let height = self.bounds.map_or(0., |b| b.size.height.as_f32());
        let scroll = self.scroll_y.as_f32();
        if top < scroll || top + self.line_height > scroll + height {
            self.scroll_y = px((top - height / 2.).max(0.));
            code.reveal = false;
            cx.notify();
        }
    }
    /// The caret as a byte offset.
    pub(crate) fn caret(&self) -> usize {
        self.cursor()
    }
    /// Moves the caret within this file as a jump Back can undo.
    pub(crate) fn jump_to_offset(&mut self, offset: usize, cx: &mut Context<Self>) {
        let Some(code) = &self.code else {
            return;
        };
        let path = code.document().path.clone();
        let from = Location::of(&path, &self.content, self.cursor());
        let to = Location::of(&path, &self.content, offset);
        crate::code_editor::hub(cx).update(cx, |hub, cx| hub.jump(from, to, cx));
        self.select_range(offset..offset, cx);
    }
    /// Applies edits that do not overlap as one undo step. `primary` names
    /// the edit the caret ends in and where inside its text; without it the
    /// caret lands after the last edit. Returns whether they were applied.
    pub(crate) fn apply_edits(
        &mut self,
        edits: &[TextEdit],
        primary: Option<(usize, usize)>,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut order: Vec<usize> = (0..edits.len()).collect();
        order.sort_by_key(|n| edits[*n].range.start);
        let mut end = 0;
        for n in &order {
            let range = &edits[*n].range;
            if range.start < end
                || range.end < range.start
                || range.end > self.content.len()
                || !self.content.is_char_boundary(range.start)
                || !self.content.is_char_boundary(range.end)
            {
                return false;
            }
            end = range.end;
        }
        let (Some(first), Some(last)) = (order.first(), order.last()) else {
            return false;
        };
        let span = edits[*first].range.start..edits[*last].range.end;
        let mut text = String::new();
        let mut at = span.start;
        let mut caret = None;
        for n in order {
            let edit = &edits[n];
            text.push_str(&self.content[at..edit.range.start]);
            if let Some((_, cursor)) = primary.filter(|(primary, _)| *primary == n) {
                caret = Some(text.len() + cursor.min(edit.text.len()));
            }
            text.push_str(&edit.text);
            at = edit.range.end;
        }
        let before = self.content.clone();
        let caret = caret.unwrap_or(text.len());
        self.replace_bytes(span, &text, caret, cx);
        self.content != before
    }

    pub(super) fn completion_request(&self, typed: Option<char>, explicit: bool) -> Option<Asked> {
        let code = self.code.as_ref()?;
        let ext = code.ext.as_ref()?;
        let provider = ext.providers.completion.clone()?;
        let doc = code.document();
        let trigger = if explicit {
            CompletionTrigger::Invoked
        } else {
            match typed? {
                c if c.is_alphanumeric() || c == '_' => CompletionTrigger::Typed,
                c if provider.trigger_characters(&doc).contains(c) => {
                    CompletionTrigger::Character(c)
                }
                _ => return None,
            }
        };
        Some(Asked {
            provider,
            doc,
            flush: ext.flush.clone(),
            trigger,
        })
    }
    pub(super) fn apply_completion(&mut self, item: CompletionItem, cx: &mut Context<Self>) {
        let cursor = item.cursor.unwrap_or(item.insert.len());
        if !item.additional_edits.is_empty() {
            let mut edits = item.additional_edits;
            edits.push(TextEdit {
                range: item.replace.clone(),
                text: item.insert.clone(),
            });
            if self.apply_edits(&edits, Some((edits.len() - 1, cursor)), cx) {
                return;
            }
        }
        self.replace_bytes(item.replace, &item.insert, cursor, cx);
    }
    /// Shows the documentation of the selected completion, asking the
    /// provider for it the first time an item without any is selected.
    pub(super) fn completion_selected(&mut self, cx: &mut Context<Self>) {
        let Some(code) = &mut self.code else {
            return;
        };
        let doc = code.document();
        let Some(ext) = &mut code.ext else {
            return;
        };
        ext.completion_doc = None;
        ext.completion_resolve = None;
        let index = code.completion_index;
        let Some(item) = code.completions.get(index) else {
            return;
        };
        if let Some(text) = &item.documentation {
            if !text.trim().is_empty() {
                ext.completion_doc = Some((index, Document::new(text)));
            }
            return;
        }
        let Some(task) = ext
            .providers
            .completion
            .clone()
            .and_then(|provider| provider.resolve(&doc, item, cx))
        else {
            return;
        };
        let source = code.completion_source.clone();
        ext.completion_resolve = Some(cx.spawn(async move |this, cx| {
            let text = task.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if let Some(code) = &mut this.code
                    && code.completion_source == source
                    && code.completion_index == index
                    && let Some(item) = code.completions.get_mut(index)
                {
                    let shown = (!text.trim().is_empty()).then(|| (index, Document::new(&text)));
                    // Remembered either way, so stepping back does not ask again.
                    item.documentation = Some(text);
                    code.ext_mut().completion_doc = shown;
                    cx.notify();
                }
            });
        }));
    }
    pub(super) fn completion_documentation(
        &self,
        left: f32,
        top: f32,
        width: f32,
        viewport_width: f32,
    ) -> Option<AnyElement> {
        let code = self.code.as_ref()?;
        let (index, doc) = code.ext.as_ref()?.completion_doc.as_ref()?;
        if *index != code.completion_index {
            return None;
        }
        let doc_width = 320_f32.min(viewport_width - 8.);
        // Beside the list where there is room, right first.
        let x = if left + width + 4. + doc_width <= viewport_width {
            left + width + 4.
        } else {
            (left - doc_width - 4.).max(4.)
        };
        Some(
            crate::code_editor::markdown::surface()
                .id("editor-completion-documentation")
                .absolute()
                .left(px(x))
                .top(px(top))
                .w(px(doc_width))
                .max_h(px(240.))
                .overflow_y_scroll()
                .px_2()
                .py_1p5()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(doc.render("completion-documentation"))
                .into_any_element(),
        )
    }
    #[cfg(test)]
    pub(crate) fn completion_documentation_text(&self) -> Option<SharedString> {
        let code = self.code.as_ref()?;
        let (index, doc) = code.ext.as_ref()?.completion_doc.as_ref()?;
        (*index == code.completion_index).then(|| doc.text.clone())
    }

    /// Asks a provider about `offset` after `delay`, once the document's
    /// subscribers have the current text. `done` runs only if the buffer is
    /// still the one that was asked about.
    pub(super) fn ask<T: 'static>(
        &self,
        offset: usize,
        delay: Duration,
        window: &Window,
        cx: &mut Context<Self>,
        call: impl FnOnce(Position, &mut App) -> Task<T> + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) -> Task<()> {
        let (doc, flush) = self.code.as_ref().map_or_else(
            || (Rc::new(DocumentInfo::new("", "")), None),
            |code| {
                (
                    code.document(),
                    code.ext.as_ref().and_then(|ext| ext.flush.clone()),
                )
            },
        );
        let text = self.content.clone();
        cx.spawn_in(window, async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let Ok(version) = cx.update(|_, cx| flush.map_or(0, |flush| flush(cx))) else {
                return;
            };
            let position = Position {
                doc,
                text: text.clone(),
                offset,
                version,
            };
            let Ok(task) = cx.update(|_, cx| call(position, cx)) else {
                return;
            };
            let value = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.content == text {
                    done(this, value, window, cx);
                }
            });
        })
    }

    /// Runs on every draw: closes popovers the caret or the view moved away
    /// from, keeps signature help following the caret, and tells gutter
    /// providers when an edit moved their marks.
    pub(in crate::input) fn sync_code_overlays(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let seen = (self.cursor(), self.content.len(), self.scroll_y);
        let Some(code) = &mut self.code else {
            return;
        };
        // Most editors have no extension: nothing below is built for them,
        // and the document is only named once a provider is to be told.
        let Some(ext) = &mut code.ext else {
            return;
        };
        for slot in GutterSlot::ALL {
            if std::mem::take(&mut ext.marks_moved[slot.index()])
                && let Some(provider) = ext.providers.gutter(slot).cloned()
                && let Some(decor) = &code.decor
            {
                let marks = decor.marks[slot.index()].clone();
                let doc =
                    (ext.doc.clone()).unwrap_or_else(|| Rc::new(DocumentInfo::new(&code.path, "")));
                // The provider may reach back into the editor's owner.
                cx.defer(move |cx| provider.moved(&doc, &marks, cx));
            }
        }
        // The first draw has nothing older to compare with.
        let Some(last) = ext.seen.replace(seen) else {
            return;
        };
        if last == seen {
            return;
        }
        let moved = (last.0, last.1) != (seen.0, seen.1);
        ext.hover = None;
        ext.hover_task = None;
        ext.hover_probe = None;
        ext.hover_pinned = false;
        if moved && ext.signature_live {
            self.request_signature(window, cx);
        }
    }
    /// A character was typed (not pasted, not composed).
    pub(in crate::input) fn code_typed(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(code) = &self.code else {
            return;
        };
        let Some(provider) = code
            .ext
            .as_ref()
            .and_then(|ext| ext.providers.signature.as_ref())
        else {
            return;
        };
        let mut chars = text.chars();
        if let (Some(typed), None) = (chars.next(), chars.next())
            && provider
                .trigger_characters(&code.document())
                .contains(typed)
        {
            self.request_signature(window, cx);
        }
    }
}
