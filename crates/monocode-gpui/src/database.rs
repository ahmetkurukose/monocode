//! The Database view: data sources and their object tree on the left, SQL
//! consoles (each with its own connection) on the right with the result grid
//! beneath. PostgreSQL only; the work lives in `monocode_core::db` and runs on
//! its tokio runtime, never on the UI thread.

mod dialog;
mod grid;
mod render;
#[cfg(test)]
mod tests;

use crate::{
    input::{self, Input},
    modal,
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
    QueryResult, TableInfo, TableKind, runtime::runtime, source, split,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

pub(crate) enum Event {
    Close,
}

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
    expanded: Vec<String>,
}

impl Default for SavedState {
    fn default() -> Self {
        Self {
            tree_width: 280.,
            editor_height: 240.,
            limit: 500,
            selected: None,
            active: HashMap::new(),
            expanded: Vec::new(),
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
}

struct SchemaNode {
    name: String,
    tables: Option<Vec<TableNode>>,
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
    editor: Entity<Input>,
    conn: Option<Arc<Connection>>,
    result: Option<Arc<QueryResult>>,
    widths: Vec<f32>,
    numeric: Vec<bool>,
    error: Option<(DbError, String)>,
    running: bool,
    selected: Option<(usize, usize)>,
    task: Option<Task<()>>,
    saver: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
enum TreeRow {
    Source(String),
    Note(usize, String),
    Schema(String, String),
    Table(String, String, usize),
    Column(usize, ColumnInfo),
}

enum Resize {
    Tree(f32, f32),
    Editor(f32, f32),
}

pub(crate) struct DatabaseView {
    store: DataSourceStore,
    console_store: Arc<ConsoleStore>,
    state_path: PathBuf,
    sources: Vec<DataSource>,
    nodes: HashMap<String, SourceNode>,
    selected: Option<String>,
    active: HashMap<String, u32>,
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

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

impl DatabaseView {
    pub(crate) fn new(dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let store = DataSourceStore::new(&dir);
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
            expanded: self.expanded.iter().cloned().collect(),
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

    fn password_for(&self, source: &DataSource) -> Option<String> {
        self.passwords.get(&source.id).cloned().or_else(|| {
            source
                .save_password
                .then(|| source::saved_password(&source.id))
                .flatten()
        })
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

    fn ensure_consoles(&mut self, source: &str, cx: &mut Context<Self>) {
        if !self.consoles.iter().any(|c| c.source == source) {
            let mut ids = self.console_store.list(source);
            if ids.is_empty() {
                ids.push(1);
            }
            for id in ids {
                let text = self.console_store.load(source, id);
                self.add_console(source, id, text, cx);
            }
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
    }

    fn add_console(&mut self, source: &str, id: u32, text: String, cx: &mut Context<Self>) {
        let editor = cx.new(Input::editor);
        editor.update(cx, |input, cx| {
            input.set_editor_path("console.sql".into());
            if !text.is_empty() {
                input.set_text(text, cx);
            }
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
        self.consoles.push(Console {
            id,
            source: source.to_owned(),
            editor,
            conn: None,
            result: None,
            widths: Vec::new(),
            numeric: Vec::new(),
            error: None,
            running: false,
            selected: None,
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
            .consoles
            .iter()
            .filter(|c| c.source == source)
            .map(|c| c.id)
            .max()
            .unwrap_or(0)
            + 1;
        self.add_console(&source, id, String::new(), cx);
        let _ = self.console_store.save(&source, id, "");
        self.activate(&source, id, window, cx);
    }

    fn activate(&mut self, source: &str, id: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.active.insert(source.to_owned(), id);
        self.save_state();
        if let Some(console) = self.active_console() {
            window.focus(&console.editor.focus_handle(cx), cx);
        }
        cx.notify();
    }

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
        self.console_store.delete(source, id);
        if self.active.get(source) == Some(&id) {
            self.active.remove(source);
        }
        if !self.consoles.iter().any(|c| c.source == source) {
            self.add_console(source, 1, String::new(), cx);
        }
        self.ensure_consoles(source, cx);
        if let Some(next) = self.active.get(source).copied() {
            self.activate(source, next, window, cx);
        }
        cx.notify();
    }

    fn console_edited(&mut self, source: &str, id: u32, cx: &mut Context<Self>) {
        let Some(ix) = self.console_index(source, id) else {
            return;
        };
        let store = self.console_store.clone();
        let editor = self.consoles[ix].editor.clone();
        let key = (source.to_owned(), id);
        self.consoles[ix].saver = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |_, cx| {
                let text = editor.read(cx).content.to_string();
                let _ = store.save(&key.0, key.1, &text);
            });
        }));
        cx.notify();
    }

    fn tab_title(&self, console: &Console, cx: &Context<Self>) -> String {
        let text = console.editor.read(cx).content.clone();
        let first = text.lines().next().unwrap_or("").trim();
        match first.strip_prefix("--").map(str::trim) {
            Some(title) if !title.is_empty() => title.chars().take(24).collect(),
            _ => format!("Console {}", console.id),
        }
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
        let statements: Vec<String> = if !selection.is_empty() {
            split::split(&text[selection])
                .into_iter()
                .map(|s| s.text)
                .collect()
        } else if all {
            split::split(&text).into_iter().map(|s| s.text).collect()
        } else {
            split::at_cursor(&text, selection.start)
                .map(|s| vec![s.text])
                .unwrap_or_default()
        };
        if statements.is_empty() {
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
        let key = (source.to_owned(), id);
        let work = async move {
            let conn = match existing {
                Some(conn) => conn,
                None => Arc::new(
                    Connection::connect(&data_source, password.as_deref())
                        .await
                        .map_err(|e| (None, e, String::new()))?,
                ),
            };
            let mut last = None;
            for statement in statements {
                match conn.execute(&statement, limit).await {
                    Ok(result) => last = Some(result),
                    Err(error) => return Err((Some(conn), error, statement)),
                }
            }
            Ok((conn, last))
        };
        let task = self.spawn_db(cx, work, move |this, out, cx| {
            let Some(ix) = this.console_index(&key.0, key.1) else {
                return;
            };
            let console = &mut this.consoles[ix];
            console.running = false;
            console.task = None;
            match out {
                Some(Ok((conn, result))) => {
                    console.conn = Some(conn);
                    if let Some(result) = result {
                        console.widths = grid::column_widths(&result);
                        console.numeric = grid::numeric_columns(&result);
                        console.result = Some(Arc::new(result));
                        console.selected = None;
                    }
                }
                Some(Err((conn, error, statement))) => {
                    if conn.is_some() {
                        console.conn = conn;
                    }
                    console.error = Some((error, statement));
                }
                None => {
                    console.error = Some((DbError::default(), String::new()));
                }
            }
            cx.notify();
        });
        self.consoles[ix].task = Some(task);
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
            let conn = Connection::connect(&data_source, password.as_deref()).await?;
            let schemas = conn.schemas(false).await?;
            Ok::<_, DbError>((Arc::new(conn), schemas))
        };
        self.spawn_db(cx, work, move |this, out, cx| {
            let node = this
                .nodes
                .entry(key.clone())
                .or_insert_with(SourceNode::new);
            match out {
                Some(Ok((conn, schemas))) => {
                    node.conn = Some(conn);
                    node.link = Link::Ready;
                    node.schemas = Some(
                        schemas
                            .into_iter()
                            .map(|name| SchemaNode { name, tables: None })
                            .collect(),
                    );
                }
                Some(Err(error)) => node.link = Link::Failed(error.message),
                None => node.link = Link::Failed("The connection task stopped".into()),
            }
            cx.notify();
        })
        .detach();
        cx.notify();
    }

    fn disconnect_source(&mut self, id: &str, cx: &mut Context<Self>) {
        self.nodes.remove(id);
        self.expanded.retain(|k| {
            !k.starts_with(&format!("s:{id}"))
                && !k.starts_with(&format!("n:{id}/"))
                && !k.starts_with(&format!("t:{id}/"))
        });
        for console in self.consoles.iter_mut().filter(|c| c.source == id) {
            console.conn = None;
        }
        cx.notify();
    }

    fn load_tables(&mut self, source: &str, schema: &str, cx: &mut Context<Self>) {
        let Some(conn) = self.nodes.get(source).and_then(|n| n.conn.clone()) else {
            return;
        };
        let (key, schema_name) = (source.to_owned(), schema.to_owned());
        let name = schema.to_owned();
        let work = async move { conn.tables(&name).await };
        self.spawn_db(cx, work, move |this, out, cx| {
            if let Some(Ok(tables)) = out
                && let Some(schemas) = this.nodes.get_mut(&key).and_then(|n| n.schemas.as_mut())
                && let Some(node) = schemas.iter_mut().find(|s| s.name == schema_name)
            {
                node.tables = Some(
                    tables
                        .into_iter()
                        .map(|info| TableNode {
                            info,
                            columns: None,
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
        let work = async move { conn.columns(&s, &t).await };
        self.spawn_db(cx, work, move |this, out, cx| {
            if let Some(Ok(columns)) = out
                && let Some(schemas) = this.nodes.get_mut(&key).and_then(|n| n.schemas.as_mut())
                && let Some(node) = schemas.iter_mut().find(|s| s.name == schema_name)
                && let Some(tables) = node.tables.as_mut()
                && let Some(table) = tables.iter_mut().find(|t| t.info.name == table_name)
            {
                table.columns = Some(columns);
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
                if tables.is_empty() {
                    rows.push(TreeRow::Note(2, "No tables".into()));
                }
                for (ix, table) in tables.iter().enumerate() {
                    rows.push(TreeRow::Table(source.id.clone(), schema.name.clone(), ix));
                    let open = format!("t:{}/{}/{}", source.id, schema.name, table.info.name);
                    if self.expanded.contains(&open) {
                        match &table.columns {
                            None => rows.push(TreeRow::Note(3, "Loading…".into())),
                            Some(columns) => {
                                rows.extend(columns.iter().map(|c| TreeRow::Column(3, c.clone())))
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
                    self.connect_source(id, cx);
                }
            }
            TreeRow::Schema(source, schema) => {
                if self.toggle(format!("n:{source}/{schema}")) {
                    self.load_tables(source, schema, cx);
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
                    self.load_columns(source, schema, &name, cx);
                }
            }
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
        let (source, id) = (self.consoles[ix].source.clone(), self.consoles[ix].id);
        self.consoles[ix]
            .editor
            .update(cx, |input, cx| input.set_text(sql, cx));
        self.run(&source, id, false, cx);
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
        let has_saved = existing
            .as_ref()
            .is_some_and(|s| s.save_password && source::saved_password(&s.id).is_some());
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
        if !data_source.save_password {
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
        self.disconnect_source(id, cx);
        self.sources.retain(|s| s.id != id);
        self.consoles.retain(|c| c.source != id);
        self.console_store.delete_source(id);
        self.active.remove(id);
        source::forget_password(id);
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
            "up" => self.consoles[ix].selected = Some((row.saturating_sub(1), col)),
            "down" => self.consoles[ix].selected = Some(((row + 1).min(rows - 1), col)),
            "left" => self.consoles[ix].selected = Some((row, col.saturating_sub(1))),
            "right" => self.consoles[ix].selected = Some((row, (col + 1).min(cols - 1))),
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
}
