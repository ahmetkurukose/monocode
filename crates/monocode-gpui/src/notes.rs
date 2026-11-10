//! The desktop Notes split view, backed by the same notes and image assets.
mod preview;

use crate::{
    explorer::DraggedFile,
    input::{self, Input},
    popover::{Anchor, AnchorSlot, popover},
    runtime::{Database, locked},
    theme::*,
};
use gpui::{
    AnyElement, App, ClipboardItem, Context, CursorStyle, Entity, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, FontWeight, MouseButton, Role, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};
use monocode_core::notes::{self, Note, NoteLanguage};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

/// What the language list is choosing for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Picker {
    /// The type of a note about to be created.
    New,
    /// A new type for the open note.
    Change,
}

pub(crate) enum Event {
    Close,
    OpenSource(i64),
    AddToChat(Note),
}

pub(crate) struct Notes {
    store: Database,
    notes: Vec<Note>,
    selected: Option<String>,
    query: Entity<Input>,
    title: Entity<Input>,
    body: Entity<Input>,
    tag: Entity<Input>,
    project_query: Entity<Input>,
    language_query: Entity<Input>,
    new_anchor: AnchorSlot,
    change_anchor: AnchorSlot,
    picker: Option<Picker>,
    languages: HashMap<String, String>,
    syntax: Arc<Mutex<Option<crate::editor_syntax::Session>>>,
    analysis: Option<Task<()>>,
    project_anchor: AnchorSlot,
    project_open: bool,
    cwd: String,
    /// With project tabs, the project whose notes the list shows; notes
    /// saved outside any project show in every project.
    scope: Option<String>,
    /// Shows every project's notes despite `scope`.
    all_projects: bool,
    projects: Vec<String>,
    appearances: HashMap<String, monocode_core::project_appearance::ProjectAppearance>,
    source: bool,
    modes: HashMap<String, bool>,
    document: Option<Entity<preview::Preview>>,
    dirty: bool,
    revision: u64,
    loading: bool,
    error: Option<String>,
    debounce: Option<Task<()>>,
    image_busy: bool,
    image_drag: bool,
    list_width: f32,
    resize: Option<(f32, f32)>,
    blurs_bound: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Event> for Notes {}
impl Focusable for Notes {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl Notes {
    pub(crate) fn new(store: Database, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| Input::field("Filter notes", "Filter notes", content(0.4), cx));
        query.update(cx, |input, _| input.set_font_size(12.));
        let title = cx.new(|cx| {
            let mut input = Input::field("Note title", "Untitled", content(0.35), cx);
            input.set_font_size(20.);
            input.set_max_chars(notes::MAX_TITLE);
            input
        });
        let body = cx.new(|cx| {
            let mut input = Input::editor(cx);
            input.set_font_size(13.);
            input.set_editor_path("note.md".into());
            input.set_max_chars(notes::MAX_BODY);
            input
        });
        let tag = cx.new(|cx| {
            let mut input = Input::field("Add note tag", "Add tag…", content(0.35), cx);
            input.set_font_size(11.);
            input.set_max_chars(notes::MAX_TAG_LENGTH + 1);
            input
        });
        let project_query =
            cx.new(|cx| Input::field("Find project", "Find project…", content(0.4), cx));
        let language_query =
            cx.new(|cx| Input::field("Filter languages", "Filter languages…", content(0.4), cx));
        let subscriptions = vec![
            cx.subscribe(&query, |_, _, _: &input::Changed, cx| cx.notify()),
            cx.subscribe(&title, |this, _, _: &input::Changed, cx| this.edited(cx)),
            cx.subscribe(&body, |this, _, _: &input::Changed, cx| {
                this.highlight_source(cx);
                this.edited(cx);
            }),
            cx.subscribe(&body, |this, _, event: &input::Rejected, cx| {
                this.error = Some(event.0.into());
                cx.notify();
            }),
            cx.subscribe(&tag, |this, _, _: &input::Changed, cx| {
                if this.tag.read(cx).content.ends_with(',') {
                    this.add_tag(cx);
                }
            }),
            cx.subscribe(&tag, |this, _, _: &input::Submitted, cx| this.add_tag(cx)),
            cx.subscribe(&title, |this, _, _: &input::Submitted, cx| this.flush(cx)),
            cx.subscribe(&project_query, |_, _, _: &input::Changed, cx| cx.notify()),
            cx.subscribe(&language_query, |_, _, _: &input::Changed, cx| cx.notify()),
        ];
        let mut this = Self {
            store,
            notes: Vec::new(),
            selected: None,
            query,
            title,
            body,
            tag,
            project_query,
            language_query,
            new_anchor: AnchorSlot::default(),
            change_anchor: AnchorSlot::default(),
            picker: None,
            languages: HashMap::new(),
            syntax: Default::default(),
            analysis: None,
            project_anchor: AnchorSlot::default(),
            project_open: false,
            cwd: String::new(),
            scope: None,
            all_projects: false,
            projects: Vec::new(),
            appearances: HashMap::new(),
            source: false,
            modes: HashMap::new(),
            document: None,
            dirty: false,
            revision: 0,
            loading: true,
            error: None,
            debounce: None,
            image_busy: false,
            image_drag: false,
            list_width: 280.,
            resize: None,
            blurs_bound: false,
            _subscriptions: subscriptions,
        };
        this.refresh(cx);
        this
    }

    pub(crate) fn set_appearances(
        &mut self,
        appearances: HashMap<String, monocode_core::project_appearance::ProjectAppearance>,
        cx: &mut Context<Self>,
    ) {
        self.appearances = appearances;
        cx.notify();
    }

    fn project_mark(&self, path: &str, side: f32) -> AnyElement {
        crate::project_settings::project_icon(
            &self.appearances.get(path).cloned().unwrap_or_default(),
            &project_name(path),
            side,
        )
    }

    /// The language of the open note, Markdown when none is open.
    fn current_language(&self) -> &'static NoteLanguage {
        self.selected
            .as_ref()
            .and_then(|id| self.languages.get(id))
            .map_or_else(
                || notes::language(notes::DEFAULT_LANGUAGE),
                |id| notes::language(id),
            )
    }

    fn language_of(&self, id: &str) -> &'static NoteLanguage {
        notes::language(self.languages.get(id).map_or("", String::as_str))
    }

    /// Colours the editor for the note's language: Markdown headings here, any
    /// other type through the same analysis the file editor uses.
    fn highlight_source(&mut self, cx: &mut Context<Self>) {
        let language = self.current_language();
        let path = format!("note.{}", language.extension);
        self.body
            .update(cx, |input, _| input.set_editor_path(path.clone()));
        if language.id != notes::DEFAULT_LANGUAGE {
            self.schedule_analysis(path, cx);
            return;
        }
        self.analysis = None;
        let text = self.body.read(cx).content.clone();
        let mut at = 0;
        let mut analysis = crate::editor_analysis::Analysis::default();
        for line in text.split_inclusive('\n') {
            let start = at;
            at += line.len();
            let trimmed = line.trim_start_matches(' ');
            let count = trimmed.bytes().take_while(|byte| *byte == b'#').count();
            if line.len() - trimmed.len() <= 3
                && (1..=6).contains(&count)
                && trimmed[count..].starts_with(char::is_whitespace)
            {
                analysis.tokens.push(crate::editor_analysis::Token {
                    from: start,
                    to: at - usize::from(line.ends_with('\n')),
                    kind: "var(--color-markdown-heading)".into(),
                });
            }
        }
        self.body
            .update(cx, |input, cx| input.set_editor_analysis(analysis, cx));
    }

    fn schedule_analysis(&mut self, path: String, cx: &mut Context<Self>) {
        let text = self.body.read(cx).content.clone();
        let syntax = crate::editor_syntax::supports(&path).then(|| self.syntax.clone());
        self.analysis = Some(cx.spawn(async move |this, cx| {
            if syntax.is_none() {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
            }
            let source = text.clone();
            let work = cx.background_executor().spawn(async move {
                match syntax {
                    Some(syntax) => crate::editor_syntax::analyze(&syntax, &path, &source, None),
                    None => crate::editor_analysis::analyze(&path, &source, None),
                }
            });
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.body.read(cx).content == text
                    && let Ok(analysis) = result
                {
                    this.body
                        .update(cx, |input, cx| input.set_editor_analysis(analysis, cx));
                }
            });
        }));
    }

    pub(crate) fn set_context(
        &mut self,
        cwd: String,
        projects: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.cwd = cwd;
        self.projects = projects;
        cx.notify();
    }

    /// Narrows the list to one project's notes, or (`None`) shows them all.
    pub(crate) fn set_scope(&mut self, scope: Option<String>, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        self.all_projects = false;
        self.reselect_shown(cx);
    }

    fn shown(&self, note: &Note) -> bool {
        self.all_projects || note.in_project(self.scope.as_deref())
    }

    fn first_shown(&self) -> Option<String> {
        let note = self.notes.iter().find(|note| self.shown(note))?;
        Some(note.id.clone())
    }

    /// Moves the selection to the first listed note when it left the list.
    fn reselect_shown(&mut self, cx: &mut Context<Self>) {
        self.flush(cx);
        let listed = (self.selected.as_ref())
            .is_some_and(|id| self.notes.iter().any(|n| &n.id == id && self.shown(n)));
        if !self.dirty && !listed {
            self.load_selection(self.first_shown(), cx);
        }
        cx.notify();
    }

    /// How many notes the list holds, once it has loaded.
    pub(crate) fn count(&self) -> Option<usize> {
        (!self.loading).then_some(self.notes.len())
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.flush(cx);
        if self.dirty {
            return;
        }
        let revision = self.revision;
        let store = self.store.clone();
        let work = cx.background_executor().spawn(async move {
            let store = locked(&store)?;
            Ok::<_, String>((store.list_notes()?, store.note_languages()?))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                if this.revision != revision {
                    return;
                }
                match result {
                    Ok((mut notes, languages)) => {
                        this.languages = languages;
                        // Preserve edits made while the refresh was in flight.
                        if this.dirty
                            && let Some(draft) = this.draft(cx)
                        {
                            notes.retain(|note| note.id != draft.id);
                            notes.insert(0, draft);
                        }
                        this.notes = notes;
                        let next = this
                            .selected
                            .clone()
                            .filter(|id| {
                                this.notes
                                    .iter()
                                    .any(|note| &note.id == id && this.shown(note))
                            })
                            .or_else(|| this.first_shown());
                        if !this.dirty || next != this.selected || this.document.is_none() {
                            this.load_selection(next, cx);
                        }
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn draft(&self, cx: &App) -> Option<Note> {
        let mut note = self
            .notes
            .iter()
            .find(|note| Some(&note.id) == self.selected.as_ref())?
            .clone();
        note.title = self.title.read(cx).content.to_string();
        note.body = self.body.read(cx).content.to_string();
        Some(note)
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        // With no note open there is nothing to save. Clearing the editors
        // fires this too, and a dirty flag nothing can clear blocks "New note".
        if self.selected.is_none() {
            return;
        }
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
        self.error = None;
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let _ = this.update(cx, |this, cx| this.persist(false, cx));
        }));
        cx.notify();
    }

    /// Serializing at the editor boundary means a delayed save never overwrites
    /// a newer edit or resurrects a deleted note.
    pub(crate) fn flush(&mut self, cx: &mut Context<Self>) {
        self.add_tag(cx);
        self.persist(true, cx);
    }

    fn persist(&mut self, finalize: bool, cx: &mut Context<Self>) {
        self.debounce = None;
        let Some(mut draft) = self.draft(cx) else {
            self.dirty = false;
            return;
        };
        let saved_title = self
            .notes
            .iter()
            .find(|note| note.id == draft.id)
            .map(|note| note.title.clone())
            .unwrap_or_default();
        if !self.dirty && !(finalize && (draft.slug_pending || draft.title != saved_title)) {
            return;
        }
        self.dirty = true;
        if draft.title.trim().is_empty() {
            draft.title = if finalize {
                notes::title(&draft.body)
            } else {
                saved_title
            };
        }
        let result =
            locked(&self.store).and_then(|store| store.upsert_note(&draft.upsert(finalize)));
        match result {
            Ok(saved) => {
                self.revision = self.revision.wrapping_add(1);
                if finalize {
                    self.title
                        .update(cx, |input, cx| input.set_text(saved.title.clone(), cx));
                }
                self.notes.retain(|note| note.id != saved.id);
                self.notes.push(saved);
                self.notes
                    .sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
                self.dirty = false;
                self.error = None;
            }
            Err(error) => self.error = Some(format!("Could not save note: {error}")),
        }
        cx.notify();
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&id) {
            return;
        }
        self.flush(cx);
        if self.dirty {
            return;
        }
        self.load_selection(Some(id), cx);
    }

    fn load_selection(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.project_open = false;
        self.selected = id;
        self.dirty = false;
        let note = self
            .notes
            .iter()
            .find(|note| Some(&note.id) == self.selected.as_ref())
            .cloned();
        self.title.update(cx, |input, cx| {
            input.set_text(
                note.as_ref()
                    .map(|note| note.title.clone())
                    .unwrap_or_default(),
                cx,
            )
        });
        self.body.update(cx, |input, cx| {
            input.set_text(
                note.as_ref()
                    .map(|note| note.body.clone())
                    .unwrap_or_default(),
                cx,
            )
        });
        self.highlight_source(cx);
        self.body
            .update(cx, |input, cx| input.set_word_wrap(true, cx));
        self.tag
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        self.source = note.as_ref().is_some_and(|note| {
            // Only Markdown has a rendered view; code types are always source.
            self.language_of(&note.id).id != notes::DEFAULT_LANGUAGE
                || self
                    .modes
                    .get(&note.id)
                    .copied()
                    .unwrap_or(note.body.trim().is_empty() && note.title == "Untitled")
        });
        self.update_preview(cx);
        cx.notify();
    }

    fn update_preview(&mut self, cx: &mut Context<Self>) {
        self.document = self.draft(cx).map(|note| {
            cx.new(|_| {
                preview::Preview::new(
                    &note.body,
                    note.source_cwd.as_deref().unwrap_or(&self.cwd),
                    &self.store,
                )
            })
        });
    }

    fn create(&mut self, language: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.flush(cx);
        if self.dirty {
            return;
        }
        let language = notes::language(language).id;
        let result = locked(&self.store).and_then(|store| {
            let note = store.create_note(Some(self.cwd.clone()))?;
            store.set_note_language(&note.id, language)?;
            Ok(note)
        });
        match result {
            Ok(note) => {
                self.revision = self.revision.wrapping_add(1);
                let id = note.id.clone();
                if language != notes::DEFAULT_LANGUAGE {
                    self.languages.insert(id.clone(), language.to_owned());
                }
                self.picker = None;
                self.notes.insert(0, note);
                self.query
                    .update(cx, |input, cx| input.set_text(String::new(), cx));
                self.load_selection(Some(id), cx);
                window.focus(&self.body.focus_handle(cx), cx);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        self.debounce = None;
        let result = locked(&self.store).and_then(|store| store.delete_note(&id));
        match result {
            Ok(()) => {
                self.revision = self.revision.wrapping_add(1);
                self.notes.retain(|note| note.id != id);
                self.modes.remove(&id);
                self.dirty = false;
                self.load_selection(self.first_shown(), cx);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn add_tag(&mut self, cx: &mut Context<Self>) {
        let text = self
            .tag
            .read(cx)
            .content
            .trim()
            .trim_end_matches(',')
            .to_owned();
        if text.is_empty() {
            return;
        }
        self.tag
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        if let Some(note) = self
            .notes
            .iter_mut()
            .find(|note| Some(&note.id) == self.selected.as_ref())
        {
            let tags = notes::normalize_tags(&[note.tags.clone(), vec![text]].concat());
            if tags != note.tags {
                note.tags = tags;
                self.edited(cx);
            }
        }
    }

    fn remove_tag(&mut self, tag: &str, cx: &mut Context<Self>) {
        if let Some(note) = self
            .notes
            .iter_mut()
            .find(|note| Some(&note.id) == self.selected.as_ref())
        {
            note.tags.retain(|saved| saved != tag);
            self.edited(cx);
        }
    }

    fn move_project(&mut self, path: String, cx: &mut Context<Self>) {
        if let Some(note) = self
            .notes
            .iter_mut()
            .find(|note| Some(&note.id) == self.selected.as_ref())
        {
            note.source_cwd = Some(path);
            self.revision = self.revision.wrapping_add(1);
            self.dirty = true;
            self.persist(false, cx);
            self.update_preview(cx);
        }
        self.project_open = false;
        cx.notify();
    }

    fn add_images(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() || self.image_busy {
            return;
        }
        let Some(id) = self.selected.clone() else {
            return;
        };
        let range = if self.source {
            self.body.read(cx).selection_range()
        } else {
            let length = self.body.read(cx).content.len();
            length..length
        };
        let store = self.store.clone();
        self.image_busy = true;
        self.image_drag = false;
        let work = cx.background_executor().spawn(async move {
            let store = locked(&store)?;
            let mut images = Vec::new();
            let mut failure = None;
            for path in paths {
                match store.save_note_image(&id, &path) {
                    Ok(image) => images.push(image),
                    Err(error) => failure = Some(error),
                }
            }
            if images.is_empty() {
                return Err(
                    failure.unwrap_or_else(|| "Drop a PNG, JPG, GIF, WebP, or SVG image.".into())
                );
            }
            Ok((id, images))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.image_busy = false;
                match result {
                    Ok((id, images)) => {
                        let block = images
                            .iter()
                            .map(notes::image_markdown)
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        if this.selected.as_deref() == Some(&id) {
                            let text = this.body.read(cx).content.to_string();
                            let from = range.start.min(text.len());
                            let to = range.end.max(from).min(text.len());
                            let range = if text.is_char_boundary(from) && text.is_char_boundary(to)
                            {
                                from..to
                            } else {
                                text.len()..text.len()
                            };
                            let before = &text[..range.start];
                            let after = &text[range.end..];
                            let leading = if before.is_empty() || before.ends_with("\n\n") {
                                ""
                            } else if before.ends_with('\n') {
                                "\n"
                            } else {
                                "\n\n"
                            };
                            let trailing = if after.is_empty() || after.starts_with("\n\n") {
                                ""
                            } else if after.starts_with('\n') {
                                "\n"
                            } else {
                                "\n\n"
                            };
                            let insert = format!("{leading}{block}{trailing}");
                            let cursor = leading.len() + block.len();
                            this.body.update(cx, |input, cx| {
                                input.replace_bytes(range, &insert, cursor, cx)
                            });
                            this.edited(cx);
                            this.update_preview(cx);
                            if this.source {
                                window.focus(&this.body.focus_handle(cx), cx);
                            }
                        } else if let Some(note) = this.notes.iter_mut().find(|note| note.id == id)
                        {
                            this.revision = this.revision.wrapping_add(1);
                            // Finish a drop on its original note after navigation.
                            note.body = format!(
                                "{}{}{}",
                                note.body,
                                if note.body.is_empty() || note.body.ends_with("\n\n") {
                                    ""
                                } else {
                                    "\n\n"
                                },
                                block
                            );
                            let update = note.upsert(false);
                            if let Err(error) =
                                locked(&this.store).and_then(|store| store.upsert_note(&update))
                            {
                                this.error = Some(error);
                            }
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_picker(&mut self, picker: Picker, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker == Some(picker) {
            self.picker = None;
        } else {
            self.picker = Some(picker);
            self.project_open = false;
            self.language_query
                .update(cx, |input, cx| input.set_text(String::new(), cx));
            window.focus(&self.language_query.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// What choosing a language does: start a note of that type, or retype the open one.
    fn pick_language(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        match self.picker {
            Some(Picker::New) => self.create(id, window, cx),
            Some(Picker::Change) => self.set_language(id, cx),
            None => {}
        }
    }

    fn set_language(&mut self, id: &str, cx: &mut Context<Self>) {
        self.picker = None;
        let Some(note) = self.selected.clone() else {
            return;
        };
        let id = notes::language(id).id;
        if self.language_of(&note).id == id {
            cx.notify();
            return;
        }
        match locked(&self.store).and_then(|store| store.set_note_language(&note, id)) {
            Ok(()) => {
                if id == notes::DEFAULT_LANGUAGE {
                    self.languages.remove(&note);
                } else {
                    self.languages.insert(note.clone(), id.to_owned());
                }
                self.source = id != notes::DEFAULT_LANGUAGE
                    || self.modes.get(&note).copied().unwrap_or(false);
                self.body.update(cx, |input, cx| {
                    input.set_editor_analysis(crate::editor_analysis::Analysis::default(), cx)
                });
                self.highlight_source(cx);
                self.update_preview(cx);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    /// The small arrow beside "+": a note of another type than Markdown.
    fn new_type_button(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .relative()
            .child(self.new_anchor.probe())
            .child(
                icon_button("create-note-typed", "New note of a type…", "chevron-down")
                    .debug_selector(|| "create-note-typed".into())
                    .when(self.picker == Some(Picker::New), |d| d.bg(content(0.08)))
                    .on_click(
                        cx.listener(|this, _, window, cx| {
                            this.open_picker(Picker::New, window, cx)
                        }),
                    ),
            )
            .into_any_element()
    }

    /// The filterable list of note types, as the IDE's "New Scratch File" is.
    fn language_picker(&self, picker: Picker, cx: &mut Context<Self>) -> AnyElement {
        let query = self.language_query.read(cx).content.to_string();
        let current = self.current_language().id;
        let weak = cx.weak_entity();
        let anchor = match picker {
            Picker::New => self.new_anchor.clone(),
            Picker::Change => self.change_anchor.clone(),
        };
        popover("note-language-picker", Anchor::Slot(anchor))
            .width(260.)
            .max_height(380.)
            .on_dismiss(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.picker = None;
                    cx.notify();
                });
            })
            .child(
                div()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .px_2()
                            .text_size(px(11.))
                            .text_color(content(0.45))
                            .child(if picker == Picker::New {
                                "New note"
                            } else {
                                "Note type"
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .child(icon("search", 12.))
                            .child(div().flex_1().min_w_0().child(self.language_query.clone())),
                    )
                    .child(
                        div()
                            .id("note-language-results")
                            .min_h_0()
                            .max_h(px(300.))
                            .overflow_y_scroll()
                            .children(notes::matching_languages(&query).into_iter().map(
                                |language| {
                                    let id = language.id;
                                    let selected = picker == Picker::Change && id == current;
                                    div()
                                        .id(SharedString::from(format!("note-language-{id}")))
                                        .debug_selector(move || format!("note-language-{id}"))
                                        .role(Role::Button)
                                        .aria_label(language.label)
                                        .aria_selected(selected)
                                        .h(px(30.))
                                        .px_2()
                                        .rounded_md()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_size(px(13.))
                                        .cursor_pointer()
                                        .when(selected, |d| d.bg(content(0.1)))
                                        .hover(|style| style.bg(content(0.08)))
                                        .child(crate::activity::file_icon(
                                            &format!("note.{}", language.extension),
                                            16.,
                                        ))
                                        .child(div().flex_1().child(language.label))
                                        .when(language.id == notes::DEFAULT_LANGUAGE, |d| {
                                            d.child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(content(0.4))
                                                    .child("default"),
                                            )
                                        })
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.pick_language(id, window, cx)
                                        }))
                                },
                            )),
                    ),
            )
            .into_any_element()
    }

    fn project_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.project_query.read(cx).content.to_lowercase();
        let mut projects = self.projects.clone();
        if notes::is_project(&self.cwd) {
            projects.insert(0, self.cwd.clone());
        }
        for note in &self.notes {
            if let Some(path) = note
                .source_cwd
                .as_ref()
                .filter(|path| notes::is_project(path))
            {
                projects.push(path.clone());
            }
        }
        let mut seen = std::collections::HashSet::new();
        projects.retain(|path| seen.insert(path.clone()) && path.to_lowercase().contains(&query));
        let weak = cx.weak_entity();
        popover(
            "note-project-picker",
            Anchor::Slot(self.project_anchor.clone()),
        )
        .width(300.)
        .max_height(340.)
        .on_dismiss(move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.project_open = false;
                cx.notify();
            });
        })
        .child(
            div()
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().px_2().py_1().child(self.project_query.clone()))
                .child(
                    div()
                        .id("note-no-project")
                        .role(Role::Button)
                        .aria_label("No project")
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .text_size(px(12.))
                        .hover(|style| style.bg(content(0.08)))
                        .child("No project")
                        .on_click(cx.listener(|this, _, _, cx| this.move_project("~".into(), cx))),
                )
                .child(
                    div()
                        .id("note-project-results")
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(projects.into_iter().enumerate().map(|(ix, path)| {
                            let name = project_name(&path);
                            div()
                                .id(("note-project", ix))
                                .role(Role::Button)
                                .aria_label(path.clone())
                                .px_2()
                                .py_1p5()
                                .rounded_md()
                                .text_size(px(12.))
                                .hover(|style| style.bg(content(0.08)))
                                .child(name)
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(10.))
                                        .text_color(content(0.4))
                                        .child(path.clone()),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.move_project(path.clone(), cx)
                                }))
                        })),
                ),
        )
        .into_any_element()
    }

    fn detail(&mut self, note: Note, cx: &mut Context<Self>) -> AnyElement {
        let body = note.body.clone();
        let source = note.source_session_id.clone();
        let can_add = !body.trim().is_empty();
        let project = div()
            .relative()
            .id("note-move-project")
            .role(Role::Button)
            .aria_label("Move note to project")
            .rounded_md()
            .px_1p5()
            .py_1()
            .hover(|style| style.bg(content(0.08)))
            .child(self.project_anchor.probe())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        note.source_cwd
                            .as_deref()
                            .map(|path| self.project_mark(path, 12.))
                            .unwrap_or_else(|| icon("folder", 12.).into_any_element()),
                    )
                    .child(note.project_name().unwrap_or_else(|| "No project".into()))
                    .child(icon("chevron-down", 11.)),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.project_open = !this.project_open;
                this.picker = None;
                if this.project_open {
                    this.project_query
                        .update(cx, |input, cx| input.set_text(String::new(), cx));
                    window.focus(&this.project_query.focus_handle(cx), cx);
                }
                cx.notify();
            }));
        let language = self.current_language();
        let is_markdown = language.id == notes::DEFAULT_LANGUAGE;
        let language_chip = div()
            .relative()
            .id("note-language-chip")
            .debug_selector(|| "note-language-chip".into())
            .role(Role::Button)
            .aria_label("Change note type")
            .rounded_md()
            .px_1p5()
            .py_1()
            .cursor_pointer()
            .hover(|style| style.bg(content(0.08)))
            .when(self.picker == Some(Picker::Change), |d| d.bg(content(0.08)))
            .child(self.change_anchor.probe())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(crate::activity::file_icon(
                        &format!("note.{}", language.extension),
                        14.,
                    ))
                    .child(if is_markdown { "Note" } else { language.label })
                    .child(icon("chevron-down", 11.)),
            )
            .on_click(
                cx.listener(|this, _, window, cx| this.open_picker(Picker::Change, window, cx)),
            );
        let tags = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1p5()
            .text_size(px(11.))
            .child(div().mr(px(2.)).text_color(content(0.45)).child("Tags"))
            .children(note.tags.iter().enumerate().map(|(ix, tag)| {
                let tag = tag.clone();
                div()
                    .h(px(24.))
                    .max_w(px(192.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded_md()
                    .bg(content(0.08))
                    .pl_2()
                    .pr_1()
                    .text_color(content(0.7))
                    .child(div().truncate().child(format!("#{tag}")))
                    .child(
                        div()
                            .id(("remove-note-tag", ix))
                            .role(Role::Button)
                            .aria_label(format!("Remove #{tag}"))
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .hover(|style| style.bg(content(0.1)))
                            .child(icon("close", 10.))
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_tag(&tag, cx))),
                    )
            }))
            .when(note.tags.len() < notes::MAX_TAGS, |d| {
                d.child(div().min_w(px(80.)).flex_1().child(self.tag.clone()))
            });
        // Solid ink on the inverted fill: dark text on a light button in dark
        // mode, light text on a dark one in light mode.
        let mut ink = shell_fill();
        ink.a = 1.;
        let danger: gpui::Hsla = gpui::rgb(0xf87171).into();
        let actions = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap_2()
            .child(
                div()
                    .id("note-add-to-chat")
                    .role(Role::Button)
                    .aria_label("Add to chat")
                    .h(px(28.))
                    .px_3()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .bg(content(1.))
                    .text_color(ink)
                    .map(|d| {
                        if can_add {
                            d.cursor_pointer()
                                .hover(move |s| s.bg(content(0.85)).text_color(ink))
                        } else {
                            d.opacity(0.4)
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if can_add {
                            this.flush(cx);
                            if !this.dirty
                                && let Some(mut note) = this.draft(cx)
                            {
                                let language = this.current_language();
                                if language.id != notes::DEFAULT_LANGUAGE {
                                    // A code note goes in as a code block, not as prose.
                                    note.body = format!(
                                        "```{}\n{}\n```",
                                        language.extension,
                                        note.body.trim_end()
                                    );
                                }
                                cx.emit(Event::AddToChat(note));
                            }
                        }
                    }))
                    .child("Add to chat"),
            )
            .child(
                div()
                    .id("delete-note")
                    .role(Role::Button)
                    .aria_label("Delete note")
                    .h(px(28.))
                    .px_3()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(danger)
                    .cursor_pointer()
                    .hover(|style| style.bg(content(0.08)))
                    .child(icon("trash", 14.).text_color(danger))
                    .child("Delete")
                    .on_click(cx.listener(|this, _, _, cx| this.delete(cx))),
            )
            .when_some(source, |d, source| {
                d.child(
                    div()
                        .id("note-source-session")
                        .role(Role::Button)
                        .aria_label("Open source conversation")
                        .rounded_md()
                        .px_2()
                        .py_1()
                        .text_size(px(12.))
                        .text_color(content(0.5))
                        .hover(|style| style.bg(content(0.08)))
                        .child("Source conversation")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.flush(cx);
                            if this.dirty {
                                return;
                            }
                            if let Some(id) = locked(&this.store)
                                .ok()
                                .and_then(|store| store.resolve_existing_session_id(&source))
                            {
                                cx.emit(Event::OpenSource(id));
                            } else {
                                this.error = Some("Source conversation no longer exists.".into());
                                cx.notify();
                            }
                        })),
                )
            })
            .child(
                icon_button("copy-note", "Copy note", "response-copy").on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(body.clone()))
                }),
            );
        let tabs = div()
            .id("note-sections")
            .role(Role::TabList)
            .aria_label("Note sections")
            .h(px(36.))
            .flex()
            .items_stretch()
            .gap_4()
            .border_b_1()
            .border_color(content(0.07))
            .children(
                [(false, "Preview"), (true, "Source")].map(|(source, label)| {
                    let tab_id = if source {
                        "note-source"
                    } else {
                        "note-preview"
                    };
                    div()
                        .id(tab_id)
                        .debug_selector(move || tab_id.into())
                        .role(Role::Tab)
                        .aria_label(label)
                        .aria_selected(self.source == source)
                        .flex()
                        .items_center()
                        .text_size(px(12.))
                        .text_color(content(if self.source == source { 1. } else { 0.5 }))
                        .when(self.source == source, |d| {
                            d.border_b_2().border_color(content(1.))
                        })
                        .hover(|style| style.text_color(content(1.)))
                        .child(label)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.source = source;
                            if let Some(id) = &this.selected {
                                this.modes.insert(id.clone(), source);
                            }
                            if source {
                                window.focus(&this.body.focus_handle(cx), cx);
                            } else {
                                this.flush(cx);
                                this.update_preview(cx);
                            }
                            cx.notify();
                        }))
                }),
            );
        let editor = div()
            .id("note-image-drop-zone")
            .relative()
            .min_h(px(448.))
            .rounded_lg()
            .border_1()
            .border_color(if self.image_drag {
                accent_color()
            } else {
                content(0.)
            })
            .on_drag_move::<ExternalPaths>(cx.listener(
                |this, event: &gpui::DragMoveEvent<ExternalPaths>, _, cx| {
                    this.image_drag = event.bounds.contains(&event.event.position);
                    cx.notify();
                },
            ))
            .on_drag_move::<DraggedFile>(cx.listener(
                |this, event: &gpui::DragMoveEvent<DraggedFile>, _, cx| {
                    this.image_drag = event.bounds.contains(&event.event.position);
                    cx.notify();
                },
            ))
            .on_file_drop_exit(cx.listener(|this, _, _, cx| {
                this.image_drag = false;
                cx.notify();
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.add_images(paths.paths().to_vec(), cx)
            }))
            .on_drop(cx.listener(|this, file: &DraggedFile, _, cx| {
                this.add_images(vec![file.0.clone()], cx)
            }))
            .when(self.source, |d| {
                d.h(px((self.body.read(cx).editor_line_count() as f32 * 20.
                    + 16.)
                    .clamp(448., 2000.)))
                    .child(self.body.clone())
            })
            .when(!self.source, |d| {
                d.when_some(self.document.clone(), |d, preview| d.child(preview))
            })
            .when(self.image_drag || self.image_busy, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded_lg()
                        .bg(pane_fill().opacity(0.9))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(12.))
                        .text_color(content(0.7))
                        .child(if self.image_busy {
                            "Adding images…"
                        } else {
                            "Drop images here"
                        }),
                )
            });
        div()
            .id("note-detail")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .gap(px(20.))
                    .w_full()
                    .max_w(px(1024.))
                    .mx_auto()
                    .p(px(32.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(px(12.))
                                    .text_color(content(0.5))
                                    .child(language_chip)
                                    .child(div().min_w_0().truncate().child(note.slug.clone()))
                                    .child(project),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(self.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(content(0.5))
                                    .child(format!("Updated {}", relative_time(note.updated_at))),
                            )
                            .child(tags)
                            .child(actions),
                    )
                    .children(is_markdown.then_some(tabs))
                    .child(editor),
            )
            .into_any_element()
    }
}

impl gpui::Render for Notes {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.blurs_bound {
            self.blurs_bound = true;
            self._subscriptions.push(cx.on_blur(
                &self.title.focus_handle(cx),
                window,
                |this, _, cx| this.flush(cx),
            ));
            self._subscriptions.push(cx.on_blur(
                &self.tag.focus_handle(cx),
                window,
                |this, _, cx| this.add_tag(cx),
            ));
        }
        if !cx.has_active_drag() {
            self.image_drag = false;
        }
        let query = self.query.read(cx).content.to_string();
        let visible: Vec<_> = self
            .notes
            .iter()
            .filter(|note| note.matches(&query) && self.shown(note))
            .cloned()
            .collect();
        let scope_toggle = self.scope.is_some().then(|| {
            let all = self.all_projects;
            div()
                .id("notes-all-projects")
                .debug_selector(|| "notes-all-projects".into())
                .role(Role::Switch)
                .aria_label("Show every project's notes")
                .aria_selected(all)
                .flex_shrink_0()
                .px_1p5()
                .py(px(2.))
                .rounded_sm()
                .text_size(px(11.))
                .text_color(content(if all { 0.9 } else { 0.5 }))
                .when(all, |d| d.bg(content(0.1)))
                .hover(|style| style.bg(content(0.08)))
                .cursor_pointer()
                .child("All")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.all_projects = !this.all_projects;
                    this.reselect_shown(cx);
                }))
        });
        let list = div().relative().w(px(self.list_width)).h_full().flex_shrink_0().min_h_0().flex().flex_col().border_r_1().border_color(content(0.07))
            .child(div().h(px(36.)).flex_shrink_0().px_2().flex().items_center().gap_1().border_b_1().border_color(content(0.07)).child(icon("search", 12.))
                .child(div().flex_1().min_w_0().child(self.query.clone()))
                .children(scope_toggle)
                .child(icon_button("create-note", "New note", "plus").debug_selector(|| "create-note".into()).on_click(cx.listener(|this, _, window, cx| this.create(notes::DEFAULT_LANGUAGE, window, cx))))
                .child(self.new_type_button(cx)))
            .child(div().id("notes-list").role(Role::ListBox).aria_label("Saved notes").flex_1().min_h_0().overflow_y_scroll().p(px(6.))
                .when(visible.is_empty(), |d| d.child(div().px_1p5().py_2().text_size(px(12.)).text_color(content(0.5)).child(if self.loading { "Loading notes…" } else if !query.trim().is_empty() { "No matching notes" } else if self.scope.is_some() && !self.all_projects && !self.notes.is_empty() { "No notes in this project yet. Create one here, or show every project's notes with All." } else { "No notes yet. Save a turn from the transcript, or create one here." })))
                .children(visible.iter().enumerate().map(|(ix, note)| {
                    let id = note.id.clone();
                    let active = self.selected.as_ref() == Some(&id);
                    let preview = note.preview();
                    div().id(("saved-note", ix)).role(Role::Button).aria_label(note.title.clone()).aria_selected(active).mb(px(2.)).px(px(10.)).py_2().rounded_md().text_color(content(0.8)).hover(|style| style.bg(content(0.05))).when(active, |d| d.bg(content(0.1)))
                        .child(div().flex().items_center().gap_2().text_size(px(11.)).text_color(content(0.45))
                            .child(div().flex_1().truncate().when_some(note.project_name(), |d, name| d.child(div().flex().items_center().gap_1p5().child(self.project_mark(note.source_cwd.as_deref().unwrap_or(""), 12.)).child(name))))
                            .child(relative_time(note.updated_at)))
                        .child(div().mt_1().flex().items_center().gap_1p5().child(crate::activity::file_icon(&format!("note.{}", self.language_of(&note.id).extension), 14.)).child(div().flex_1().min_w_0().truncate().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(content(1.)).child(note.title.clone())))
                        .when(!preview.is_empty(), |d| d.child(div().mt_1().truncate().text_size(px(12.)).text_color(content(0.45)).child(preview)))
                        .when(!note.tags.is_empty(), |d| d.child(div().mt_1p5().flex().items_center().gap_1().overflow_hidden()
                            .children(note.tags.iter().take(3).map(|tag| div().max_w(px(96.)).truncate().rounded_sm().bg(content(0.08)).px_1p5().py(px(2.)).text_size(px(10.)).text_color(content(0.55)).child(format!("#{tag}"))))
                            .when(note.tags.len() > 3, |d| d.child(div().text_size(px(10.)).text_color(content(0.4)).child(format!("+{}", note.tags.len() - 3))))))
                        .on_click(cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)))
                })))
            .child(div().id("resize-notes-list").role(Role::Splitter).aria_label("Resize notes list").absolute().top_0().bottom_0().right(px(-3.)).w(px(6.)).cursor(CursorStyle::ResizeLeftRight).hover(|style| style.bg(content(0.1)))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| { if event.click_count == 2 { this.list_width = 280.; } else { this.resize = Some((event.position.x.as_f32(), this.list_width)); } cx.notify(); })));
        let detail = if let Some(note) = self.draft(cx) {
            self.detail(note, cx)
        } else {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .text_size(px(13.))
                .text_color(content(0.45))
                .child(icon("note", 24.))
                .child("Select a note")
                .into_any_element()
        };
        div()
            .id("notes-view")
            .role(Role::Region)
            .aria_label("Notes")
            .size_full()
            .flex()
            .flex_col()
            .text_color(content(1.))
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if key == "enter"
                    && this.picker.is_some()
                    && this.language_query.focus_handle(cx).is_focused(window)
                {
                    let query = this.language_query.read(cx).content.to_string();
                    if let Some(language) = notes::matching_languages(&query).first() {
                        this.pick_language(language.id, window, cx);
                    }
                    cx.stop_propagation();
                } else if key == "enter"
                    && this.title.focus_handle(cx).is_focused(window)
                    && !this.title.read(cx).is_composing()
                {
                    this.flush(cx);
                    window.focus(&this.query.focus_handle(cx), cx);
                    cx.stop_propagation();
                } else if key == "escape" {
                    if this.picker.is_some() {
                        this.picker = None;
                    } else if this.project_open {
                        this.project_open = false;
                    } else {
                        this.flush(cx);
                        if !this.dirty {
                            cx.emit(Event::Close);
                        }
                    }
                    cx.stop_propagation();
                    cx.notify();
                } else if (event.keystroke.modifiers.platform || event.keystroke.modifiers.control)
                    && key == "s"
                {
                    this.flush(cx);
                    cx.stop_propagation();
                } else if (event.keystroke.modifiers.platform || event.keystroke.modifiers.control)
                    && key == "n"
                {
                    this.create(notes::DEFAULT_LANGUAGE, window, cx);
                    cx.stop_propagation();
                } else if key == "backspace"
                    && this.tag.focus_handle(cx).is_focused(window)
                    && this.tag.read(cx).content.is_empty()
                    && let Some(tag) = this.draft(cx).and_then(|note| note.tags.last().cloned())
                {
                    this.remove_tag(&tag, cx);
                }
            }))
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                    if let Some((start, width)) = this.resize {
                        this.list_width = (width + event.position.x.as_f32() - start).clamp(
                            240.,
                            420_f32
                                .min(window.viewport_size().width.as_f32() * 0.5)
                                .max(240.),
                        );
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.resize = None),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.resize = None),
            )
            .child(div().flex_1().min_h_0().flex().child(list).child(detail))
            .when_some(self.error.clone(), |d, error| {
                d.child(
                    div()
                        .id("note-error")
                        .role(Role::Alert)
                        .flex_shrink_0()
                        .px_4()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(12.))
                        .text_color(gpui::rgb(0xf87171))
                        .child(div().flex_1().child(error))
                        .child(
                            div()
                                .id("retry-note-save")
                                .role(Role::Button)
                                .aria_label("Retry saving note")
                                .underline()
                                .child("Retry")
                                .on_click(cx.listener(|this, _, _, cx| this.flush(cx))),
                        ),
                )
            })
            .when(self.project_open, |d| d.child(self.project_picker(cx)))
            .when_some(self.picker, |d, picker| {
                d.child(self.language_picker(picker, cx))
            })
    }
}

fn project_name(path: &str) -> String {
    monocode_core::files::leaf_name(path)
}
fn relative_time(at: i64) -> String {
    let seconds = (monocode_core::time::now_ms() as i64 - at).max(0) / 1000;
    match seconds {
        0..60 => "now".into(),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        86400..604800 => format!("{}d", seconds / 86400),
        _ => chrono::DateTime::from_timestamp_millis(at)
            .map(|date| date.format("%b %-d").to_string())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use monocode_core::store::Store;
    use std::sync::{Arc, Mutex};

    fn open(cx: &mut TestAppContext) -> gpui::WindowHandle<Notes> {
        let store: Database = Arc::new(Mutex::new(
            Store::open(std::path::Path::new(":memory:")).unwrap(),
        ));
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| {
                    let mut notes = Notes::new(store, cx);
                    notes.set_context("/work".into(), vec!["/work".into()], cx);
                    notes
                })
            })
            .unwrap()
        });
        cx.refresh().unwrap();
        cx.run_until_parked();
        window
    }

    fn filter(window: gpui::WindowHandle<Notes>, visual: &mut VisualTestContext, text: &str) {
        let text = text.to_owned();
        window
            .update(visual, |notes, _, cx| {
                notes
                    .language_query
                    .update(cx, |input, cx| input.set_text(text, cx));
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
    }

    #[gpui::test]
    fn the_plus_button_creates_a_markdown_note_even_in_an_empty_list(cx: &mut TestAppContext) {
        cx.update(input::bind);
        let window = open(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        for expected in 1..=2 {
            let plus = visual.debug_bounds("create-note").expect("the + button");
            visual.simulate_click(plus.center(), Default::default());
            visual.run_until_parked();
            window
                .update(visual, |notes, _, _| {
                    assert_eq!(notes.notes.len(), expected, "error: {:?}", notes.error);
                    let id = notes.selected.clone().unwrap();
                    assert_eq!(notes.language_of(&id).id, notes::DEFAULT_LANGUAGE);
                })
                .unwrap();
        }
    }

    #[gpui::test]
    fn the_new_note_arrow_offers_types_and_the_note_takes_the_chosen_one(cx: &mut TestAppContext) {
        cx.update(|cx| {
            input::bind(cx);
            crate::popover::bind(cx);
        });
        let window = open(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        // A plain "+" is a normal Markdown note.
        window
            .update(visual, |notes, window, cx| {
                notes.create(notes::DEFAULT_LANGUAGE, window, cx)
            })
            .unwrap();
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("note-preview").is_some(),
            "a Markdown note has Preview and Source"
        );
        // The arrow lists the types; choosing one starts a note of that type.
        let arrow = visual.debug_bounds("create-note-typed").unwrap();
        visual.simulate_click(arrow.center(), Default::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("note-language-markdown").is_some());
        // The list scrolls, so narrow it to reach the row, as typing would.
        filter(window, visual, "pyth");
        let python = visual
            .debug_bounds("note-language-python")
            .expect("the picker lists languages");
        visual.simulate_click(python.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |notes, _, cx| {
                assert_eq!(notes.notes.len(), 2, "error: {:?}", notes.error);
                let id = notes.selected.clone().unwrap();
                assert_eq!(notes.language_of(&id).id, "python");
                assert_eq!(notes.picker, None, "the picker closes");
                assert!(notes.source, "code types are always source");
                assert_eq!(
                    locked(&notes.store).unwrap().note_languages().unwrap()[&id],
                    "python"
                );
                let _ = cx;
            })
            .unwrap();
        assert!(
            visual.debug_bounds("note-preview").is_none(),
            "no rendered view for a code note"
        );
        // The header names the type and retypes the note.
        let chip = visual.debug_bounds("note-language-chip").unwrap();
        visual.simulate_click(chip.center(), Default::default());
        visual.run_until_parked();
        filter(window, visual, "sql");
        let sql = visual.debug_bounds("note-language-sql").unwrap();
        visual.simulate_click(sql.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |notes, _, _| {
                let id = notes.selected.clone().unwrap();
                assert_eq!(notes.language_of(&id).id, "sql");
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_language_filter_narrows_the_list_and_enter_picks_the_first(cx: &mut TestAppContext) {
        cx.update(|cx| {
            input::bind(cx);
            crate::popover::bind(cx);
        });
        let window = open(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let arrow = visual.debug_bounds("create-note-typed").unwrap();
        visual.simulate_click(arrow.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |notes, _, cx| {
                notes
                    .language_query
                    .update(cx, |input, cx| input.set_text("rus".into(), cx));
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        assert!(visual.debug_bounds("note-language-rust").is_some());
        assert!(visual.debug_bounds("note-language-python").is_none());
    }

    #[gpui::test]
    fn a_project_scope_lists_its_notes_and_unfiled_ones_and_all_shows_every_note(
        cx: &mut TestAppContext,
    ) {
        cx.update(input::bind);
        let window = open(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let (a, b) = ("/work/a", "/work/b");
        let ids = window
            .update(visual, |notes, _, _| {
                let store = locked(&notes.store).unwrap();
                [Some(a), Some(b), None]
                    .map(|cwd| store.create_note(cwd.map(Into::into)).unwrap().id)
            })
            .unwrap();
        let shown = |visual: &mut VisualTestContext| {
            window
                .update(visual, |notes, _, _| {
                    let mut shown: Vec<_> = (notes.notes.iter())
                        .filter(|note| notes.shown(note))
                        .map(|note| note.id.clone())
                        .collect();
                    shown.sort();
                    (shown, notes.selected.clone())
                })
                .unwrap()
        };
        let sorted = |mut ids: Vec<String>| {
            ids.sort();
            ids
        };
        window
            .update(visual, |notes, _, cx| {
                notes.set_context(b.into(), vec![a.into(), b.into()], cx);
                notes.set_scope(Some(b.into()), cx);
                notes.refresh(cx);
            })
            .unwrap();
        visual.run_until_parked();
        let (list, selected) = shown(visual);
        assert_eq!(list, sorted(vec![ids[1].clone(), ids[2].clone()]));
        assert!(selected.is_some_and(|id| list.contains(&id)));
        // A's own note was selected nowhere; moving to A selects one of its.
        window
            .update(visual, |notes, _, cx| {
                notes.load_selection(Some(ids[1].clone()), cx);
                notes.set_scope(Some(a.into()), cx);
            })
            .unwrap();
        visual.run_until_parked();
        let (list, selected) = shown(visual);
        assert_eq!(list, sorted(vec![ids[0].clone(), ids[2].clone()]));
        assert!(selected.is_some_and(|id| list.contains(&id)));
        visual.refresh().unwrap();
        visual.run_until_parked();
        let all = visual
            .debug_bounds("notes-all-projects")
            .expect("the All switch");
        visual.simulate_click(all.center(), Default::default());
        visual.run_until_parked();
        assert_eq!(shown(visual).0, sorted(ids.to_vec()));
        // Without a scope every note shows and the switch goes away.
        window
            .update(visual, |notes, _, cx| notes.set_scope(None, cx))
            .unwrap();
        visual.refresh().unwrap();
        visual.run_until_parked();
        assert_eq!(shown(visual).0, sorted(ids.to_vec()));
        assert!(visual.debug_bounds("notes-all-projects").is_none());
    }
}
