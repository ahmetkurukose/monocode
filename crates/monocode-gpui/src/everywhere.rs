//! Search Everywhere (⇧⇧): one modal over files, file contents, symbols,
//! agent conversations, projects and actions.
//!
//! Every source runs off the UI thread and answers for one query
//! generation; a newer keystroke cancels what is still running. File names
//! come from a cached index (`monocode_core::search::fast`), so the list is
//! there the moment the modal opens and is refreshed behind it.
use crate::{
    code_shell::commands::{self as features, Action},
    input::{self, Input},
    runtime::{Database, locked},
    theme::*,
};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    HighlightStyle, MouseButton, MouseMoveEvent, Pixels, Point, Render, Role, ScrollHandle,
    SharedString, StyledText, Subscription, Task, Window, div, prelude::*, px, relative,
};
use monocode_core::{
    search::{
        self as core_search, Hit, Target,
        fast::{self, FileIndex},
        project::SearchOptions,
    },
    store::Session,
    transcript::Row as TranscriptRow,
};
use std::{
    collections::HashMap,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// A file index older than this is listed again behind the cached one.
const INDEX_FRESH_MS: u64 = 15_000;
const FILE_ROWS: usize = 120;
const CONTENT_ROWS: usize = 200;
/// How many rows of each kind the All tab shows.
const ALL_LIMITS: Limits = Limits {
    files: 6,
    content: 6,
    symbols: 5,
    conversations: 4,
    messages: 4,
    projects: 3,
    actions: 4,
};
/// The widest modal, with the preview beside the results.
const WIDE: f32 = 1080.;
const NARROW: f32 = 720.;
/// Below this the preview does not fit beside the list.
const PREVIEW_MIN: f32 = 940.;
const LIST_HEIGHT: f32 = 432.;

struct Limits {
    files: usize,
    content: usize,
    symbols: usize,
    conversations: usize,
    messages: usize,
    projects: usize,
    actions: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub(crate) enum Tab {
    #[default]
    All,
    Files,
    Content,
    Symbols,
    Agents,
    Projects,
    Actions,
}
impl Tab {
    pub(crate) const ALL: [Tab; 7] = [
        Tab::All,
        Tab::Files,
        Tab::Content,
        Tab::Symbols,
        Tab::Agents,
        Tab::Projects,
        Tab::Actions,
    ];
    fn label(self) -> &'static str {
        match self {
            Tab::All => "All",
            Tab::Files => "Files",
            Tab::Content => "Content",
            Tab::Symbols => "Symbols",
            Tab::Agents => "Agents",
            Tab::Projects => "Projects",
            Tab::Actions => "Actions",
        }
    }
    fn glyph(self) -> &'static str {
        match self {
            Tab::All => "sparkles",
            Tab::Files => "file",
            Tab::Content => "search",
            Tab::Symbols => "command-compact",
            Tab::Agents => "bot",
            Tab::Projects => "folder",
            Tab::Actions => "keyboard",
        }
    }
    /// Typed first on the All tab, jumps to the tab: `>` actions, `@`
    /// symbols, `#` contents.
    fn prefix(self) -> Option<char> {
        match self {
            Tab::Actions => Some('>'),
            Tab::Symbols => Some('@'),
            Tab::Content => Some('#'),
            _ => None,
        }
    }
    fn placeholder(self) -> &'static str {
        match self {
            Tab::All => "Search files, code, agents, projects and actions…",
            Tab::Files => "Search files by name  (main.rs:42 jumps to a line)",
            Tab::Content => "Search inside files…",
            Tab::Symbols => "Search symbols…",
            Tab::Agents => "Search agent conversations and messages…",
            Tab::Projects => "Search projects…",
            Tab::Actions => "Search actions…",
        }
    }
    fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

/// An action the modal offers: a palette command, already resolved.
#[derive(Clone)]
pub(crate) struct ActionItem {
    pub id: SharedString,
    pub group: SharedString,
    pub title: SharedString,
    pub keys: Option<SharedString>,
    /// Shown in place of the group: what a feature's command acts on.
    pub note: Option<SharedString>,
    pub glyph: &'static str,
    pub run: Action,
}
impl ActionItem {
    pub(crate) fn note_or_group(&self) -> SharedString {
        self.note.clone().unwrap_or_else(|| self.group.clone())
    }
}

pub(crate) struct Sources {
    pub cwd: String,
    pub store: Database,
    pub summaries: Vec<Session>,
    pub live: HashMap<i64, Arc<Vec<TranscriptRow>>>,
    pub projects: Vec<String>,
    pub recent_files: Vec<String>,
    pub logos: HashMap<String, String>,
    pub actions: Vec<ActionItem>,
    pub include_ignored: bool,
}

pub(crate) enum Event {
    OpenFile {
        path: String,
        at: Option<(u32, u32)>,
    },
    Conversation(i64),
    Message {
        session: i64,
        row: usize,
        query: String,
    },
    Project(String),
    Run(Action),
    AskAgent(String),
    FindInPanel(String),
    Close,
}

#[derive(Clone)]
enum Mark {
    File(SharedString),
    Glyph(&'static str),
    Provider(SharedString),
    Logo(SharedString),
}

#[derive(Clone)]
enum Pick {
    File {
        path: String,
        at: Option<(u32, u32)>,
    },
    Conversation(i64),
    Message {
        session: i64,
        row: usize,
    },
    Project(String),
    /// An index into the modal's actions: rows are built off the UI
    /// thread, where an action's closure cannot go.
    Run(usize),
    AskAgent(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    RecentFiles,
    Files,
    Content,
    Symbols,
    RecentAgents,
    Conversations,
    Messages,
    Ask,
    Projects,
    RecentActions,
    Actions,
}
impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::RecentFiles => "Recent files",
            Section::Files => "Files",
            Section::Content => "In file contents",
            Section::Symbols => "Symbols",
            Section::RecentAgents => "Recent conversations",
            Section::Conversations => "Conversations",
            Section::Messages => "Messages",
            Section::Ask => "Ask an agent",
            Section::Projects => "Projects",
            Section::RecentActions => "Recently used",
            Section::Actions => "Actions",
        }
    }
    /// The tab that lists every row of the section.
    fn tab(self) -> Tab {
        match self {
            Section::RecentFiles | Section::Files => Tab::Files,
            Section::Content => Tab::Content,
            Section::Symbols => Tab::Symbols,
            Section::RecentAgents | Section::Conversations | Section::Messages | Section::Ask => {
                Tab::Agents
            }
            Section::Projects => Tab::Projects,
            Section::RecentActions | Section::Actions => Tab::Actions,
        }
    }
}

#[derive(Clone)]
struct Row {
    section: Section,
    title: SharedString,
    title_ranges: Vec<Range<usize>>,
    detail: SharedString,
    detail_ranges: Vec<Range<usize>>,
    mark: Mark,
    /// The title is a line of code.
    code: bool,
    keys: Option<SharedString>,
    pick: Pick,
}
impl Row {
    fn file_target(&self) -> Option<(&str, Option<(u32, u32)>)> {
        match &self.pick {
            Pick::File { path, at } => Some((path.as_str(), *at)),
            _ => None,
        }
    }
}

/// What a source found for one query.
#[derive(Default)]
struct Found {
    rows: Vec<Row>,
    loading: bool,
    elapsed: Option<Duration>,
}

/// The text of a file around the row in front.
struct Preview {
    path: String,
    line: u32,
    /// First line shown (1-based) and the lines.
    first: u32,
    lines: Vec<SharedString>,
    /// Where the query is on each line, worked out with the lines.
    ranges: Vec<Vec<Range<usize>>>,
    /// The query and flags the ranges are for.
    query: String,
    flags: ContentFlags,
    error: Option<SharedString>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct ContentFlags {
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
}

pub(crate) struct Everywhere {
    pub(crate) input: Entity<Input>,
    include: Entity<Input>,
    cwd: String,
    store: Database,
    summaries: Vec<Session>,
    live: HashMap<i64, Arc<Vec<TranscriptRow>>>,
    projects: Vec<String>,
    recent_files: Vec<String>,
    logos: HashMap<String, String>,
    actions: Vec<ActionItem>,
    include_ignored: bool,

    tab: Tab,
    flags: ContentFlags,
    index: Option<Arc<FileIndex>>,
    index_loading: bool,
    generation: u64,
    /// Set by each new query: what still runs for the one before stops.
    cancel: Arc<AtomicBool>,
    /// Set when the modal closes: listing the project outlives queries.
    index_cancel: Arc<AtomicBool>,

    files: Found,
    content: Found,
    content_truncated: bool,
    symbols: Found,
    symbols_hint: Option<&'static str>,
    agents: Found,
    agent_error: Option<String>,
    content_error: Option<String>,

    rows: Vec<Row>,
    /// List child of each row, counting section headings.
    children: Vec<usize>,
    active: usize,
    scroll: ScrollHandle,
    pointer: Option<Point<Pixels>>,
    preview_on: bool,
    preview: Option<Preview>,
    preview_task: Option<Task<()>>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Event> for Everywhere {}
impl Focusable for Everywhere {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}
impl Drop for Everywhere {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.index_cancel.store(true, Ordering::Release);
    }
}

/// Recently run actions, newest first, kept for the app's lifetime.
#[derive(Default)]
pub(crate) struct ActionHistory(pub Vec<SharedString>);
impl gpui::Global for ActionHistory {}
pub(crate) fn remember_action(id: SharedString, cx: &mut App) {
    let history = cx.default_global::<ActionHistory>();
    history.0.retain(|known| *known != id);
    history.0.insert(0, id);
    history.0.truncate(12);
}
fn action_history(cx: &App) -> Vec<SharedString> {
    cx.try_global::<ActionHistory>()
        .map(|h| h.0.clone())
        .unwrap_or_default()
}

/// Ranks `actions` for `query`: by fuzzy score on "Group: Title" with the
/// title's highlights, recently run ones first when nothing is typed.
pub(crate) fn rank_actions(
    actions: &[ActionItem],
    query: &str,
    history: &[SharedString],
) -> Vec<(ActionItem, Vec<Range<usize>>, bool)> {
    let query = query.trim();
    if query.is_empty() {
        let mut recent: Vec<_> = history
            .iter()
            .filter_map(|id| actions.iter().find(|a| &a.id == id))
            .take(5)
            .map(|a| (a.clone(), vec![], true))
            .collect();
        recent.extend(actions.iter().map(|a| (a.clone(), vec![], false)));
        return recent;
    }
    let mut scored: Vec<_> = actions
        .iter()
        .filter_map(|a| {
            let (title_score, ranges) = core_search::fuzzy(query, &a.title)
                .map(|(s, r)| (s + 40, r))
                .or_else(|| {
                    let full = format!("{}: {}", a.group, a.title);
                    core_search::fuzzy(query, &full).map(|(s, _)| (s, vec![]))
                })?;
            let recent = history.iter().position(|id| *id == a.id);
            let bonus = recent.map_or(0, |ix| (12 - ix.min(12)) as i64 * 4);
            Some((title_score + bonus, a.clone(), ranges))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.title.len().cmp(&b.1.title.len())));
    scored.into_iter().map(|(_, a, r)| (a, r, false)).collect()
}

impl Everywhere {
    pub(crate) fn new(sources: Sources, tab: Tab, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| Input::command_field("Search Everywhere", tab.placeholder(), cx));
        let include = cx.new(|cx| {
            let mut field = Input::field(
                "Files to include",
                "Files to include, e.g. src, *.rs",
                content(0.32),
                cx,
            );
            field.set_font_size(12.);
            field
        });
        let subscriptions = vec![
            cx.subscribe(&input, |this, _, _: &input::Changed, cx| {
                this.active = 0;
                this.refresh(cx);
            }),
            cx.subscribe(&include, |this, _, _: &input::Changed, cx| {
                if this.effective(cx).0 == Tab::Content {
                    this.refresh(cx);
                }
            }),
        ];
        let index = fast::cached_index(&sources.cwd, sources.include_ignored);
        let mut this = Self {
            input,
            include,
            cwd: sources.cwd,
            store: sources.store,
            summaries: sources.summaries,
            live: sources.live,
            projects: sources.projects,
            recent_files: sources.recent_files,
            logos: sources.logos,
            actions: sources.actions,
            include_ignored: sources.include_ignored,
            tab,
            flags: ContentFlags::default(),
            index,
            index_loading: false,
            generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            index_cancel: Arc::new(AtomicBool::new(false)),
            files: Found::default(),
            content: Found::default(),
            content_truncated: false,
            symbols: Found::default(),
            symbols_hint: None,
            agents: Found::default(),
            agent_error: None,
            content_error: None,
            rows: vec![],
            children: vec![],
            active: 0,
            scroll: ScrollHandle::new(),
            pointer: None,
            preview_on: true,
            preview: None,
            preview_task: None,
            tasks: vec![],
            _subscriptions: subscriptions,
        };
        this.load_index(cx);
        this.refresh(cx);
        this
    }

    /// Lists the project behind the cached index when that is missing or old.
    fn load_index(&mut self, cx: &mut Context<Self>) {
        let fresh = self.index.as_ref().is_some_and(|index| {
            monocode_core::time::now_ms().saturating_sub(index.built_at()) < INDEX_FRESH_MS
        });
        if fresh {
            return;
        }
        self.index_loading = true;
        let cwd = self.cwd.clone();
        let include_ignored = self.include_ignored;
        let cancel = self.index_cancel.clone();
        let work = cx
            .background_executor()
            .spawn(async move { fast::load_index(&cwd, include_ignored, &cancel) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.index_loading = false;
                if let Ok(index) = result {
                    this.index = Some(index);
                    this.search_files(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The tab whose rows are listed, and the query for it: on the All tab a
    /// leading `>`, `@` or `#` switches to its tab.
    fn effective(&self, cx: &App) -> (Tab, String) {
        let text = self.input.read(cx).content.trim_start();
        if self.tab == Tab::All
            && let Some(first) = text.chars().next()
            && let Some(tab) = Tab::ALL.iter().find(|t| t.prefix() == Some(first))
            // Where there are no symbols, `@types` is a folder's name.
            && (*tab != Tab::Symbols
                || features::mode_with_prefix('@', cx).is_some_and(|mode| (mode.applies)(cx)))
        {
            return (*tab, text[first.len_utf8()..].trim().to_owned());
        }
        (self.tab, text.trim().to_owned())
    }

    pub(crate) fn tab(&self) -> Tab {
        self.tab
    }

    pub(crate) fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        // A prefix typed on All gives way to the tab it stood for.
        let text = self.input.read(cx).content.to_string();
        if self.tab == Tab::All
            && let Some(first) = text.trim_start().chars().next()
            && Tab::ALL.iter().any(|t| t.prefix() == Some(first))
        {
            let rest = text.trim_start()[first.len_utf8()..]
                .trim_start()
                .to_owned();
            self.input.update(cx, |input, cx| input.set_text(rest, cx));
        }
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.active = 0;
        let placeholder = tab.placeholder();
        self.input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, cx);
        });
        self.refresh(cx);
    }

    fn cycle_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        let current = self.effective(cx).0.index() as isize;
        let next = (current + delta).rem_euclid(Tab::ALL.len() as isize) as usize;
        self.select_tab(Tab::ALL[next], cx);
    }

    /// The query and tab, to open on again next time.
    pub(crate) fn draft(&self, cx: &App) -> (String, Tab) {
        (self.input.read(cx).content.to_string(), self.tab)
    }

    pub(crate) fn set_query(&mut self, text: String, cx: &mut Context<Self>) {
        let len = text.len();
        self.input.update(cx, |input, cx| {
            input.set_text(text, cx);
            input.select_range(0..len, cx);
        });
    }

    /// A conversation changed while the modal is open.
    pub(crate) fn update_session(
        &mut self,
        summary: Session,
        rows: Arc<Vec<TranscriptRow>>,
        _: &mut Context<Self>,
    ) {
        if let Some(old) = self.summaries.iter_mut().find(|s| s.id == summary.id) {
            *old = summary.clone();
        } else {
            self.summaries.push(summary.clone());
        }
        self.live.insert(summary.id, rows);
    }

    // ---- searching -------------------------------------------------------

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Release);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.generation += 1;
        self.tasks.clear();
        let (tab, _) = self.effective(cx);
        let all = tab == Tab::All;
        // Cheap enough to run for every tab, so the Files badge is right.
        self.search_files(cx);
        if all || tab == Tab::Content {
            self.search_content(cx);
        } else {
            self.content = Found::default();
        }
        if all || tab == Tab::Symbols {
            self.search_symbols(cx);
        } else {
            self.symbols = Found::default();
        }
        if all || tab == Tab::Agents {
            self.search_agents(cx);
        } else {
            self.agents = Found::default();
        }
        self.rebuild(cx);
    }

    fn search_files(&mut self, cx: &mut Context<Self>) {
        let (_, query) = self.effective(cx);
        let Some(index) = self.index.clone() else {
            self.files.loading = true;
            return;
        };
        let generation = self.generation;
        let cancel = self.cancel.clone();
        let recent = self.recent_files.clone();
        let limit = FILE_ROWS;
        self.files.loading = true;
        let work = cx.background_executor().spawn(async move {
            let started = Instant::now();
            let (name, at) = fast::split_location(&query);
            let hits = index.search(name, &recent, limit, &cancel);
            let rows: Vec<Row> = hits
                .into_iter()
                .filter_map(|hit| {
                    let file = index.files().get(hit.index)?;
                    Some(Row {
                        section: if name.is_empty() {
                            Section::RecentFiles
                        } else {
                            Section::Files
                        },
                        title: file.name.clone().into(),
                        title_ranges: hit.name_ranges,
                        detail: file.relative.clone().into(),
                        detail_ranges: hit.path_ranges,
                        mark: Mark::File(file.name.clone().into()),
                        code: false,
                        keys: None,
                        pick: Pick::File {
                            path: file.path.clone(),
                            at,
                        },
                    })
                })
                .collect();
            (rows, started.elapsed())
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let (rows, elapsed) = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.files = Found {
                    rows,
                    loading: false,
                    elapsed: Some(elapsed),
                };
                this.rebuild(cx);
            });
        }));
    }

    fn content_options(&self, query: &str, cx: &App) -> SearchOptions {
        let include = self.include.read(cx).content.trim().to_owned();
        SearchOptions {
            cwd: self.cwd.clone(),
            query: query.to_owned(),
            case_sensitive: self.flags.case_sensitive,
            whole_word: self.flags.whole_word,
            regex: self.flags.regex,
            include: (!include.is_empty()).then_some(include),
            exclude: None,
            search_id: String::new(),
            include_ignored: self.include_ignored,
        }
    }

    fn search_content(&mut self, cx: &mut Context<Self>) {
        let (tab, query) = self.effective(cx);
        let min = if tab == Tab::All { 2 } else { 1 };
        if query.chars().count() < min {
            self.content = Found::default();
            self.content_truncated = false;
            self.content_error = None;
            return;
        }
        let generation = self.generation;
        let cancel = self.cancel.clone();
        let options = self.content_options(&query, cx);
        let flags = self.flags;
        let root = self.cwd.clone();
        let limit = CONTENT_ROWS;
        self.content.loading = true;
        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(140))
                .await;
            if cancel.load(Ordering::Acquire) {
                return;
            }
            let flag = cancel.clone();
            let found = cx
                .background_executor()
                .spawn(async move {
                    let started = Instant::now();
                    let result = fast::content_searcher().search(
                        std::path::Path::new(&root),
                        &options,
                        &flag,
                    );
                    result.map(|result| {
                        let rows: Vec<Row> = result
                            .matches
                            .into_iter()
                            .take(limit)
                            .map(|m| content_row(m, &options.query, flags))
                            .collect();
                        (rows, result.truncated, started.elapsed())
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match found {
                    Ok((rows, truncated, elapsed)) => {
                        this.content_error = None;
                        this.content_truncated = truncated;
                        this.content = Found {
                            rows,
                            loading: false,
                            elapsed: Some(elapsed),
                        };
                    }
                    Err(error) => {
                        this.content_error = Some(error);
                        this.content = Found::default();
                    }
                }
                this.rebuild(cx);
            });
        }));
    }

    fn search_symbols(&mut self, cx: &mut Context<Self>) {
        let (tab, query) = self.effective(cx);
        let Some(mode) = features::mode_with_prefix('@', cx) else {
            self.symbols = Found::default();
            self.symbols_hint = Some("Symbol search is unavailable");
            return;
        };
        if !(mode.applies)(cx) {
            self.symbols = Found::default();
            self.symbols_hint = Some("Open a source file to search the symbols of its language");
            return;
        }
        self.symbols_hint = None;
        // On All, symbols join once something is typed.
        if tab == Tab::All && query.is_empty() {
            self.symbols = Found::default();
            return;
        }
        let generation = self.generation;
        let cancel = self.cancel.clone();
        let scope = crate::code_shell::context::scope(&self.cwd, cx);
        let limit = 200;
        self.symbols.loading = true;
        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(110))
                .await;
            if cancel.load(Ordering::Acquire) {
                return;
            }
            let started = Instant::now();
            let search = cx.update(|cx| (mode.search)(query.clone(), scope, cx));
            let items = search.await;
            let elapsed = started.elapsed();
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let rows = items
                    .into_iter()
                    .take(limit)
                    .map(|item| {
                        let title = item.title.to_string();
                        let title_ranges = core_search::fuzzy(&query, &title)
                            .map(|(_, r)| r)
                            .unwrap_or_default();
                        Row {
                            section: Section::Symbols,
                            title: title.into(),
                            title_ranges,
                            detail: item.detail.clone(),
                            detail_ranges: vec![],
                            mark: Mark::Glyph(item.icon.unwrap_or("command-compact")),
                            code: false,
                            keys: None,
                            pick: match item.path {
                                Some(path) => Pick::File {
                                    path,
                                    at: (item.line > 0).then_some((item.line, item.column.max(1))),
                                },
                                None => Pick::AskAgent(String::new()),
                            },
                        }
                    })
                    .filter(|row| !matches!(row.pick, Pick::AskAgent(_)))
                    .collect();
                this.symbols = Found {
                    rows,
                    loading: false,
                    elapsed: Some(elapsed),
                };
                this.rebuild(cx);
            });
        }));
    }

    fn search_agents(&mut self, cx: &mut Context<Self>) {
        let (tab, query) = self.effective(cx);
        self.agent_error = None;
        if query.is_empty() {
            // Recent conversations, no search.
            let take = if tab == Tab::All { 5 } else { 24 };
            let mut recent: Vec<_> = self
                .summaries
                .iter()
                .filter(|s| !s.archived && s.has_messages)
                .collect();
            recent.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.id.cmp(&a.id)));
            let rows = recent
                .into_iter()
                .take(take)
                .map(|s| conversation_row(s, &[], Section::RecentAgents))
                .collect();
            self.agents = Found {
                rows,
                loading: false,
                elapsed: None,
            };
            return;
        }
        let generation = self.generation;
        let cancel = self.cancel.clone();
        let store = self.store.clone();
        let live = self.live.clone();
        let summaries = self.summaries.clone();
        self.agents.loading = true;
        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            if cancel.load(Ordering::Acquire) {
                return;
            }
            let flag = cancel.clone();
            let found = cx
                .background_executor()
                .spawn(async move {
                    let started = Instant::now();
                    let titles = core_search::titles(&summaries, &query);
                    let mut local = vec![];
                    for s in &summaries {
                        if flag.load(Ordering::Acquire) {
                            break;
                        }
                        if let Some(rows) = live.get(&s.id) {
                            local.extend(core_search::messages(s, rows, &query, &flag));
                        }
                    }
                    let saved =
                        locked(&store)
                            .and_then(|db| db.search_reader())
                            .and_then(|reader| match reader {
                                Some(reader) => reader.search_sessions(&query, flag.clone()),
                                None => locked(&store)
                                    .and_then(|db| db.search_sessions(&query, flag.clone())),
                            });
                    let (saved, error) = match saved {
                        Ok((hits, _)) => (hits, None),
                        Err(error) => (vec![], Some(error)),
                    };
                    let hits = core_search::grouped(
                        titles.into_iter().chain(local).chain(saved),
                        core_search::Scope::Conversations,
                    );
                    (hits, error, started.elapsed())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let (hits, error, elapsed) = found;
                this.agent_error = error;
                let rows = hits
                    .into_iter()
                    .map(|hit| hit_row(hit, &this.logos))
                    .collect();
                this.agents = Found {
                    rows,
                    loading: false,
                    elapsed: Some(elapsed),
                };
                this.rebuild(cx);
            });
        }));
    }

    fn project_rows(&self, query: &str) -> Vec<Row> {
        if query.is_empty() {
            return self
                .projects
                .iter()
                .map(|path| {
                    hit_row(
                        Hit {
                            id: format!("project:{path}"),
                            target: Target::Project(path.clone()),
                            title: core_search::project_name(path),
                            meta: path.clone(),
                            provider: String::new(),
                            highlights: vec![],
                            score: 0,
                            updated_at: 0,
                        },
                        &self.logos,
                    )
                })
                .collect();
        }
        let mut hits = core_search::projects(&self.projects, query);
        hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.title.cmp(&b.title)));
        hits.into_iter()
            .map(|hit| hit_row(hit, &self.logos))
            .collect()
    }

    fn action_rows(&self, query: &str, cx: &App) -> Vec<Row> {
        rank_actions(&self.actions, query, &action_history(cx))
            .into_iter()
            .filter_map(|(action, ranges, recent)| {
                let ix = self.actions.iter().position(|a| a.id == action.id)?;
                Some((ix, action, ranges, recent))
            })
            .map(|(ix, action, ranges, recent)| Row {
                section: if recent {
                    Section::RecentActions
                } else {
                    Section::Actions
                },
                title: action.title.clone(),
                title_ranges: ranges,
                detail: action.note_or_group(),
                detail_ranges: vec![],
                mark: Mark::Glyph(action.glyph),
                code: false,
                keys: action.keys.clone(),
                pick: Pick::Run(ix),
            })
            .collect()
    }

    /// Lays out the rows of the tab in front from what each source found.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let (tab, query) = self.effective(cx);
        let mut rows: Vec<Row> = vec![];
        let take = |found: &Found, n: usize| found.rows.iter().take(n).cloned().collect::<Vec<_>>();
        match tab {
            Tab::All => {
                rows.extend(take(&self.files, ALL_LIMITS.files));
                rows.extend(take(&self.symbols, ALL_LIMITS.symbols));
                rows.extend(take(&self.content, ALL_LIMITS.content));
                let agents: Vec<Row> = self.agents.rows.clone();
                let conversations = agents
                    .iter()
                    .filter(|r| matches!(r.section, Section::Conversations | Section::RecentAgents))
                    .take(ALL_LIMITS.conversations)
                    .cloned();
                let messages = agents
                    .iter()
                    .filter(|r| r.section == Section::Messages)
                    .take(ALL_LIMITS.messages)
                    .cloned();
                rows.extend(conversations);
                rows.extend(messages);
                if !query.is_empty() {
                    rows.extend(
                        self.project_rows(&query)
                            .into_iter()
                            .take(ALL_LIMITS.projects),
                    );
                    rows.extend(
                        self.action_rows(&query, cx)
                            .into_iter()
                            .take(ALL_LIMITS.actions),
                    );
                    rows.push(ask_row(&query));
                }
            }
            Tab::Files => rows.extend(self.files.rows.iter().cloned()),
            Tab::Content => rows.extend(self.content.rows.iter().cloned()),
            Tab::Symbols => rows.extend(self.symbols.rows.iter().cloned()),
            Tab::Agents => {
                rows.extend(self.agents.rows.iter().cloned());
                if !query.is_empty() {
                    rows.push(ask_row(&query));
                }
            }
            Tab::Projects => rows.extend(self.project_rows(&query)),
            Tab::Actions => rows.extend(self.action_rows(&query, cx)),
        }
        // Rows of one section stay together, in the order sections appeared.
        let mut order: Vec<Section> = vec![];
        for row in &rows {
            if !order.contains(&row.section) {
                order.push(row.section);
            }
        }
        rows.sort_by_key(|row| order.iter().position(|s| *s == row.section));
        let mut children = Vec::with_capacity(rows.len());
        let mut child = usize::from(self.notice(tab).is_some());
        let mut last = None;
        for row in &rows {
            if last != Some(row.section) {
                child += 1;
                last = Some(row.section);
            }
            children.push(child);
            child += 1;
        }
        self.rows = rows;
        self.children = children;
        if self.active >= self.rows.len() {
            self.active = 0;
        }
        self.pointer = None;
        self.update_preview(cx);
        cx.notify();
    }

    fn notice(&self, tab: Tab) -> Option<&'static str> {
        (self.content_truncated && tab == Tab::Content)
            .then_some("Showing the first matches — narrow the query or the files to include")
    }

    fn loading(&self, cx: &App) -> bool {
        let (tab, _) = self.effective(cx);
        match tab {
            Tab::All => {
                self.files.loading
                    || self.content.loading
                    || self.symbols.loading
                    || self.agents.loading
            }
            Tab::Files => self.files.loading || self.index_loading && self.index.is_none(),
            Tab::Content => self.content.loading,
            Tab::Symbols => self.symbols.loading,
            Tab::Agents => self.agents.loading,
            Tab::Projects | Tab::Actions => false,
        }
    }

    /// How many rows each tab has for the current query, for its badge.
    fn count(&self, tab: Tab, query: &str, cx: &App) -> Option<usize> {
        if query.is_empty() {
            return None;
        }
        let n = match tab {
            Tab::All => return None,
            Tab::Files => self.files.rows.len(),
            Tab::Content => self.content.rows.len(),
            Tab::Symbols => self.symbols.rows.len(),
            Tab::Agents => self.agents.rows.len(),
            Tab::Projects => self.project_rows(query).len(),
            Tab::Actions => self.action_rows(query, cx).len(),
        };
        (n > 0).then_some(n)
    }

    fn elapsed(&self, cx: &App) -> Option<Duration> {
        let (tab, _) = self.effective(cx);
        match tab {
            Tab::Files => self.files.elapsed,
            Tab::Content => self.content.elapsed,
            Tab::Symbols => self.symbols.elapsed,
            Tab::Agents => self.agents.elapsed,
            Tab::All => [
                self.files.elapsed,
                self.content.elapsed,
                self.symbols.elapsed,
                self.agents.elapsed,
            ]
            .into_iter()
            .flatten()
            .max(),
            _ => None,
        }
    }

    // ---- preview ---------------------------------------------------------

    fn update_preview(&mut self, cx: &mut Context<Self>) {
        if !self.preview_on {
            self.preview = None;
            self.preview_task = None;
            return;
        }
        let Some((path, at)) = self
            .rows
            .get(self.active)
            .and_then(|row| row.file_target())
            .map(|(p, at)| (p.to_owned(), at))
        else {
            self.preview = None;
            self.preview_task = None;
            return;
        };
        let line = at.map_or(1, |(line, _)| line.max(1));
        let (_, query) = self.effective(cx);
        let flags = self.flags;
        if self.preview.as_ref().is_some_and(|p| {
            p.path == path && p.line == line && p.query == query && p.flags == flags
        }) {
            return;
        }
        let work = cx.background_executor().spawn({
            let path = path.clone();
            async move { read_preview(&path, line, query, flags) }
        });
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            // Arrowing through rows reads only the one that is stopped on.
            let preview = work.await;
            let _ = this.update(cx, |this, cx| {
                this.preview = Some(preview);
                cx.notify();
            });
        }));
    }

    fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        self.preview_on = !self.preview_on;
        self.preview = None;
        self.update_preview(cx);
        cx.notify();
    }

    fn toggle_flag(&mut self, flag: fn(&mut ContentFlags) -> &mut bool, cx: &mut Context<Self>) {
        let value = flag(&mut self.flags);
        *value = !*value;
        self.refresh(cx);
    }

    // ---- choosing --------------------------------------------------------

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.input.read(cx).is_composing() || self.rows.is_empty() {
            return;
        }
        self.active = (self.active as isize + delta).rem_euclid(self.rows.len() as isize) as usize;
        self.pointer = None;
        if let Some(child) = self.children.get(self.active) {
            self.scroll.scroll_to_item(*child);
        }
        self.update_preview(cx);
        cx.notify();
    }

    fn pick(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.input.read(cx).is_composing() {
            return;
        }
        let Some(row) = self.rows.get(ix).cloned() else {
            return;
        };
        let (_, query) = self.effective(cx);
        let event = match row.pick {
            Pick::File { path, at } => Event::OpenFile { path, at },
            Pick::Conversation(id) => Event::Conversation(id),
            Pick::Message { session, row } => Event::Message {
                session,
                row,
                query,
            },
            Pick::Project(path) => Event::Project(path),
            Pick::Run(ix) => {
                let Some(item) = self.actions.get(ix).cloned() else {
                    return;
                };
                remember_action(item.id.clone(), cx);
                Event::Run(item.run)
            }
            Pick::AskAgent(prompt) => Event::AskAgent(prompt),
        };
        cx.emit(event);
    }

    fn find_in_panel(&mut self, cx: &mut Context<Self>) {
        let (_, query) = self.effective(cx);
        if !query.is_empty() {
            cx.emit(Event::FindInPanel(query));
        }
    }

    #[cfg(any(test, feature = "selftest"))]
    pub(crate) fn row_labels(&self) -> Vec<(String, String)> {
        (self.rows.iter())
            .map(|row| (row.title.to_string(), row.detail.to_string()))
            .collect()
    }
    #[cfg(test)]
    pub(crate) fn pick_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.pick(ix, cx)
    }
}

fn content_row(m: core_search::project::SearchMatch, query: &str, flags: ContentFlags) -> Row {
    let title = m.preview.trim().to_owned();
    let title_ranges = fast::match_ranges(&title, query, flags.case_sensitive, flags.regex);
    let name = m
        .relative
        .rsplit('/')
        .next()
        .unwrap_or(&m.relative)
        .to_owned();
    Row {
        section: Section::Content,
        title: title.into(),
        title_ranges,
        detail: format!("{}:{}", m.relative, m.line).into(),
        detail_ranges: vec![],
        mark: Mark::File(name.into()),
        code: true,
        keys: None,
        pick: Pick::File {
            path: m.path,
            at: Some((m.line, m.column)),
        },
    }
}

fn conversation_row(s: &Session, ranges: &[Range<usize>], section: Section) -> Row {
    Row {
        section,
        title: monocode_core::session_title::display_title(&s.title, &s.provider)
            .to_owned()
            .into(),
        title_ranges: ranges.to_vec(),
        detail: core_search::project_name(&s.cwd).into(),
        detail_ranges: vec![],
        mark: Mark::Provider(s.provider.clone().into()),
        code: false,
        keys: None,
        pick: Pick::Conversation(s.id),
    }
}

fn hit_row(hit: Hit, logos: &HashMap<String, String>) -> Row {
    let (section, mark, pick) = match &hit.target {
        Target::Conversation(id) => (
            Section::Conversations,
            Mark::Provider(hit.provider.clone().into()),
            Pick::Conversation(*id),
        ),
        Target::Message { session, row } => (
            Section::Messages,
            Mark::Glyph("message-square"),
            Pick::Message {
                session: *session,
                row: *row,
            },
        ),
        Target::File { path, location } => (
            Section::Files,
            Mark::File(core_search::project_name(path).into()),
            Pick::File {
                path: path.clone(),
                at: *location,
            },
        ),
        Target::Project(path) => (
            Section::Projects,
            logos
                .get(path)
                .map(|logo| Mark::Logo(logo.clone().into()))
                .unwrap_or(Mark::Glyph("folder")),
            Pick::Project(path.clone()),
        ),
    };
    Row {
        section,
        title: hit.title.into(),
        title_ranges: hit.highlights,
        detail: hit.meta.into(),
        detail_ranges: vec![],
        mark,
        code: false,
        keys: None,
        pick,
    }
}

fn ask_row(query: &str) -> Row {
    let title = format!("Ask a new agent: “{query}”");
    let start = "Ask a new agent: “".len();
    Row {
        section: Section::Ask,
        title_ranges: std::iter::once(start..start + query.len()).collect(),
        title: title.into(),
        detail: "Opens a new conversation with this as its prompt".into(),
        detail_ranges: vec![],
        mark: Mark::Glyph("sparkles"),
        code: false,
        keys: None,
        pick: Pick::AskAgent(query.to_owned()),
    }
}

/// Up to `PREVIEW_LINES` lines of `path` around `line`.
const PREVIEW_LINES: u32 = 40;
const PREVIEW_BYTES: u64 = 2 * 1024 * 1024;
const PREVIEW_LINE_CHARS: usize = 400;
fn read_preview(path: &str, line: u32, query: String, flags: ContentFlags) -> Preview {
    let fail = |error: &str| Preview {
        path: path.to_owned(),
        line,
        first: 1,
        lines: vec![],
        ranges: vec![],
        query: query.clone(),
        flags,
        error: Some(error.to_owned().into()),
    };
    if path.starts_with("remote://") {
        return fail("No preview for remote files");
    }
    let image = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "icns" | "pdf"
            )
        });
    if image {
        return fail("No text preview for this file type");
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return fail("File not found");
    };
    if meta.len() > PREVIEW_BYTES {
        return fail("File too large to preview");
    }
    let Ok(bytes) = std::fs::read(path) else {
        return fail("Could not read the file");
    };
    if bytes.iter().take(8000).any(|b| *b == 0) {
        return fail("Binary file");
    }
    let text = String::from_utf8_lossy(&bytes);
    let first = line.saturating_sub(PREVIEW_LINES / 3).max(1);
    let lines: Vec<SharedString> = text
        .lines()
        .skip(first as usize - 1)
        .take(PREVIEW_LINES as usize)
        .map(|l| {
            let l = l.replace('\t', "    ");
            if l.chars().count() > PREVIEW_LINE_CHARS {
                let cut: String = l.chars().take(PREVIEW_LINE_CHARS).collect();
                format!("{cut}…").into()
            } else {
                l.into()
            }
        })
        .collect();
    let ranges = lines
        .iter()
        .map(|l| match query.is_empty() {
            true => vec![],
            false => fast::match_ranges(l, &query, flags.case_sensitive, flags.regex),
        })
        .collect();
    Preview {
        path: path.to_owned(),
        line,
        first,
        lines,
        ranges,
        query,
        flags,
        error: None,
    }
}

// ---- rendering -------------------------------------------------------------

pub(crate) fn keycap(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .min_w(px(18.))
        .h(px(18.))
        .px(px(5.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .bg(content(0.06))
        .border_1()
        .border_color(content(0.08))
        .text_size(px(10.5))
        .text_color(content(0.55))
        .child(text.into())
}
pub(crate) fn hint(keys: &[&'static str], label: &'static str) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .children(keys.iter().map(|k| keycap(*k)))
        .child(div().pl(px(2.)).child(label))
}
fn highlighted(text: SharedString, ranges: &[Range<usize>], strong: bool) -> StyledText {
    let len = text.len();
    let ranges: Vec<_> = ranges
        .iter()
        .filter(|r| r.start < r.end && r.end <= len)
        .filter(|r| text.is_char_boundary(r.start) && text.is_char_boundary(r.end))
        .cloned()
        .collect();
    StyledText::new(text).with_highlights(ranges.into_iter().map(move |range| {
        (
            range,
            HighlightStyle {
                font_weight: strong.then_some(FontWeight::SEMIBOLD),
                color: Some(accent_color()),
                ..Default::default()
            },
        )
    }))
}
fn mark_element(mark: &Mark, active: bool) -> AnyElement {
    let ink = content(if active { 0.75 } else { 0.55 });
    match mark {
        Mark::File(name) => crate::file_icons::file_type_icon(name, false, false, 15.),
        Mark::Glyph(glyph) => icon(glyph, 14.).text_color(ink).into_any_element(),
        Mark::Provider(provider) => provider_icon(provider, 14.).into_any_element(),
        Mark::Logo(logo) => crate::image_memory::thumbnail(std::path::PathBuf::from(logo.as_ref()))
            .size(px(16.))
            .rounded_sm()
            .into_any_element(),
    }
}

impl Everywhere {
    fn row_element(&self, ix: usize, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active == ix;
        let selection = if is_light() { 0.07 } else { 0.09 };
        div()
            .id(("everywhere-row", ix))
            .debug_selector(move || format!("everywhere-row-{ix}"))
            .role(Role::ListItem)
            .aria_selected(active)
            .aria_label(format!("{} {}", row.title, row.detail))
            .h(px(38.))
            .w_full()
            .flex_shrink_0()
            .relative()
            .rounded(px(8.))
            .pl(px(8.))
            .pr(px(10.))
            .flex()
            .gap(px(10.))
            .items_center()
            .cursor_pointer()
            .when(active, |d| d.bg(content(selection)))
            .when(active, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(0.))
                        .top(px(10.))
                        .w(px(2.5))
                        .h(px(18.))
                        .rounded_full()
                        .bg(accent_color()),
                )
            })
            .child(
                div()
                    .size(px(26.))
                    .flex_shrink_0()
                    .rounded(px(7.))
                    .bg(content(if active { 0.08 } else { 0.045 }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(mark_element(&row.mark, active)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_baseline()
                    .gap(px(10.))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .when(row.code, |d| {
                                d.font_family(crate::fonts::mono()).text_size(px(12.5))
                            })
                            .when(!row.code, |d| d.text_size(px(13.5)))
                            .text_color(content(if active { 1. } else { 0.86 }))
                            .child(highlighted(row.title.clone(), &row.title_ranges, !row.code)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .max_w(relative(0.6))
                            .truncate()
                            .text_size(px(12.))
                            .text_color(content(if active { 0.5 } else { 0.38 }))
                            .child(highlighted(row.detail.clone(), &row.detail_ranges, false)),
                    ),
            )
            .children(row.keys.clone().map(|keys| {
                div()
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(content(0.45))
                    .child(keycap(keys))
            }))
            .when(active && row.keys.is_none(), |d| d.child(keycap("↵")))
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if this.pointer.is_some_and(|old| old != event.position) && this.active != ix {
                    this.active = ix;
                    this.update_preview(cx);
                    cx.notify();
                }
                this.pointer = Some(event.position);
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| this.pick(ix, cx)))
            .into_any_element()
    }

    fn heading(
        &self,
        section: Section,
        first: bool,
        tab: Tab,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let target = section.tab();
        let jump = tab == Tab::All && target != Tab::All;
        div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .px(px(10.))
            .pt(px(if first { 6. } else { 12. }))
            .pb(px(5.))
            .text_size(px(11.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(content(0.42))
            .child(div().flex_1().child(section.label()))
            .when(jump && section != Section::Ask, |d| {
                d.child(
                    div()
                        .id(("everywhere-see-all", section as usize))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .text_color(content(0.36))
                        .hover(|s| s.text_color(accent_color()))
                        .child(format!("All {}", target.label().to_lowercase()))
                        .child(keycap(format!(
                            "{}{}",
                            if cfg!(target_os = "macos") {
                                "⌘"
                            } else {
                                "Ctrl+"
                            },
                            target.index() + 1
                        )))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| this.select_tab(target, cx))),
                )
            })
            .into_any_element()
    }

    fn empty_text(&self, tab: Tab, query: &str) -> String {
        if let Some(error) = match tab {
            Tab::Content => self.content_error.clone(),
            Tab::Agents => self.agent_error.clone(),
            _ => None,
        } {
            return error;
        }
        if tab == Tab::Symbols
            && let Some(hint) = self.symbols_hint
        {
            return hint.into();
        }
        if query.is_empty() {
            return match tab {
                Tab::All => "Type to search files, code, agents, projects and actions".into(),
                Tab::Files if self.index.is_none() => "Indexing project files…".into(),
                Tab::Files => "No recent files — type to search by name".into(),
                Tab::Content => "Type to search inside the files".into(),
                Tab::Symbols => "Type to search symbols".into(),
                Tab::Agents => "No conversations yet".into(),
                Tab::Projects => "No projects yet".into(),
                Tab::Actions => "No actions".into(),
            };
        }
        format!("No results for “{query}”")
    }

    fn tabs(&self, query: &str, tab: Tab, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("everywhere-tabs")
            .debug_selector(|| "everywhere-tabs".into())
            .role(Role::TabList)
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(12.))
            .pb(px(10.))
            .children(Tab::ALL.into_iter().map(|target| {
                let selected = tab == target;
                let count = self.count(target, query, cx);
                div()
                    .id(("everywhere-tab", target.index()))
                    .debug_selector(move || format!("everywhere-tab-{}", target.label()))
                    .role(Role::Tab)
                    .aria_label(target.label())
                    .aria_selected(selected)
                    .cursor_pointer()
                    .h(px(26.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded_full()
                    .border_1()
                    .border_color(if selected {
                        accent_color().opacity(0.45)
                    } else {
                        content(0.08)
                    })
                    .bg(if selected {
                        accent_color().opacity(0.14)
                    } else {
                        content(0.)
                    })
                    .text_size(px(12.))
                    .text_color(content(if selected { 1. } else { 0.55 }))
                    .hover(|s| s.bg(content(0.05)).text_color(content(0.95)))
                    .child(icon(target.glyph(), 12.).text_color(if selected {
                        accent_color()
                    } else {
                        content(0.45)
                    }))
                    .child(target.label())
                    .children(count.map(|n| {
                        div()
                            .px(px(5.))
                            .h(px(15.))
                            .flex()
                            .items_center()
                            .rounded_full()
                            .bg(content(if selected { 0.1 } else { 0.06 }))
                            .text_size(px(10.))
                            .text_color(content(0.6))
                            .child(if n >= 100 {
                                "99+".to_owned()
                            } else {
                                n.to_string()
                            })
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_tab(target, cx)))
            }))
            .into_any_element()
    }

    fn content_options_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let toggle = |id: &'static str,
                      glyph: &'static str,
                      label: &'static str,
                      on: bool,
                      flag: fn(&mut ContentFlags) -> &mut bool,
                      cx: &mut Context<Self>| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::Button)
                .aria_label(label)
                .aria_selected(on)
                .cursor_pointer()
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .border_1()
                .border_color(if on {
                    accent_color().opacity(0.5)
                } else {
                    content(0.)
                })
                .bg(if on {
                    accent_color().opacity(0.16)
                } else {
                    content(0.)
                })
                .hover(|s| s.bg(content(0.07)))
                .child(icon(glyph, 14.).text_color(if on { accent_color() } else { content(0.55) }))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_flag(flag, cx)))
        };
        div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(14.))
            .pb(px(10.))
            .child(toggle(
                "everywhere-case",
                "case-sensitive",
                "Match case (⌥C)",
                self.flags.case_sensitive,
                |f| &mut f.case_sensitive,
                cx,
            ))
            .child(toggle(
                "everywhere-word",
                "whole-word",
                "Whole word (⌥W)",
                self.flags.whole_word,
                |f| &mut f.whole_word,
                cx,
            ))
            .child(toggle(
                "everywhere-regex",
                "regex",
                "Regular expression (⌥R)",
                self.flags.regex,
                |f| &mut f.regex,
                cx,
            ))
            .child(
                div()
                    .ml(px(6.))
                    .flex_1()
                    .min_w_0()
                    .h(px(26.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(7.))
                    .bg(content(0.04))
                    .border_1()
                    .border_color(content(0.07))
                    .child(icon("folder", 12.).text_color(content(0.4)))
                    .child(div().flex_1().min_w_0().child(self.include.clone())),
            )
            .into_any_element()
    }

    fn preview_element(&self) -> Option<AnyElement> {
        let preview = self.preview.as_ref()?;
        let shown = preview
            .path
            .strip_prefix(&self.cwd)
            .map(|p| p.trim_start_matches(['/', '\\']))
            .unwrap_or(&preview.path)
            .to_owned();
        let body: AnyElement = if let Some(error) = &preview.error {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(content(0.4))
                .child(error.clone())
                .into_any_element()
        } else {
            let gutter = (preview.first as usize + preview.lines.len())
                .to_string()
                .len();
            div()
                .id("everywhere-preview-lines")
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .py(px(6.))
                .font_family(crate::fonts::mono())
                .text_size(px(12.))
                .line_height(px(19.))
                .children(preview.lines.iter().enumerate().map(|(i, text)| {
                    let number = preview.first + i as u32;
                    let current = number == preview.line;
                    let ranges = preview.ranges.get(i).map(Vec::as_slice).unwrap_or_default();
                    div()
                        .flex()
                        .w_full()
                        .when(current, |d| d.bg(accent_color().opacity(0.12)))
                        .child(
                            div()
                                .flex_shrink_0()
                                .w(px(8. * gutter as f32 + 18.))
                                .pr(px(10.))
                                .text_right()
                                .text_color(content(if current { 0.7 } else { 0.28 }))
                                .child(number.to_string()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_color(content(0.82))
                                .child(highlighted(text.clone(), ranges, false)),
                        )
                }))
                .into_any_element()
        };
        Some(
            div()
                .id("everywhere-preview")
                .debug_selector(|| "everywhere-preview".into())
                .w(relative(0.44))
                .flex_shrink_0()
                .h(px(LIST_HEIGHT))
                .border_l_1()
                .border_color(content(0.07))
                .bg(content(0.02))
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(30.))
                        .flex_shrink_0()
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .border_b_1()
                        .border_color(content(0.06))
                        .text_size(px(11.5))
                        .text_color(content(0.5))
                        .child(crate::file_icons::file_type_icon(
                            &core_search::project_name(&preview.path),
                            false,
                            false,
                            13.,
                        ))
                        .child(div().min_w_0().flex_1().truncate().child(shown))
                        .child(format!("{}", preview.line)),
                )
                .child(body)
                .into_any_element(),
        )
    }
}

impl Render for Everywhere {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (tab, query) = self.effective(cx);
        let mut list: Vec<AnyElement> = vec![];
        if let Some(notice) = self.notice(tab) {
            list.push(
                div()
                    .px(px(10.))
                    .pt_1()
                    .text_size(px(11.))
                    .text_color(content(0.4))
                    .child(notice)
                    .into_any_element(),
            );
        }
        let rows = std::mem::take(&mut self.rows);
        let mut last = None;
        for (ix, row) in rows.iter().enumerate() {
            if last != Some(row.section) {
                last = Some(row.section);
                list.push(self.heading(row.section, ix == 0, tab, cx));
            }
            list.push(self.row_element(ix, row, cx));
        }
        self.rows = rows;
        let loading = self.loading(cx);
        let count = self.rows.len();
        let viewport = window.viewport_size().width.as_f32();
        let wide = self.wants_wide(cx) && viewport >= PREVIEW_MIN + 64.;
        let preview = wide.then(|| self.preview_element()).flatten();
        let elapsed = self.elapsed(cx);
        div()
            .id("everywhere")
            .key_context("Everywhere")
            .debug_selector(|| "everywhere".into())
            .role(Role::Dialog)
            .aria_label("Search Everywhere")
            .w_full()
            .flex()
            .flex_col()
            .text_color(content(1.))
            .capture_action(cx.listener(|this, _: &input::Down, _, cx| {
                this.step(1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &input::Up, _, cx| {
                this.step(-1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &input::Submit, _, cx| {
                this.pick(this.active, cx);
                cx.stop_propagation();
            }))
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                let k = &event.keystroke;
                let m = &k.modifiers;
                let platform = if cfg!(target_os = "macos") {
                    m.platform
                } else {
                    m.control
                };
                if k.key == "tab" && !m.platform && !m.control && !m.alt {
                    this.cycle_tab(if m.shift { -1 } else { 1 }, cx);
                    cx.stop_propagation();
                } else if platform
                    && !m.shift
                    && !m.alt
                    && let Some(n) = k.key.parse::<usize>().ok().filter(|n| (1..=7).contains(n))
                {
                    this.select_tab(Tab::ALL[n - 1], cx);
                    cx.stop_propagation();
                } else if platform && k.key == "/" {
                    this.toggle_preview(cx);
                    cx.stop_propagation();
                } else if platform && k.key == "enter" {
                    this.find_in_panel(cx);
                    cx.stop_propagation();
                } else if m.alt && !platform && !m.shift {
                    let flag: Option<fn(&mut ContentFlags) -> &mut bool> = match k.key.as_str() {
                        "c" => Some(|f| &mut f.case_sensitive),
                        "w" => Some(|f| &mut f.whole_word),
                        "r" => Some(|f| &mut f.regex),
                        _ => None,
                    };
                    if let Some(flag) = flag
                        && this.effective(cx).0 == Tab::Content
                    {
                        this.toggle_flag(flag, cx);
                        cx.stop_propagation();
                    }
                } else if k.key == "pagedown" || k.key == "pageup" {
                    this.step(if k.key == "pagedown" { 8 } else { -8 }, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &crate::shell::DismissMenu, _, cx| {
                if !this.input.read(cx).is_composing() {
                    cx.emit(Event::Close);
                }
                cx.stop_propagation();
            }))
            // Tab is the shell's focus-next action; here it flips the tab.
            .on_action(cx.listener(|this, _: &crate::shell::FocusNext, _, cx| {
                this.cycle_tab(1, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &crate::shell::FocusPrevious, _, cx| {
                this.cycle_tab(-1, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, tab: &crate::shell::ActivateTab, _, cx| {
                if let Some(target) = usize::try_from(tab.0).ok().and_then(|n| Tab::ALL.get(n)) {
                    this.select_tab(*target, cx);
                }
                cx.stop_propagation();
            }))
            .child(
                div()
                    .id("everywhere-header")
                    .h(px(56.))
                    .flex_shrink_0()
                    .px(px(18.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(icon(tab.glyph(), 18.).text_color(content(0.45)))
                    .child(
                        div()
                            .debug_selector(|| "everywhere-field".into())
                            .flex_1()
                            .min_w_0()
                            .child(self.input.clone()),
                    )
                    .when(loading, |d| {
                        d.child(crate::right_panel::spinner(
                            "everywhere-loading",
                            "loader",
                            14.,
                        ))
                    })
                    .child(keycap("esc")),
            )
            .child(self.tabs(&query, tab, cx))
            .when(tab == Tab::Content, |d| {
                d.child(self.content_options_bar(cx))
            })
            .child(
                div()
                    .flex()
                    .border_t_1()
                    .border_color(content(0.07))
                    .child(
                        div()
                            .id("everywhere-results")
                            .debug_selector(|| "everywhere-results".into())
                            .role(Role::ListBox)
                            .aria_label("Search results")
                            .flex_1()
                            .min_w_0()
                            .h(px(LIST_HEIGHT))
                            .px(px(8.))
                            .py(px(6.))
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .children(list)
                            .when(count == 0 && !loading, |d| {
                                d.child(empty_message(self.empty_text(tab, &query)))
                            }),
                    )
                    .children(preview),
            )
            .child(
                div()
                    .h(px(36.))
                    .flex_shrink_0()
                    .px(px(14.))
                    .border_t_1()
                    .border_color(content(0.07))
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .text_size(px(11.))
                    .text_color(content(0.45))
                    .child(hint(&["↑", "↓"], "Navigate"))
                    .child(hint(&["↵"], "Open"))
                    .child(hint(&["⇥"], "Switch tab"))
                    .when(tab == Tab::Content, |d| {
                        d.child(hint(
                            &[if cfg!(target_os = "macos") {
                                "⌘↵"
                            } else {
                                "Ctrl+↵"
                            }],
                            "Find panel",
                        ))
                    })
                    .when(viewport >= PREVIEW_MIN + 64., |d| {
                        d.child(
                            div()
                                .id("everywhere-preview-toggle")
                                .cursor_pointer()
                                .hover(|s| s.text_color(content(0.8)))
                                .child(hint(
                                    &[if cfg!(target_os = "macos") {
                                        "⌘/"
                                    } else {
                                        "Ctrl+/"
                                    }],
                                    if self.preview_on {
                                        "Hide preview"
                                    } else {
                                        "Show preview"
                                    },
                                ))
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_preview(cx))),
                        )
                    })
                    .child(div().flex_1())
                    .when(tab == Tab::All && query.is_empty(), |d| {
                        d.child(
                            div()
                                .text_color(content(0.35))
                                .child("> actions   @ symbols   # contents"),
                        )
                    })
                    .when(!query.is_empty() && count > 0, |d| {
                        d.child(format!(
                            "{count} result{}{}",
                            if count == 1 { "" } else { "s" },
                            elapsed
                                .map(|e| format!(" · {} ms", e.as_millis()))
                                .unwrap_or_default()
                        ))
                    }),
            )
    }
}

/// The modal's width for the viewport: wide enough for the preview when it
/// fits.
pub(crate) fn modal_width(viewport: f32, preview: bool) -> f32 {
    let room = viewport - 64.;
    if preview && room >= PREVIEW_MIN {
        WIDE.min(room)
    } else {
        NARROW.min(viewport - 32.)
    }
}

impl Everywhere {
    /// Wide, with the preview, where the rows are places in files.
    pub(crate) fn wants_wide(&self, cx: &App) -> bool {
        self.preview_on
            && matches!(
                self.effective(cx).0,
                Tab::All | Tab::Files | Tab::Content | Tab::Symbols
            )
    }
}

fn empty_message(text: String) -> AnyElement {
    div()
        .py(px(48.))
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(
            div()
                .size(px(40.))
                .rounded(px(12.))
                .bg(content(0.05))
                .flex()
                .items_center()
                .justify_center()
                .child(icon("search", 18.).text_color(content(0.4))),
        )
        .child(
            div()
                .max_w(px(420.))
                .text_center()
                .text_size(px(13.))
                .text_color(content(0.5))
                .child(text),
        )
        .into_any_element()
}
