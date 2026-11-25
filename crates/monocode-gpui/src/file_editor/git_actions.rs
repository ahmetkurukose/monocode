//! Git from the code editor: the file's history, its changes against the
//! last commit, blame, a line's history and the file on its remote, and
//! comparing the editor's text with the clipboard.
use super::{
    CompareWithClipboard, FileEditor, FileEvent, NextChange, OpenOnRemote, PreviousChange,
    RevertChange, ShowLineHistory, ToggleBlame,
};
use crate::{
    code_editor::{
        self, Annotation, AnnotationClick, AnnotationProvider, Annotations, DocumentEvent,
        DocumentInfo, GutterButton, GutterMenu, LineAnnotation,
    },
    file_history::Request,
    git_status::{GitChanged, ProjectGit},
    popover::{MenuAction, MenuItem},
};
use gpui::{
    Action, App, AppContext, ClipboardItem, Context, Entity, EntityId, EventEmitter, Focusable,
    Global, SharedString, Subscription, Task, WeakEntity, Window,
};
use monocode_core::scm::{self, Blame, BlameCommit, RemoteWeb};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, OnceLock},
};

/// The hub source blame's annotations are stored under.
const BLAME_SOURCE: &str = "git-blame";
/// How many shades of recency the column tints commits with, newest first.
const AGE_BUCKETS: usize = 5;
/// How many lines of a commit message's body its tooltip shows.
const BODY_LINES: usize = 6;

/// A shortcut as this platform writes it.
fn keys(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        mac
    } else {
        other
    }
}

/// What the Git rows can do with this file, from what is on disk and the
/// project's last status read, without running git.
struct GitState {
    repo: bool,
    /// Committed: not untracked or only added.
    tracked: bool,
    remote: Option<RemoteWeb>,
}

/// What the annotation column shows of a commit, IntelliJ's View options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BlameView {
    date: bool,
    author: bool,
    commit: bool,
}
impl Default for BlameView {
    fn default() -> Self {
        Self {
            date: true,
            author: true,
            commit: false,
        }
    }
}
impl BlameView {
    fn shown(&self) -> usize {
        [self.date, self.author, self.commit]
            .into_iter()
            .filter(|on| *on)
            .count()
    }
}

/// One file's blame, shown in every editor of it while it is on.
struct FileBlame {
    blame: Option<Arc<Blame>>,
    /// The revision blamed and the file's path there; `None` for HEAD.
    at: Option<(String, String)>,
    /// For each buffer line, its line in `blame` while it is unchanged.
    rows: Arc<Vec<Option<usize>>>,
    /// The buffer text `rows` will describe once they are worked out.
    text: SharedString,
    /// The column's entries for `blame` as the view is, one per commit.
    entries: Option<Rc<Vec<Annotation>>>,
    load: Option<Task<()>>,
    align: Option<Task<()>>,
    _head: Option<Subscription>,
}

/// A press in a blame column that one editor answers.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum BlameRequest {
    /// Show this commit's change to the file, from the editor pressed.
    Open { editor: EntityId, sha: String },
}

/// The files blame is on for. Blame is the file's, as the hub's notes are:
/// two editors of one file show it together.
#[derive(Default)]
pub(crate) struct BlameStore {
    files: HashMap<PathBuf, FileBlame>,
    view: BlameView,
    _hub: Option<Subscription>,
}
impl EventEmitter<BlameRequest> for BlameStore {}
struct Blames(Entity<BlameStore>);
impl Global for Blames {}

fn blames(cx: &mut App) -> Entity<BlameStore> {
    if let Some(blames) = cx.try_global::<Blames>() {
        return blames.0.clone();
    }
    let hub = code_editor::hub(cx);
    let store = cx.new(|cx| BlameStore {
        files: HashMap::new(),
        view: BlameView::default(),
        _hub: Some(cx.subscribe(
            &hub,
            |this: &mut BlameStore, hub, event: &DocumentEvent, cx| {
                this.document_event(&hub, event, cx)
            },
        )),
    });
    cx.set_global(Blames(store.clone()));
    store
}
fn blames_ref(cx: &App) -> Option<&BlameStore> {
    cx.try_global::<Blames>().map(|blames| blames.0.read(cx))
}

/// How a locale writes a short date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DateStyle {
    /// Day, month, year, padded, such as `24.02.2026`.
    DayFirst(char),
    /// `2/24/2026`, as the US writes it.
    MonthFirst,
    /// Year, month, day, padded, such as `2026-02-24`.
    YearFirst(char),
}

/// The date style of a locale name such as `tr_TR.UTF-8` or `en-GB`.
fn date_style(locale: &str) -> DateStyle {
    let mut words = locale.split(['_', '-', '.', '@']);
    let language = words.next().unwrap_or_default().to_ascii_lowercase();
    let region = words.next().unwrap_or_default().to_ascii_uppercase();
    match (language.as_str(), region.as_str()) {
        ("en", "US" | "PH" | "") => DateStyle::MonthFirst,
        ("en" | "fr", "CA") => DateStyle::YearFirst('-'),
        ("en" | "fr" | "es" | "it" | "pt" | "el" | "vi" | "id" | "ms" | "he" | "ar" | "ca", _) => {
            DateStyle::DayFirst('/')
        }
        (
            "tr" | "de" | "ru" | "pl" | "cs" | "sk" | "fi" | "nb" | "nn" | "no" | "da" | "uk"
            | "ro" | "hr" | "sl" | "sr" | "bg" | "et" | "lv" | "az" | "kk" | "be" | "is",
            _,
        ) => DateStyle::DayFirst('.'),
        ("nl", _) => DateStyle::DayFirst('-'),
        ("ja" | "zh", _) => DateStyle::YearFirst('/'),
        ("ko" | "hu", _) => DateStyle::YearFirst('.'),
        _ => DateStyle::YearFirst('-'),
    }
}

/// The user's locale: the environment's, or on macOS the system's.
fn user_locale() -> &'static str {
    static LOCALE: OnceLock<String> = OnceLock::new();
    LOCALE.get_or_init(|| {
        let set = ["LC_ALL", "LC_TIME", "LANG"]
            .into_iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| {
                !value.is_empty() && value != "C" && !value.starts_with("C.") && value != "POSIX"
            });
        if let Some(locale) = set {
            return locale;
        }
        #[cfg(target_os = "macos")]
        if let Ok(out) = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleLocale"])
            .output()
            && out.status.success()
        {
            return String::from_utf8_lossy(&out.stdout).trim().to_owned();
        }
        String::new()
    })
}

fn format_date(date: chrono::NaiveDate, style: DateStyle) -> String {
    use chrono::Datelike;
    let (y, m, d) = (date.year(), date.month(), date.day());
    match style {
        DateStyle::DayFirst(sep) => format!("{d:02}{sep}{m:02}{sep}{y}"),
        DateStyle::MonthFirst => format!("{m}/{d}/{y}"),
        DateStyle::YearFirst(sep) => format!("{y}{sep}{m:02}{sep}{d:02}"),
    }
}

/// A commit's date and time in the local zone, as the tooltip shows it.
fn local_time(time: u64, style: DateStyle) -> (String, String) {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(time as i64, 0).single() {
        Some(at) => (
            format_date(at.date_naive(), style),
            at.format("%H:%M").to_string(),
        ),
        None => (String::new(), String::new()),
    }
}

/// What the column calls an author: the family name of a full name, as
/// `Kartal` for `Ahmet Kartal` or `Kartal, Ahmet`, else the name itself.
fn short_author(name: &str) -> &str {
    let name = name.trim();
    if let Some((family, given)) = name.split_once(',')
        && !family.trim().is_empty()
        && !given.trim().is_empty()
    {
        return family.trim();
    }
    name.split_whitespace().last().unwrap_or(name)
}

/// How strongly each commit tints its rows, by recency among the file's
/// commits: the newest fifth fully, fading to nothing for the oldest.
fn age_tints(times: &[u64]) -> Vec<f32> {
    let mut distinct = times.to_vec();
    distinct.sort_unstable_by(|a, b| b.cmp(a));
    distinct.dedup();
    let count = distinct.len().max(1);
    times
        .iter()
        .map(|time| {
            let rank = distinct.partition_point(|newer| newer > time);
            let bucket = rank * AGE_BUCKETS / count;
            (AGE_BUCKETS - 1 - bucket) as f32 / (AGE_BUCKETS - 1) as f32
        })
        .collect()
}

/// The tooltip of a commit: its hash, author, time and message.
fn commit_tooltip(commit: &BlameCommit, style: DateStyle) -> String {
    let (date, time) = local_time(commit.time, style);
    let mut tip = format!("Commit {}\n{}", commit.short_sha, commit.author);
    if !commit.email.is_empty() {
        tip.push_str(&format!(" <{}>", commit.email));
    }
    tip.push_str(&format!("\n{date} {time}\n\n{}", commit.subject));
    let body: Vec<&str> = commit.body.lines().take(BODY_LINES).collect();
    if !body.is_empty() {
        tip.push_str("\n\n");
        tip.push_str(body.join("\n").trim_end());
        if commit.body.lines().count() > BODY_LINES {
            tip.push_str("\n…");
        }
    }
    tip
}

/// The column's entry for each commit of a blame, as `view` shows it.
fn blame_entries(blame: &Blame, view: BlameView, style: DateStyle) -> Vec<Annotation> {
    let times: Vec<u64> = blame.commits.iter().map(|commit| commit.time).collect();
    blame
        .commits
        .iter()
        .zip(age_tints(&times))
        .map(|(commit, tint)| {
            let mut parts = Vec::new();
            if view.date {
                parts.push(local_time(commit.time, style).0.into());
            }
            if view.author {
                parts.push(short_author(&commit.author).to_owned().into());
            }
            if view.commit {
                parts.push(commit.short_sha.clone().into());
            }
            Annotation {
                id: commit.sha.clone().into(),
                parts,
                tooltip: commit_tooltip(commit, style).into(),
                tint,
            }
        })
        .collect()
}

/// The rows of a buffer whose lines sit at `rows` of `blame`: unchanged
/// lines show their commit (an entry of `blame_entries`), changed ones
/// nothing.
fn blame_rows(blame: &Blame, rows: &[Option<usize>]) -> Vec<LineAnnotation> {
    rows.iter()
        .enumerate()
        .filter_map(|(row, line)| {
            Some(LineAnnotation {
                row,
                entry: blame.lines.get((*line)?)?.commit,
            })
        })
        .collect()
}

/// Takes presses in one file's blame column for the store.
struct BlameColumn {
    store: WeakEntity<BlameStore>,
    path: PathBuf,
}
impl AnnotationProvider for BlameColumn {
    fn click(&self, _: &DocumentInfo, click: &AnnotationClick, _: &mut Window, cx: &mut App) {
        let (Some(store), Some(entry)) = (self.store.upgrade(), &click.entry) else {
            return;
        };
        let sha = entry.id.to_string();
        let editor = click.editor;
        store.update(cx, |_, cx| cx.emit(BlameRequest::Open { editor, sha }));
    }
    fn menu(
        &self,
        _: &DocumentInfo,
        click: &AnnotationClick,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<GutterMenu> {
        if click.button != GutterButton::Right {
            return None;
        }
        let store = self.store.upgrade()?;
        let (view, at, previous) = {
            let store = store.read(cx);
            let file = store.files.get(&self.path)?;
            let previous = click.entry.as_ref().and_then(|entry| {
                let blame = file.blame.as_ref()?;
                let commit = blame.commits.iter().find(|c| c.sha == entry.id.as_ref())?;
                commit.previous.clone()
            });
            (store.view, file.at.clone(), previous)
        };
        let some = click.entry.is_some();
        let mut items: Vec<MenuItem> = vec![
            MenuAction::new("close", "Close Annotations").into(),
            MenuItem::Separator,
            MenuAction::new("copy", "Copy Revision Number")
                .disabled(!some)
                .into(),
            MenuAction::new("diff", "Show Diff").disabled(!some).into(),
            MenuAction::new("previous", "Annotate Previous Revision")
                .disabled(previous.is_none())
                .into(),
        ];
        if at.is_some() {
            items.push(MenuAction::new("current", "Annotate Current Revision").into());
        }
        // The last part shown cannot be hidden: the column would be empty.
        let last = view.shown() == 1;
        items.extend([
            MenuItem::Separator,
            MenuItem::Header("View".into()),
            MenuAction::new("view-date", "Date")
                .checked(view.date)
                .disabled(last && view.date)
                .into(),
            MenuAction::new("view-author", "Author")
                .checked(view.author)
                .disabled(last && view.author)
                .into(),
            MenuAction::new("view-commit", "Commit")
                .checked(view.commit)
                .disabled(last && view.commit)
                .into(),
        ]);
        let (weak, path, editor) = (self.store.clone(), self.path.clone(), click.editor);
        let sha = click.entry.as_ref().map(|entry| entry.id.to_string());
        Some(GutterMenu {
            label: "Annotations".into(),
            items,
            width: 240.,
            on_pick: Rc::new(move |id, _, cx| {
                let Some(store) = weak.upgrade() else {
                    return;
                };
                match (id, &sha) {
                    ("close", _) => store.update(cx, |store, cx| store.turn_off(&path, cx)),
                    ("copy", Some(sha)) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(sha.clone()))
                    }
                    ("diff", Some(sha)) => store.update(cx, |_, cx| {
                        cx.emit(BlameRequest::Open {
                            editor,
                            sha: sha.clone(),
                        })
                    }),
                    ("previous", _) => {
                        let at = previous.clone();
                        store.update(cx, |store, cx| store.annotate_at(&path, at, cx))
                    }
                    ("current", _) => {
                        store.update(cx, |store, cx| store.annotate_at(&path, None, cx))
                    }
                    ("view-date" | "view-author" | "view-commit", _) => {
                        store.update(cx, |store, cx| {
                            let view = &mut store.view;
                            match id {
                                "view-date" => view.date = !view.date,
                                "view-author" => view.author = !view.author,
                                _ => view.commit = !view.commit,
                            }
                            store.view_changed(cx)
                        })
                    }
                    _ => {}
                }
            }),
        })
    }
}

impl BlameStore {
    fn is_on(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }
    /// The commit that last changed a buffer row, as of the last line-up.
    fn commit_at(&self, path: &Path, row: usize) -> Option<&BlameCommit> {
        let file = self.files.get(path)?;
        let blame = file.blame.as_ref()?;
        let line = (*file.rows.get(row)?)?;
        blame.commits.get(blame.lines.get(line)?.commit)
    }
    fn turn_on(
        &mut self,
        path: PathBuf,
        text: SharedString,
        git: &Entity<ProjectGit>,
        cx: &mut Context<Self>,
    ) {
        let head = {
            let path = path.clone();
            cx.subscribe(git, move |this, _, _: &GitChanged, cx| {
                this.refresh(&path, cx)
            })
        };
        self.files.insert(
            path.clone(),
            FileBlame {
                blame: None,
                at: None,
                rows: Arc::default(),
                text,
                entries: None,
                load: None,
                align: None,
                _head: Some(head),
            },
        );
        self.load(&path, cx);
    }
    fn turn_off(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.files.remove(path).is_some() {
            let path = path.to_owned();
            code_editor::hub(cx).update(cx, |hub, cx| hub.set_annotations(&path, None, cx));
            cx.notify();
        }
    }
    /// Blames another revision of the file (`None`: HEAD's) in its place.
    fn annotate_at(&mut self, path: &Path, at: Option<(String, String)>, cx: &mut Context<Self>) {
        if let Some(file) = self.files.get_mut(path) {
            file.at = at;
            self.load(path, cx);
        }
    }
    /// Shows the column again after the view options changed.
    fn view_changed(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self.files.keys().cloned().collect();
        for path in paths {
            if let Some(file) = self.files.get_mut(&path) {
                file.entries = None;
            }
            self.publish(&path, cx);
        }
    }
    /// Blames the file again, off the UI thread.
    fn load(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(file) = self.files.get_mut(path) else {
            return;
        };
        let read = path.to_owned();
        let at = file.at.clone();
        let work = cx.background_executor().spawn(async move {
            // Read once, off the UI thread, for the dates.
            user_locale();
            scm::blame_file(
                &read,
                at.as_ref().map(|(rev, path)| (rev.as_str(), path.as_str())),
            )
        });
        let path = path.to_owned();
        file.load = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(blame) => {
                    if let Some(file) = this.files.get_mut(&path) {
                        file.blame = Some(Arc::new(blame));
                        file.entries = None;
                        let text = file.text.clone();
                        this.align(&path, text, cx);
                    }
                }
                // A file that stopped being blamable (its commit is gone,
                // or it was never committed) shows nothing.
                Err(error) => {
                    eprintln!("monocode: git blame failed: {error}");
                    this.turn_off(&path, cx);
                }
            });
        }));
    }
    /// Blames again when HEAD moved; otherwise lines the buffer up again.
    /// An older revision being shown stays as it is.
    fn refresh(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(file) = self.files.get(path).filter(|file| file.at.is_none()) else {
            return;
        };
        let blamed = file.blame.as_ref().map(|blame| blame.head.clone());
        let read = path.to_owned();
        let work = cx
            .background_executor()
            .spawn(async move { scm::head_revision(&read) });
        let path = path.to_owned();
        cx.spawn(async move |this, cx| {
            let head = work.await;
            let _ = this.update(cx, |this, cx| {
                if head.is_some() && head != blamed {
                    this.load(&path, cx);
                }
            });
        })
        .detach();
    }
    /// Works out which buffer lines are still the blamed ones, then shows
    /// the column.
    fn align(&mut self, path: &Path, text: SharedString, cx: &mut Context<Self>) {
        let Some(file) = self.files.get_mut(path) else {
            return;
        };
        file.text = text.clone();
        let Some(blame) = file.blame.clone() else {
            return;
        };
        let lined = blame.clone();
        let work = cx.background_executor().spawn(async move {
            let base: Vec<&str> = lined.lines.iter().map(|l| l.text.as_str()).collect();
            scm::align_lines(&base, &text)
        });
        let path = path.to_owned();
        file.align = Some(cx.spawn(async move |this, cx| {
            let rows = Arc::new(work.await);
            let _ = this.update(cx, |this, cx| {
                let Some(file) = this.files.get_mut(&path) else {
                    return;
                };
                if !file.blame.as_ref().is_some_and(|b| Arc::ptr_eq(b, &blame)) {
                    return;
                }
                file.rows = rows;
                this.publish(&path, cx);
            });
        }));
    }
    /// Hands the file's column to the hub as the blame and rows are now.
    fn publish(&mut self, path: &Path, cx: &mut Context<Self>) {
        let view = self.view;
        let Some(file) = self.files.get_mut(path) else {
            return;
        };
        let Some(blame) = file.blame.clone() else {
            return;
        };
        let entries = file
            .entries
            .get_or_insert_with(|| Rc::new(blame_entries(&blame, view, date_style(user_locale()))))
            .clone();
        let annotations = Annotations {
            source: BLAME_SOURCE.into(),
            entries,
            rows: blame_rows(&blame, &file.rows),
            provider: Some(Rc::new(BlameColumn {
                store: cx.weak_entity(),
                path: path.to_owned(),
            })),
        };
        let path = path.to_owned();
        code_editor::hub(cx).update(cx, |hub, cx| {
            hub.set_annotations(&path, Some(annotations), cx)
        });
        cx.notify();
    }
    fn document_event(
        &mut self,
        hub: &Entity<code_editor::Hub>,
        event: &DocumentEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            DocumentEvent::Changed { doc, text, .. } if self.is_on(&doc.path) => {
                self.align(&doc.path, text.clone(), cx)
            }
            DocumentEvent::Saved { doc, text, .. } if self.is_on(&doc.path) => {
                self.align(&doc.path, text.clone(), cx);
                self.refresh(&doc.path, cx);
            }
            DocumentEvent::Closed { doc } if hub.read(cx).document(&doc.path).is_none() => {
                self.turn_off(&doc.path, cx)
            }
            _ => {}
        }
    }
}

/// Lets an editor answer the presses in its own blame column.
pub(super) fn watch_blame(cx: &mut Context<FileEditor>) -> Subscription {
    let store = blames(cx);
    cx.subscribe(&store, |this, _, event: &BlameRequest, cx| match event {
        BlameRequest::Open { editor, sha } if *editor == this.input.entity_id() => {
            this.open_history(Some(Request::Commit(sha.clone())), cx)
        }
        _ => {}
    })
}

impl FileEditor {
    fn git_state(&self, cx: &App) -> GitState {
        let Some(repo) = scm::local_repo(Path::new(&self.path)) else {
            return GitState {
                repo: false,
                tracked: false,
                remote: None,
            };
        };
        let status = self.git.read(cx).statuses.files.get(&self.path);
        GitState {
            repo: true,
            tracked: self.document.is_some()
                && !matches!(status.map(String::as_str), Some("untracked" | "added")),
            remote: repo.remote,
        }
    }
    fn blame_on(&self, cx: &App) -> bool {
        blames_ref(cx).is_some_and(|store| store.is_on(&self.extensions.doc.path))
    }
    /// The commit that last changed the caret's line, while blame is on.
    fn caret_commit(&self, cx: &App) -> Option<BlameCommit> {
        let input = self.input.read(cx);
        let row = code_editor::row_of(&input.content, input.caret());
        blames_ref(cx)?
            .commit_at(&self.extensions.doc.path, row)
            .cloned()
    }
    /// The rows of the code menu's Git submenu.
    pub(super) fn git_menu_items(&self, cx: &App) -> Vec<MenuAction> {
        let input = self.input.read(cx);
        let changes = !input.editor_analysis().changes.is_empty();
        let writable = !input.editing_caps().read_only;
        let selected = !input.selection_range().is_empty();
        let state = self.git_state(cx);
        let committed = state.repo && state.tracked;
        let blame_on = self.blame_on(cx);
        let host = state.remote.as_ref().map(|remote| remote.name().to_owned());
        let mut items = vec![
            MenuAction::new("blame", "Annotate with Git Blame")
                .checked(blame_on)
                .disabled(!committed && !blame_on),
        ];
        if let Some(commit) = self.caret_commit(cx) {
            items.push(
                MenuAction::new("blame-commit", format!("Show Commit {}", commit.short_sha))
                    .description(commit.subject),
            );
        }
        items.extend([
            MenuAction::new("history", "Show History").disabled(!state.repo),
            MenuAction::new(
                "line-history",
                if selected {
                    "Show History for Selection"
                } else {
                    "Show History for Line"
                },
            )
            .disabled(!committed),
            MenuAction::new(
                "open-remote",
                match &host {
                    Some(host) => format!("Open on {host}"),
                    None => "Open on Remote".into(),
                },
            )
            .icon("external-link")
            .disabled(!committed || host.is_none()),
            MenuAction::new(
                "copy-remote-link",
                match &host {
                    Some(host) => format!("Copy {host} Link"),
                    None => "Copy Link".into(),
                },
            )
            .disabled(!committed || host.is_none()),
            MenuAction::new("previous-change", "Previous Change")
                .shortcut(keys("⌃⌥⇧↑", "Ctrl+Alt+Shift+Up"))
                .disabled(!changes),
            MenuAction::new("next-change", "Next Change")
                .shortcut(keys("⌃⌥⇧↓", "Ctrl+Alt+Shift+Down"))
                .disabled(!changes),
            MenuAction::new("revert-change", "Revert Change").disabled(!changes || !writable),
        ]);
        items
    }
    /// Takes a picked Git row; `false` when it is not one.
    pub(super) fn git_menu_pick(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let action: Box<dyn Action> = match id {
            "history" => {
                self.open_history(None, cx);
                return true;
            }
            "blame-commit" => {
                if let Some(commit) = self.caret_commit(cx) {
                    self.open_history(Some(Request::Commit(commit.sha)), cx);
                }
                return true;
            }
            "copy-remote-link" => {
                self.remote_link(false, window, cx);
                return true;
            }
            "blame" => Box::new(ToggleBlame),
            "line-history" => Box::new(ShowLineHistory),
            "open-remote" => Box::new(OpenOnRemote),
            "previous-change" => Box::new(PreviousChange),
            "next-change" => Box::new(NextChange),
            "revert-change" => Box::new(RevertChange),
            _ => return false,
        };
        window.focus(&self.input.focus_handle(cx), cx);
        window.dispatch_action(action, cx);
        true
    }
    /// Opens the history window for this file, showing `request`.
    fn open_history(&mut self, request: Option<Request>, cx: &mut Context<Self>) {
        cx.emit(FileEvent::FileHistory {
            cwd: self.cwd.clone(),
            path: self.path.clone(),
            request,
        });
    }
    /// The selected lines (one-based), or `None` without a selection.
    fn selected_lines(&self, cx: &App) -> Option<(usize, usize)> {
        let selected = !self.input.read(cx).selection_range().is_empty();
        selected.then(|| self.menu_target(cx).lines)
    }

    pub(super) fn toggle_blame(&mut self, _: &ToggleBlame, _: &mut Window, cx: &mut Context<Self>) {
        let path = self.extensions.doc.path.clone();
        let store = blames(cx);
        if store.read(cx).is_on(&path) {
            store.update(cx, |store, cx| store.turn_off(&path, cx));
            cx.notify();
            return;
        }
        let state = self.git_state(cx);
        if !state.repo || !state.tracked {
            let why = match state.repo {
                false => "This file is not in a Git repository.",
                true => "Git has no committed version of this file to annotate.",
            };
            self.show_notice(why.into(), cx);
            return;
        }
        let text = self.input.read(cx).content.clone();
        let git = self.git.clone();
        store.update(cx, |store, cx| store.turn_on(path, text, &git, cx));
        cx.notify();
    }
    pub(super) fn show_line_history(
        &mut self,
        _: &ShowLineHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.document.is_none() {
            return;
        }
        let (first, last) = self.menu_target(cx).lines;
        let text = self.input.read(cx).content.to_string();
        self.open_history(
            Some(Request::Lines {
                first,
                last,
                text: Some(text),
            }),
            cx,
        );
    }
    pub(super) fn open_on_remote(
        &mut self,
        _: &OpenOnRemote,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remote_link(true, window, cx);
    }
    /// Opens the file's page on its remote at HEAD, with the selected lines
    /// marked, or copies its address.
    fn remote_link(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.document.is_none() {
            return;
        }
        let path = PathBuf::from(&self.path);
        let lines = self.selected_lines(cx);
        let text = self.input.read(cx).content.to_string();
        let work = cx
            .background_executor()
            .spawn(async move { scm::remote_file_link(&path, lines, Some(&text)) });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(url) if open => crate::shell::browser_links::open_link(&url, false, window, cx),
                Ok(url) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(url));
                    this.show_notice("Link copied".into(), cx);
                }
                Err(error) => this.show_notice(error, cx),
            });
        })
        .detach();
    }
    pub(super) fn compare_with_clipboard(
        &mut self,
        _: &CompareWithClipboard,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.document.is_none() {
            return;
        }
        let Some(clipboard) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.show_notice("The clipboard has no text to compare.".into(), cx);
            return;
        };
        let input = self.input.read(cx);
        let range = input.selection_range();
        let content = input.content.clone();
        let name = self.relative_path();
        let (title, right) = match self.selected_lines(cx) {
            Some((first, last)) if range.end <= content.len() => (
                match first == last {
                    true => format!("Clipboard ↔ {name}: line {first}"),
                    false => format!("Clipboard ↔ {name}: lines {first}–{last}"),
                },
                content[range].to_owned(),
            ),
            _ => (format!("Clipboard ↔ {name}"), content.to_string()),
        };
        self.open_history(
            Some(Request::Compare {
                title,
                left: clipboard,
                right,
            }),
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, WindowHandle};
    use std::cell::RefCell;

    fn input_id(editor: &FileEditor) -> EntityId {
        editor.input.entity_id()
    }

    fn git(dir: &Path, args: &[&str]) {
        let done = std::process::Command::new("git")
            .args(["-c", "user.name=Ann", "-c", "user.email=ann@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            done.status.success(),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
    }

    struct Setup {
        window: WindowHandle<FileEditor>,
        history: Rc<RefCell<Vec<(String, Option<Request>)>>>,
        dir: tempfile::TempDir,
        path: String,
    }

    /// An editor on `a.txt`, committed twice when `repo`, else outside Git.
    fn setup(cx: &mut TestAppContext, repo: bool) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        if repo {
            git(root, &["init"]);
            git(root, &["add", "-A"]);
            git(root, &["commit", "-m", "first"]);
            std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
            git(root, &["commit", "-am", "second"]);
        }
        cx.update(|cx| {
            cx.set_global(crate::settings::Preferences::default());
            crate::popover::bind(cx);
        });
        let path = root.join("a.txt").to_string_lossy().into_owned();
        let project = root.to_string_lossy().into_owned();
        let opened = path.clone();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                let git = cx.new(|cx| ProjectGit::new(project, cx));
                cx.new(|cx| FileEditor::new(opened, git, cx))
            })
            .unwrap()
        });
        let history = Rc::new(RefCell::new(Vec::new()));
        let log = history.clone();
        window
            .update(cx, |editor, _, cx| {
                cx.subscribe(&cx.entity(), move |_, _, event: &FileEvent, _| {
                    if let FileEvent::FileHistory { path, request, .. } = event {
                        log.borrow_mut().push((path.clone(), request.clone()));
                    }
                })
                .detach();
                editor.set_visible(true, cx);
            })
            .unwrap();
        cx.run_until_parked();
        Setup {
            window,
            history,
            dir,
            path,
        }
    }

    fn rows(editor: &FileEditor, cx: &App) -> Vec<(String, String, bool, bool)> {
        editor
            .git_menu_items(cx)
            .into_iter()
            .map(|a| (a.id.to_string(), a.label.to_string(), a.disabled, a.checked))
            .collect()
    }
    fn row(rows: &[(String, String, bool, bool)], id: &str) -> (String, bool, bool) {
        let row = rows.iter().find(|r| r.0 == id).expect(id);
        (row.1.clone(), row.2, row.3)
    }
    /// The hub's blame column of a file as (row, text) pairs.
    fn column(cx: &mut TestAppContext, path: &str) -> Option<Vec<(usize, String)>> {
        cx.update(|cx| {
            let hub = code_editor::hub(cx);
            let annotations = hub.read(cx).annotations(Path::new(path))?;
            Some(
                annotations
                    .rows
                    .iter()
                    .map(|line| (line.row, annotations.entries[line.entry].parts.join(" ")))
                    .collect(),
            )
        })
    }

    fn commit(sha: char, author: &str, time: u64) -> BlameCommit {
        BlameCommit {
            sha: sha.to_string().repeat(40),
            short_sha: sha.to_string().repeat(7),
            author: author.into(),
            email: "ann@example.com".into(),
            time,
            subject: "First".into(),
            body: "Why.\n\nMore.".into(),
            previous: None,
        }
    }

    #[test]
    fn dates_follow_the_locales_order_and_padding() {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 2, 4).unwrap();
        let cases = [
            ("tr_TR.UTF-8", "04.02.2026"),
            ("de-DE", "04.02.2026"),
            ("en_US", "2/4/2026"),
            ("en", "2/4/2026"),
            ("en_GB", "04/02/2026"),
            ("fr_CA", "2026-02-04"),
            ("nl_NL", "04-02-2026"),
            ("ja_JP", "2026/02/04"),
            ("sv_SE", "2026-02-04"),
            ("", "2026-02-04"),
        ];
        for (locale, shown) in cases {
            assert_eq!(format_date(day, date_style(locale)), shown, "{locale}");
        }
    }

    #[test]
    fn authors_go_by_their_family_name() {
        assert_eq!(short_author("Ahmet Kartal"), "Kartal");
        assert_eq!(short_author("  Ann  "), "Ann");
        assert_eq!(short_author("Kartal, Ahmet"), "Kartal");
        assert_eq!(short_author("Jean Paul Sartre"), "Sartre");
        assert_eq!(short_author("bot@ci"), "bot@ci");
        assert_eq!(short_author(""), "");
    }

    #[test]
    fn recent_commits_tint_strongest_and_the_oldest_not_at_all() {
        let tints = age_tints(&[500, 100, 400, 300, 200, 500]);
        assert_eq!(tints, [1., 0., 0.75, 0.5, 0.25, 1.]);
        assert_eq!(age_tints(&[7]), [1.]);
        // Many commits share a shade per fifth.
        let many: Vec<u64> = (0..10).collect();
        let tints = age_tints(&many);
        assert_eq!((tints[9], tints[8], tints[0]), (1., 1., 0.));
    }

    #[test]
    fn the_column_skips_changed_lines_and_names_when_and_who() {
        let blame = Blame {
            head: "h".into(),
            path: "a.txt".into(),
            commits: vec![
                commit('a', "Ahmet Kartal", 1_000),
                commit('b', "Bob", 2_000),
            ],
            lines: vec![
                scm::BlameLine {
                    commit: 0,
                    text: "one".into(),
                },
                scm::BlameLine {
                    commit: 1,
                    text: "two".into(),
                },
            ],
        };
        let style = DateStyle::DayFirst('.');
        let entries = blame_entries(&blame, BlameView::default(), style);
        let date = local_time(1_000, style).0;
        assert_eq!(
            entries[0].parts,
            [date.clone().into(), SharedString::from("Kartal")]
        );
        assert_eq!(entries[0].id.as_ref(), "a".repeat(40));
        assert_eq!((entries[0].tint, entries[1].tint), (0.5, 1.), "b is newer");
        let tip = entries[0].tooltip.to_string();
        for part in [
            "Commit aaaaaaa",
            "Ahmet Kartal <ann@example.com>",
            &date,
            "First",
            "Why.",
        ] {
            assert!(tip.contains(part), "{part} in {tip}");
        }
        let view = BlameView {
            date: false,
            author: true,
            commit: true,
        };
        assert_eq!(
            blame_entries(&blame, view, style)[1].parts,
            [SharedString::from("Bob"), "bbbbbbb".into()]
        );
        let rows = blame_rows(&blame, &[Some(0), None, Some(1)]);
        let shown: Vec<_> = rows.iter().map(|line| (line.row, line.entry)).collect();
        assert_eq!(shown, [(0, 0), (2, 1)]);
    }

    #[gpui::test]
    fn outside_a_repository_the_git_rows_are_disabled(cx: &mut TestAppContext) {
        let s = setup(cx, false);
        s.window
            .update(cx, |editor, _, cx| {
                let rows = rows(editor, cx);
                for id in [
                    "blame",
                    "history",
                    "line-history",
                    "open-remote",
                    "copy-remote-link",
                ] {
                    assert!(row(&rows, id).1, "{id} is disabled");
                }
                assert_eq!(row(&rows, "open-remote").0, "Open on Remote");
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_rows_follow_the_files_state_and_remote(cx: &mut TestAppContext) {
        let s = setup(cx, true);
        s.window
            .update(cx, |editor, _, cx| {
                let rows = rows(editor, cx);
                assert_eq!(
                    row(&rows, "blame"),
                    ("Annotate with Git Blame".into(), false, false)
                );
                assert!(!row(&rows, "line-history").1);
                assert_eq!(row(&rows, "line-history").0, "Show History for Line");
                assert!(row(&rows, "open-remote").1, "no remote yet");
            })
            .unwrap();
        git(
            s.dir.path(),
            &["remote", "add", "origin", "https://gitlab.com/acme/app.git"],
        );
        s.window
            .update(cx, |editor, _, cx| {
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(0..5, cx));
                let rows = rows(editor, cx);
                assert_eq!(
                    row(&rows, "open-remote"),
                    ("Open on GitLab".into(), false, false)
                );
                assert_eq!(row(&rows, "copy-remote-link").0, "Copy GitLab Link");
                assert_eq!(row(&rows, "line-history").0, "Show History for Selection");
            })
            .unwrap();
        // An untracked file has no history, blame or page to offer.
        let new = s.dir.path().join("new.txt");
        std::fs::write(&new, "x\n").unwrap();
        let project = s.dir.path().to_string_lossy().into_owned();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                let git = cx.new(|cx| ProjectGit::new(project, cx));
                cx.new(|cx| FileEditor::new(new.to_string_lossy().into_owned(), git, cx))
            })
            .unwrap()
        });
        cx.run_until_parked();
        window
            .update(cx, |editor, _, cx| {
                let rows = rows(editor, cx);
                for id in ["blame", "line-history", "open-remote"] {
                    assert!(row(&rows, id).1, "{id} is disabled");
                }
                assert!(!row(&rows, "history").1);
            })
            .unwrap();
    }

    #[gpui::test]
    fn blame_annotates_committed_lines_follows_edits_and_turns_off(cx: &mut TestAppContext) {
        let s = setup(cx, true);
        let plain = s
            .window
            .update(cx, |editor, window, cx| {
                let gutter = editor.input.read(cx).annotation_width();
                editor.toggle_blame(&ToggleBlame, window, cx);
                gutter
            })
            .unwrap();
        assert_eq!(plain, 0.);
        cx.run_until_parked();
        let shown = column(cx, &s.path).expect("a column");
        assert_eq!(shown.len(), 3, "{shown:?}");
        assert!(shown[0].1.ends_with(" Ann"), "{shown:?}");
        assert!(cx.update(|cx| {
            code_editor::hub(cx)
                .read(cx)
                .inline_notes(Path::new(&s.path))
                .is_empty()
        }));
        s.window
            .update(cx, |editor, _, cx| {
                let input = editor.input.read(cx);
                assert!(input.annotation_width() > 20., "as wide as its text");
                assert!(input.annotation_text(0).unwrap().ends_with("Ann"));
                assert_eq!(row(&rows(editor, cx), "blame").2, true, "checked");
                let commit = editor.caret_commit(cx).expect("the caret's line has one");
                assert_eq!(commit.subject, "first");
                assert_eq!(
                    row(&rows(editor, cx), "blame-commit").0,
                    format!("Show Commit {}", commit.short_sha)
                );
                // A line typed above the first moves the rows down and is blank.
                editor.input.update(cx, |input, cx| {
                    input.replace_all("zero\none\ntwo\nTHREE\n".into(), cx)
                });
                editor.sync_document(cx);
            })
            .unwrap();
        cx.run_until_parked();
        let rows_shown: Vec<_> = column(cx, &s.path)
            .unwrap()
            .into_iter()
            .map(|n| n.0)
            .collect();
        assert_eq!(
            rows_shown,
            [1, 2],
            "the typed and the edited line are blank"
        );
        s.window
            .update(cx, |editor, _, cx| {
                let input = editor.input.read(cx);
                assert_eq!(input.annotation_text(0).as_deref(), Some(""));
                assert_eq!(input.annotation_text(3).as_deref(), Some(""));
                assert!(!input.annotation_text(1).unwrap().is_empty());
            })
            .unwrap();
        // Picking the commit row opens the history at that commit.
        s.window
            .update(cx, |editor, window, cx| {
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(6..6, cx));
                assert!(editor.git_menu_pick("blame-commit", window, cx));
            })
            .unwrap();
        let asked = s.history.borrow().clone();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, s.path);
        assert!(matches!(asked[0].1, Some(Request::Commit(_))));
        s.window
            .update(cx, |editor, window, cx| {
                editor.toggle_blame(&ToggleBlame, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        assert!(column(cx, &s.path).is_none());
        s.window
            .update(cx, |editor, _, cx| {
                assert_eq!(editor.input.read(cx).annotation_width(), 0.);
                assert_eq!(editor.input.read(cx).annotation_text(0), None);
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_column_hovers_clicks_and_closes_from_its_menu(cx: &mut TestAppContext) {
        let s = setup(cx, true);
        s.window
            .update(cx, |editor, window, cx| {
                editor.toggle_blame(&ToggleBlame, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        let visual = &mut gpui::VisualTestContext::from_window(s.window.into(), cx);
        visual.run_until_parked();
        let (first, third) = s
            .window
            .update(visual, |editor, _, cx| {
                let input = editor.input.read(cx);
                (
                    input.annotation_center(0).expect("drawn"),
                    input.annotation_center(2).unwrap(),
                )
            })
            .unwrap();
        visual.simulate_mouse_move(third, None, gpui::Modifiers::default());
        visual.run_until_parked();
        s.window
            .update(visual, |editor, _, cx| {
                let tip = editor.input.read(cx).gutter_tip_text().expect("a tooltip");
                assert!(tip.contains("Ann <ann@example.com>"), "{tip}");
                assert!(tip.contains("second"), "{tip}");
            })
            .unwrap();
        visual.simulate_click(first, gpui::Modifiers::default());
        visual.run_until_parked();
        let head = scm::blame_file(Path::new(&s.path), None).unwrap();
        let first_sha = head.commits[head.lines[0].commit].sha.clone();
        assert_eq!(
            s.history.borrow().last().cloned(),
            Some((s.path.clone(), Some(Request::Commit(first_sha.clone()))))
        );
        // A right press opens the column's menu instead.
        visual.simulate_mouse_down(first, gpui::MouseButton::Right, gpui::Modifiers::default());
        visual.simulate_mouse_up(first, gpui::MouseButton::Right, gpui::Modifiers::default());
        visual.run_until_parked();
        assert_eq!(s.history.borrow().len(), 1);
        s.window
            .update(visual, |editor, _, cx| {
                assert!(editor.input.read(cx).gutter_menu_open());
            })
            .unwrap();
        // Its rows, picked as the menu would.
        let menu = s
            .window
            .update(visual, |editor, window, cx| {
                let hub = code_editor::hub(cx);
                let annotations = hub.read(cx).annotations(Path::new(&s.path)).unwrap();
                let provider = annotations.provider.clone().unwrap();
                let click = AnnotationClick {
                    row: 0,
                    entry: annotations.entry(0).cloned(),
                    button: GutterButton::Right,
                    position: first,
                    editor: input_id(editor),
                };
                provider.menu(&editor.extensions.doc.clone(), &click, window, cx)
            })
            .unwrap()
            .expect("a menu");
        let ids: Vec<String> = menu
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action(action) => Some(action.id.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            ids,
            [
                "close",
                "copy",
                "diff",
                "previous",
                "view-date",
                "view-author",
                "view-commit"
            ]
        );
        let pick = |id: &str, visual: &mut gpui::VisualTestContext| {
            let on_pick = menu.on_pick.clone();
            let id = id.to_owned();
            visual.update(|window, cx| on_pick(&id, window, cx));
            visual.run_until_parked();
        };
        pick("copy", visual);
        let copied = visual.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
        assert_eq!(copied, Some(first_sha.clone()));
        pick("view-commit", visual);
        let shown = column(visual, &s.path).unwrap();
        assert!(shown[0].1.ends_with(&first_sha[..7]), "{shown:?}");
        pick("view-commit", visual);
        pick("close", visual);
        assert!(column(visual, &s.path).is_none());
        s.window
            .update(visual, |editor, _, cx| {
                assert_eq!(row(&rows(editor, cx), "blame").2, false, "unchecked");
            })
            .unwrap();
    }

    #[gpui::test]
    fn annotate_previous_revision_blames_the_commit_before(cx: &mut TestAppContext) {
        let s = setup(cx, true);
        s.window
            .update(cx, |editor, window, cx| {
                editor.toggle_blame(&ToggleBlame, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        let path = PathBuf::from(&s.path);
        let previous = cx.update(|cx| {
            let store = blames_ref(cx).unwrap();
            let file = &store.files[&path];
            let blame = file.blame.as_ref().unwrap();
            // Line 3 came with "second"; the revision before it is "first".
            blame.commits[blame.lines[2].commit].previous.clone()
        });
        assert!(previous.is_some());
        cx.update(|cx| blames(cx).update(cx, |store, cx| store.annotate_at(&path, previous, cx)));
        cx.run_until_parked();
        let shown = column(cx, &s.path).unwrap();
        assert_eq!(
            shown.iter().map(|r| r.0).collect::<Vec<_>>(),
            [0, 1],
            "the third line was not there yet"
        );
    }

    #[gpui::test]
    fn line_history_compare_and_link_requests_carry_the_selection(cx: &mut TestAppContext) {
        let s = setup(cx, true);
        git(
            s.dir.path(),
            &["remote", "add", "origin", "git@github.com:acme/app.git"],
        );
        let last = |s: &Setup| s.history.borrow().last().and_then(|(_, r)| r.clone());
        s.window
            .update(cx, |editor, window, cx| {
                // "two\nthree" is lines 2 and 3.
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(4..13, cx));
                editor.show_line_history(&ShowLineHistory, window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            last(&s),
            Some(Request::Lines {
                first: 2,
                last: 3,
                text: Some("one\ntwo\nthree\n".into()),
            })
        );
        s.window
            .update(cx, |editor, window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string("two\n3".into()));
                editor.compare_with_clipboard(&CompareWithClipboard, window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            last(&s),
            Some(Request::Compare {
                title: "Clipboard ↔ a.txt: lines 2–3".into(),
                left: "two\n3".into(),
                right: "two\nthree".into(),
            })
        );
        s.window
            .update(cx, |editor, window, cx| {
                editor.git_menu_pick("copy-remote-link", window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(s.history.borrow().len(), 2);
        let head = scm::head_revision(Path::new(&s.path)).unwrap();
        let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|c| c.text()));
        assert_eq!(
            copied.as_deref(),
            Some(format!("https://github.com/acme/app/blob/{head}/a.txt#L2-L3").as_str())
        );
    }
}
