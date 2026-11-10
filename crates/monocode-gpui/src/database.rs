//! The Database view: data sources and their object tree on the left, SQL
//! consoles (each with its own connection) on the right with the result grid
//! beneath. PostgreSQL only; the work lives in `monocode_core::db` and runs on
//! its tokio runtime, never on the UI thread.

mod dialog;
mod grid;
mod render;
mod send;
mod sql;
#[cfg(test)]
mod tests;

use crate::{
    input::{self, Input},
    modal,
    popover::{Anchor, Menu, MenuAction, MenuEvent, MenuItem, menu_popover},
    theme::*,
};
use dialog::{DialogEvent, SourceDialog};
use gpui::{
    AnyElement, ClipboardItem, Context, CursorStyle, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, MouseButton, Role, SharedString, Subscription, Task, Window, div, prelude::*, px,
    rgb, uniform_list,
};
use monocode_core::db::{
    ColumnInfo, Connection, ConsoleStore, DataSource, DataSourceStore, DbColor, DbError,
    HistoryEntry, HistoryStore, QueryResult, RoutineKind, SchemaObjects, TableDetails, TableInfo,
    TableKind, analysis, format, params, runtime::runtime, source, split,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    future::Future,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, RwLock},
    time::Duration,
};

pub(crate) enum Event {
    Close,
}

#[cfg(test)]
pub(crate) use send::write_sources;
pub(crate) use send::{MAX_SEND_BYTES, Origin, Target, send_targets};

const LIMITS: [usize; 4] = [100, 500, 1000, 5000];
const TREE_ROW: f32 = 26.;

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct SavedState {
    tree_width: f32,
    editor_height: f32,
    limit: usize,
    selected: Option<String>,
    active: HashMap<String, u32>,
    /// The consoles open as tabs per data source; the rest are only saved.
    open: HashMap<String, Vec<u32>>,
    expanded: Vec<String>,
    results_hidden: bool,
    /// Where text sent from a file went last.
    send_target: Option<String>,
    /// The console made for each file sent to a data source, by `source\0file`.
    sent: HashMap<String, u32>,
}

impl Default for SavedState {
    fn default() -> Self {
        Self {
            tree_width: 280.,
            editor_height: 240.,
            limit: 500,
            selected: None,
            active: HashMap::new(),
            open: HashMap::new(),
            expanded: Vec::new(),
            results_hidden: false,
            send_target: None,
            sent: HashMap::new(),
        }
    }
}

enum Link {
    Idle,
    Connecting,
    Ready,
    Failed(String),
}

struct TableNode {
    info: TableInfo,
    columns: Option<Vec<ColumnInfo>>,
    details: Option<TableDetails>,
}

/// The folders under an expanded table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GroupKind {
    Columns,
    Indexes,
    ForeignKeys,
    Constraints,
    ReferencedBy,
    Triggers,
}

impl GroupKind {
    const ALL: [Self; 6] = [
        Self::Columns,
        Self::Indexes,
        Self::ForeignKeys,
        Self::Constraints,
        Self::ReferencedBy,
        Self::Triggers,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Columns => "Columns",
            Self::Indexes => "Indexes",
            Self::ForeignKeys => "Foreign keys",
            Self::Constraints => "Constraints",
            Self::ReferencedBy => "Referenced by",
            Self::Triggers => "Triggers",
        }
    }
}

/// The rows of one group of a table (depth 4, under the group's folder).
fn group_rows(
    columns: &[ColumnInfo],
    details: Option<&TableDetails>,
    kind: GroupKind,
) -> Vec<TreeRow> {
    let leaf = |glyph: &'static str, label: &str, detail: String, full: String| TreeRow::Leaf {
        glyph,
        label: label.to_owned(),
        detail,
        full,
    };
    let Some(details) = details else {
        return match kind {
            GroupKind::Columns => columns
                .iter()
                .map(|c| TreeRow::Column(5, c.clone()))
                .collect(),
            _ => Vec::new(),
        };
    };
    match kind {
        GroupKind::Columns => columns
            .iter()
            .map(|c| TreeRow::Column(5, c.clone()))
            .collect(),
        GroupKind::Indexes => details
            .indexes
            .iter()
            .map(|i| {
                let using = i
                    .definition
                    .split_once(" USING ")
                    .map_or(i.definition.as_str(), |(_, rest)| rest);
                let flags = match (i.primary, i.unique) {
                    (true, _) => "primary · ",
                    (_, true) => "unique · ",
                    _ => "",
                };
                leaf(
                    "db-key",
                    &i.name,
                    format!("{flags}{using}"),
                    i.definition.clone(),
                )
            })
            .collect(),
        GroupKind::ForeignKeys => details
            .constraints
            .iter()
            .filter(|c| c.kind == 'f')
            .map(|c| {
                leaf(
                    "db-column",
                    &c.name,
                    c.definition.replacen("FOREIGN KEY ", "", 1),
                    format!("{}\n{}", c.name, c.definition),
                )
            })
            .collect(),
        GroupKind::Constraints => details
            .constraints
            .iter()
            .filter(|c| c.kind != 'f')
            .map(|c| {
                leaf(
                    "db-column",
                    &c.name,
                    c.definition.clone(),
                    format!("{}\n{}", c.name, c.definition),
                )
            })
            .collect(),
        GroupKind::ReferencedBy => details
            .referenced_by
            .iter()
            .map(|(table, definition)| {
                leaf(
                    "db-table",
                    table,
                    definition.clone(),
                    format!("{table}\n{definition}"),
                )
            })
            .collect(),
        GroupKind::Triggers => details
            .triggers
            .iter()
            .map(|t| leaf("db-column", &t.name, String::new(), t.definition.clone()))
            .collect(),
    }
}

struct SchemaNode {
    name: String,
    tables: Option<Vec<TableNode>>,
    /// Functions, procedures and sequences, loaded with the tables.
    objects: SchemaObjects,
}

/// The folders under an expanded schema.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SchemaFolder {
    Tables,
    Views,
    MaterializedViews,
    Functions,
    Procedures,
    Sequences,
}

impl SchemaFolder {
    const ALL: [Self; 6] = [
        Self::Tables,
        Self::Views,
        Self::MaterializedViews,
        Self::Functions,
        Self::Procedures,
        Self::Sequences,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Tables => "Tables",
            Self::Views => "Views",
            Self::MaterializedViews => "Materialized views",
            Self::Functions => "Functions",
            Self::Procedures => "Procedures",
            Self::Sequences => "Sequences",
        }
    }

    fn holds(self, kind: TableKind) -> bool {
        matches!(
            (self, kind),
            (Self::Tables, TableKind::Table | TableKind::Foreign)
                | (Self::Views, TableKind::View)
                | (Self::MaterializedViews, TableKind::MaterializedView)
        )
    }
}

fn folder_key(source: &str, schema: &str, folder: SchemaFolder) -> String {
    format!("f:{source}/{schema}/{}", folder.label())
}

/// What a DDL request is for.
#[derive(Clone)]
enum DdlTarget {
    Table {
        schema: String,
        table: String,
        kind: TableKind,
    },
    Routine {
        oid: String,
        label: String,
    },
    Sequence {
        schema: String,
        name: String,
    },
}

/// What to do with a fetched script.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DdlThen {
    Preview,
    Clipboard,
    Console,
}

struct OpenMenu {
    row: TreeRow,
    anchor: gpui::Point<gpui::Pixels>,
    menu: Entity<Menu>,
    _events: Subscription,
}

struct SourceNode {
    conn: Option<Arc<Connection>>,
    link: Link,
    schemas: Option<Vec<SchemaNode>>,
}

impl SourceNode {
    fn new() -> Self {
        Self {
            conn: None,
            link: Link::Idle,
            schemas: None,
        }
    }
}

struct Console {
    id: u32,
    source: String,
    /// The name the user gave it; otherwise the first `-- comment`, else the number.
    name: Option<String>,
    editor: Entity<Input>,
    conn: Option<Arc<Connection>>,
    result: Option<Arc<QueryResult>>,
    /// Every result of the last run that returned rows; `result` is one of them.
    all: Vec<Arc<QueryResult>>,
    current: usize,
    widths: Vec<f32>,
    numeric: Vec<bool>,
    error: Option<(DbError, String)>,
    running: bool,
    selected: Option<(usize, usize)>,
    scroll_x: f32,
    scroll_y: f32,
    note: Option<String>,
    paging: Option<Paging>,
    /// What "save all rows" runs when it is not the shown statement.
    export_sql: Option<String>,
    task: Option<Task<()>>,
    saver: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// A statement to run and where it sits in the editor (to mark its outcome).
#[derive(Clone)]
struct Stmt {
    text: String,
    /// Byte range in the editor.
    start: usize,
    end: usize,
    /// Whether the editor shows its outcome (a table page is not in the text).
    mark: bool,
}

/// What happened to one statement.
struct Outcome {
    stmt: Stmt,
    /// The editor's text of the statement, to tell whether parameters were bound into it.
    original: String,
    rows: Option<u64>,
    elapsed_ms: f64,
    error: Option<DbError>,
}

#[derive(Default)]
struct Finished {
    conn: Option<Arc<Connection>>,
    results: Vec<QueryResult>,
    outcomes: Vec<Outcome>,
    /// The error, and the statement it came from.
    failure: Option<(DbError, String)>,
}

/// A run waiting on the user.
enum Pending {
    Params {
        source: String,
        id: u32,
        statements: Vec<Stmt>,
        paging: Option<Paging>,
    },
    Confirm {
        source: String,
        id: u32,
        statements: Vec<Stmt>,
        paging: Option<Paging>,
        reason: String,
        /// The sweeping statement.
        at: usize,
    },
}

/// A console showing a table page by page, optionally sorted.
#[derive(Clone, PartialEq, Debug)]
struct Paging {
    schema: String,
    table: String,
    page: usize,
    /// Column name and whether it is descending.
    sort: Option<(String, bool)>,
}

/// The statement for one page of a table.
fn page_sql(paging: &Paging, limit: usize) -> String {
    let order = paging
        .sort
        .as_ref()
        .map(|(column, desc)| {
            format!(
                " order by {} {}",
                quote_ident(column),
                if *desc { "desc" } else { "asc" }
            )
        })
        .unwrap_or_default();
    format!(
        "select * from {}.{}{order} limit {limit} offset {}",
        quote_ident(&paging.schema),
        quote_ident(&paging.table),
        paging.page * limit
    )
}

/// Header click: ascending, descending, then unsorted; another column starts ascending.
fn next_sort(current: Option<(String, bool)>, clicked: String) -> Option<(String, bool)> {
    match current {
        Some((name, false)) if name == clicked => Some((clicked, true)),
        Some((name, true)) if name == clicked => None,
        _ => Some((clicked, false)),
    }
}

/// The same table read whole, for exporting every row.
fn table_sql(paging: &Paging) -> String {
    let order = paging
        .sort
        .as_ref()
        .map(|(column, desc)| {
            format!(
                " order by {} {}",
                quote_ident(column),
                if *desc { "desc" } else { "asc" }
            )
        })
        .unwrap_or_default();
    format!(
        "select * from {}.{}{order}",
        quote_ident(&paging.schema),
        quote_ident(&paging.table)
    )
}

#[derive(Clone)]
enum TreeRow {
    Source(String),
    Note(usize, String),
    Consoles(String),
    SavedConsole(String, u32, String, bool),
    Schema(String, String),
    Table(String, String, usize),
    /// A folder under a table: source, schema, table, kind.
    Group(String, String, String, GroupKind),
    /// A folder under a schema: source, schema, folder.
    Folder(String, String, SchemaFolder),
    /// Index into the schema's `objects.routines` / `objects.sequences`.
    Routine(String, String, usize),
    Sequence(String, String, usize),
    Column(usize, ColumnInfo),
    Leaf {
        glyph: &'static str,
        label: String,
        detail: String,
        full: String,
    },
}

enum Resize {
    Tree(f32, f32),
    Editor(f32, f32),
    /// Dragging a grid scrollbar: (vertical, mouse start, scroll start).
    Bar(bool, f32, f32),
}

pub(crate) struct DatabaseView {
    store: DataSourceStore,
    console_store: Arc<ConsoleStore>,
    state_path: PathBuf,
    sources: Vec<DataSource>,
    nodes: HashMap<String, SourceNode>,
    selected: Option<String>,
    active: HashMap<String, u32>,
    open: HashMap<String, Vec<u32>>,
    saved: HashMap<String, Vec<(u32, String)>>,
    catalogs: HashMap<String, Arc<RwLock<sql::Catalog>>>,
    catalog_started: HashSet<String>,
    grid_viewport: Rc<Cell<(f32, f32)>>,
    export_menu: bool,
    menu: Option<OpenMenu>,
    export_format: monocode_core::db::ExportFormat,
    export_header: bool,
    export_transpose: bool,
    export_table: Entity<Input>,
    pending: Option<Pending>,
    param_inputs: Vec<(String, Entity<Input>)>,
    param_subscriptions: Vec<Subscription>,
    focus_params: bool,
    param_values: HashMap<String, String>,
    rename: Option<(String, u32)>,
    rename_input: Entity<Input>,
    results_hidden: bool,
    send_target: Option<String>,
    sent: HashMap<String, u32>,
    history: Arc<HistoryStore>,
    history_open: bool,
    history_entries: Vec<HistoryEntry>,
    history_filter: Entity<Input>,
    consoles: Vec<Console>,
    expanded: HashSet<String>,
    passwords: HashMap<String, String>,
    dialog: Option<(Entity<SourceDialog>, Subscription)>,
    confirm_remove: Option<String>,
    preview: Option<(String, String)>,
    tree_width: f32,
    editor_height: f32,
    limit: usize,
    resize: Option<Resize>,
    notice: Option<String>,
    focus: FocusHandle,
    grid_focus: FocusHandle,
}

impl EventEmitter<Event> for DatabaseView {}

impl Focusable for DatabaseView {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.active_console()
            .map(|c| c.editor.focus_handle(cx))
            .unwrap_or_else(|| self.focus.clone())
    }
}

/// A console's name: its first line when that is a `-- comment`, else its number.
fn title_of(name: Option<&str>, text: &str, id: u32) -> String {
    if let Some(name) = name.filter(|n| !n.trim().is_empty()) {
        return name.to_owned();
    }
    let first = text.lines().next().unwrap_or("").trim();
    match first.strip_prefix("--").map(str::trim) {
        Some(title) if !title.is_empty() => title.chars().take(24).collect(),
        _ => format!("Console {id}"),
    }
}

fn group_key(source: &str, schema: &str, table: &str, kind: GroupKind) -> String {
    format!("g:{source}/{schema}/{table}/{}", kind.label())
}

impl Console {
    /// Makes result `i` of the last run the one the grid shows.
    fn select_result(&mut self, i: usize) {
        let Some(result) = self.all.get(i).cloned() else {
            return;
        };
        self.current = i;
        self.widths = grid::column_widths(&result);
        self.numeric = grid::numeric_columns(&result);
        self.result = Some(result);
        self.selected = None;
        self.scroll_x = 0.;
        self.scroll_y = 0.;
    }
}

/// A password to look up where it is safe to wait.
struct PasswordLookup {
    /// Typed this run, not saved.
    session: Option<String>,
    id: String,
    saved: bool,
}

impl PasswordLookup {
    /// The password to connect with. An `Err` is a keychain that could not be
    /// read; it says so rather than letting the server answer "password missing".
    fn resolve(self) -> Result<Option<String>, DbError> {
        if self.session.is_some() || !self.saved {
            return Ok(self.session);
        }
        source::saved_password_checked(&self.id).map_err(|error| DbError {
            message: format!(
                "The saved password could not be read from the keychain ({error}). \
                 Allow Monocode access when macOS asks, then connect again."
            ),
            ..DbError::default()
        })
    }
}

fn uuid_like() -> String {
    // A fresh id for a duplicated data source.
    DataSource::new("").id
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

impl DatabaseView {
    pub(crate) fn new(dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let store = DataSourceStore::new(&dir);
        let rename_input = cx.new(|cx| {
            let mut input = Input::field("Console name", "Console name", content(0.35), cx);
            input.set_font_size(13.);
            input
        });
        cx.subscribe(&rename_input, |this, _, _: &input::Submitted, cx| {
            this.commit_rename(cx)
        })
        .detach();
        let export_table = cx.new(|cx| {
            let mut input = Input::field("Table name", "table_name", content(0.35), cx);
            input.set_font_size(13.);
            input
        });
        let history_filter = cx.new(|cx| {
            let mut input = Input::field("Filter history", "Filter history", content(0.35), cx);
            input.set_font_size(13.);
            input
        });
        cx.subscribe(&history_filter, |_, _, _: &input::Changed, cx| cx.notify())
            .detach();
        let state_path = dir.join("state.json");
        let saved: SavedState = std::fs::read(&state_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let sources = store.load();
        let selected = saved
            .selected
            .filter(|id| sources.iter().any(|s| &s.id == id))
            .or_else(|| sources.first().map(|s| s.id.clone()));
        let mut this = Self {
            console_store: Arc::new(ConsoleStore::new(&dir)),
            store,
            state_path,
            nodes: HashMap::new(),
            selected,
            active: saved.active,
            open: saved.open,
            saved: HashMap::new(),
            catalogs: HashMap::new(),
            catalog_started: HashSet::new(),
            grid_viewport: Rc::new(Cell::new((0., 0.))),
            export_menu: false,
            menu: None,
            export_format: monocode_core::db::ExportFormat::Csv,
            export_header: true,
            export_transpose: false,
            export_table,
            pending: None,
            param_inputs: Vec::new(),
            param_subscriptions: Vec::new(),
            focus_params: false,
            param_values: HashMap::new(),
            rename: None,
            rename_input,
            results_hidden: saved.results_hidden,
            send_target: saved.send_target,
            sent: saved.sent,
            history: Arc::new(HistoryStore::new(&dir)),
            history_open: false,
            history_entries: Vec::new(),
            history_filter,
            consoles: Vec::new(),
            expanded: saved.expanded.into_iter().collect(),
            passwords: HashMap::new(),
            dialog: None,
            confirm_remove: None,
            preview: None,
            tree_width: saved.tree_width.clamp(200., 700.),
            editor_height: saved.editor_height.clamp(80., 900.),
            limit: if LIMITS.contains(&saved.limit) {
                saved.limit
            } else {
                500
            },
            resize: None,
            notice: None,
            focus: cx.focus_handle(),
            grid_focus: cx.focus_handle(),
            sources,
        };
        if let Some(id) = this.selected.clone() {
            this.ensure_consoles(&id, cx);
        }
        this
    }

    fn save_state(&self) {
        let state = SavedState {
            tree_width: self.tree_width,
            editor_height: self.editor_height,
            limit: self.limit,
            selected: self.selected.clone(),
            active: self.active.clone(),
            open: self.open.clone(),
            expanded: self.expanded.iter().cloned().collect(),
            results_hidden: self.results_hidden,
            send_target: self.send_target.clone(),
            sent: self.sent.clone(),
        };
        if let Ok(json) = serde_json::to_vec_pretty(&state) {
            if let Some(parent) = self.state_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&self.state_path, json);
        }
    }

    fn source(&self, id: &str) -> Option<&DataSource> {
        self.sources.iter().find(|s| s.id == id)
    }

    /// How to get a source's password, resolved off the UI thread: the
    /// credential store may stop to ask the user, and the window must not.
    fn password_for(&self, source: &DataSource) -> PasswordLookup {
        PasswordLookup {
            session: self.passwords.get(&source.id).cloned(),
            id: source.id.clone(),
            saved: source.save_password,
        }
    }

    /// Runs `work` on the database runtime and applies its result here.
    fn spawn_db<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl Future<Output = T> + Send + 'static,
        done: impl FnOnce(&mut Self, Option<T>, &mut Context<Self>) + 'static,
    ) -> Task<()> {
        let handle = runtime().spawn(work);
        cx.spawn(async move |this, cx| {
            let out = handle.await.ok();
            let _ = this.update(cx, |this, cx| done(this, out, cx));
        })
    }

    // ---- consoles ----

    fn active_console(&self) -> Option<&Console> {
        let source = self.selected.as_ref()?;
        let id = self.active.get(source)?;
        self.consoles
            .iter()
            .find(|c| &c.source == source && c.id == *id)
    }

    fn console_index(&self, source: &str, id: u32) -> Option<usize> {
        self.consoles
            .iter()
            .position(|c| c.source == source && c.id == id)
    }

    fn active_index(&self) -> Option<usize> {
        let source = self.selected.as_ref()?;
        self.console_index(source, *self.active.get(source)?)
    }

    fn catalog_for(&mut self, source: &str) -> Arc<RwLock<sql::Catalog>> {
        self.catalogs.entry(source.to_owned()).or_default().clone()
    }

    /// Re-reads which consoles are saved for `source` and their titles.
    fn refresh_saved(&mut self, source: &str) {
        let list = self
            .console_store
            .list(source)
            .into_iter()
            .map(|id| {
                let text = self.console_store.load(source, id);
                (
                    id,
                    title_of(self.console_store.name(source, id).as_deref(), &text, id),
                )
            })
            .collect();
        self.saved.insert(source.to_owned(), list);
    }

    fn ensure_consoles(&mut self, source: &str, cx: &mut Context<Self>) {
        if !self.consoles.iter().any(|c| c.source == source) {
            let mut ids = self.console_store.list(source);
            if ids.is_empty() {
                // A console that exists is a console that is saved, so it
                // is listed (and can be renamed) from the start.
                let _ = self.console_store.save(source, 1, "");
                ids.push(1);
            }
            let wanted = self.open.get(source).cloned();
            let mut opened: Vec<u32> = ids
                .iter()
                .copied()
                .filter(|id| wanted.as_ref().is_none_or(|open| open.contains(id)))
                .collect();
            if opened.is_empty() {
                opened.push(ids[0]);
            }
            for id in opened {
                let text = self.console_store.load(source, id);
                self.add_console(source, id, text, cx);
            }
            self.refresh_saved(source);
        }
        let first = self
            .consoles
            .iter()
            .find(|c| c.source == source)
            .map(|c| c.id);
        let valid = self
            .active
            .get(source)
            .is_some_and(|id| self.console_index(source, *id).is_some());
        if !valid && let Some(first) = first {
            self.active.insert(source.to_owned(), first);
        }
        self.remember_open(source);
    }

    fn remember_open(&mut self, source: &str) {
        let ids = self
            .consoles
            .iter()
            .filter(|c| c.source == source)
            .map(|c| c.id)
            .collect();
        self.open.insert(source.to_owned(), ids);
    }

    fn add_console(&mut self, source: &str, id: u32, text: String, cx: &mut Context<Self>) {
        let catalog = self.catalog_for(source);
        let editor = cx.new(Input::editor);
        editor.update(cx, |input, cx| {
            input.set_editor_path("console.sql".into());
            input.set_editor_completer(Arc::new(move |text, at, explicit| match catalog.read() {
                Ok(catalog) => sql::complete(&catalog, text, at, explicit),
                Err(_) => Vec::new(),
            }));
            if !text.is_empty() {
                input.set_text(text.clone(), cx);
            }
            input.set_editor_analysis(sql::analyse(&text), cx);
        });
        let key = (source.to_owned(), id);
        let subscriptions = vec![
            cx.subscribe(&editor, {
                let key = key.clone();
                move |this, _, _: &input::Changed, cx| this.console_edited(&key.0, key.1, cx)
            }),
            cx.subscribe(&editor, {
                let key = key.clone();
                move |this, _, _: &input::Submitted, cx| this.run(&key.0, key.1, false, cx)
            }),
        ];
        let name = self.console_store.name(source, id);
        self.consoles.push(Console {
            id,
            source: source.to_owned(),
            name,
            editor,
            conn: None,
            result: None,
            all: Vec::new(),
            current: 0,
            widths: Vec::new(),
            numeric: Vec::new(),
            error: None,
            running: false,
            selected: None,
            scroll_x: 0.,
            scroll_y: 0.,
            note: None,
            paging: None,
            export_sql: None,
            task: None,
            saver: None,
            _subscriptions: subscriptions,
        });
    }

    fn new_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self.selected.clone() else {
            return;
        };
        let id = self
            .console_store
            .list(&source)
            .into_iter()
            .chain(
                self.consoles
                    .iter()
                    .filter(|c| c.source == source)
                    .map(|c| c.id),
            )
            .max()
            .unwrap_or(0)
            + 1;
        self.add_console(&source, id, String::new(), cx);
        let _ = self.console_store.save(&source, id, "");
        self.refresh_saved(&source);
        self.remember_open(&source);
        self.activate(&source, id, window, cx);
    }

    /// Brings a saved console back as a tab (or just switches to it).
    fn open_console(&mut self, source: &str, id: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.select_source(source, cx);
        if self.console_index(source, id).is_none() {
            let text = self.console_store.load(source, id);
            self.add_console(source, id, text, cx);
            self.remember_open(source);
        }
        self.activate(source, id, window, cx);
    }

    fn activate(&mut self, source: &str, id: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.active.insert(source.to_owned(), id);
        self.save_state();
        if let Some(console) = self.active_console() {
            window.focus(&console.editor.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Closes the tab. The text stays saved and is listed under Consoles in
    /// the tree; an empty console is not worth keeping.
    fn close_console(
        &mut self,
        source: &str,
        id: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.console_index(source, id) else {
            return;
        };
        let console = self.consoles.remove(ix);
        if let Some(conn) = &console.conn
            && console.running
        {
            conn.cancel();
        }
        let text = console.editor.read(cx).content.to_string();
        if text.trim().is_empty() {
            self.console_store.delete(source, id);
        } else {
            let _ = self.console_store.save(source, id, &text);
        }
        self.refresh_saved(source);
        if self.active.get(source) == Some(&id) {
            self.active.remove(source);
        }
        if !self.consoles.iter().any(|c| c.source == source) {
            let next = self
                .console_store
                .list(source)
                .first()
                .copied()
                .unwrap_or(1);
            let text = self.console_store.load(source, next);
            self.add_console(source, next, text, cx);
        }
        self.ensure_consoles(source, cx);
        if let Some(next) = self.active.get(source).copied() {
            self.activate(source, next, window, cx);
        }
        self.refresh_saved(source);
        cx.notify();
    }

    /// Forgets a console for good.
    fn delete_console(
        &mut self,
        source: &str,
        id: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.console_index(source, id) {
            // Close without saving the text back.
            let console = self.consoles.remove(ix);
            if let Some(conn) = &console.conn
                && console.running
            {
                conn.cancel();
            }
            if self.active.get(source) == Some(&id) {
                self.active.remove(source);
            }
        }
        self.console_store.delete(source, id);
        if !self.consoles.iter().any(|c| c.source == source) {
            self.add_console(source, 1, String::new(), cx);
        }
        self.ensure_consoles(source, cx);
        self.refresh_saved(source);
        if let Some(next) = self.active.get(source).copied() {
            self.activate(source, next, window, cx);
        }
        cx.notify();
    }

    fn console_edited(&mut self, source: &str, id: u32, cx: &mut Context<Self>) {
        let Some(ix) = self.console_index(source, id) else {
            return;
        };
        let editor = self.consoles[ix].editor.clone();
        let text = editor.read(cx).content.to_string();
        // Colours follow the text at once; the analysis is a single pass.
        editor.update(cx, |input, cx| {
            // The edit already moved the notes with the text; the colours are redone,
            // and an error underline is stale once the text changes.
            let mut analysis = input.editor_analysis().clone();
            analysis.tokens = sql::analyse(&text).tokens;
            analysis.diagnostics.clear();
            input.set_editor_analysis(analysis, cx);
        });
        let store = self.console_store.clone();
        let key = (source.to_owned(), id);
        self.consoles[ix].saver = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |this, cx| {
                let text = editor.read(cx).content.to_string();
                let _ = store.save(&key.0, key.1, &text);
                let name = this
                    .console_index(&key.0, key.1)
                    .and_then(|ix| this.consoles[ix].name.clone());
                let title = title_of(name.as_deref(), &text, key.1);
                match this
                    .saved
                    .get_mut(&key.0)
                    .and_then(|list| list.iter_mut().find(|(i, _)| *i == key.1))
                {
                    Some(entry) => entry.1 = title,
                    None => this.refresh_saved(&key.0),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn tab_title(&self, console: &Console, cx: &Context<Self>) -> String {
        title_of(
            console.name.as_deref(),
            &console.editor.read(cx).content,
            console.id,
        )
    }

    // ---- running statements ----

    fn run(&mut self, source: &str, id: u32, all: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.console_index(source, id) else {
            return;
        };
        if self.consoles[ix].running {
            return;
        }
        let editor = self.consoles[ix].editor.read(cx);
        let text = editor.content.to_string();
        let selection = editor.selection_range();
        let stmt = |offset: usize, s: split::Statement| Stmt {
            text: s.text,
            start: offset + s.from,
            end: offset + s.to,
            mark: true,
        };
        let statements: Vec<Stmt> = if !selection.is_empty() {
            let offset = selection.start;
            split::split(&text[selection])
                .into_iter()
                .map(|s| stmt(offset, s))
                .collect()
        } else if all {
            split::split(&text)
                .into_iter()
                .map(|s| stmt(0, s))
                .collect()
        } else {
            split::at_cursor(&text, selection.start)
                .map(|s| vec![stmt(0, s)])
                .unwrap_or_default()
        };
        self.start(source, id, statements, None, cx);
    }

    /// Asks for parameter values if the statements have any, then vets them.
    fn start(
        &mut self,
        source: &str,
        id: u32,
        statements: Vec<Stmt>,
        paging: Option<Paging>,
        cx: &mut Context<Self>,
    ) {
        if statements.is_empty() || self.console_index(source, id).is_none() {
            return;
        }
        let texts: Vec<String> = statements.iter().map(|s| s.text.clone()).collect();
        let names = params::names(&texts);
        if names.is_empty() {
            self.vet(source, id, statements, paging, cx);
            return;
        }
        self.param_inputs = names
            .iter()
            .map(|name| {
                let value = self.param_values.get(name).cloned().unwrap_or_default();
                let input = cx.new(|cx| {
                    let mut input = Input::field(
                        name.clone(),
                        "value (SQL, e.g. 'text' or 42)",
                        content(0.35),
                        cx,
                    );
                    input.set_font_size(13.);
                    input
                });
                input.update(cx, |input, cx| input.set_text(value, cx));
                (name.clone(), input)
            })
            .collect();
        self.param_subscriptions = self
            .param_inputs
            .iter()
            .map(|(_, input)| {
                cx.subscribe(input, |this, _, _: &input::Submitted, cx| {
                    this.submit_params(cx)
                })
            })
            .collect();
        self.focus_params = true;
        self.pending = Some(Pending::Params {
            source: source.to_owned(),
            id,
            statements,
            paging,
        });
        cx.notify();
    }

    /// The parameter dialog's OK: values are kept for next time and bound into the text.
    fn submit_params(&mut self, cx: &mut Context<Self>) {
        let Some(Pending::Params {
            source,
            id,
            statements,
            paging,
        }) = self.pending.take()
        else {
            return;
        };
        let values: HashMap<String, String> = self
            .param_inputs
            .iter()
            .map(|(name, input)| (name.clone(), input.read(cx).content.to_string()))
            .collect();
        self.param_values.extend(values.clone());
        self.param_inputs.clear();
        self.param_subscriptions.clear();
        let bound = statements
            .into_iter()
            .map(|s| Stmt {
                text: params::bind(&s.text, &|name| {
                    values.get(name).cloned().unwrap_or_default()
                }),
                ..s
            })
            .collect();
        self.vet(&source, id, bound, paging, cx);
    }

    /// Refuses writes on a read-only source without asking the server, and
    /// asks before a statement that sweeps a whole table.
    fn vet(
        &mut self,
        source: &str,
        id: u32,
        statements: Vec<Stmt>,
        paging: Option<Paging>,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.console_index(source, id) else {
            return;
        };
        let read_only = self.source(source).is_some_and(|s| s.read_only);
        let impacts: Vec<analysis::Impact> = statements
            .iter()
            .map(|s| analysis::classify(&s.text))
            .collect();
        if read_only && let Some(at) = impacts.iter().position(|i| i.writes) {
            let blocked = &statements[at];
            self.results_hidden = false;
            let message = format!(
                "{} is read-only, and this statement would change data. Turn off Read-only in the data source's settings to run it.",
                self.source(source)
                    .map(|s| s.name.clone())
                    .unwrap_or_default()
            );
            self.consoles[ix].error = Some((
                DbError {
                    message: message.clone(),
                    ..DbError::default()
                },
                blocked.text.clone(),
            ));
            self.mark_outcomes(
                ix,
                &[Outcome {
                    stmt: blocked.clone(),
                    original: blocked.text.clone(),
                    rows: None,
                    elapsed_ms: 0.,
                    error: Some(DbError {
                        message: "Read-only data source".into(),
                        ..DbError::default()
                    }),
                }],
                cx,
            );
            cx.notify();
            return;
        }
        if let Some(at) = impacts.iter().position(|i| i.sweeping) {
            let first = statements[at]
                .text
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_uppercase();
            let reason = match first.as_str() {
                "UPDATE" => "UPDATE without WHERE changes every row of the table.",
                "DELETE" => "DELETE without WHERE removes every row of the table.",
                "TRUNCATE" => "TRUNCATE empties the table at once.",
                _ => "DROP removes the object for good.",
            };
            self.pending = Some(Pending::Confirm {
                source: source.to_owned(),
                id,
                statements,
                paging,
                reason: reason.to_owned(),
                at,
            });
            cx.notify();
            return;
        }
        self.execute(source, id, statements, paging, cx);
    }

    fn confirm_run(&mut self, cx: &mut Context<Self>) {
        if let Some(Pending::Confirm {
            source,
            id,
            statements,
            paging,
            ..
        }) = self.pending.take()
        {
            self.execute(&source, id, statements, paging, cx);
        }
    }

    /// Runs `statements` in order on the console's connection (opening it if
    /// needed). `paging` marks a table view.
    fn execute(
        &mut self,
        source: &str,
        id: u32,
        statements: Vec<Stmt>,
        paging: Option<Paging>,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.console_index(source, id) else {
            return;
        };
        if self.consoles[ix].running || statements.is_empty() {
            return;
        }
        let Some(data_source) = self.source(source).cloned() else {
            return;
        };
        let password = self.password_for(&data_source);
        let limit = self.limit;
        let existing = self.consoles[ix].conn.clone().filter(|c| !c.is_closed());
        self.consoles[ix].running = true;
        self.consoles[ix].error = None;
        self.results_hidden = false;
        // What the editor holds, to tell later whether a statement was bound.
        let originals: Vec<String> = statements
            .iter()
            .map(|s| {
                let text = self.consoles[ix].editor.read(cx).content.clone();
                text.get(s.start..s.end).unwrap_or("").to_owned()
            })
            .collect();
        let key = (source.to_owned(), id);
        let history = self.history.clone();
        let history_key = source.to_owned();
        let work = async move {
            let mut done = Finished::default();
            let conn = match existing {
                Some(conn) => conn,
                None => {
                    let connected = match password.resolve() {
                        Ok(password) => {
                            Connection::connect(&data_source, password.as_deref()).await
                        }
                        Err(error) => Err(error),
                    };
                    match connected {
                        Ok(conn) => Arc::new(conn),
                        Err(error) => {
                            done.failure = Some((
                                error,
                                statements
                                    .first()
                                    .map(|s| s.text.clone())
                                    .unwrap_or_default(),
                            ));
                            return done;
                        }
                    }
                }
            };
            for (stmt, original) in statements.into_iter().zip(originals) {
                let outcome = conn.execute(&stmt.text, limit).await;
                let (rows, error, elapsed_ms) = match &outcome {
                    Ok(r) => (
                        Some(if r.columns.is_empty() {
                            r.affected.unwrap_or(0)
                        } else {
                            r.rows.len() as u64
                        }),
                        None,
                        r.elapsed_ms,
                    ),
                    Err(e) => (None, Some(e.message.clone()), 0.),
                };
                history.record(
                    &history_key,
                    &HistoryEntry {
                        at: HistoryEntry::now(),
                        statement: stmt.text.clone(),
                        elapsed_ms,
                        rows,
                        error,
                    },
                );
                match outcome {
                    Ok(result) => {
                        done.outcomes.push(Outcome {
                            stmt,
                            original,
                            rows,
                            elapsed_ms,
                            error: None,
                        });
                        done.results.push(result);
                    }
                    Err(error) => {
                        done.failure = Some((error.clone(), stmt.text.clone()));
                        done.outcomes.push(Outcome {
                            stmt,
                            original,
                            rows: None,
                            elapsed_ms: 0.,
                            error: Some(error),
                        });
                        break;
                    }
                }
            }
            done.conn = Some(conn);
            done
        };
        let task = self.spawn_db(cx, work, move |this, out, cx| {
            let Some(ix) = this.console_index(&key.0, key.1) else {
                return;
            };
            let Some(done) = out else {
                this.consoles[ix].running = false;
                this.consoles[ix].task = None;
                this.consoles[ix].error = Some((DbError::default(), String::new()));
                cx.notify();
                return;
            };
            this.mark_outcomes(ix, &done.outcomes, cx);
            let console = &mut this.consoles[ix];
            console.running = false;
            console.task = None;
            if let Some(conn) = done.conn.clone() {
                console.conn = Some(conn);
            }
            if !done.results.is_empty() {
                // Statements that return rows get a tab each; when none does,
                // the last one (its status) is what is shown.
                let mut shown: Vec<Arc<QueryResult>> = done
                    .results
                    .iter()
                    .filter(|r| !r.columns.is_empty())
                    .cloned()
                    .map(Arc::new)
                    .collect();
                if shown.is_empty() {
                    shown.extend(done.results.last().cloned().map(Arc::new));
                }
                let last = shown.len() - 1;
                console.all = shown;
                console.select_result(last);
                console.note = None;
                console.export_sql = paging.as_ref().map(table_sql);
                console.paging = paging;
            }
            if let Some(failure) = done.failure {
                console.error = Some(failure);
            }
            if let Some(conn) = done.conn {
                this.load_catalog(&key.0, conn, cx);
            }
            cx.notify();
        });
        self.consoles[ix].task = Some(task);
        cx.notify();
    }

    /// Writes each statement's outcome into the editor: a note after it
    /// (✓ time and rows, or ✗ the error) and, for an error, an underline at
    /// the position the server reported.
    fn mark_outcomes(&mut self, ix: usize, outcomes: &[Outcome], cx: &mut Context<Self>) {
        let editor = self.consoles[ix].editor.clone();
        editor.update(cx, |input, cx| {
            let mut analysis = input.editor_analysis().clone();
            analysis.diagnostics.clear();
            for o in outcomes.iter().filter(|o| o.stmt.mark) {
                analysis.hints.retain(|h| h.pos != o.stmt.end);
                let text = match (&o.error, o.rows) {
                    (Some(e), _) => format!(
                        "✗ {}",
                        e.message
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(70)
                            .collect::<String>()
                    ),
                    (None, Some(rows)) => format!("✓ {:.0} ms · {rows} rows", o.elapsed_ms),
                    _ => format!("✓ {:.0} ms", o.elapsed_ms),
                };
                analysis.hints.push(crate::editor_analysis::Hint {
                    pos: o.stmt.end,
                    ok: o.error.is_none(),
                    text,
                });
                if let Some(error) = &o.error {
                    // The server's position is into what it ran; when parameters
                    // were bound that is not the editor's text, so underline all of it.
                    let content = input.content.clone();
                    let (from, to) = match error.position {
                        Some(at) if o.original == o.stmt.text => {
                            let from = (o.stmt.start + at).min(content.len());
                            let to = content[from..]
                                .char_indices()
                                .find(|(i, c)| *i > 0 && !(c.is_alphanumeric() || *c == '_'))
                                .map_or(content.len(), |(i, _)| from + i)
                                .min(o.stmt.end.max(from + 1).min(content.len()));
                            (from, to.max((from + 1).min(content.len())))
                        }
                        _ => (o.stmt.start, o.stmt.end),
                    };
                    analysis
                        .diagnostics
                        .push(crate::editor_analysis::Diagnostic {
                            from,
                            to,
                            severity: "error".into(),
                            message: error.message.clone(),
                        });
                }
            }
            input.set_editor_analysis(analysis, cx);
        });
    }

    /// Reformats the selection, or all of the console.
    fn format_console(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.active_index() else {
            return;
        };
        let editor = self.consoles[ix].editor.clone();
        let (text, selection) = {
            let input = editor.read(cx);
            (input.content.to_string(), input.selection_range())
        };
        let range = (!selection.is_empty()).then_some((selection.start, selection.end));
        let edits = format::format_sql(&text, range);
        let (Some(first), Some(last)) = (edits.first(), edits.last()) else {
            return;
        };
        let (from, to) = (first.from, last.to);
        let mut formatted = String::new();
        let mut at = from;
        for e in &edits {
            formatted.push_str(&text[at..e.from]);
            formatted.push_str(&e.insert);
            at = e.to;
        }
        if formatted == text[from..to] {
            return;
        }
        let cursor = formatted.len();
        editor.update(cx, |input, cx| {
            input.replace_bytes(from..to, &formatted, cursor, cx)
        });
    }

    /// Reads the page `paging` describes into console `ix`.
    fn show_page(&mut self, ix: usize, paging: Paging, cx: &mut Context<Self>) {
        let (source, id) = (self.consoles[ix].source.clone(), self.consoles[ix].id);
        let sql = page_sql(&paging, self.limit);
        let stmt = Stmt {
            text: sql,
            start: 0,
            end: 0,
            mark: false,
        };
        self.execute(&source, id, vec![stmt], Some(paging), cx);
    }

    fn change_page(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(ix) = self.active_index() else {
            return;
        };
        let Some(mut paging) = self.consoles[ix].paging.clone() else {
            return;
        };
        let next = paging.page as isize + delta;
        if next < 0 {
            return;
        }
        paging.page = next as usize;
        self.show_page(ix, paging, cx);
    }

    /// Header click in a table view: ascending, descending, then unsorted.
    fn sort_by(&mut self, column: usize, cx: &mut Context<Self>) {
        let Some(ix) = self.active_index() else {
            return;
        };
        let Some(mut paging) = self.consoles[ix].paging.clone() else {
            return;
        };
        let Some(name) = self.consoles[ix]
            .result
            .as_ref()
            .and_then(|r| r.columns.get(column))
            .map(|c| c.name.clone())
        else {
            return;
        };
        paging.sort = next_sort(paging.sort.take(), name);
        paging.page = 0;
        self.show_page(ix, paging, cx);
    }

    fn open_rename(&mut self, source: &str, id: u32, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .saved
            .get(source)
            .and_then(|l| l.iter().find(|(i, _)| *i == id))
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| format!("Console {id}"));
        let text = self.console_store.name(source, id).unwrap_or(current);
        self.rename_input
            .update(cx, |input, cx| input.set_text(text, cx));
        self.rename = Some((source.to_owned(), id));
        window.focus(&self.rename_input.focus_handle(cx), cx);
        cx.notify();
    }

    /// Applies the name in the rename box; blank goes back to the automatic title.
    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some((source, id)) = self.rename.take() else {
            return;
        };
        let name = self.rename_input.read(cx).content.trim().to_owned();
        self.console_store
            .set_name(&source, id, (!name.is_empty()).then_some(name.as_str()));
        if let Some(ix) = self.console_index(&source, id) {
            self.consoles[ix].name = (!name.is_empty()).then_some(name);
        }
        self.refresh_saved(&source);
        cx.notify();
    }

    /// Writes the active console to a `.sql` file of the user's choosing.
    fn save_console_file(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.active_index() else {
            return;
        };
        let console = &self.consoles[ix];
        let text = console.editor.read(cx).content.to_string();
        let (source, id) = (console.source.clone(), console.id);
        let stem: String = self
            .tab_title(console, cx)
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let directory = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Documents"))
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let picker = cx.prompt_for_new_path(&directory, Some(&format!("{stem}.sql")));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picker.await else {
                return;
            };
            let outcome = std::fs::write(&path, text);
            let _ = this.update(cx, |this, cx| {
                if let Some(ix) = this.console_index(&source, id) {
                    this.consoles[ix].note = Some(match outcome {
                        Ok(()) => format!("Saved to {}", path.display()),
                        Err(error) => format!("Could not save: {error}"),
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_results(&mut self, cx: &mut Context<Self>) {
        self.results_hidden = !self.results_hidden;
        self.save_state();
        cx.notify();
    }

    /// The table's `CREATE` script, in the preview.
    fn show_ddl(
        &mut self,
        source: &str,
        schema: &str,
        table: &str,
        kind: TableKind,
        cx: &mut Context<Self>,
    ) {
        let target = DdlTarget::Table {
            schema: schema.to_owned(),
            table: table.to_owned(),
            kind,
        };
        self.fetch_ddl(source, target, DdlThen::Preview, cx);
    }

    fn routine_target(&self, source: &str, schema: &str, ix: usize) -> Option<DdlTarget> {
        let routine = self.schema_node(source, schema)?.objects.routines.get(ix)?;
        Some(DdlTarget::Routine {
            oid: routine.oid.clone(),
            label: format!("{schema}.{}({})", routine.name, routine.arguments),
        })
    }

    fn sequence_target(&self, source: &str, schema: &str, ix: usize) -> Option<DdlTarget> {
        let sequence = self
            .schema_node(source, schema)?
            .objects
            .sequences
            .get(ix)?;
        Some(DdlTarget::Sequence {
            schema: schema.to_owned(),
            name: sequence.name.clone(),
        })
    }

    fn schema_node(&self, source: &str, schema: &str) -> Option<&SchemaNode> {
        self.nodes
            .get(source)?
            .schemas
            .as_ref()?
            .iter()
            .find(|s| s.name == schema)
    }

    /// Fetches a CREATE script and shows it, copies it, or puts it in the console.
    fn fetch_ddl(
        &mut self,
        source: &str,
        target: DdlTarget,
        then: DdlThen,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.nodes.get(source).and_then(|n| n.conn.clone()) else {
            self.notice = Some("Connect first".into());
            cx.notify();
            return;
        };
        let title = match &target {
            DdlTarget::Table { schema, table, .. } => format!("{schema}.{table}"),
            DdlTarget::Routine { label, .. } => label.clone(),
            DdlTarget::Sequence { schema, name } => format!("{schema}.{name}"),
        };
        let work = async move {
            match target {
                DdlTarget::Table {
                    schema,
                    table,
                    kind,
                } => conn.ddl(&schema, &table, kind).await,
                DdlTarget::Routine { oid, .. } => conn.routine_ddl(&oid).await,
                DdlTarget::Sequence { schema, name } => conn.sequence_ddl(&schema, &name).await,
            }
        };
        self.spawn_db(cx, work, move |this, out, cx| {
            let text = match out {
                Some(Ok(text)) => text,
                Some(Err(error)) => format!("-- {}", error.message),
                None => "-- The request stopped".into(),
            };
            match then {
                DdlThen::Preview => this.preview = Some((title, text)),
                DdlThen::Clipboard => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    this.notice = Some("DDL copied".into());
                }
                DdlThen::Console => {
                    if let Some(ix) = this.active_index() {
                        this.append_to_console(ix, &text, cx);
                    }
                }
            }
            cx.notify();
        })
        .detach();
    }

    /// Adds `text` after what the console holds (or as all of it, when empty).
    fn append_to_console(&mut self, ix: usize, text: &str, cx: &mut Context<Self>) {
        let editor = self.consoles[ix].editor.clone();
        let (empty, end) = {
            let input = editor.read(cx);
            (input.content.trim().is_empty(), input.content.len())
        };
        let (at, text) = if empty {
            (0, text.trim_end().to_owned())
        } else {
            (end, format!("\n\n{}", text.trim_end()))
        };
        editor.update(cx, |input, cx| {
            input.replace_bytes(at..at, &text, text.len(), cx)
        });
    }

    /// The reference to a tree row as SQL would write it.
    fn reference(&self, row: &TreeRow) -> Option<String> {
        use monocode_core::db::introspect::quote;
        Some(match row {
            TreeRow::Source(id) => self.source(id)?.name.clone(),
            TreeRow::Schema(_, schema) => quote(schema),
            TreeRow::Table(source, schema, ix) => {
                let table = self
                    .schema_node(source, schema)?
                    .tables
                    .as_ref()?
                    .get(*ix)?;
                format!("{}.{}", quote(schema), quote(&table.info.name))
            }
            TreeRow::Routine(source, schema, ix) => {
                let r = self
                    .schema_node(source, schema)?
                    .objects
                    .routines
                    .get(*ix)?;
                format!("{}.{}", quote(schema), quote(&r.name))
            }
            TreeRow::Sequence(source, schema, ix) => {
                let q = self
                    .schema_node(source, schema)?
                    .objects
                    .sequences
                    .get(*ix)?;
                format!("{}.{}", quote(schema), quote(&q.name))
            }
            TreeRow::Column(_, column) => quote(&column.name),
            TreeRow::Leaf { label, .. } => label.clone(),
            _ => return None,
        })
    }

    /// The context menu of a tree row.
    fn open_menu(
        &mut self,
        row: TreeRow,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut items: Vec<MenuItem> = Vec::new();
        let copy = || MenuAction::new("copy-name", "Copy name").into();
        match &row {
            TreeRow::Source(id) => {
                let live = self.is_connected(id);
                items.push(
                    MenuAction::new(
                        if live { "disconnect" } else { "connect" },
                        if live { "Disconnect" } else { "Connect" },
                    )
                    .into(),
                );
                items.push(MenuAction::new("refresh", "Refresh").into());
                items.push(MenuItem::Separator);
                items.push(MenuAction::new("console", "New console").into());
                items.push(MenuItem::Separator);
                items.push(MenuAction::new("edit", "Edit…").into());
                items.push(MenuAction::new("duplicate", "Duplicate").into());
                items.push(MenuAction::new("remove", "Remove…").danger().into());
            }
            TreeRow::Schema(..) => {
                items.push(MenuAction::new("refresh", "Refresh").into());
                items.push(MenuAction::new("console", "New console").into());
                items.push(MenuItem::Separator);
                items.push(copy());
            }
            TreeRow::Table(..) => {
                items.push(MenuAction::new("data", "Open data").into());
                items.push(MenuAction::new("refresh", "Refresh").into());
                items.push(MenuItem::Separator);
                items.push(MenuAction::new("ddl", "Show DDL").into());
                items.push(MenuAction::new("ddl-copy", "Copy DDL").into());
                items.push(MenuAction::new("ddl-console", "DDL to console").into());
                items.push(MenuItem::Separator);
                items.push(MenuAction::new("select-console", "Select * in console").into());
                items.push(copy());
            }
            TreeRow::Routine(..) | TreeRow::Sequence(..) => {
                items.push(MenuAction::new("ddl", "Show DDL").into());
                items.push(MenuAction::new("ddl-copy", "Copy DDL").into());
                items.push(MenuAction::new("ddl-console", "DDL to console").into());
                items.push(MenuItem::Separator);
                items.push(copy());
            }
            TreeRow::Column(..) | TreeRow::Leaf { .. } => items.push(copy()),
            TreeRow::Consoles(_) => items.push(MenuAction::new("console", "New console").into()),
            TreeRow::SavedConsole(..) => {
                items.push(MenuAction::new("open-console", "Open").into());
                items.push(MenuAction::new("rename", "Rename…").into());
                items.push(MenuAction::new("delete-console", "Delete").danger().into());
            }
            _ => return,
        }
        self.menu = None;
        let menu = cx.new(|cx| Menu::new("Data source actions", items, window, cx));
        let events = cx.subscribe_in(&menu, window, |this, _, event: &MenuEvent, window, cx| {
            let Some(open) = this.menu.take() else {
                return;
            };
            if let MenuEvent::Picked(id, _) = event {
                this.run_menu_action(id, &open.row, window, cx);
            }
            cx.notify();
        });
        self.menu = Some(OpenMenu {
            row,
            anchor: at,
            menu,
            _events: events,
        });
        cx.notify();
    }

    fn run_menu_action(
        &mut self,
        id: &str,
        row: &TreeRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = match row {
            TreeRow::Source(s)
            | TreeRow::Schema(s, _)
            | TreeRow::Table(s, _, _)
            | TreeRow::Routine(s, _, _)
            | TreeRow::Sequence(s, _, _)
            | TreeRow::Consoles(s)
            | TreeRow::SavedConsole(s, _, _, _) => Some(s.clone()),
            _ => None,
        };
        match id {
            "connect" => {
                if let TreeRow::Source(id) = row {
                    self.select_source(id, cx);
                    self.expanded.insert(format!("s:{id}"));
                    self.expanded.insert(format!("c:{id}"));
                    self.connect_source(id, cx);
                }
            }
            "disconnect" => {
                if let TreeRow::Source(id) = row {
                    self.disconnect_source(id, cx);
                }
            }
            "refresh" => match row {
                TreeRow::Source(id) => {
                    self.expanded.insert(format!("s:{id}"));
                    self.refresh_source(id, cx);
                }
                TreeRow::Schema(source, schema) => self.load_tables(source, schema, cx),
                TreeRow::Table(source, schema, ix) => {
                    let name = self
                        .schema_node(source, schema)
                        .and_then(|s| s.tables.as_ref())
                        .and_then(|t| t.get(*ix))
                        .map(|t| t.info.name.clone());
                    if let Some(name) = name {
                        self.load_columns(source, schema, &name, cx);
                    }
                }
                _ => {}
            },
            "console" => {
                if let Some(source) = source {
                    self.select_source(&source, cx);
                    self.new_console(window, cx);
                }
            }
            "edit" => {
                if let TreeRow::Source(id) = row {
                    self.open_dialog(Some(id.clone()), window, cx);
                }
            }
            "duplicate" => {
                if let TreeRow::Source(id) = row
                    && let Some(mut copy) = self.source(id).cloned()
                {
                    copy.id = uuid_like();
                    copy.name = format!("{} (copy)", copy.name);
                    // A saved password is copied to the new entry's own slot.
                    if copy.save_password
                        && let Some(password) = source::saved_password(id)
                    {
                        let _ = source::save_password(&copy.id, &password);
                    }
                    self.save_source(copy, None, cx);
                }
            }
            "remove" => {
                if let TreeRow::Source(id) = row {
                    self.confirm_remove = Some(id.clone());
                }
            }
            "data" => {
                if let TreeRow::Table(source, schema, ix) = row
                    && let Some(name) = self
                        .schema_node(source, schema)
                        .and_then(|s| s.tables.as_ref())
                        .and_then(|t| t.get(*ix))
                        .map(|t| t.info.name.clone())
                {
                    self.open_table(source, schema, &name, window, cx);
                }
            }
            "select-console" => {
                if let TreeRow::Table(..) = row
                    && let Some(name) = self.reference(row)
                {
                    self.select_source(source.as_deref().unwrap_or_default(), cx);
                    if let Some(ix) = self.active_index() {
                        self.append_to_console(ix, &format!("select * from {name} limit 100;"), cx);
                    }
                }
            }
            "ddl" | "ddl-copy" | "ddl-console" => {
                let then = match id {
                    "ddl" => DdlThen::Preview,
                    "ddl-copy" => DdlThen::Clipboard,
                    _ => DdlThen::Console,
                };
                let target = match row {
                    TreeRow::Table(source, schema, ix) => self
                        .schema_node(source, schema)
                        .and_then(|s| s.tables.as_ref())
                        .and_then(|t| t.get(*ix))
                        .map(|t| DdlTarget::Table {
                            schema: schema.clone(),
                            table: t.info.name.clone(),
                            kind: t.info.kind,
                        }),
                    TreeRow::Routine(source, schema, ix) => {
                        self.routine_target(source, schema, *ix)
                    }
                    TreeRow::Sequence(source, schema, ix) => {
                        self.sequence_target(source, schema, *ix)
                    }
                    _ => None,
                };
                if let (Some(source), Some(target)) = (source, target) {
                    if then == DdlThen::Console {
                        self.select_source(&source, cx);
                    }
                    self.fetch_ddl(&source, target, then, cx);
                }
            }
            "copy-name" => {
                if let Some(name) = self.reference(row) {
                    cx.write_to_clipboard(ClipboardItem::new_string(name));
                }
            }
            "open-console" => {
                if let TreeRow::SavedConsole(source, id, ..) = row {
                    self.open_console(source, *id, window, cx);
                }
            }
            "rename" => {
                if let TreeRow::SavedConsole(source, id, ..) = row {
                    self.open_rename(source, *id, window, cx);
                }
            }
            "delete-console" => {
                if let TreeRow::SavedConsole(source, id, ..) = row {
                    self.delete_console(source, *id, window, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// The plan of the statement at the cursor. `analyze` really runs it, so
    /// it only takes statements that read.
    fn explain(&mut self, source: &str, id: u32, analyze: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.console_index(source, id) else {
            return;
        };
        if self.consoles[ix].running {
            return;
        }
        let editor = self.consoles[ix].editor.read(cx);
        let text = editor.content.to_string();
        let selection = editor.selection_range();
        let Some(statement) = (if selection.is_empty() {
            split::at_cursor(&text, selection.start).map(|s| s.text)
        } else {
            split::split(&text[selection])
                .into_iter()
                .next()
                .map(|s| s.text)
        }) else {
            return;
        };
        let first = statement
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase();
        if analyze && !matches!(first.as_str(), "select" | "with" | "values" | "table") {
            self.notice =
                Some("Analyze runs the statement; it only accepts queries that read".into());
            cx.notify();
            return;
        }
        let Some(data_source) = self.source(source).cloned() else {
            return;
        };
        let password = self.password_for(&data_source);
        let existing = self.consoles[ix].conn.clone().filter(|c| !c.is_closed());
        self.consoles[ix].running = true;
        self.consoles[ix].error = None;
        let key = (source.to_owned(), id);
        let sql = format!(
            "explain ({}) {statement}",
            if analyze { "analyze, buffers" } else { "costs" }
        );
        let work = async move {
            let conn = match existing {
                Some(conn) => conn,
                None => Arc::new(
                    Connection::connect(
                        &data_source,
                        password.resolve().map_err(|e| (None, e))?.as_deref(),
                    )
                    .await
                    .map_err(|e| (None, e))?,
                ),
            };
            match conn.execute(&sql, 10_000).await {
                Ok(result) => Ok((conn, result)),
                Err(error) => Err((Some(conn), error)),
            }
        };
        let task = self.spawn_db(cx, work, move |this, out, cx| {
            let Some(ix) = this.console_index(&key.0, key.1) else {
                return;
            };
            this.consoles[ix].running = false;
            this.consoles[ix].task = None;
            match out {
                Some(Ok((conn, result))) => {
                    this.consoles[ix].conn = Some(conn);
                    let plan: Vec<String> = result
                        .rows
                        .iter()
                        .filter_map(|r| r.first().cloned().flatten())
                        .collect();
                    this.preview = Some((
                        if analyze { "Plan (analyzed)" } else { "Plan" }.to_owned(),
                        plan.join("\n"),
                    ));
                }
                Some(Err((conn, error))) => {
                    if conn.is_some() {
                        this.consoles[ix].conn = conn;
                    }
                    this.results_hidden = false;
                    this.consoles[ix].error = Some((error, String::new()));
                }
                None => {}
            }
            cx.notify();
        });
        self.consoles[ix].task = Some(task);
        cx.notify();
    }

    fn open_history(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.selected.clone() else {
            return;
        };
        self.history_entries = self.history.list(&source);
        self.history_filter
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        self.history_open = true;
        cx.notify();
    }

    /// Puts a history statement into the active console at the cursor.
    fn insert_from_history(
        &mut self,
        statement: &str,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history_open = false;
        let Some(ix) = self.active_index() else {
            return;
        };
        let (source, id) = (self.consoles[ix].source.clone(), self.consoles[ix].id);
        let editor = self.consoles[ix].editor.clone();
        let (at, empty) = {
            let input = editor.read(cx);
            (input.selection_range().end, input.content.trim().is_empty())
        };
        let text = if empty {
            statement.to_owned()
        } else {
            format!("\n\n{statement}")
        };
        let end = editor.read(cx).content.len();
        let at = if empty { 0 } else { end.max(at) };
        editor.update(cx, |input, cx| {
            input.replace_bytes(at..at, &text, text.len(), cx);
            if empty {
                let length = input.content.len();
                input.select_range(0..length, cx);
            }
        });
        window.focus(&editor.focus_handle(cx), cx);
        if run {
            self.run(&source, id, false, cx);
        }
        cx.notify();
    }

    fn cancel(&mut self, source: &str, id: u32) {
        if let Some(ix) = self.console_index(source, id)
            && let Some(conn) = &self.consoles[ix].conn
        {
            conn.cancel();
        }
    }

    // ---- tree ----

    fn connect_source(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(data_source) = self.source(id).cloned() else {
            return;
        };
        let node = self
            .nodes
            .entry(id.to_owned())
            .or_insert_with(SourceNode::new);
        if matches!(node.link, Link::Connecting)
            || (matches!(node.link, Link::Ready)
                && node.conn.as_ref().is_some_and(|c| !c.is_closed()))
        {
            return;
        }
        node.link = Link::Connecting;
        let password = self.password_for(&data_source);
        let key = id.to_owned();
        let work = async move {
            let conn = Connection::connect(&data_source, password.resolve()?.as_deref()).await?;
            let schemas = conn.schemas(false).await?;
            let catalog = conn.catalog().await.unwrap_or_default();
            Ok::<_, DbError>((Arc::new(conn), schemas, catalog))
        };
        self.spawn_db(cx, work, move |this, out, cx| {
            match out {
                Some(Ok((conn, schemas, catalog))) => {
                    if let Ok(mut shared) = this.catalog_for(&key).write() {
                        *shared = sql::Catalog::from_entries(catalog);
                    }
                    this.catalog_started.insert(key.clone());
                    let node = this
                        .nodes
                        .entry(key.clone())
                        .or_insert_with(SourceNode::new);
                    node.conn = Some(conn);
                    node.link = Link::Ready;
                    node.schemas = Some(
                        schemas
                            .into_iter()
                            .map(|name| SchemaNode {
                                name,
                                tables: None,
                                objects: SchemaObjects::default(),
                            })
                            .collect(),
                    );
                }
                Some(Err(error)) => {
                    let node = this
                        .nodes
                        .entry(key.clone())
                        .or_insert_with(SourceNode::new);
                    node.link = Link::Failed(error.message);
                }
                None => {
                    let node = this
                        .nodes
                        .entry(key.clone())
                        .or_insert_with(SourceNode::new);
                    node.link = Link::Failed("The connection task stopped".into());
                }
            }
            cx.notify();
        })
        .detach();
        cx.notify();
    }

    /// Loads the completion catalog once per connection, over a connection
    /// that is already open (a console's).
    fn load_catalog(&mut self, source: &str, conn: Arc<Connection>, cx: &mut Context<Self>) {
        if !self.catalog_started.insert(source.to_owned()) {
            return;
        }
        let key = source.to_owned();
        let work = async move { conn.catalog().await };
        self.spawn_db(cx, work, move |this, out, cx| {
            match out {
                Some(Ok(entries)) => {
                    if let Ok(mut shared) = this.catalog_for(&key).write() {
                        *shared = sql::Catalog::from_entries(entries);
                    }
                }
                _ => {
                    this.catalog_started.remove(&key);
                }
            }
            cx.notify();
        })
        .detach();
    }

    fn is_connected(&self, id: &str) -> bool {
        self.nodes
            .get(id)
            .is_some_and(|n| n.conn.as_ref().is_some_and(|c| !c.is_closed()))
            || self
                .consoles
                .iter()
                .any(|c| c.source == id && c.conn.as_ref().is_some_and(|c| !c.is_closed()))
    }

    fn disconnect_source(&mut self, id: &str, cx: &mut Context<Self>) {
        self.nodes.remove(id);
        self.expanded.retain(|k| {
            !k.starts_with(&format!("s:{id}"))
                && !k.starts_with(&format!("n:{id}/"))
                && !k.starts_with(&format!("t:{id}/"))
        });
        for console in self.consoles.iter_mut().filter(|c| c.source == id) {
            if console.running
                && let Some(conn) = &console.conn
            {
                conn.cancel();
            }
            console.conn = None;
        }
        self.catalog_started.remove(id);
        cx.notify();
    }

    fn load_tables(&mut self, source: &str, schema: &str, cx: &mut Context<Self>) {
        let Some(conn) = self.nodes.get(source).and_then(|n| n.conn.clone()) else {
            return;
        };
        let (key, schema_name) = (source.to_owned(), schema.to_owned());
        let name = schema.to_owned();
        let work = async move {
            let tables = conn.tables(&name).await?;
            let objects = conn.schema_objects(&name).await?;
            Ok::<_, DbError>((tables, objects))
        };
        self.spawn_db(cx, work, move |this, out, cx| {
            if let Some(Ok((tables, objects))) = out
                && let Some(schemas) = this.nodes.get_mut(&key).and_then(|n| n.schemas.as_mut())
                && let Some(node) = schemas.iter_mut().find(|s| s.name == schema_name)
            {
                node.objects = objects;
                node.tables = Some(
                    tables
                        .into_iter()
                        .map(|info| TableNode {
                            info,
                            columns: None,
                            details: None,
                        })
                        .collect(),
                );
            }
            cx.notify();
        })
        .detach();
    }

    fn load_columns(&mut self, source: &str, schema: &str, table: &str, cx: &mut Context<Self>) {
        let Some(conn) = self.nodes.get(source).and_then(|n| n.conn.clone()) else {
            return;
        };
        let (key, schema_name, table_name) =
            (source.to_owned(), schema.to_owned(), table.to_owned());
        let (s, t) = (schema.to_owned(), table.to_owned());
        let work = async move {
            let columns = conn.columns(&s, &t).await?;
            let details = conn.table_details(&s, &t).await?;
            Ok::<_, DbError>((columns, details))
        };
        self.spawn_db(cx, work, move |this, out, cx| {
            if let Some(Ok((columns, details))) = out
                && let Some(schemas) = this.nodes.get_mut(&key).and_then(|n| n.schemas.as_mut())
                && let Some(node) = schemas.iter_mut().find(|s| s.name == schema_name)
                && let Some(tables) = node.tables.as_mut()
                && let Some(table) = tables.iter_mut().find(|t| t.info.name == table_name)
            {
                table.columns = Some(columns);
                table.details = Some(details);
            }
            cx.notify();
        })
        .detach();
    }

    fn refresh_source(&mut self, id: &str, cx: &mut Context<Self>) {
        self.nodes.remove(id);
        self.connect_source(id, cx);
    }

    fn rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        for source in &self.sources {
            rows.push(TreeRow::Source(source.id.clone()));
            if !self.expanded.contains(&format!("s:{}", source.id)) {
                continue;
            }
            rows.push(TreeRow::Consoles(source.id.clone()));
            if self.expanded.contains(&format!("c:{}", source.id)) {
                for (id, title) in self.saved.get(&source.id).into_iter().flatten() {
                    let open = self.console_index(&source.id, *id).is_some();
                    rows.push(TreeRow::SavedConsole(
                        source.id.clone(),
                        *id,
                        title.clone(),
                        open,
                    ));
                }
            }
            let Some(node) = self.nodes.get(&source.id) else {
                continue;
            };
            match &node.link {
                Link::Connecting => rows.push(TreeRow::Note(1, "Connecting…".into())),
                Link::Failed(message) => rows.push(TreeRow::Note(1, message.clone())),
                _ => {}
            }
            for schema in node.schemas.iter().flatten() {
                rows.push(TreeRow::Schema(source.id.clone(), schema.name.clone()));
                if !self
                    .expanded
                    .contains(&format!("n:{}/{}", source.id, schema.name))
                {
                    continue;
                }
                let Some(tables) = &schema.tables else {
                    rows.push(TreeRow::Note(2, "Loading…".into()));
                    continue;
                };
                let empty = tables.is_empty()
                    && schema.objects.routines.is_empty()
                    && schema.objects.sequences.is_empty();
                if empty {
                    rows.push(TreeRow::Note(2, "Empty schema".into()));
                }
                for folder in SchemaFolder::ALL {
                    let members: Vec<usize> = match folder {
                        SchemaFolder::Tables
                        | SchemaFolder::Views
                        | SchemaFolder::MaterializedViews => tables
                            .iter()
                            .enumerate()
                            .filter(|(_, t)| folder.holds(t.info.kind))
                            .map(|(i, _)| i)
                            .collect(),
                        SchemaFolder::Functions => schema
                            .objects
                            .routines
                            .iter()
                            .enumerate()
                            .filter(|(_, r)| r.kind == RoutineKind::Function)
                            .map(|(i, _)| i)
                            .collect(),
                        SchemaFolder::Procedures => schema
                            .objects
                            .routines
                            .iter()
                            .enumerate()
                            .filter(|(_, r)| r.kind == RoutineKind::Procedure)
                            .map(|(i, _)| i)
                            .collect(),
                        SchemaFolder::Sequences => (0..schema.objects.sequences.len()).collect(),
                    };
                    if members.is_empty() {
                        continue;
                    }
                    rows.push(TreeRow::Folder(
                        source.id.clone(),
                        schema.name.clone(),
                        folder,
                    ));
                    if !self
                        .expanded
                        .contains(&folder_key(&source.id, &schema.name, folder))
                    {
                        continue;
                    }
                    for ix in members {
                        match folder {
                            SchemaFolder::Functions | SchemaFolder::Procedures => rows
                                .push(TreeRow::Routine(source.id.clone(), schema.name.clone(), ix)),
                            SchemaFolder::Sequences => rows.push(TreeRow::Sequence(
                                source.id.clone(),
                                schema.name.clone(),
                                ix,
                            )),
                            _ => {
                                let table = &tables[ix];
                                rows.push(TreeRow::Table(
                                    source.id.clone(),
                                    schema.name.clone(),
                                    ix,
                                ));
                                let open =
                                    format!("t:{}/{}/{}", source.id, schema.name, table.info.name);
                                if !self.expanded.contains(&open) {
                                    continue;
                                }
                                let Some(columns) = &table.columns else {
                                    rows.push(TreeRow::Note(4, "Loading…".into()));
                                    continue;
                                };
                                for kind in GroupKind::ALL {
                                    let leaves = group_rows(columns, table.details.as_ref(), kind);
                                    if leaves.is_empty() && kind != GroupKind::Columns {
                                        continue;
                                    }
                                    rows.push(TreeRow::Group(
                                        source.id.clone(),
                                        schema.name.clone(),
                                        table.info.name.clone(),
                                        kind,
                                    ));
                                    if self.expanded.contains(&group_key(
                                        &source.id,
                                        &schema.name,
                                        &table.info.name,
                                        kind,
                                    )) {
                                        rows.extend(leaves);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        rows
    }

    fn toggle(&mut self, key: String) -> bool {
        if self.expanded.remove(&key) {
            false
        } else {
            self.expanded.insert(key);
            true
        }
    }

    fn click_row(
        &mut self,
        row: &TreeRow,
        clicks: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match row {
            TreeRow::Source(id) => {
                self.select_source(id, cx);
                if self.toggle(format!("s:{id}")) {
                    self.expanded.insert(format!("c:{id}"));
                    self.connect_source(id, cx);
                }
            }
            TreeRow::Schema(source, schema) => {
                if self.toggle(format!("n:{source}/{schema}")) {
                    for folder in [SchemaFolder::Tables, SchemaFolder::Views] {
                        self.expanded.insert(folder_key(source, schema, folder));
                    }
                    self.load_tables(source, schema, cx);
                }
            }
            TreeRow::Folder(source, schema, folder) => {
                self.toggle(folder_key(source, schema, *folder));
            }
            TreeRow::Routine(source, schema, ix) => {
                if clicks >= 2
                    && let Some(target) = self.routine_target(source, schema, *ix)
                {
                    self.fetch_ddl(source, target, DdlThen::Preview, cx);
                }
            }
            TreeRow::Sequence(source, schema, ix) => {
                if clicks >= 2
                    && let Some(target) = self.sequence_target(source, schema, *ix)
                {
                    self.fetch_ddl(source, target, DdlThen::Preview, cx);
                }
            }
            TreeRow::Table(source, schema, ix) => {
                let Some(table) = self
                    .nodes
                    .get(source)
                    .and_then(|n| n.schemas.as_ref())
                    .and_then(|s| s.iter().find(|s| &s.name == schema))
                    .and_then(|s| s.tables.as_ref())
                    .and_then(|t| t.get(*ix))
                else {
                    return;
                };
                let name = table.info.name.clone();
                if clicks >= 2 {
                    self.open_table(source, schema, &name, window, cx);
                } else if self.toggle(format!("t:{source}/{schema}/{name}")) {
                    self.expanded
                        .insert(group_key(source, schema, &name, GroupKind::Columns));
                    self.load_columns(source, schema, &name, cx);
                }
            }
            TreeRow::Group(source, schema, table, kind) => {
                self.toggle(group_key(source, schema, table, *kind));
            }
            TreeRow::Leaf { label, full, .. } => {
                self.preview = Some((label.clone(), full.clone()));
            }
            TreeRow::Consoles(source) => {
                self.toggle(format!("c:{source}"));
            }
            TreeRow::SavedConsole(source, id, ..) => self.open_console(source, *id, window, cx),
            TreeRow::Column(..) | TreeRow::Note(..) => {}
        }
        self.save_state();
        cx.notify();
    }

    fn select_source(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.selected.as_deref() != Some(id) {
            self.selected = Some(id.to_owned());
            self.ensure_consoles(id, cx);
        }
    }

    /// Double click on a table: its first rows, in the active console.
    fn open_table(
        &mut self,
        source: &str,
        schema: &str,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_source(source, cx);
        let sql = format!(
            "select * from {}.{}",
            quote_ident(schema),
            quote_ident(table)
        );
        let Some(ix) = self.active_index() else {
            return;
        };
        self.consoles[ix]
            .editor
            .update(cx, |input, cx| input.set_text(sql, cx));
        let paging = Paging {
            schema: schema.to_owned(),
            table: table.to_owned(),
            page: 0,
            sort: None,
        };
        self.show_page(ix, paging, cx);
        window.focus(&self.consoles[ix].editor.focus_handle(cx), cx);
    }

    // ---- data source dialog ----

    fn open_dialog(
        &mut self,
        editing: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = editing.as_deref().and_then(|id| self.source(id)).cloned();
        let has_saved = existing.as_ref().is_some_and(|s| s.save_password);
        let is_edit = existing.is_some();
        let data_source = existing.unwrap_or_else(|| DataSource::new(""));
        let dialog = cx.new(|cx| SourceDialog::new(data_source, is_edit, has_saved, cx));
        let subscription = cx.subscribe_in(
            &dialog,
            window,
            |this, _, event: &DialogEvent, window, cx| match event {
                DialogEvent::Close => this.close_dialog(window, cx),
                DialogEvent::Save { source, password } => {
                    this.save_source(source.as_ref().clone(), password.clone(), cx);
                    this.close_dialog(window, cx);
                }
            },
        );
        window.focus(&dialog.focus_handle(cx), cx);
        self.dialog = Some((dialog, subscription));
        cx.notify();
    }

    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = None;
        window.focus(&self.focus_handle(cx), cx);
        cx.notify();
    }

    fn save_source(
        &mut self,
        data_source: DataSource,
        password: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let id = data_source.id.clone();
        // Only a source that kept a password has one to forget; asking the
        // keychain about any other would be a read (and a prompt) for nothing.
        let had_saved = self.source(&id).is_some_and(|s| s.save_password);
        if let Some(password) = password {
            if data_source.save_password {
                if let Err(error) = source::save_password(&id, &password) {
                    self.notice = Some(format!("The password was not saved: {error}"));
                }
                self.passwords.remove(&id);
            } else {
                self.passwords.insert(id.clone(), password);
            }
        }
        if !data_source.save_password && had_saved {
            source::forget_password(&id);
        }
        match self.sources.iter_mut().find(|s| s.id == id) {
            Some(existing) => *existing = data_source,
            None => self.sources.push(data_source),
        }
        if let Err(error) = self.store.save(&self.sources) {
            self.notice = Some(format!("Data sources were not saved: {error}"));
        }
        // Connection settings may have changed: start clean.
        self.disconnect_source(&id, cx);
        self.selected = Some(id.clone());
        self.ensure_consoles(&id, cx);
        self.save_state();
        cx.notify();
    }

    fn remove_source(&mut self, id: &str, cx: &mut Context<Self>) {
        let had_saved = self.source(id).is_some_and(|s| s.save_password);
        self.disconnect_source(id, cx);
        self.sources.retain(|s| s.id != id);
        self.consoles.retain(|c| c.source != id);
        self.console_store.delete_source(id);
        self.active.remove(id);
        if had_saved {
            source::forget_password(id);
        }
        self.passwords.remove(id);
        let _ = self.store.save(&self.sources);
        if self.selected.as_deref() == Some(id) {
            self.selected = self.sources.first().map(|s| s.id.clone());
            if let Some(next) = self.selected.clone() {
                self.ensure_consoles(&next, cx);
            }
        }
        self.confirm_remove = None;
        self.save_state();
        cx.notify();
    }

    // ---- grid ----

    fn cell_value(&self, ix: usize, row: usize, col: usize) -> Option<String> {
        self.consoles
            .get(ix)?
            .result
            .as_ref()?
            .rows
            .get(row)?
            .get(col)?
            .clone()
    }

    fn grid_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        let Some(ix) = self.active_index() else {
            return;
        };
        if event.keystroke.key == "escape" {
            cx.emit(Event::Close);
            cx.stop_propagation();
            return;
        }
        let Some(result) = self.consoles[ix].result.clone() else {
            return;
        };
        let (rows, cols) = (result.rows.len(), result.columns.len());
        if rows == 0 || cols == 0 {
            return;
        }
        let key = event.keystroke.key.as_str();
        let command = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;
        let (row, col) = self.consoles[ix].selected.unwrap_or((0, 0));
        match key {
            "c" if command => {
                let text = if event.keystroke.modifiers.shift {
                    grid::row_tsv(&result.rows[row.min(rows - 1)])
                } else {
                    self.cell_value(ix, row, col).unwrap_or_default()
                };
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            "up" | "down" | "left" | "right" | "pageup" | "pagedown" | "home" | "end" => {
                let page = ((self.grid_viewport.get().1 - grid::HEADER_HEIGHT) / grid::ROW_HEIGHT)
                    .max(1.) as usize;
                let next = match key {
                    "up" => (row.saturating_sub(1), col),
                    "down" => ((row + 1).min(rows - 1), col),
                    "left" => (row, col.saturating_sub(1)),
                    "right" => (row, (col + 1).min(cols - 1)),
                    "pageup" => (row.saturating_sub(page), col),
                    "pagedown" => ((row + page).min(rows - 1), col),
                    "home" => (0, col),
                    _ => (rows - 1, col),
                };
                self.consoles[ix].selected = Some(next);
                self.reveal_cell(ix, next);
            }
            "space" | "enter" => self.open_preview(ix, row, col),
            "escape" => cx.emit(Event::Close),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn open_preview(&mut self, ix: usize, row: usize, col: usize) {
        let Some(result) = self.consoles.get(ix).and_then(|c| c.result.clone()) else {
            return;
        };
        let title = result
            .columns
            .get(col)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let text = match self.cell_value(ix, row, col) {
            Some(value) => grid::pretty_json(&value).unwrap_or(value),
            None => "NULL".to_owned(),
        };
        self.preview = Some((title, text));
    }

    /// Total content width and row count of console `ix`'s grid.
    fn grid_size(&self, ix: usize) -> (f32, usize) {
        let console = &self.consoles[ix];
        (
            console.widths.iter().sum(),
            console.result.as_ref().map_or(0, |r| r.rows.len()),
        )
    }

    fn grid_max_scroll(&self, ix: usize) -> (f32, f32) {
        let (total, rows) = self.grid_size(ix);
        let (vw, vh) = self.grid_viewport.get();
        grid::max_scroll(total, rows, vw, vh)
    }

    fn scroll_grid_to(&mut self, ix: usize, x: f32, y: f32) {
        let (max_x, max_y) = self.grid_max_scroll(ix);
        let console = &mut self.consoles[ix];
        console.scroll_x = grid::clamp(x, max_x);
        console.scroll_y = grid::clamp(y, max_y);
    }

    fn reveal_cell(&mut self, ix: usize, (row, col): (usize, usize)) {
        let (vw, vh) = self.grid_viewport.get();
        let console = &self.consoles[ix];
        let left: f32 = console.widths.iter().take(col).sum();
        let width = console.widths.get(col).copied().unwrap_or(0.);
        let x = grid::reveal(console.scroll_x, left, width, (vw - grid::BAR).max(0.));
        let body = (vh - grid::HEADER_HEIGHT - grid::BAR).max(0.);
        let y = grid::reveal(
            console.scroll_y,
            row as f32 * grid::ROW_HEIGHT,
            grid::ROW_HEIGHT,
            body,
        );
        self.scroll_grid_to(ix, x, y);
    }

    // ---- export ----

    fn export_options(&self, cx: &Context<Self>) -> monocode_core::db::ExportOptions {
        let table = self.export_table.read(cx).content.trim().to_owned();
        monocode_core::db::ExportOptions {
            header: self.export_header,
            transpose: self.export_transpose,
            table: if table.is_empty() {
                "table_name".into()
            } else {
                table
            },
        }
    }

    /// Opens the export dialog, naming the table SQL inserts would target.
    fn open_export(&mut self, cx: &mut Context<Self>) {
        let table = self.active_console().and_then(|console| {
            console
                .paging
                .as_ref()
                .map(|p| {
                    format!(
                        "{}.{}",
                        monocode_core::db::introspect::quote(&p.schema),
                        monocode_core::db::introspect::quote(&p.table)
                    )
                })
                .or_else(|| {
                    console
                        .result
                        .as_ref()
                        .and_then(|r| analysis::table_of(&r.statement))
                })
        });
        self.export_table.update(cx, |input, cx| {
            input.set_text(table.unwrap_or_else(|| "table_name".into()), cx)
        });
        self.export_menu = true;
        cx.notify();
    }

    fn copy_result(&mut self, format: monocode_core::db::ExportFormat, cx: &mut Context<Self>) {
        let Some(result) = self.active_console().and_then(|c| c.result.clone()) else {
            return;
        };
        let options = self.export_options(cx);
        let columns: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
        let text = monocode_core::db::format_rows(format, &columns, &result.rows, &options);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        if let Some(ix) = self.active_index() {
            self.consoles[ix].note = Some(format!(
                "Copied {} rows as {}",
                result.rows.len(),
                format.label()
            ));
        }
        self.export_menu = false;
        cx.notify();
    }

    /// Asks for a file, then runs the console's last statement again and
    /// streams every row of it there, past the row limit.
    fn export_all(&mut self, format: monocode_core::db::ExportFormat, cx: &mut Context<Self>) {
        self.export_menu = false;
        let Some(ix) = self.active_index() else {
            return;
        };
        let console = &self.consoles[ix];
        let (Some(result), Some(conn)) = (console.result.clone(), console.conn.clone()) else {
            return;
        };
        let (source, id) = (console.source.clone(), console.id);
        let export_sql = console.export_sql.clone();
        let options = self.export_options(cx);
        let directory = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Downloads"))
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let name = format!("result.{}", format.extension());
        let picker = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picker.await else {
                return;
            };
            let statement = export_sql.unwrap_or_else(|| result.statement.clone());
            let target = path.clone();
            let handle = runtime()
                .spawn(async move { conn.export(&statement, &target, format, options).await });
            let _ = this.update(cx, |this, cx| {
                if let Some(ix) = this.console_index(&source, id) {
                    this.consoles[ix].note = Some("Exporting…".into());
                }
                cx.notify();
            });
            let outcome = handle.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(ix) = this.console_index(&source, id) {
                    this.consoles[ix].note = Some(match outcome {
                        Ok(Ok(rows)) => format!("Exported {rows} rows to {}", path.display()),
                        Ok(Err(error)) => format!("Export failed: {}", error.message),
                        Err(_) => "Export stopped".into(),
                    });
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
