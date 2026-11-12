//! The API view: a project's Postman collections on the left, request tabs on
//! the right with the response beneath. Collections are ordinary Postman v2.1
//! files under `<project>/.monocode/api/`, edited in place by
//! `monocode_core::api`; sending runs on its own thread, never the UI's.

mod disk;
mod render;
mod run;
mod tabs;
#[cfg(test)]
mod tests;

use crate::{
    input::{self, Input},
    modal,
    popover::{Anchor, Menu, MenuAction, MenuEvent, MenuItem, menu_popover},
    theme::*,
};
use gpui::{
    AnyElement, ClipboardItem, Context, CursorStyle, Entity, FocusHandle, Focusable, FontWeight,
    MouseButton, Role, Subscription, Window, div, prelude::*, px, rgb, uniform_list,
};
use monocode_core::api::{
    self, ApiRequest, Auth, Body, COLLECTION_SUFFIX, Collection, ENVIRONMENT_SUFFIX, Environment,
    FormPart, ItemPath, KeyValue, PartKind, RawLanguage, Scope, SendOptions, TreeEntry, api_dir,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, SystemTime},
};

/// Offered on a `curl` command in a reply: makes it a request in the API view.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = api, no_json)]
pub(crate) struct SendCurl(pub String);

pub(crate) enum Event {
    /// Start a chat about a request with `prompt` waiting in its composer.
    AskAgent {
        project: String,
        title: String,
        prompt: String,
    },
}

impl gpui::EventEmitter<Event> for ApiView {}

const TREE_ROW: f32 = 28.;
/// The most of a response the editor is given; the rest stays out of it.
const SHOWN_BYTES: usize = 2 * 1024 * 1024;
const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct SavedState {
    tree_width: f32,
    response_height: f32,
    /// Collapsed rows of the tree (everything starts open).
    collapsed: Vec<String>,
    /// Before environments were chosen per project: the one in use.
    environment: Option<String>,
    /// The environment file in use for each project.
    environments: HashMap<String, String>,
    /// The request tabs that were open, and which one was in front.
    tabs: Vec<SavedTab>,
    active_tab: Option<usize>,
}

impl Default for SavedState {
    fn default() -> Self {
        Self {
            tree_width: 280.,
            response_height: 300.,
            collapsed: Vec::new(),
            environment: None,
            environments: HashMap::new(),
            tabs: Vec::new(),
            active_tab: None,
        }
    }
}

/// An open request tab as saved: where it is, not what was typed in it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedTab {
    file: String,
    path: ItemPath,
}

struct LoadedCollection {
    file: PathBuf,
    project: String,
    collection: Collection,
    /// The flattened tree, kept until the collection changes.
    entries: RefCell<Option<Rc<Vec<TreeEntry>>>>,
}

impl LoadedCollection {
    fn entries(&self) -> Rc<Vec<TreeEntry>> {
        self.entries
            .borrow_mut()
            .get_or_insert_with(|| Rc::new(self.collection.entries()))
            .clone()
    }
}

struct LoadedEnvironment {
    file: PathBuf,
    project: String,
    environment: Environment,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Path,
    Query,
    Headers,
    Body,
    Auth,
    Scripts,
    Notes,
}

/// Which of a request's two scripts the Scripts tab shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScriptKind {
    PreRequest,
    Tests,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResponseTab {
    Pretty,
    Raw,
    Headers,
    Tests,
    Console,
}

/// The key/value tables a request has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Table {
    Query,
    Headers,
    Path,
    UrlEncoded,
    Form,
}

#[derive(Clone, PartialEq, Eq)]
enum BodyMode {
    None,
    Raw,
    UrlEncoded,
    Form,
    Unsupported(String),
}

struct Row {
    key: Entity<Input>,
    value: Entity<Input>,
    disabled: bool,
    /// A form-data row that uploads the file named in `value`.
    file: bool,
    content_type: String,
    _subscriptions: Vec<Subscription>,
}

/// What a row starts with.
#[derive(Clone)]
struct RowData {
    key: String,
    value: String,
    disabled: bool,
    file: bool,
    content_type: String,
}

impl From<&KeyValue> for RowData {
    fn from(row: &KeyValue) -> Self {
        Self {
            key: row.key.clone(),
            value: row.value.clone(),
            disabled: row.disabled,
            file: false,
            content_type: String::new(),
        }
    }
}

struct AuthInputs {
    username: Entity<Input>,
    password: Entity<Input>,
    token: Entity<Input>,
    key: Entity<Input>,
    value: Entity<Input>,
}

struct Done {
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
    elapsed: Duration,
    size: usize,
    truncated: bool,
    pretty: String,
    raw: String,
    /// The file name the viewer takes its highlighting from.
    syntax: &'static str,
}

enum Exchange {
    Idle,
    Sending,
    Done(Box<Done>),
    Failed(String),
}

struct RequestTab {
    id: u64,
    file: PathBuf,
    path: ItemPath,
    draft: ApiRequest,
    saved: ApiRequest,
    url: Entity<Input>,
    body: Entity<Input>,
    notes: Entity<Input>,
    /// The request's own pre-request and test scripts.
    pre_script: Entity<Input>,
    test_script: Entity<Input>,
    script_kind: ScriptKind,
    /// What the last run's test scripts and console reported.
    tests: Vec<api::TestResult>,
    logs: Vec<String>,
    auth: AuthInputs,
    query: Vec<Row>,
    headers: Vec<Row>,
    path_vars: Vec<Row>,
    urlencoded: Vec<Row>,
    form: Vec<Row>,
    body_mode: BodyMode,
    raw_language: RawLanguage,
    section: Section,
    response_tab: ResponseTab,
    exchange: Exchange,
    /// Only the newest send's answer is kept.
    generation: u64,
    viewer: Entity<Input>,
    /// Shown at the top of the response after a cURL paste or a failed save.
    _subscriptions: Vec<Subscription>,
}

impl RequestTab {
    fn dirty(&self) -> bool {
        self.draft != self.saved
    }

    fn rows(&self, table: Table) -> &Vec<Row> {
        match table {
            Table::Query => &self.query,
            Table::Headers => &self.headers,
            Table::Path => &self.path_vars,
            Table::UrlEncoded => &self.urlencoded,
            Table::Form => &self.form,
        }
    }

    fn rows_mut(&mut self, table: Table) -> &mut Vec<Row> {
        match table {
            Table::Query => &mut self.query,
            Table::Headers => &mut self.headers,
            Table::Path => &mut self.path_vars,
            Table::UrlEncoded => &mut self.urlencoded,
            Table::Form => &mut self.form,
        }
    }
}

/// A node of the tree: a collection file and an item inside it (empty for
/// the collection itself).
#[derive(Clone, PartialEq, Eq)]
struct Target {
    file: PathBuf,
    path: ItemPath,
}

#[derive(Clone)]
enum TreeRow {
    Collection(usize),
    Entry(usize, TreeEntry),
    Note(usize, String),
}

enum MenuKind {
    Method(u64),
    Auth(u64),
    BodyLanguage(u64),
    /// The environments of one project.
    Environment(String),
    Tab(u64),
    Tabs,
    Add,
    Node(Target, bool),
}

struct OpenMenu {
    kind: MenuKind,
    anchor: gpui::Point<gpui::Pixels>,
    menu: Entity<Menu>,
    _events: Subscription,
}

enum DialogKind {
    Rename(Target),
    NewCollection,
    Delete(Target),
    CloseDirty(u64),
    /// Closing several tabs at once, some with unsaved changes.
    CloseMany(Vec<u64>),
}

enum Resize {
    Tree(f32, f32),
    Response(f32, f32),
}

/// What the environment editor is showing.
#[derive(Clone, PartialEq, Eq)]
enum EnvTarget {
    Environment(PathBuf),
    /// A collection's own variables.
    Variables(PathBuf),
}

struct EnvRow {
    key: Entity<Input>,
    value: Entity<Input>,
    enabled: bool,
    secret: bool,
    _subscriptions: Vec<Subscription>,
}

struct EnvEditor {
    target: EnvTarget,
    name: Entity<Input>,
    rows: Vec<EnvRow>,
    dirty: bool,
    confirm_delete: bool,
    _subscription: Subscription,
}

pub(crate) struct ApiView {
    state_path: PathBuf,
    scope: Option<String>,
    projects: Vec<String>,
    /// Where new collections go.
    current: String,
    collections: Vec<LoadedCollection>,
    environments: Vec<LoadedEnvironment>,
    /// The environment in use, per project.
    environment: HashMap<String, PathBuf>,
    /// Variables scripts set with `pm.globals`, for this session.
    globals: Vec<KeyValue>,
    load_errors: Vec<String>,
    tabs: Vec<RequestTab>,
    active: Option<u64>,
    next_id: u64,
    /// Tabs closed this session, newest last, for "reopen".
    closed: Vec<(PathBuf, ItemPath)>,
    /// Saved tabs whose project is not shown right now; they come back with it.
    parked: Vec<SavedTab>,
    /// The saved front tab, until its tab has been reopened.
    restore_active: Option<SavedTab>,
    /// Size and time of each file as last read or written by this view.
    fingerprints: HashMap<PathBuf, (SystemTime, u64)>,
    watch: Option<crate::fs_watch::Watch>,
    _watch_task: Option<gpui::Task<()>>,
    collapsed: HashSet<String>,
    filter: Entity<Input>,
    dialog: Option<DialogKind>,
    env_editor: Option<EnvEditor>,
    dialog_input: Entity<Input>,
    menu: Option<OpenMenu>,
    tree_width: f32,
    response_height: f32,
    resize: Option<Resize>,
    notice: Option<String>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for ApiView {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        match self.active_tab() {
            Some(tab) => tab.url.focus_handle(cx),
            None => self.focus.clone(),
        }
    }
}

fn field(label: &str, placeholder: &str, cx: &mut Context<ApiView>) -> Entity<Input> {
    cx.new(|cx| {
        let mut input = Input::field(label.to_owned(), placeholder.to_owned(), content(0.35), cx);
        input.set_font_size(13.);
        input
    })
}

fn text_of(input: &Entity<Input>, cx: &gpui::App) -> String {
    input.read(cx).content.to_string()
}

fn set_text(input: &Entity<Input>, text: &str, cx: &mut Context<ApiView>) {
    if input.read(cx).content.as_ref() != text {
        input.update(cx, |input, cx| input.set_text(text.to_owned(), cx));
    }
}

fn syntax_for(language: RawLanguage) -> &'static str {
    match language {
        RawLanguage::Json => "body.json",
        RawLanguage::Xml => "body.xml",
        RawLanguage::Html => "body.html",
        RawLanguage::JavaScript => "body.js",
        RawLanguage::Text => "body.txt",
    }
}

pub(crate) fn method_color(method: &str) -> u32 {
    match method {
        "GET" => 0x4ade80,
        "POST" => 0xfb923c,
        "PUT" => 0x60a5fa,
        "PATCH" => 0xc084fc,
        "DELETE" => 0xf87171,
        _ => 0x9ca3af,
    }
}

fn status_color(status: u16) -> u32 {
    match status {
        200..=299 => 0x4ade80,
        300..=399 => 0x60a5fa,
        400..=499 => 0xfb923c,
        _ => 0xf87171,
    }
}

fn human_size(bytes: usize) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KB", bytes as f64 / 1024.),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

fn human_time(elapsed: Duration) -> String {
    if elapsed.as_millis() < 1000 {
        format!("{} ms", elapsed.as_millis())
    } else {
        format!("{:.2} s", elapsed.as_secs_f64())
    }
}

fn file_stem(name: &str) -> String {
    let slug: String = name
        .trim()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase();
    if slug.is_empty() {
        "collection".into()
    } else {
        slug
    }
}

/// A path in `dir` named `stem` + `suffix` that does not exist yet.
fn free_path(dir: &Path, stem: &str, suffix: &str) -> PathBuf {
    let first = dir.join(format!("{stem}{suffix}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}{suffix}")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

fn project_name(project: &str) -> String {
    Path::new(project)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| project.to_owned())
}

impl ApiView {
    pub(crate) fn new(dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let state_path = dir.join("state.json");
        let saved: SavedState = std::fs::read(&state_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let filter = field("Filter requests", "Search requests…", cx);
        let dialog_input = field("Name", "Name", cx);
        let subscriptions = vec![
            cx.subscribe(&filter, |_, _, _: &input::Changed, cx| cx.notify()),
            cx.subscribe(&dialog_input, |this, _, _: &input::Submitted, cx| {
                this.confirm_dialog(cx)
            }),
        ];
        let mut environment: HashMap<String, PathBuf> = saved
            .environments
            .into_iter()
            .map(|(project, file)| (project, PathBuf::from(file)))
            .collect();
        // `<project>/.monocode/api/<file>`: the project is three levels up.
        if let Some(old) = saved.environment.map(PathBuf::from)
            && let Some(project) = old.ancestors().nth(3)
        {
            environment
                .entry(project.to_string_lossy().into_owned())
                .or_insert(old);
        }
        let restore_active = saved
            .active_tab
            .and_then(|index| saved.tabs.get(index).cloned());
        Self {
            state_path,
            scope: None,
            projects: Vec::new(),
            current: String::new(),
            collections: Vec::new(),
            environments: Vec::new(),
            environment,
            globals: Vec::new(),
            load_errors: Vec::new(),
            tabs: Vec::new(),
            active: None,
            next_id: 1,
            closed: Vec::new(),
            parked: saved.tabs,
            restore_active,
            fingerprints: HashMap::new(),
            watch: None,
            _watch_task: None,
            collapsed: saved.collapsed.into_iter().collect(),
            filter,
            dialog: None,
            env_editor: None,
            dialog_input,
            menu: None,
            tree_width: saved.tree_width.clamp(200., 700.),
            response_height: saved.response_height.clamp(120., 900.),
            resize: None,
            notice: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn save_state(&self) {
        let saved_tabs: Vec<SavedTab> = self
            .tabs
            .iter()
            .map(|t| SavedTab {
                file: t.file.to_string_lossy().into_owned(),
                path: t.path.clone(),
            })
            .chain(self.parked.iter().cloned())
            .collect();
        let state = SavedState {
            tree_width: self.tree_width,
            response_height: self.response_height,
            collapsed: self.collapsed.iter().cloned().collect(),
            environment: None,
            environments: self
                .environment
                .iter()
                .map(|(project, file)| (project.clone(), file.to_string_lossy().into_owned()))
                .collect(),
            tabs: saved_tabs.clone(),
            active_tab: self
                .active
                .and_then(|id| self.tabs.iter().position(|t| t.id == id))
                // A front tab still waiting for its project stays in front.
                .or_else(|| {
                    let waiting = self.restore_active.as_ref()?;
                    saved_tabs.iter().position(|t| t == waiting)
                }),
        };
        if let Ok(json) = serde_json::to_vec_pretty(&state) {
            if let Some(parent) = self.state_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&self.state_path, json);
        }
    }

    /// The projects whose collections the tree lists.
    fn shown_projects(&self) -> Vec<String> {
        match &self.scope {
            Some(scope) => vec![scope.clone()],
            None => self.projects.clone(),
        }
    }

    /// Where a new collection or environment is made.
    fn target_project(&self) -> Option<String> {
        self.scope
            .clone()
            .or_else(|| (!self.current.is_empty()).then(|| self.current.clone()))
            .or_else(|| self.projects.first().cloned())
    }

    pub(crate) fn set_scope(
        &mut self,
        scope: Option<String>,
        projects: Vec<String>,
        current: String,
        cx: &mut Context<Self>,
    ) {
        let changed = self.scope != scope || self.projects != projects;
        self.scope = scope;
        self.projects = projects;
        self.current = current;
        if changed || self.collections.is_empty() {
            self.reload(cx);
        }
    }

    fn notify_user(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(text.into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |this, cx| {
                this.notice = None;
                cx.notify();
            });
        })
        .detach();
    }

    // ---- files ----

    /// Reads every project's collections and environments again. Open tabs
    /// keep their drafts and are reattached to their file when it is still
    /// there.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.collections.clear();
        self.environments.clear();
        self.load_errors.clear();
        for project in self.shown_projects() {
            let Ok(entries) = std::fs::read_dir(api_dir(&project)) else {
                continue;
            };
            let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            files.sort();
            for file in files {
                let name = file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if name.ends_with(COLLECTION_SUFFIX) {
                    match Collection::load(&file) {
                        Ok(collection) => self.collections.push(LoadedCollection {
                            file,
                            project: project.clone(),
                            collection,
                            entries: RefCell::new(None),
                        }),
                        Err(error) => self.load_errors.push(format!("{name}: {error}")),
                    }
                } else if name.ends_with(ENVIRONMENT_SUFFIX) {
                    match Environment::load(&file) {
                        Ok(environment) => self.environments.push(LoadedEnvironment {
                            file,
                            project: project.clone(),
                            environment,
                        }),
                        Err(error) => self.load_errors.push(format!("{name}: {error}")),
                    }
                }
            }
        }
        // A chosen environment that is gone from disk is forgotten; one
        // that only belongs to a project not shown now is kept.
        self.environment.retain(|_, file| file.exists());
        self.after_reload(cx);
    }

    fn collection(&self, file: &Path) -> Option<&LoadedCollection> {
        self.collections.iter().find(|c| c.file == file)
    }

    fn collection_mut(&mut self, file: &Path) -> Option<&mut LoadedCollection> {
        let loaded = self.collections.iter_mut().find(|c| c.file == file)?;
        loaded.entries.take();
        Some(loaded)
    }

    fn tab(&self, id: u64) -> Option<&RequestTab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    fn tab_mut(&mut self, id: u64) -> Option<&mut RequestTab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    fn active_tab(&self) -> Option<&RequestTab> {
        self.tab(self.active?)
    }

    /// The project the front tab belongs to, else the one new things go in.
    fn current_project(&self) -> Option<String> {
        self.active_tab()
            .and_then(|tab| self.collection(&tab.file))
            .map(|c| c.project.clone())
            .or_else(|| self.target_project())
    }

    fn environment_for(&self, project: &str) -> Option<&LoadedEnvironment> {
        let file = self.environment.get(project)?;
        self.environments.iter().find(|e| &e.file == file)
    }

    fn selected_environment(&self) -> Option<&LoadedEnvironment> {
        self.environment_for(&self.current_project()?)
    }

    fn select_environment(&mut self, project: &str, file: Option<PathBuf>) {
        match file {
            Some(file) => self.environment.insert(project.to_owned(), file),
            None => self.environment.remove(project),
        };
        self.save_state();
    }

    fn collection_key(file: &Path) -> String {
        format!("c:{}", file.display())
    }

    fn folder_key(file: &Path, path: &[usize]) -> String {
        let path: Vec<String> = path.iter().map(usize::to_string).collect();
        format!("f:{}:{}", file.display(), path.join("."))
    }

    /// Closes every folder; the collections stay open.
    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        let mut keys = Vec::new();
        for loaded in &self.collections {
            for entry in loaded.entries().iter().filter(|e| e.folder) {
                keys.push(Self::folder_key(&loaded.file, &entry.path));
            }
        }
        self.collapsed.extend(keys);
        self.save_state();
        cx.notify();
    }

    fn expand_all(&mut self, cx: &mut Context<Self>) {
        self.collapsed.clear();
        self.save_state();
        cx.notify();
    }

    fn toggle(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        self.save_state();
        cx.notify();
    }

    /// The tree as visible rows: collections, then their folders and
    /// requests, honouring collapsed folders and the filter.
    fn rows(&self, cx: &gpui::App) -> Vec<TreeRow> {
        let filter = text_of(&self.filter, cx).trim().to_lowercase();
        let mut rows = Vec::new();
        for (ci, loaded) in self.collections.iter().enumerate() {
            let entries = loaded.entries();
            let keep: Option<HashSet<ItemPath>> = (!filter.is_empty()).then(|| {
                let mut keep = HashSet::new();
                for entry in entries.iter().filter(|e| !e.folder) {
                    let hit = entry.name.to_lowercase().contains(&filter)
                        || entry
                            .method
                            .as_deref()
                            .is_some_and(|m| m.to_lowercase().contains(&filter));
                    if hit {
                        for depth in 1..=entry.path.len() {
                            keep.insert(entry.path[..depth].to_vec());
                        }
                    }
                }
                keep
            });
            if keep.as_ref().is_some_and(|keep| keep.is_empty()) {
                continue;
            }
            rows.push(TreeRow::Collection(ci));
            if keep.is_none() && self.collapsed.contains(&Self::collection_key(&loaded.file)) {
                continue;
            }
            if entries.is_empty() {
                rows.push(TreeRow::Note(
                    1,
                    "No requests yet. Use + to add one.".into(),
                ));
            }
            let mut hidden_below: Option<usize> = None;
            for entry in entries.iter() {
                if let Some(depth) = hidden_below {
                    if entry.depth > depth {
                        continue;
                    }
                    hidden_below = None;
                }
                if let Some(keep) = &keep {
                    if !keep.contains(&entry.path) {
                        continue;
                    }
                } else if entry.folder
                    && self
                        .collapsed
                        .contains(&Self::folder_key(&loaded.file, &entry.path))
                {
                    hidden_below = Some(entry.depth);
                }
                rows.push(TreeRow::Entry(ci, entry.clone()));
            }
        }
        rows
    }

    // ---- tabs ----

    fn new_input_rows(
        &mut self,
        id: u64,
        table: Table,
        data: Vec<RowData>,
        cx: &mut Context<Self>,
    ) -> Vec<Row> {
        data.into_iter()
            .map(|data| {
                let key = field("Name", "Key", cx);
                let value = field("Value", "Value", cx);
                if table == Table::Path {
                    key.update(cx, |input, _| input.set_read_only(true));
                }
                set_text(&key, &data.key, cx);
                set_text(&value, &data.value, cx);
                let subscriptions = vec![
                    cx.subscribe(&key, move |this, _, _: &input::Changed, cx| {
                        this.rows_changed(id, table, cx)
                    }),
                    cx.subscribe(&value, move |this, _, _: &input::Changed, cx| {
                        this.rows_changed(id, table, cx)
                    }),
                ];
                Row {
                    key,
                    value,
                    disabled: data.disabled,
                    file: data.file,
                    content_type: data.content_type,
                    _subscriptions: subscriptions,
                }
            })
            .collect()
    }

    fn read_rows(tab: &RequestTab, table: Table, cx: &gpui::App) -> Vec<RowData> {
        tab.rows(table)
            .iter()
            .map(|row| RowData {
                key: text_of(&row.key, cx),
                value: text_of(&row.value, cx),
                disabled: row.disabled,
                file: row.file,
                content_type: row.content_type.clone(),
            })
            .collect()
    }

    fn key_values(data: &[RowData]) -> Vec<KeyValue> {
        data.iter()
            .map(|row| KeyValue {
                key: row.key.clone(),
                value: row.value.clone(),
                disabled: row.disabled,
            })
            .collect()
    }

    fn mode_of(body: &Body) -> BodyMode {
        match body {
            Body::None => BodyMode::None,
            Body::Raw { .. } => BodyMode::Raw,
            Body::UrlEncoded(_) => BodyMode::UrlEncoded,
            Body::FormData(_) => BodyMode::Form,
            Body::Unsupported(mode) => BodyMode::Unsupported(mode.clone()),
        }
    }

    /// The tables' starting rows for a request.
    fn tables_of(request: &ApiRequest) -> [(Table, Vec<RowData>); 5] {
        let form = match &request.body {
            Body::FormData(parts) => parts
                .iter()
                .map(|part| RowData {
                    key: part.key.clone(),
                    value: part.value.clone(),
                    disabled: part.disabled,
                    file: part.kind == PartKind::File,
                    content_type: part.content_type.clone(),
                })
                .collect(),
            _ => Vec::new(),
        };
        let urlencoded = match &request.body {
            Body::UrlEncoded(rows) => rows.iter().map(RowData::from).collect(),
            _ => Vec::new(),
        };
        [
            (
                Table::Query,
                request.query.iter().map(RowData::from).collect(),
            ),
            (
                Table::Headers,
                request.headers.iter().map(RowData::from).collect(),
            ),
            (
                Table::Path,
                request.path_vars.iter().map(RowData::from).collect(),
            ),
            (Table::UrlEncoded, urlencoded),
            (Table::Form, form),
        ]
    }

    pub(crate) fn open_request(
        &mut self,
        file: &Path,
        path: ItemPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Its folders open so the row can be seen.
        self.collapsed.remove(&Self::collection_key(file));
        for depth in 1..path.len() {
            self.collapsed
                .remove(&Self::folder_key(file, &path[..depth]));
        }
        let existing = self
            .tabs
            .iter()
            .find(|t| t.file == file && t.path == path)
            .map(|t| t.id);
        let Some(id) = existing.or_else(|| self.add_tab(file, path, cx)) else {
            return;
        };
        self.active = Some(id);
        self.save_state();
        self.focus_url(window, cx);
        cx.notify();
    }

    /// Makes a tab for a request without showing it.
    fn add_tab(&mut self, file: &Path, path: ItemPath, cx: &mut Context<Self>) -> Option<u64> {
        let request = self
            .collection(file)
            .and_then(|c| c.collection.request(&path))?;
        let id = self.next_id;
        self.next_id += 1;

        let url = field("URL", "Enter a URL, or paste a cURL command", cx);
        set_text(&url, &request.url_text(), cx);
        let body = cx.new(Input::editor);
        let language = match &request.body {
            Body::Raw { language, .. } => *language,
            _ => RawLanguage::Json,
        };
        body.update(cx, |input, cx| {
            input.set_editor_path(syntax_for(language).into());
            if let Body::Raw { text, .. } = &request.body {
                input.set_text(text.clone(), cx);
            }
        });
        let notes = cx.new(|cx| Input::multiline_field("Notes", "Describe this request", cx));
        set_text(&notes, &request.description, cx);
        let (username, password, token, key, value) = match &request.auth {
            Auth::Basic { username, password } => (
                username.clone(),
                password.clone(),
                String::new(),
                String::new(),
                String::new(),
            ),
            Auth::Bearer { token } => (
                String::new(),
                String::new(),
                token.clone(),
                String::new(),
                String::new(),
            ),
            Auth::ApiKey { key, value, .. } => (
                String::new(),
                String::new(),
                String::new(),
                key.clone(),
                value.clone(),
            ),
            _ => Default::default(),
        };
        let auth = AuthInputs {
            username: field("Username", "Username", cx),
            password: field("Password", "Password", cx),
            token: field("Token", "Token", cx),
            key: field("Key", "Key", cx),
            value: field("Value", "Value", cx),
        };
        auth.password.update(cx, |input, _| input.set_secret(true));
        for (input, text) in [
            (&auth.username, username),
            (&auth.password, password),
            (&auth.token, token),
            (&auth.key, key),
            (&auth.value, value),
        ] {
            set_text(input, &text, cx);
        }
        let viewer = cx.new(Input::editor);
        viewer.update(cx, |input, _| {
            input.set_read_only(true);
            input.set_editor_path("response.txt".into());
        });
        let pre_script = cx.new(Input::editor);
        pre_script.update(cx, |input, cx| {
            input.set_editor_path("pre-request.js".into());
            input.set_text(request.pre_request.clone(), cx);
        });
        let test_script = cx.new(Input::editor);
        test_script.update(cx, |input, cx| {
            input.set_editor_path("tests.js".into());
            input.set_text(request.tests.clone(), cx);
        });

        let mut subscriptions = vec![
            cx.subscribe(&pre_script, move |this, _, _: &input::Changed, cx| {
                this.scripts_changed(id, cx)
            }),
            cx.subscribe(&test_script, move |this, _, _: &input::Changed, cx| {
                this.scripts_changed(id, cx)
            }),
            cx.subscribe(&url, move |this, _, _: &input::Changed, cx| {
                this.url_changed(id, cx)
            }),
            cx.subscribe(&url, move |this, _, _: &input::Submitted, cx| {
                this.send(id, cx)
            }),
            cx.subscribe(&body, move |this, _, _: &input::Changed, cx| {
                this.body_changed(id, cx)
            }),
            cx.subscribe(&notes, move |this, _, _: &input::Changed, cx| {
                this.notes_changed(id, cx)
            }),
        ];
        for input in [
            &auth.username,
            &auth.password,
            &auth.token,
            &auth.key,
            &auth.value,
        ] {
            subscriptions.push(cx.subscribe(input, move |this, _, _: &input::Changed, cx| {
                this.auth_changed(id, cx)
            }));
        }
        let mut tab = RequestTab {
            id,
            file: file.to_owned(),
            path,
            saved: request.clone(),
            body_mode: Self::mode_of(&request.body),
            raw_language: language,
            draft: request.clone(),
            url,
            body,
            notes,
            pre_script,
            test_script,
            script_kind: ScriptKind::PreRequest,
            tests: Vec::new(),
            logs: Vec::new(),
            auth,
            query: Vec::new(),
            headers: Vec::new(),
            path_vars: Vec::new(),
            urlencoded: Vec::new(),
            form: Vec::new(),
            section: Section::Query,
            response_tab: ResponseTab::Pretty,
            exchange: Exchange::Idle,
            generation: 0,
            viewer,
            _subscriptions: subscriptions,
        };
        for (table, data) in Self::tables_of(&request) {
            *tab.rows_mut(table) = self.new_input_rows(id, table, data, cx);
        }
        tab.section = if tab.path_vars.is_empty() {
            Section::Query
        } else {
            Section::Path
        };
        self.tabs.push(tab);
        Some(id)
    }

    fn focus_url(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_tab() {
            window.focus(&tab.url.focus_handle(cx), cx);
        }
    }

    fn activate(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab(id).is_some() {
            self.active = Some(id);
            self.save_state();
            self.focus_url(window, cx);
            cx.notify();
        }
    }

    fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab(id).is_some_and(RequestTab::dirty) {
            self.dialog = Some(DialogKind::CloseDirty(id));
            cx.notify();
            return;
        }
        self.discard_tab(id, window, cx);
    }

    fn discard_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        let tab = self.tabs.remove(index);
        self.closed.push((tab.file, tab.path));
        if self.closed.len() > 20 {
            self.closed.remove(0);
        }
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(index)
                .or_else(|| index.checked_sub(1).and_then(|i| self.tabs.get(i)))
                .map(|t| t.id);
            self.focus_url(window, cx);
        }
        self.save_state();
        cx.notify();
    }

    // ---- editing ----

    fn after_edit(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.tab(id).is_some() {
            cx.notify();
        }
    }

    fn url_changed(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let text = text_of(&tab.url, cx);
        if text == tab.draft.url_text() {
            return;
        }
        if text.trim_start().starts_with("curl ") {
            match api::parse_curl(&text) {
                Ok(import) => self.apply_curl(id, import, cx),
                Err(error) => self.notify_user(error.to_string(), cx),
            }
            return;
        }
        let Some(tab) = self.tab_mut(id) else {
            return;
        };
        tab.draft.set_url_text(&text);
        let query: Vec<RowData> = tab.draft.query.iter().map(RowData::from).collect();
        let path: Vec<RowData> = tab.draft.path_vars.iter().map(RowData::from).collect();
        let rows = self.new_input_rows(id, Table::Query, query, cx);
        let path_rows = self.new_input_rows(id, Table::Path, path, cx);
        if let Some(tab) = self.tab_mut(id) {
            tab.query = rows;
            tab.path_vars = path_rows;
        }
        self.after_edit(id, cx);
    }

    /// Replaces a request's contents with a pasted cURL command's.
    fn apply_curl(&mut self, id: u64, import: api::CurlImport, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_mut(id) else {
            return;
        };
        let name = tab.draft.name.clone();
        tab.draft = import.request;
        tab.draft.name = name;
        let warnings = import.warnings;
        self.refresh_inputs(id, cx);
        match warnings.first() {
            Some(first) => {
                let more = warnings.len() - 1;
                let suffix = if more > 0 {
                    format!(" (+{more} more)")
                } else {
                    String::new()
                };
                self.notify_user(format!("Imported the cURL command. {first}{suffix}"), cx)
            }
            None => self.notify_user("Imported the cURL command.", cx),
        }
    }

    /// Rebuilds every input of a tab from its draft.
    fn refresh_inputs(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let draft = tab.draft.clone();
        let (url, body, notes) = (tab.url.clone(), tab.body.clone(), tab.notes.clone());
        let auth = [
            tab.auth.username.clone(),
            tab.auth.password.clone(),
            tab.auth.token.clone(),
            tab.auth.key.clone(),
            tab.auth.value.clone(),
        ];
        set_text(&url, &draft.url_text(), cx);
        let language = match &draft.body {
            Body::Raw { language, .. } => *language,
            _ => self.tab(id).map_or(RawLanguage::Json, |t| t.raw_language),
        };
        body.update(cx, |input, cx| {
            input.set_editor_path(syntax_for(language).into());
            if let Body::Raw { text, .. } = &draft.body {
                input.set_text(text.clone(), cx);
            }
        });
        set_text(&notes, &draft.description, cx);
        let (pre_script, test_script) = match self.tab(id) {
            Some(tab) => (tab.pre_script.clone(), tab.test_script.clone()),
            None => return,
        };
        set_text(&pre_script, &draft.pre_request, cx);
        set_text(&test_script, &draft.tests, cx);
        let (username, password, token, key, value) = match &draft.auth {
            Auth::Basic { username, password } => (
                username.clone(),
                password.clone(),
                String::new(),
                String::new(),
                String::new(),
            ),
            Auth::Bearer { token } => (
                String::new(),
                String::new(),
                token.clone(),
                String::new(),
                String::new(),
            ),
            Auth::ApiKey { key, value, .. } => (
                String::new(),
                String::new(),
                String::new(),
                key.clone(),
                value.clone(),
            ),
            _ => Default::default(),
        };
        for (input, text) in auth.iter().zip([username, password, token, key, value]) {
            set_text(input, &text, cx);
        }
        let mut built = Vec::new();
        for (table, data) in Self::tables_of(&draft) {
            built.push((table, self.new_input_rows(id, table, data, cx)));
        }
        if let Some(tab) = self.tab_mut(id) {
            for (table, rows) in built {
                *tab.rows_mut(table) = rows;
            }
            tab.body_mode = Self::mode_of(&draft.body);
            tab.raw_language = language;
        }
        self.after_edit(id, cx);
    }

    fn rows_changed(&mut self, id: u64, table: Table, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let data = Self::read_rows(tab, table, cx);
        let url_text;
        {
            let Some(tab) = self.tab_mut(id) else {
                return;
            };
            match table {
                Table::Query => tab.draft.query = Self::key_values(&data),
                Table::Headers => tab.draft.headers = Self::key_values(&data),
                Table::Path => tab.draft.path_vars = Self::key_values(&data),
                Table::UrlEncoded | Table::Form => {}
            }
            url_text = tab.draft.url_text();
        }
        if matches!(table, Table::UrlEncoded | Table::Form) {
            self.body_changed(id, cx);
            return;
        }
        if table == Table::Query
            && let Some(tab) = self.tab(id)
        {
            let url = tab.url.clone();
            set_text(&url, &url_text, cx);
        }
        self.after_edit(id, cx);
    }

    fn compose_body(tab: &RequestTab, cx: &gpui::App) -> Body {
        match &tab.body_mode {
            BodyMode::None => Body::None,
            BodyMode::Raw => Body::Raw {
                text: text_of(&tab.body, cx),
                language: tab.raw_language,
            },
            BodyMode::UrlEncoded => Body::UrlEncoded(Self::key_values(&Self::read_rows(
                tab,
                Table::UrlEncoded,
                cx,
            ))),
            BodyMode::Form => Body::FormData(
                Self::read_rows(tab, Table::Form, cx)
                    .into_iter()
                    .map(|row| FormPart {
                        key: row.key,
                        value: row.value,
                        kind: if row.file {
                            PartKind::File
                        } else {
                            PartKind::Text
                        },
                        content_type: row.content_type,
                        disabled: row.disabled,
                    })
                    .collect(),
            ),
            BodyMode::Unsupported(mode) => Body::Unsupported(mode.clone()),
        }
    }

    fn body_changed(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let body = Self::compose_body(tab, cx);
        if let Some(tab) = self.tab_mut(id) {
            tab.draft.body = body;
        }
        self.after_edit(id, cx);
    }

    fn scripts_changed(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let (pre, test) = (text_of(&tab.pre_script, cx), text_of(&tab.test_script, cx));
        if let Some(tab) = self.tab_mut(id) {
            tab.draft.pre_request = pre;
            tab.draft.tests = test;
        }
        self.after_edit(id, cx);
    }

    fn notes_changed(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let text = text_of(&tab.notes, cx);
        if let Some(tab) = self.tab_mut(id) {
            tab.draft.description = text;
        }
        self.after_edit(id, cx);
    }

    fn auth_changed(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let (username, password, token, key, value) = (
            text_of(&tab.auth.username, cx),
            text_of(&tab.auth.password, cx),
            text_of(&tab.auth.token, cx),
            text_of(&tab.auth.key, cx),
            text_of(&tab.auth.value, cx),
        );
        if let Some(tab) = self.tab_mut(id) {
            match &mut tab.draft.auth {
                Auth::Basic {
                    username: u,
                    password: p,
                } => {
                    *u = username;
                    *p = password;
                }
                Auth::Bearer { token: t } => *t = token,
                Auth::ApiKey {
                    key: k, value: v, ..
                } => {
                    *k = key;
                    *v = value;
                }
                _ => {}
            }
        }
        self.after_edit(id, cx);
    }

    fn set_method(&mut self, id: u64, method: &str, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_mut(id) {
            tab.draft.method = method.to_owned();
        }
        self.after_edit(id, cx);
    }

    fn set_auth_kind(&mut self, id: u64, kind: &str, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let (username, password, token, key, value) = (
            text_of(&tab.auth.username, cx),
            text_of(&tab.auth.password, cx),
            text_of(&tab.auth.token, cx),
            text_of(&tab.auth.key, cx),
            text_of(&tab.auth.value, cx),
        );
        let in_header = !matches!(
            tab.draft.auth,
            Auth::ApiKey {
                in_header: false,
                ..
            }
        );
        if let Some(tab) = self.tab_mut(id) {
            tab.draft.auth = match kind {
                "none" => Auth::None,
                "basic" => Auth::Basic { username, password },
                "bearer" => Auth::Bearer { token },
                "apikey" => Auth::ApiKey {
                    key,
                    value,
                    in_header,
                },
                _ => Auth::Inherit,
            };
        }
        self.after_edit(id, cx);
    }

    fn set_api_key_placement(&mut self, id: u64, header: bool, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_mut(id)
            && let Auth::ApiKey { in_header, .. } = &mut tab.draft.auth
        {
            *in_header = header;
        }
        self.after_edit(id, cx);
    }

    fn set_body_mode(&mut self, id: u64, mode: BodyMode, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_mut(id) {
            if matches!(tab.body_mode, BodyMode::Unsupported(_)) {
                return;
            }
            tab.body_mode = mode;
        }
        self.body_changed(id, cx);
    }

    fn set_raw_language(&mut self, id: u64, language: RawLanguage, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_mut(id) else {
            return;
        };
        tab.raw_language = language;
        let body = tab.body.clone();
        body.update(cx, |input, _| {
            input.set_editor_path(syntax_for(language).into())
        });
        self.body_changed(id, cx);
    }

    fn add_row(&mut self, id: u64, table: Table, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let mut data = Self::read_rows(tab, table, cx);
        data.push(RowData {
            key: String::new(),
            value: String::new(),
            disabled: false,
            file: false,
            content_type: String::new(),
        });
        let rows = self.new_input_rows(id, table, data, cx);
        if let Some(tab) = self.tab_mut(id) {
            *tab.rows_mut(table) = rows;
        }
        self.rows_changed(id, table, cx);
    }

    fn row_edit(
        &mut self,
        id: u64,
        table: Table,
        index: usize,
        edit: impl FnOnce(&mut Vec<RowData>, usize),
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let mut data = Self::read_rows(tab, table, cx);
        if index >= data.len() {
            return;
        }
        edit(&mut data, index);
        let rows = self.new_input_rows(id, table, data, cx);
        if let Some(tab) = self.tab_mut(id) {
            *tab.rows_mut(table) = rows;
        }
        self.rows_changed(id, table, cx);
    }

    // ---- saving ----

    fn save(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        let Some(tab) = self.tab(id) else {
            return false;
        };
        let (file, path, draft) = (tab.file.clone(), tab.path.clone(), tab.draft.clone());
        let Some(loaded) = self.collection_mut(&file) else {
            self.notify_user("This collection's file is gone.", cx);
            return false;
        };
        if !loaded.collection.set_request(&path, &draft) {
            self.notify_user("This request is no longer in the collection.", cx);
            return false;
        }
        if let Err(error) = loaded.collection.save() {
            self.notify_user(error.to_string(), cx);
            return false;
        }
        self.touch(&file);
        if let Some(tab) = self.tab_mut(id) {
            tab.saved = draft;
        }
        cx.notify();
        true
    }

    fn revert(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_mut(id) {
            tab.draft = tab.saved.clone();
        }
        self.refresh_inputs(id, cx);
    }

    // ---- the tree ----

    fn save_collection(&mut self, file: &Path, cx: &mut Context<Self>) -> bool {
        let Some(loaded) = self.collection(file) else {
            return false;
        };
        match loaded.collection.save() {
            Ok(()) => {
                self.touch(file);
                true
            }
            Err(error) => {
                self.notify_user(error.to_string(), cx);
                false
            }
        }
    }

    /// Items of `parent` from `from` on moved by `delta`: open tabs follow.
    fn shift_tabs(&mut self, file: &Path, parent: &[usize], from: usize, delta: isize) {
        for tab in self.tabs.iter_mut().filter(|t| t.file == file) {
            if tab.path.len() > parent.len()
                && tab.path.starts_with(parent)
                && tab.path[parent.len()] >= from
            {
                let index = tab.path[parent.len()] as isize + delta;
                tab.path[parent.len()] = index.max(0) as usize;
            }
        }
    }

    fn new_item(
        &mut self,
        file: &Path,
        parent: &[usize],
        folder: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(loaded) = self.collection_mut(file) else {
            return;
        };
        let added = if folder {
            loaded.collection.add_folder(parent, "New Folder")
        } else {
            loaded.collection.add_request(parent, "New Request")
        };
        let Some(path) = added else {
            return;
        };
        if !self.save_collection(file, cx) {
            return;
        }
        for key in [Self::collection_key(file), Self::folder_key(file, parent)] {
            self.collapsed.remove(&key);
        }
        if folder {
            self.start_rename(
                Target {
                    file: file.to_owned(),
                    path,
                },
                window,
                cx,
            );
        } else {
            self.open_request(file, path.clone(), window, cx);
            self.start_rename(
                Target {
                    file: file.to_owned(),
                    path,
                },
                window,
                cx,
            );
        }
    }

    fn start_rename(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let Some(loaded) = self.collection(&target.file) else {
            return;
        };
        let Some(name) = loaded.collection.item_name(&target.path) else {
            return;
        };
        set_text(&self.dialog_input, &name, cx);
        self.dialog = Some(DialogKind::Rename(target));
        window.focus(&self.dialog_input.focus_handle(cx), cx);
        cx.notify();
    }

    fn start_new_collection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.target_project().is_none() {
            self.notify_user("Open a project first.", cx);
            return;
        }
        set_text(&self.dialog_input, "", cx);
        self.dialog = Some(DialogKind::NewCollection);
        window.focus(&self.dialog_input.focus_handle(cx), cx);
        cx.notify();
    }

    fn confirm_dialog(&mut self, cx: &mut Context<Self>) {
        let name = text_of(&self.dialog_input, cx).trim().to_owned();
        match self.dialog.take() {
            Some(DialogKind::Rename(target)) => {
                if name.is_empty() {
                    self.dialog = Some(DialogKind::Rename(target));
                    return;
                }
                let Some(loaded) = self.collection_mut(&target.file) else {
                    return;
                };
                loaded.collection.rename(&target.path, &name);
                if self.save_collection(&target.file, cx) {
                    for tab in self.tabs.iter_mut().filter(|t| t.file == target.file) {
                        if tab.path == target.path {
                            tab.draft.name = name.clone();
                            tab.saved.name = name.clone();
                        }
                    }
                }
            }
            Some(DialogKind::NewCollection) => {
                if name.is_empty() {
                    self.dialog = Some(DialogKind::NewCollection);
                    return;
                }
                let Some(project) = self.target_project() else {
                    return;
                };
                let file = free_path(&api_dir(&project), &file_stem(&name), COLLECTION_SUFFIX);
                let mut collection = Collection::new(&name);
                collection.set_path(file.clone());
                if let Err(error) = collection.save() {
                    self.notify_user(error.to_string(), cx);
                } else {
                    self.reload(cx);
                }
            }
            Some(DialogKind::Delete(target)) => self.delete(&target, cx),
            Some(DialogKind::CloseDirty(_) | DialogKind::CloseMany(_)) | None => {}
        }
        cx.notify();
    }

    fn delete(&mut self, target: &Target, cx: &mut Context<Self>) {
        if target.path.is_empty() {
            // The whole collection: its file.
            if let Err(error) = std::fs::remove_file(&target.file) {
                self.notify_user(format!("Could not delete the file: {error}"), cx);
                return;
            }
            self.tabs.retain(|t| t.file != target.file);
            self.reload(cx);
            return;
        }
        let Some(loaded) = self.collection_mut(&target.file) else {
            return;
        };
        if !loaded.collection.remove(&target.path) {
            return;
        }
        if !self.save_collection(&target.file, cx) {
            return;
        }
        let (index, parent) = match target.path.split_last() {
            Some((index, parent)) => (*index, parent.to_vec()),
            None => return,
        };
        self.tabs
            .retain(|t| !(t.file == target.file && t.path.starts_with(&target.path)));
        self.shift_tabs(&target.file, &parent, index + 1, -1);
        if self.active.is_some_and(|id| self.tab(id).is_none()) {
            self.active = self.tabs.last().map(|t| t.id);
        }
    }

    fn duplicate(&mut self, target: &Target, cx: &mut Context<Self>) {
        let Some(loaded) = self.collection_mut(&target.file) else {
            return;
        };
        let Some(copy) = loaded.collection.duplicate(&target.path) else {
            return;
        };
        if !self.save_collection(&target.file, cx) {
            return;
        }
        if let Some((index, parent)) = copy.split_last() {
            self.shift_tabs(&target.file, parent, *index, 1);
        }
    }

    fn copy_item_curl(&mut self, target: &Target, cx: &mut Context<Self>) {
        let Some(request) = self
            .collection(&target.file)
            .and_then(|c| c.collection.request(&target.path))
        else {
            return;
        };
        let tab_like = self
            .tabs
            .iter()
            .find(|t| t.file == target.file && t.path == target.path)
            .map(|t| t.draft.clone())
            .unwrap_or(request);
        let scope = self.variables_scope(&target.file, &target.path);
        let auth = match &tab_like.auth {
            Auth::Inherit => self
                .collection(&target.file)
                .map(|c| c.collection.inherited_auth(&target.path))
                .unwrap_or(Auth::None),
            own => own.clone(),
        };
        let prepared = api::prepare(&tab_like, &auth, &scope);
        cx.write_to_clipboard(ClipboardItem::new_string(api::to_curl(&prepared)));
        self.notify_user("Copied as cURL.", cx);
    }

    fn open_menu(
        &mut self,
        kind: MenuKind,
        items: Vec<MenuItem>,
        label: &'static str,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        let menu = cx.new(|cx| Menu::new(label, items, window, cx));
        let events = cx.subscribe_in(&menu, window, |this, _, event: &MenuEvent, window, cx| {
            let Some(open) = this.menu.take() else {
                return;
            };
            if let MenuEvent::Picked(id, _) = event {
                this.run_menu_action(id, open.kind, window, cx);
            }
            cx.notify();
        });
        self.menu = Some(OpenMenu {
            kind,
            anchor: at,
            menu,
            _events: events,
        });
        cx.notify();
    }

    fn open_node_menu(
        &mut self,
        target: Target,
        folder: bool,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut items: Vec<MenuItem> = Vec::new();
        let container = folder || target.path.is_empty();
        if container {
            items.push(MenuAction::new("new-request", "New request").into());
            items.push(MenuAction::new("new-folder", "New folder").into());
            items.push(MenuItem::Separator);
        } else {
            items.push(MenuAction::new("open", "Open").into());
            items.push(MenuAction::new("copy-curl", "Copy as cURL").into());
            items.push(MenuItem::Separator);
        }
        items.push(MenuAction::new("rename", "Rename…").into());
        if !target.path.is_empty() {
            items.push(MenuAction::new("duplicate", "Duplicate").into());
        }
        items.push(MenuAction::new("reveal", "Show in file manager").into());
        items.push(MenuItem::Separator);
        items.push(MenuAction::new("delete", "Delete…").danger().into());
        self.open_menu(
            MenuKind::Node(target, folder),
            items,
            "Collection actions",
            at,
            window,
            cx,
        );
    }

    fn run_menu_action(
        &mut self,
        id: &str,
        kind: MenuKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match kind {
            MenuKind::Method(tab) => self.set_method(tab, id, cx),
            MenuKind::Auth(tab) => self.set_auth_kind(tab, id, cx),
            MenuKind::BodyLanguage(tab) => {
                let language = RawLanguage::from_id(id);
                self.set_raw_language(tab, language, cx);
            }
            MenuKind::Environment(project) => match id {
                "manage" => self.open_env_editor(None, cx),
                "none" => self.select_environment(&project, None),
                path => self.select_environment(&project, Some(PathBuf::from(path))),
            },
            MenuKind::Tab(tab) => self.run_tab_action(tab, id, window, cx),
            MenuKind::Tabs => self.run_tabs_action(id, window, cx),
            MenuKind::Add => match id {
                "collection" => self.start_new_collection(window, cx),
                "import" => self.import(cx),
                "request" | "folder" => {
                    let file = self
                        .active_tab()
                        .map(|t| t.file.clone())
                        .or_else(|| self.collections.first().map(|c| c.file.clone()));
                    if let Some(file) = file {
                        self.new_item(&file, &[], id == "folder", window, cx);
                    }
                }
                _ => {}
            },
            MenuKind::Node(target, folder) => match id {
                "new-request" => self.new_item(&target.file, &target.path, false, window, cx),
                "new-folder" => self.new_item(&target.file, &target.path, true, window, cx),
                "open" => self.open_request(&target.file, target.path.clone(), window, cx),
                "copy-curl" => self.copy_item_curl(&target, cx),
                "rename" => {
                    let _ = folder;
                    self.start_rename(target, window, cx)
                }
                "duplicate" => self.duplicate(&target, cx),
                "reveal" => cx.reveal_path(&target.file),
                "delete" => {
                    self.dialog = Some(DialogKind::Delete(target));
                }
                _ => {}
            },
        }
    }

    /// The names of the open request tabs, for tests of the shell.
    #[cfg(test)]
    pub(crate) fn open_request_names(&self) -> Vec<String> {
        self.tabs.iter().map(|t| t.draft.name.clone()).collect()
    }

    // ---- from the rest of Monocode ----

    /// Makes a request of a cURL command (from a reply) in the project's
    /// collection and opens it.
    pub(crate) fn import_curl_request(
        &mut self,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let import = match api::parse_curl(command) {
            Ok(import) => import,
            Err(error) => {
                self.notify_user(error.to_string(), cx);
                return;
            }
        };
        let Some(project) = self.target_project() else {
            self.notify_user("Open a project first.", cx);
            return;
        };
        let existing = self
            .active_tab()
            .and_then(|t| self.collection(&t.file))
            .filter(|c| c.project == project)
            .or_else(|| self.collections.iter().find(|c| c.project == project))
            .map(|c| c.file.clone());
        let file = match existing {
            Some(file) => file,
            None => {
                let file = free_path(&api_dir(&project), "api", COLLECTION_SUFFIX);
                let mut collection = Collection::new(&project_name(&project));
                collection.set_path(file.clone());
                if let Err(error) = collection.save() {
                    self.notify_user(error.to_string(), cx);
                    return;
                }
                self.reload(cx);
                file
            }
        };
        let mut request = import.request;
        let name = request.name.clone();
        request.name = name.clone();
        let added = self
            .collection_mut(&file)
            .and_then(|loaded| {
                loaded
                    .collection
                    .add_request(&[], &name)
                    .map(|p| (loaded, p))
            })
            .map(|(loaded, path)| {
                loaded.collection.set_request(&path, &request);
                path
            });
        let Some(path) = added else {
            self.notify_user("Could not add the request to the collection.", cx);
            return;
        };
        if !self.save_collection(&file, cx) {
            return;
        }
        self.open_request(&file, path, window, cx);
        match import.warnings.first() {
            Some(first) => self.notify_user(format!("Added “{name}”. {first}"), cx),
            None => self.notify_user(format!("Added “{name}” from the cURL command."), cx),
        }
    }

    // ---- environments ----

    fn env_row(
        &mut self,
        key: &str,
        value: &str,
        enabled: bool,
        secret: bool,
        cx: &mut Context<Self>,
    ) -> EnvRow {
        let key_input = field("Variable", "Variable", cx);
        let value_input = field("Value", "Value", cx);
        value_input.update(cx, |input, _| input.set_secret(secret));
        set_text(&key_input, key, cx);
        set_text(&value_input, value, cx);
        let subscriptions = vec![
            cx.subscribe(&key_input, |this, _, _: &input::Changed, cx| {
                this.env_changed(cx)
            }),
            cx.subscribe(&value_input, |this, _, _: &input::Changed, cx| {
                this.env_changed(cx)
            }),
        ];
        EnvRow {
            key: key_input,
            value: value_input,
            enabled,
            secret,
            _subscriptions: subscriptions,
        }
    }

    fn env_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.env_editor {
            editor.dirty = true;
        }
        cx.notify();
    }

    /// Opens the editor on `target`, else the environment in use, else the
    /// first thing there is to edit, else makes an environment.
    fn open_env_editor(&mut self, target: Option<EnvTarget>, cx: &mut Context<Self>) {
        let project = self.current_project();
        let target = target
            .or_else(|| {
                let file = self.environment.get(project.as_deref()?)?;
                Some(EnvTarget::Environment(file.clone()))
            })
            .or_else(|| {
                self.environments
                    .iter()
                    .find(|e| Some(&e.project) == project.as_ref())
                    .or_else(|| self.environments.first())
                    .map(|e| EnvTarget::Environment(e.file.clone()))
            })
            .or_else(|| {
                self.collections
                    .first()
                    .map(|c| EnvTarget::Variables(c.file.clone()))
            });
        match target {
            Some(target) => self.load_env_editor(target, cx),
            None => self.new_environment(cx),
        }
    }

    fn load_env_editor(&mut self, target: EnvTarget, cx: &mut Context<Self>) {
        let (name, rows): (String, Vec<(String, String, bool, bool)>) = match &target {
            EnvTarget::Environment(file) => {
                let Some(loaded) = self.environments.iter().find(|e| &e.file == file) else {
                    return;
                };
                (
                    loaded.environment.name(),
                    loaded
                        .environment
                        .values()
                        .into_iter()
                        .map(|v| (v.key, v.value, v.enabled, v.secret))
                        .collect(),
                )
            }
            EnvTarget::Variables(file) => {
                let Some(loaded) = self.collection(file) else {
                    return;
                };
                (
                    loaded.collection.name(),
                    loaded
                        .collection
                        .own_variables()
                        .into_iter()
                        .map(|v| (v.key, v.value, !v.disabled, false))
                        .collect(),
                )
            }
        };
        let name_input = field("Environment name", "Environment name", cx);
        set_text(&name_input, &name, cx);
        let subscription = cx.subscribe(&name_input, |this, _, _: &input::Changed, cx| {
            this.env_changed(cx)
        });
        let mut built = Vec::new();
        for (key, value, enabled, secret) in rows {
            built.push(self.env_row(&key, &value, enabled, secret, cx));
        }
        self.env_editor = Some(EnvEditor {
            target,
            name: name_input,
            rows: built,
            dirty: false,
            confirm_delete: false,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Writes the editor's rows to their file. False when that failed.
    fn save_env_editor(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(editor) = &self.env_editor else {
            return true;
        };
        if !editor.dirty {
            return true;
        }
        let rows: Vec<(String, String, bool, bool)> = editor
            .rows
            .iter()
            .map(|row| {
                (
                    text_of(&row.key, cx).trim().to_owned(),
                    text_of(&row.value, cx),
                    row.enabled,
                    row.secret,
                )
            })
            .filter(|(key, value, ..)| !key.is_empty() || !value.is_empty())
            .collect();
        let name = text_of(&editor.name, cx).trim().to_owned();
        let target = editor.target.clone();
        let result = match &target {
            EnvTarget::Environment(file) => {
                let Some(loaded) = self.environments.iter_mut().find(|e| &e.file == file) else {
                    return false;
                };
                let values: Vec<api::EnvVar> = rows
                    .into_iter()
                    .map(|(key, value, enabled, secret)| api::EnvVar {
                        key,
                        value,
                        enabled,
                        secret,
                    })
                    .collect();
                loaded.environment.set_values(&values);
                if !name.is_empty() {
                    loaded.environment.set_name(&name);
                }
                loaded.environment.save()
            }
            EnvTarget::Variables(file) => {
                let Some(loaded) = self.collection_mut(file) else {
                    return false;
                };
                let values: Vec<KeyValue> = rows
                    .into_iter()
                    .map(|(key, value, enabled, _)| KeyValue {
                        key,
                        value,
                        disabled: !enabled,
                    })
                    .collect();
                loaded.collection.set_variables(&values);
                loaded.collection.save()
            }
        };
        match result {
            Ok(()) => {
                let (EnvTarget::Environment(file) | EnvTarget::Variables(file)) = &target;
                self.touch(file);
                if let Some(editor) = &mut self.env_editor {
                    editor.dirty = false;
                }
                cx.notify();
                true
            }
            Err(error) => {
                self.notify_user(error.to_string(), cx);
                false
            }
        }
    }

    fn close_env_editor(&mut self, save: bool, cx: &mut Context<Self>) {
        if save && !self.save_env_editor(cx) {
            return;
        }
        self.env_editor = None;
        cx.notify();
    }

    fn switch_env_target(&mut self, target: EnvTarget, cx: &mut Context<Self>) {
        if self.env_editor.as_ref().is_some_and(|e| e.target == target) {
            return;
        }
        if self.save_env_editor(cx) {
            self.load_env_editor(target, cx);
        }
    }

    fn new_environment(&mut self, cx: &mut Context<Self>) {
        if !self.save_env_editor(cx) {
            return;
        }
        let Some(project) = self.target_project() else {
            self.notify_user("Open a project first.", cx);
            return;
        };
        let file = free_path(
            &api_dir(&project),
            &file_stem("new environment"),
            ENVIRONMENT_SUFFIX,
        );
        let mut environment = Environment::new("New Environment");
        environment.set_path(file.clone());
        if let Err(error) = environment.save() {
            self.notify_user(error.to_string(), cx);
            return;
        }
        self.reload(cx);
        self.select_environment(&project, Some(file.clone()));
        self.load_env_editor(EnvTarget::Environment(file), cx);
    }

    fn delete_environment(&mut self, cx: &mut Context<Self>) {
        let Some(EnvTarget::Environment(file)) = self.env_editor.as_ref().map(|e| e.target.clone())
        else {
            return;
        };
        if let Err(error) = std::fs::remove_file(&file) {
            self.notify_user(format!("Could not delete the file: {error}"), cx);
            return;
        }
        self.env_editor = None;
        self.reload(cx);
        if self.environments.is_empty() && self.collections.is_empty() {
            cx.notify();
        } else {
            self.open_env_editor(None, cx);
        }
    }

    fn use_environment(&mut self, file: PathBuf, cx: &mut Context<Self>) {
        if let Some(project) = self
            .environments
            .iter()
            .find(|e| e.file == file)
            .map(|e| e.project.clone())
        {
            self.select_environment(&project, Some(file));
        }
        cx.notify();
    }

    /// Whether `file` is the environment in use for its project.
    fn env_in_use(&self, file: &Path) -> bool {
        self.environments
            .iter()
            .find(|e| e.file == file)
            .is_some_and(|e| self.environment.get(&e.project).is_some_and(|f| f == file))
    }

    fn add_env_row(&mut self, cx: &mut Context<Self>) {
        let row = self.env_row("", "", true, false, cx);
        if let Some(editor) = &mut self.env_editor {
            editor.rows.push(row);
            editor.dirty = true;
        }
        cx.notify();
    }

    fn toggle_env_secret(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.env_editor
            && let Some(row) = editor.rows.get_mut(index)
        {
            row.secret = !row.secret;
            let secret = row.secret;
            row.value.update(cx, |input, _| input.set_secret(secret));
            editor.dirty = true;
        }
        cx.notify();
    }

    fn env_row_edit(
        &mut self,
        index: usize,
        edit: impl FnOnce(&mut Vec<EnvRow>, usize),
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = &mut self.env_editor
            && index < editor.rows.len()
        {
            edit(&mut editor.rows, index);
            editor.dirty = true;
        }
        cx.notify();
    }

    // ---- importing ----

    /// Copies Postman collection and environment files into the project.
    fn import(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.target_project() else {
            self.notify_user("Open a project first.", cx);
            return;
        };
        let picker = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picker.await {
                let _ = this.update(cx, |this, cx| this.import_files(&project, &paths, cx));
            }
        })
        .detach();
    }

    fn import_files(&mut self, project: &str, paths: &[PathBuf], cx: &mut Context<Self>) {
        let dir = api_dir(project);
        let mut imported = 0;
        let mut problems: Vec<String> = Vec::new();
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) => {
                    problems.push(format!("{name}: {error}"));
                    continue;
                }
            };
            let stem = file_stem(name.split(".postman_").next().unwrap_or(&name));
            let stem = stem.trim_end_matches(".json").to_owned();
            let result = if let Ok(mut collection) = Collection::parse(&text) {
                let file = free_path(&dir, &file_stem(&collection.name()), COLLECTION_SUFFIX);
                collection.set_path(file);
                collection.save()
            } else {
                match Environment::parse(&text) {
                    Ok(mut environment) => {
                        let file = free_path(&dir, &stem, ENVIRONMENT_SUFFIX);
                        environment.set_path(file);
                        environment.save()
                    }
                    Err(_) => Err(api::ApiError::new(
                        "Not a Postman collection or environment.",
                    )),
                }
            };
            match result {
                Ok(()) => imported += 1,
                Err(error) => problems.push(format!("{name}: {error}")),
            }
        }
        self.reload(cx);
        let mut message = match imported {
            0 => String::new(),
            1 => "Imported 1 file.".to_owned(),
            n => format!("Imported {n} files."),
        };
        if let Some(first) = problems.first() {
            message.push_str(&format!(" {first}"));
        }
        if !message.is_empty() {
            self.notify_user(message.trim().to_owned(), cx);
        }
    }
}

/// What the response panel needs from a response, computed once.
fn summarize(response: &api::Response) -> Done {
    let size = response.body.len();
    let shown = &response.body[..size.min(SHOWN_BYTES)];
    let clipped = size > SHOWN_BYTES;
    let (raw, binary) = match std::str::from_utf8(shown) {
        Ok(text) => (text.to_owned(), false),
        // A cut can land inside a character; only the cut itself is excused.
        Err(error) if clipped && error.error_len().is_none() => (
            String::from_utf8_lossy(&shown[..error.valid_up_to()]).into_owned(),
            false,
        ),
        Err(_) => (format!("Binary data, {}.", human_size(size)), true),
    };
    let json = (!binary && !clipped)
        .then(|| response.pretty_json())
        .flatten();
    let content_type = response.content_type().to_ascii_lowercase();
    let syntax = if json.is_some() {
        "response.json"
    } else if content_type.contains("xml") {
        "response.xml"
    } else if content_type.contains("html") {
        "response.html"
    } else {
        "response.txt"
    };
    let mut pretty = json.unwrap_or_else(|| raw.clone());
    let mut raw = raw;
    if response.truncated || clipped {
        let note = format!("\n\n… showing the first {}.", human_size(shown.len()));
        pretty.push_str(&note);
        raw.push_str(&note);
    }
    Done {
        status: response.status,
        reason: response.reason.clone(),
        headers: response.headers.clone(),
        elapsed: response.elapsed,
        size,
        truncated: response.truncated || clipped,
        pretty,
        raw,
        syntax,
    }
}
