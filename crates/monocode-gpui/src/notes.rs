//! The desktop Notes split view, backed by the same notes and image assets.
mod kanban;
mod monday;
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
use monday::DraggedWorkItem;
use monocode_core::{
    integrations::WorkItem,
    kanban::{Board, Column, KANBAN, TaskSource},
    monday_note::{MONDAY, MondayNote},
    notes::{self, Note, NoteLanguage},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

/// The name a new monday.com note has until a board is chosen.
const MONDAY_TITLE: &str = "monday.com board";

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
    /// Settings → Integrations, to connect monday.com.
    OpenIntegrations,
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
    /// The open note's board, when it is a kanban note.
    kanban: Entity<kanban::KanbanBoard>,
    /// The open note's board, when it is a monday.com note.
    monday: Entity<monday::MondayBoard>,
    /// Where integrations keep what they fetched.
    data_dir: Option<PathBuf>,
    /// A monday card is being dragged; kanban notes in the list take it.
    item_drag: bool,
    /// The kanban note a dragged monday card is over.
    item_drop: Option<String>,
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
        let board = cx.new(kanban::KanbanBoard::new);
        let monday = cx.new(monday::MondayBoard::new);
        let data_dir = locked(&store)
            .ok()
            .and_then(|store| store.data_dir().map(std::path::Path::to_path_buf));
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
            // A board edit becomes the note's body, saved like any typing.
            cx.subscribe(&board, |this, board, _: &kanban::Changed, cx| {
                let json = board.read(cx).board().to_json();
                this.body.update(cx, |input, cx| input.set_text(json, cx));
            }),
            cx.subscribe(
                &monday,
                |this, monday, event: &monday::Event, cx| match event {
                    // A late fetch never writes into another note opened since.
                    monday::Event::Changed if !this.selected_is_monday() => {}
                    monday::Event::Changed => {
                        let note = monday.read(cx).note().clone();
                        this.body
                            .update(cx, |input, cx| input.set_text(note.to_json(), cx));
                        // A board under the default name takes the board's.
                        let title = this.title.read(cx).content.to_string();
                        if !note.board_name.is_empty()
                            && (title.trim().is_empty() || title == MONDAY_TITLE)
                        {
                            this.title.update(cx, |input, cx| {
                                input.set_text(note.board_name.clone(), cx)
                            });
                        }
                    }
                    monday::Event::AddToKanban { kanban, item } => {
                        this.add_to_kanban(&kanban.clone(), &item.clone(), cx)
                    }
                    monday::Event::OpenIntegrations => cx.emit(Event::OpenIntegrations),
                },
            ),
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
            kanban: board,
            monday,
            data_dir,
            item_drag: false,
            item_drop: None,
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

    fn is_kanban(&self, id: &str) -> bool {
        self.languages
            .get(id)
            .is_some_and(|language| language == KANBAN)
    }

    fn selected_is_kanban(&self) -> bool {
        self.selected
            .as_deref()
            .is_some_and(|id| self.is_kanban(id))
    }

    fn is_monday(&self, id: &str) -> bool {
        self.languages
            .get(id)
            .is_some_and(|language| language == MONDAY)
    }

    fn selected_is_monday(&self) -> bool {
        self.selected
            .as_deref()
            .is_some_and(|id| self.is_monday(id))
    }

    /// The kanban notes the list shows, as a card's menu offers them.
    fn kanban_targets(&self) -> Vec<(String, String)> {
        (self.notes.iter())
            .filter(|note| self.is_kanban(&note.id) && self.shown(note))
            .map(|note| (note.id.clone(), note.title.clone()))
            .collect()
    }

    /// Puts a monday item in a kanban note's Backlog, once. The open board
    /// takes it like any edit; another is read, changed and saved.
    fn add_to_kanban(&mut self, id: &str, item: &WorkItem, cx: &mut Context<Self>) {
        if !self.is_kanban(id) {
            return;
        }
        if self.selected.as_deref() == Some(id) {
            self.kanban
                .update(cx, |board, cx| board.add_item(item, Column::Backlog, cx));
            return;
        }
        let Some(note) = self.notes.iter().find(|note| note.id == id) else {
            return;
        };
        let mut board = Board::parse(&note.body);
        let before = board.tasks.len();
        board.add_from_source(
            &item.title,
            TaskSource::from_item(item),
            Column::Backlog,
            monocode_core::time::now_ms() as i64,
        );
        if board.tasks.len() == before {
            return;
        }
        let mut update = note.upsert(false);
        update.body = board.to_json();
        match locked(&self.store).and_then(|store| store.upsert_note(&update)) {
            Ok(saved) => {
                self.revision = self.revision.wrapping_add(1);
                self.notes.retain(|note| note.id != saved.id);
                self.notes.push(saved);
                self.notes
                    .sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
            }
            Err(error) => self.error = Some(format!("Could not save note: {error}")),
        }
        cx.notify();
    }

    fn language_of(&self, id: &str) -> &'static NoteLanguage {
        notes::language(self.languages.get(id).map_or("", String::as_str))
    }

    /// Colours the editor for the note's language: Markdown headings here, any
    /// other type through the same analysis the file editor uses.
    fn highlight_source(&mut self, cx: &mut Context<Self>) {
        if self.selected_is_kanban() || self.selected_is_monday() {
            // A board's body is its JSON, never shown as text.
            self.analysis = None;
            return;
        }
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

    #[cfg(test)]
    fn draft_title(&self) -> String {
        (self.notes.iter())
            .find(|note| Some(&note.id) == self.selected.as_ref())
            .map(|note| note.title.clone())
            .unwrap_or_default()
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
                if self.is_kanban(&draft.id) {
                    "Untitled board".into()
                } else if self.is_monday(&draft.id) {
                    MONDAY_TITLE.into()
                } else {
                    notes::title(&draft.body)
                }
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
        if let Some(note) = note.as_ref().filter(|note| self.is_kanban(&note.id)) {
            let body = note.body.clone();
            self.kanban.update(cx, |board, cx| board.load(&body, cx));
        }
        if let Some(note) = note.as_ref().filter(|note| self.is_monday(&note.id)) {
            let (body, dir) = (note.body.clone(), self.data_dir.clone());
            self.monday
                .update(cx, |board, cx| board.load(&body, dir, cx));
        }
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
        let language = notes::note_type(language);
        let result = locked(&self.store).and_then(|store| {
            let mut note = store.create_note(Some(self.cwd.clone()))?;
            store.set_note_language(&note.id, language)?;
            if language == KANBAN || language == MONDAY {
                // A board starts empty, under a name of its own.
                let mut upsert = note.upsert(false);
                if language == KANBAN {
                    upsert.title = "Untitled board".into();
                    upsert.body = Board::default().to_json();
                } else {
                    upsert.title = MONDAY_TITLE.into();
                    upsert.body = MondayNote::default().to_json();
                }
                note = store.upsert_note(&upsert)?;
            }
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
                if language == MONDAY {
                    let focus = self.monday.read(cx).search_focus(cx);
                    window.focus(&focus, cx);
                } else if language == KANBAN {
                    window.focus(&self.title.focus_handle(cx), cx);
                } else {
                    window.focus(&self.body.focus_handle(cx), cx);
                }
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
                    .children((picker == Picker::New).then(|| {
                        div().flex().flex_col().children(
                            notes::matching_board_types(&query)
                                .into_iter()
                                .map(|(id, label)| {
                                    div()
                                        .id(SharedString::from(format!("note-type-{id}")))
                                        .debug_selector(move || format!("note-type-{id}"))
                                        .role(Role::Button)
                                        .aria_label(label)
                                        .h(px(30.))
                                        .px_2()
                                        .rounded_md()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_size(px(13.))
                                        .cursor_pointer()
                                        .hover(|style| style.bg(content(0.08)))
                                        .child(
                                            icon(
                                                if id == KANBAN {
                                                    "kanban"
                                                } else {
                                                    "inbox-monday"
                                                },
                                                16.,
                                            )
                                            .text_color(content(0.7)),
                                        )
                                        .child(div().flex_1().child(label))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.create(id, window, cx)
                                        }))
                                }),
                        )
                    }))
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
        let is_kanban = self.is_kanban(&note.id);
        let is_monday = self.is_monday(&note.id);
        let monday_markdown = is_monday
            .then(|| self.monday.read(cx).to_markdown(&note.title))
            .flatten();
        // A board reads as Markdown checklists everywhere outside its view.
        let body = if is_kanban {
            Board::parse(&note.body).to_markdown(&note.title)
        } else if is_monday {
            monday_markdown.clone().unwrap_or_default()
        } else {
            note.body.clone()
        };
        let source = note.source_session_id.clone();
        let can_add = if is_kanban {
            !Board::parse(&note.body).tasks.is_empty()
        } else if is_monday {
            monday_markdown.is_some()
        } else {
            !body.trim().is_empty()
        };
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
            .child(div().flex().items_center().gap_1p5().map(|d| {
                if is_kanban {
                    // A board keeps its type: its body is no text to retype.
                    d.child(icon("kanban", 14.).text_color(content(0.7)))
                        .child("Kanban")
                } else if is_monday {
                    d.child(icon("inbox-monday", 14.).text_color(content(0.7)))
                        .child("monday.com")
                } else {
                    d.child(crate::activity::file_icon(
                        &format!("note.{}", language.extension),
                        14.,
                    ))
                    .child(if is_markdown { "Note" } else { language.label })
                    .child(icon("chevron-down", 11.))
                }
            }))
            .when(!is_kanban && !is_monday, |d| {
                d.on_click(
                    cx.listener(|this, _, window, cx| this.open_picker(Picker::Change, window, cx)),
                )
            });
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
                                if this.is_kanban(&note.id) {
                                    note.body = Board::parse(&note.body).to_markdown(&note.title);
                                } else if this.is_monday(&note.id) {
                                    note.body = (this.monday.read(cx).to_markdown(&note.title))
                                        .unwrap_or_default();
                                } else if language.id != notes::DEFAULT_LANGUAGE {
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
        let header = div()
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
            .child(actions);
        if is_kanban || is_monday {
            // The board takes the whole pane, its columns scrolling inside it.
            return div()
                .id("note-detail")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_shrink_0()
                        .px(px(32.))
                        .pt(px(28.))
                        .pb(px(18.))
                        .child(header),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .px(px(24.))
                        .pb(px(20.))
                        .flex()
                        .flex_col()
                        .map(|d| {
                            if is_monday {
                                d.child(self.monday.clone())
                            } else {
                                d.child(self.kanban.clone())
                            }
                        }),
                )
                .into_any_element();
        }
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
                    .child(header)
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
            self.item_drag = false;
            self.item_drop = None;
        }
        if self.selected_is_monday() {
            let kanbans = self.kanban_targets();
            self.monday
                .update(cx, |board, _| board.set_kanbans(kanbans));
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
                .child(icon_button("create-kanban", "New kanban board", "kanban").debug_selector(|| "create-kanban".into()).on_click(cx.listener(|this, _, window, cx| this.create(KANBAN, window, cx))))
                .child(self.new_type_button(cx)))
            .child(div().id("notes-list").role(Role::ListBox).aria_label("Saved notes").flex_1().min_h_0().overflow_y_scroll().p(px(6.))
                .when(visible.is_empty(), |d| d.child(div().px_1p5().py_2().text_size(px(12.)).text_color(content(0.5)).child(if self.loading { "Loading notes…" } else if !query.trim().is_empty() { "No matching notes" } else if self.scope.is_some() && !self.all_projects && !self.notes.is_empty() { "No notes in this project yet. Create one here, or show every project's notes with All." } else { "No notes yet. Save a turn from the transcript, or create one here." })))
                .children(visible.iter().enumerate().map(|(ix, note)| {
                    let id = note.id.clone();
                    let active = self.selected.as_ref() == Some(&id);
                    let kanban = self.is_kanban(&note.id);
                    let monday = self.is_monday(&note.id);
                    let preview = if kanban { Board::parse(&note.body).summary() } else if monday { MondayNote::parse(&note.body).summary() } else { note.preview() };
                    let drop_here = self.item_drop.as_ref() == Some(&id);
                    let (drag_id, drop_id) = (id.clone(), id.clone());
                    div().id(("saved-note", ix)).debug_selector({ let id = id.clone(); move || format!("saved-note-{id}") }).role(Role::Button).aria_label(note.title.clone()).aria_selected(active).mb(px(2.)).px(px(10.)).py_2().rounded_md().text_color(content(0.8)).hover(|style| style.bg(content(0.05))).when(active, |d| d.bg(content(0.1)))
                        .child(div().flex().items_center().gap_2().text_size(px(11.)).text_color(content(0.45))
                            .child(div().flex_1().truncate().when_some(note.project_name(), |d, name| d.child(div().flex().items_center().gap_1p5().child(self.project_mark(note.source_cwd.as_deref().unwrap_or(""), 12.)).child(name))))
                            .child(relative_time(note.updated_at)))
                        .child(div().mt_1().flex().items_center().gap_1p5().child(if kanban { icon("kanban", 14.).text_color(content(0.7)).into_any_element() } else if monday { icon("inbox-monday", 14.).text_color(content(0.7)).into_any_element() } else { crate::activity::file_icon(&format!("note.{}", self.language_of(&note.id).extension), 14.).into_any_element() }).child(div().flex_1().min_w_0().truncate().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(content(1.)).child(note.title.clone())))
                        .when(!preview.is_empty(), |d| d.child(div().mt_1().truncate().text_size(px(12.)).text_color(content(0.45)).child(preview)))
                        .when(!note.tags.is_empty(), |d| d.child(div().mt_1p5().flex().items_center().gap_1().overflow_hidden()
                            .children(note.tags.iter().take(3).map(|tag| div().max_w(px(96.)).truncate().rounded_sm().bg(content(0.08)).px_1p5().py(px(2.)).text_size(px(10.)).text_color(content(0.55)).child(format!("#{tag}"))))
                            .when(note.tags.len() > 3, |d| d.child(div().text_size(px(10.)).text_color(content(0.4)).child(format!("+{}", note.tags.len() - 3))))))
                        // A monday card dropped here lands in this board's Backlog.
                        .when(kanban, |d| d
                            .border_1().border_color(if drop_here { accent_color() } else if self.item_drag { content(0.18) } else { content(0.) })
                            .when(self.item_drag, |d| d.border_dashed())
                            .when(drop_here, |d| d.bg(accent_color().opacity(0.12)))
                            .on_drag_move::<DraggedWorkItem>(cx.listener(move |this, event: &gpui::DragMoveEvent<DraggedWorkItem>, _, cx| {
                                let inside = event.bounds.contains(&event.event.position);
                                let was = this.item_drop.as_ref() == Some(&drag_id);
                                if !this.item_drag || inside != was {
                                    this.item_drag = true;
                                    if inside { this.item_drop = Some(drag_id.clone()); } else if was { this.item_drop = None; }
                                    cx.notify();
                                }
                            }))
                            .on_drop(cx.listener(move |this, dragged: &DraggedWorkItem, _, cx| {
                                this.item_drop = None;
                                this.add_to_kanban(&drop_id, &dragged.item, cx);
                            })))
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
                    let board = (this.picker == Some(Picker::New) && !query.trim().is_empty())
                        .then(|| {
                            notes::matching_board_types(&query)
                                .first()
                                .map(|(id, _)| *id)
                        })
                        .flatten();
                    if let Some(id) = board {
                        this.create(id, window, cx);
                    } else if let Some(language) = notes::matching_languages(&query).first() {
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
                    if this.kanban.update(cx, |board, cx| board.dismiss(cx))
                        || this.monday.update(cx, |board, cx| board.dismiss(cx))
                    {
                    } else if this.picker.is_some() {
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

    fn board(window: gpui::WindowHandle<Notes>, visual: &mut VisualTestContext) -> Board {
        window
            .update(visual, |notes, _, cx| notes.kanban.read(cx).board().clone())
            .unwrap()
    }

    fn click(visual: &mut VisualTestContext, selector: &str) {
        let bounds = visual
            .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
            .unwrap_or_else(|| panic!("{selector} is on screen"));
        visual.simulate_click(bounds.center(), Default::default());
        visual.run_until_parked();
    }

    fn new_board(cx: &mut TestAppContext) -> (gpui::WindowHandle<Notes>, VisualTestContext) {
        cx.update(|cx| {
            input::bind(cx);
            crate::popover::bind(cx);
            cx.set_reduce_motion(true);
        });
        let window = open(cx);
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.simulate_resize(gpui::size(px(1600.), px(900.)));
        click(&mut visual, "create-kanban");
        (window, visual)
    }

    #[gpui::test]
    fn the_kanban_button_starts_a_board_with_blitzits_four_columns(cx: &mut TestAppContext) {
        let (window, mut visual) = new_board(cx);
        let visual = &mut visual;
        window
            .update(visual, |notes, _, _| {
                let id = notes.selected.clone().unwrap();
                assert!(notes.is_kanban(&id), "error: {:?}", notes.error);
                assert_eq!(
                    locked(&notes.store).unwrap().note_languages().unwrap()[&id],
                    KANBAN
                );
                assert_eq!(notes.draft_title(), "Untitled board");
            })
            .unwrap();
        let mut left = px(0.);
        for column in ["backlog", "this-week", "today", "done"] {
            let bounds = visual
                .debug_bounds(Box::leak(
                    format!("kanban-column-{column}").into_boxed_str(),
                ))
                .unwrap_or_else(|| panic!("the {column} column"));
            assert!(bounds.left() > left, "columns run left to right");
            assert!(
                bounds.size.height > px(400.),
                "{column} fills the pane: {bounds:?}"
            );
            left = bounds.left();
        }
        assert!(
            visual.debug_bounds("note-source").is_none(),
            "a board has no Preview or Source"
        );
        assert!(
            visual.debug_bounds("kanban-add-done").is_none(),
            "Done takes no new tasks"
        );
    }

    #[gpui::test]
    fn quick_add_reads_the_estimate_and_column_and_the_board_is_saved(cx: &mut TestAppContext) {
        let (window, mut visual) = new_board(cx);
        let visual = &mut visual;
        click(visual, "kanban-add-backlog");
        assert!(visual.debug_bounds("kanban-add-form-backlog").is_some());
        visual.simulate_input("Write report 1h30m @today");
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        visual.simulate_input("Plan");
        let form = visual.debug_bounds("kanban-add-form-backlog").unwrap();
        let confirm = visual.debug_bounds("kanban-add-confirm").unwrap();
        assert!(
            confirm.right() <= form.right(),
            "Confirm stays inside its column"
        );
        click(visual, "kanban-add-confirm");
        let tasks = board(window, visual).tasks;
        assert_eq!(tasks.len(), 2, "{tasks:?}");
        assert_eq!(tasks[0].title, "Write report");
        assert_eq!(tasks[0].column, monocode_core::kanban::Column::Today);
        assert_eq!(tasks[0].estimate_min, 90);
        assert_eq!(tasks[1].column, monocode_core::kanban::Column::Backlog);
        // Escape closes the form, not the Notes view.
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(visual.debug_bounds("kanban-add-form-backlog").is_none());
        assert!(visual.debug_bounds("kanban-board").is_some());
        window
            .update(visual, |notes, _, cx| {
                notes.flush(cx);
                let id = notes.selected.clone().unwrap();
                let saved = locked(&notes.store)
                    .unwrap()
                    .list_notes()
                    .unwrap()
                    .into_iter()
                    .find(|note| note.id == id)
                    .unwrap();
                assert_eq!(Board::parse(&saved.body).tasks.len(), 2, "saved as JSON");
                assert_eq!(saved.title, "Untitled board");
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_card_checks_off_to_done_and_drags_between_columns(cx: &mut TestAppContext) {
        use monocode_core::kanban::Column;
        let (window, mut visual) = new_board(cx);
        let visual = &mut visual;
        let (a, b) = window
            .update(visual, |notes, _, cx| {
                notes.kanban.update(cx, |board, cx| {
                    let mut next = Board::default();
                    let a = next.add("first", 30, Column::Today, 1);
                    let b = next.add("second", 0, Column::Today, 1);
                    board.load(&next.to_json(), cx);
                    (a, b)
                })
            })
            .unwrap();
        visual.run_until_parked();
        let card = visual
            .debug_bounds(Box::leak(format!("kanban-card-{a}").into_boxed_str()))
            .unwrap();
        visual.simulate_mouse_move(card.center(), None, Default::default());
        visual.run_until_parked();
        click(visual, &format!("kanban-check-{a}"));
        assert_eq!(board(window, visual).task(&a).unwrap().column, Column::Done);
        assert!(board(window, visual).task(&a).unwrap().done_at.is_some());

        let card = visual
            .debug_bounds(Box::leak(format!("kanban-card-{b}").into_boxed_str()))
            .unwrap();
        let backlog = visual.debug_bounds("kanban-column-backlog").unwrap();
        let to = backlog.center();
        visual.simulate_mouse_down(card.center(), MouseButton::Left, Default::default());
        visual.simulate_mouse_move(
            card.center() + gpui::point(px(12.), px(0.)),
            MouseButton::Left,
            Default::default(),
        );
        visual.run_until_parked();
        visual.simulate_mouse_move(to, MouseButton::Left, Default::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("kanban-placeholder").is_some(),
            "the drop place shows while dragging"
        );
        visual.simulate_mouse_up(to, MouseButton::Left, Default::default());
        visual.run_until_parked();
        assert_eq!(
            board(window, visual).task(&b).unwrap().column,
            Column::Backlog
        );
        assert!(visual.debug_bounds("kanban-placeholder").is_none());
    }

    fn monday_snapshot() -> monocode_core::integrations::BoardSnapshot {
        use monocode_core::integrations::{
            BoardSchema, BoardSnapshot, Group, PeopleCell, Provider, StatusCell, StatusOption,
        };
        let item = |id: &str, status: &str| WorkItem {
            provider: Provider::Monday,
            id: id.into(),
            board_id: "9".into(),
            title: format!("Item {id}"),
            url: format!("https://t.monday.com/boards/9/pulses/{id}"),
            group: Some(Group {
                id: "sprint".into(),
                title: "Sprint 4".into(),
                color: Some("#579bfc".into()),
            }),
            statuses: vec![StatusCell {
                column: "status".into(),
                label: status.into(),
                color: Some("#fdab3d".into()),
            }],
            people: vec![PeopleCell {
                column: "person".into(),
                people: vec!["7".into()],
                names: vec!["Ayşe".into()],
            }],
            updated_at: None,
        };
        let option = |label: &str| StatusOption {
            label: label.into(),
            color: Some("green".into()),
            done: label == "Done",
        };
        BoardSnapshot {
            schema: BoardSchema {
                id: "9".into(),
                name: "Sprint".into(),
                url: Some("https://t.monday.com/boards/9".into()),
                groups: vec![Group {
                    id: "sprint".into(),
                    title: "Sprint 4".into(),
                    color: None,
                }],
                statuses: vec![(
                    "status".into(),
                    vec![option("Working on it"), option("Done")],
                )],
                ..Default::default()
            },
            items: vec![item("1", "Working on it"), item("2", "Done")],
            // Fresh, so nothing would ask monday even with a token.
            fetched_at: monocode_core::time::now_ms(),
            truncated: false,
        }
    }

    /// A kanban note and a monday note on a board cached in a temporary data
    /// folder, with no token saved. The monday note is open.
    fn monday_note(
        cx: &mut TestAppContext,
    ) -> (
        gpui::WindowHandle<Notes>,
        VisualTestContext,
        tempfile::TempDir,
        String,
    ) {
        use monocode_core::integrations::{Cache, Provider};
        let (window, mut visual) = new_board(cx);
        let dir = tempfile::tempdir().unwrap();
        Cache::new(dir.path(), Provider::Monday)
            .save_board(&monday_snapshot())
            .unwrap();
        let path = dir.path().to_path_buf();
        let kanban = window
            .update(&mut visual, |notes, window, cx| {
                let kanban = notes.selected.clone().unwrap();
                notes.data_dir = Some(path);
                notes.create(MONDAY, window, cx);
                notes.monday.update(cx, |board, cx| {
                    board.choose_board("9".into(), "Sprint".into(), cx)
                });
                kanban
            })
            .unwrap();
        visual.run_until_parked();
        (window, visual, dir, kanban)
    }

    fn saved_board(
        window: gpui::WindowHandle<Notes>,
        visual: &mut VisualTestContext,
        id: &str,
    ) -> Board {
        window
            .update(visual, |notes, _, cx| {
                notes.flush(cx);
                let body = locked(&notes.store)
                    .unwrap()
                    .list_notes()
                    .unwrap()
                    .into_iter()
                    .find(|note| note.id == id)
                    .unwrap()
                    .body;
                Board::parse(&body)
            })
            .unwrap()
    }

    fn drag_to(visual: &mut VisualTestContext, from: &str, to: &str) {
        let from = visual
            .debug_bounds(Box::leak(from.to_owned().into_boxed_str()))
            .unwrap_or_else(|| panic!("{from} is on screen"));
        let to = visual
            .debug_bounds(Box::leak(to.to_owned().into_boxed_str()))
            .unwrap_or_else(|| panic!("{to} is on screen"))
            .center();
        visual.simulate_mouse_down(from.center(), MouseButton::Left, Default::default());
        visual.simulate_mouse_move(
            from.center() + gpui::point(px(12.), px(0.)),
            MouseButton::Left,
            Default::default(),
        );
        visual.run_until_parked();
        visual.simulate_mouse_move(to, MouseButton::Left, Default::default());
        visual.run_until_parked();
    }

    #[gpui::test]
    fn the_type_picker_starts_a_monday_note_that_asks_to_connect(cx: &mut TestAppContext) {
        cx.update(|cx| {
            input::bind(cx);
            crate::popover::bind(cx);
        });
        let window = open(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        window
            .update(visual, |notes, _, _| notes.data_dir = Some(path))
            .unwrap();
        click(visual, "create-note-typed");
        click(visual, "note-type-monday");
        window
            .update(visual, |notes, _, cx| {
                let id = notes.selected.clone().unwrap();
                assert!(notes.is_monday(&id), "error: {:?}", notes.error);
                assert_eq!(
                    locked(&notes.store).unwrap().note_languages().unwrap()[&id],
                    MONDAY
                );
                assert_eq!(notes.title.read(cx).content.as_str(), MONDAY_TITLE);
                assert!(!MondayNote::parse(&notes.body.read(cx).content).has_board());
            })
            .unwrap();
        // No token: the picker says so and leads to Settings → Integrations.
        let opened = Arc::new(Mutex::new(false));
        let entity = window.root(visual).unwrap();
        visual.update({
            let opened = opened.clone();
            move |_, cx| {
                cx.subscribe(&entity, move |_, event: &Event, _| {
                    if matches!(event, Event::OpenIntegrations) {
                        *opened.lock().unwrap() = true;
                    }
                })
                .detach()
            }
        });
        assert!(visual.debug_bounds("monday-board-picker").is_none());
        click(visual, "monday-connect");
        assert!(*opened.lock().unwrap(), "the button opens Settings");
    }

    #[gpui::test]
    fn a_monday_note_shows_the_cached_board_without_a_token(cx: &mut TestAppContext) {
        let (window, mut visual, _dir, _) = monday_note(cx);
        let visual = &mut visual;
        window
            .update(visual, |notes, _, cx| {
                assert_eq!(notes.title.read(cx).content.as_str(), "Sprint");
                let note = MondayNote::parse(&notes.body.read(cx).content);
                assert_eq!(
                    (note.board.as_str(), note.board_name.as_str()),
                    ("9", "Sprint")
                );
            })
            .unwrap();
        assert!(
            visual
                .debug_bounds("monday-lane-status:Working on it")
                .is_some()
        );
        assert!(visual.debug_bounds("monday-lane-status:Done").is_some());
        assert!(visual.debug_bounds("monday-card-1").is_some());
        assert!(
            visual.debug_bounds("monday-offline").is_some(),
            "a cached board says it cannot refresh"
        );
        click(visual, "monday-group-by-group");
        assert!(visual.debug_bounds("monday-lane-group:sprint").is_some());
        assert!(visual.debug_bounds("monday-lane-status:Done").is_none());
        // No account is known yet, so nothing is "mine".
        click(visual, "monday-mine");
        assert!(visual.debug_bounds("monday-card-1").is_none());
        assert!(visual.debug_bounds("monday-no-account").is_some());
        window
            .update(visual, |notes, _, cx| {
                notes.flush(cx);
                let id = notes.selected.clone().unwrap();
                let saved = locked(&notes.store)
                    .unwrap()
                    .list_notes()
                    .unwrap()
                    .into_iter()
                    .find(|note| note.id == id)
                    .unwrap();
                let note = MondayNote::parse(&saved.body);
                assert_eq!(note.group_by, monocode_core::monday_note::GroupBy::Group);
                assert!(note.mine);
                assert_eq!(saved.title, "Sprint");
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_monday_card_dropped_on_a_kanban_note_lands_in_its_backlog_once(cx: &mut TestAppContext) {
        let (window, mut visual, _dir, kanban) = monday_note(cx);
        let visual = &mut visual;
        let row = format!("saved-note-{kanban}");
        drag_to(visual, "monday-card-1", &row);
        window
            .update(visual, |notes, _, _| {
                assert!(notes.item_drag, "kanban rows take the card");
                assert_eq!(
                    notes.item_drop.as_ref(),
                    Some(&kanban),
                    "the row is the target"
                );
            })
            .unwrap();
        let to = visual
            .debug_bounds(Box::leak(row.clone().into_boxed_str()))
            .unwrap();
        visual.simulate_mouse_up(to.center(), MouseButton::Left, Default::default());
        visual.run_until_parked();
        let board = saved_board(window, visual, &kanban);
        assert_eq!(board.tasks.len(), 1, "{board:?}");
        let task = &board.tasks[0];
        assert_eq!(task.title, "Item 1");
        assert_eq!(task.column, Column::Backlog);
        let source = task.source.clone().expect("the task links its item");
        assert_eq!(
            (
                source.provider.as_str(),
                source.item_id.as_str(),
                source.board_id.as_str()
            ),
            ("monday", "1", "9")
        );
        assert_eq!(source.url, "https://t.monday.com/boards/9/pulses/1");
        window
            .update(visual, |notes, _, _| assert!(notes.item_drop.is_none()))
            .unwrap();

        // The same card again changes nothing.
        drag_to(visual, "monday-card-1", &row);
        let to = visual
            .debug_bounds(Box::leak(row.clone().into_boxed_str()))
            .unwrap();
        visual.simulate_mouse_up(to.center(), MouseButton::Left, Default::default());
        visual.run_until_parked();
        assert_eq!(saved_board(window, visual, &kanban).tasks.len(), 1);

        // A card's menu offers the kanban notes by title.
        click(visual, "monday-card-2");
        click(visual, "monday-menu-kanban-0");
        let board = saved_board(window, visual, &kanban);
        assert_eq!(board.tasks.len(), 2);
        assert_eq!(board.tasks[1].source.as_ref().unwrap().item_id, "2");

        // On the kanban board the card wears its monday mark.
        window
            .update(visual, |notes, _, cx| notes.select(kanban.clone(), cx))
            .unwrap();
        visual.run_until_parked();
        let id = board.tasks[0].id.clone();
        assert!(
            visual
                .debug_bounds(Box::leak(format!("kanban-source-{id}").into_boxed_str()))
                .is_some()
        );
    }
}
