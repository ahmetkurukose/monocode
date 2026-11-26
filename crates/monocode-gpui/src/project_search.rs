//! ProjectSearch.tsx: content search inside the Explorer, scoped to its checkout.
use crate::{
    file_icons::file_type_icon,
    input::{self, Input},
    right_panel::{RED_400, spinner, stroke, tabular},
    theme::*,
};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle, Role,
    ScrollHandle, StyledText, Subscription, Task, Window, div, point, prelude::*, px, rgb,
};
use monocode_core::{
    composer,
    files::ProjectFile,
    search::project::{self, SearchMatch, SearchOptions},
};
use std::{
    collections::HashMap,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn accent(alpha: f32) -> gpui::Hsla {
    let mut color = accent_color();
    color.a = alpha;
    color
}

/// The search as the Explorer shows it. Its hosts (the right panel's cached
/// Explorer) give it a size but are not flex containers themselves, so it
/// takes all of that size rather than asking to flex: with `flex_1` the
/// results scroller was given no height and the list never showed.
/// Most files listed by name.
const NAME_HITS: usize = 100;

pub(crate) fn pane(search: &Entity<ProjectSearch>) -> AnyElement {
    search
        .clone()
        .cached(gpui::StyleRefinement::default().size_full())
        .into_any_element()
}

pub(crate) enum SearchEvent {
    Open {
        cwd: String,
        matched: SearchMatch,
        pin: bool,
    },
    Close,
}

struct Match {
    source: SearchMatch,
    highlight: Option<Range<usize>>,
}

struct Group {
    /// The project the file is in, and its name when several are searched.
    cwd: String,
    project: Option<String>,
    relative: String,
    name: String,
    matches: Vec<Match>,
}

/// What typing searches: file names, or what is inside the files.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Mode {
    #[default]
    Files,
    Content,
}

pub(crate) struct ProjectSearch {
    mode: Mode,
    cwd: String,
    active: bool,
    /// Search every project open in the rail, not only this checkout.
    all_projects: bool,
    /// The folders the last search looked in: this checkout, or every open project.
    roots: Vec<String>,
    /// The project's files, listed once per folder, for matching names.
    files: Arc<Vec<ProjectFile>>,
    files_key: Option<(Vec<String>, bool)>,
    files_task: Option<Task<()>>,
    /// Files whose name or path matches the query, best first.
    name_hits: Vec<ProjectFile>,
    query: Entity<Input>,
    include: Entity<Input>,
    exclude: Entity<Input>,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    loading: bool,
    error: Option<String>,
    groups: Vec<Group>,
    count: usize,
    truncated: bool,
    cancel: Arc<AtomicBool>,
    debounce: Option<Task<()>>,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SearchEvent> for ProjectSearch {}

impl Focusable for ProjectSearch {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl Drop for ProjectSearch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl ProjectSearch {
    pub(crate) fn set_query(&mut self, text: String, cx: &mut Context<Self>) {
        self.query.update(cx, |input, cx| input.set_text(text, cx));
    }
    pub(crate) fn new(cwd: String, active: bool, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            let mut input = Input::field("Search", "Search", content(0.35), cx);
            input.set_font_size(12.);
            input
        });
        let include = cx.new(|cx| {
            let mut input = Input::field("files to include", "files to include", content(0.35), cx);
            input.set_font_size(11.);
            input
        });
        let exclude = cx.new(|cx| {
            let mut input = Input::field("files to exclude", "files to exclude", content(0.35), cx);
            input.set_font_size(11.);
            input
        });
        let subscriptions = [&query, &include, &exclude]
            .into_iter()
            .map(|input| cx.subscribe(input, |this, _, _: &input::Changed, cx| this.refresh(cx)))
            .collect::<Vec<_>>();
        let mut subscriptions = subscriptions;
        subscriptions.push(cx.observe_global::<crate::open_projects::OpenProjects>(
            |this: &mut Self, cx| {
                if this.all_projects {
                    this.refresh(cx);
                }
            },
        ));
        Self {
            mode: Mode::Files,
            roots: vec![cwd.clone()],
            cwd,
            active,
            all_projects: false,
            files: Arc::new(Vec::new()),
            files_key: None,
            files_task: None,
            name_hits: Vec::new(),
            query,
            include,
            exclude,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            loading: false,
            error: None,
            groups: Vec::new(),
            count: 0,
            truncated: false,
            cancel: Arc::new(AtomicBool::new(false)),
            debounce: None,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.query.update(cx, |input, cx| {
            let length = input.content.len();
            input.select_range(0..length, cx);
        });
        window.focus(&self.query.focus_handle(cx), cx);
    }

    pub(crate) fn is_composing(&self, cx: &App) -> bool {
        [&self.query, &self.include, &self.exclude]
            .into_iter()
            .any(|input| input.read(cx).is_composing())
    }

    pub(crate) fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            self.refresh(cx);
        }
    }

    pub(crate) fn set_root(&mut self, cwd: String, cx: &mut Context<Self>) {
        if self.cwd != cwd {
            self.cwd = cwd;
            self.groups.clear();
            self.count = 0;
            self.files = Arc::new(Vec::new());
            self.files_key = None;
            self.name_hits.clear();
            self.refresh(cx);
        }
    }

    /// Where a search looks: this checkout, or every open project with it first.
    fn search_roots(&self, cx: &App) -> Vec<String> {
        if self.all_projects {
            crate::open_projects::local_projects(cx, Some(&self.cwd))
        } else {
            vec![self.cwd.clone()]
        }
    }

    fn set_all_projects(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.all_projects != on {
            self.all_projects = on;
            self.refresh(cx);
        }
    }

    fn options(&self, cx: &App) -> SearchOptions {
        let filter = |input: &Entity<Input>| {
            let text = input.read(cx).content.trim();
            (!text.is_empty()).then(|| text.to_owned())
        };
        SearchOptions {
            cwd: self.cwd.clone(),
            query: self.query.read(cx).content.trim().to_owned(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            regex: self.regex,
            include: filter(&self.include),
            exclude: filter(&self.exclude),
            search_id: String::new(),
            include_ignored: crate::settings::show_excluded_files(cx),
        }
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.refresh(cx);
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Release);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.debounce = None;
        self.loading = false;
        self.error = None;
        self.truncated = false;
        let options = self.options(cx);
        self.roots = self.search_roots(cx);
        let nothing = options.query.is_empty() || self.cwd.is_empty() || self.cwd == "~";
        if nothing || self.mode == Mode::Files {
            // File names are matched here, from a list kept in memory; no
            // content search runs.
            self.groups.clear();
            self.count = 0;
            self.scroll.set_offset(point(px(0.), px(0.)));
            if nothing {
                self.name_hits.clear();
            } else {
                if self.active {
                    self.load_files(cx);
                }
                self.rank_names(cx);
            }
            cx.notify();
            return;
        }
        self.name_hits.clear();
        if self.active {
            let check = self.cancel.clone();
            self.debounce = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                if !check.load(Ordering::Acquire) {
                    let _ = this.update(cx, |this, cx| {
                        let roots = this.roots.clone();
                        this.start(options, roots, cx)
                    });
                }
            }));
        }
        cx.notify();
    }

    /// Lists the project's files once (again if the excluded-files setting changes).
    fn load_files(&mut self, cx: &mut Context<Self>) {
        let key = (self.roots.clone(), crate::settings::show_excluded_files(cx));
        if self.files_key.as_ref() == Some(&key) {
            return;
        }
        self.files_key = Some(key.clone());
        let (roots, include_ignored) = key.clone();
        let work = cx.background_executor().spawn(async move {
            if let [cwd] = roots.as_slice() {
                return monocode_core::files::list_project_files_with(cwd, None, include_ignored);
            }
            // Across projects a file's path starts with its project's name, so
            // typing the name narrows to that project.
            let mut all = Vec::new();
            for root in &roots {
                let name = crate::open_projects::project_name(root);
                let Ok(files) =
                    monocode_core::files::list_project_files_with(root, None, include_ignored)
                else {
                    continue;
                };
                all.extend(files.into_iter().map(|mut file| {
                    file.relative = format!("{name}/{}", file.relative);
                    file
                }));
            }
            Ok(all)
        });
        self.files_task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.files_key.as_ref() != Some(&key) {
                    return;
                }
                match result {
                    Ok(files) => this.files = Arc::new(files),
                    Err(_) => this.files_key = None,
                }
                this.rank_names(cx);
                cx.notify();
            });
        }));
    }

    /// The files whose name or path matches what is typed, the way Quick Open ranks them.
    fn rank_names(&mut self, cx: &Context<Self>) {
        let query = self.query.read(cx).content.trim().to_owned();
        self.name_hits = if query.is_empty() {
            Vec::new()
        } else {
            composer::rank_files(&self.files, &query, NAME_HITS)
                .into_iter()
                .cloned()
                .collect()
        };
    }

    fn pick_file(&self, index: usize, pin: bool, cx: &mut Context<Self>) {
        if let Some(file) = self.name_hits.get(index) {
            let cwd = crate::open_projects::owning_root(&self.roots, &file.path)
                .unwrap_or(&self.cwd)
                .clone();
            let relative = if self.roots.len() > 1 {
                file.relative
                    .split_once('/')
                    .map_or(file.relative.clone(), |(_, rest)| rest.to_owned())
            } else {
                file.relative.clone()
            };
            cx.emit(SearchEvent::Open {
                cwd,
                matched: SearchMatch {
                    path: file.path.clone(),
                    relative,
                    line: 1,
                    column: 1,
                    preview: String::new(),
                },
                pin,
            });
        }
    }

    fn start(&mut self, options: SearchOptions, roots: Vec<String>, cx: &mut Context<Self>) {
        self.loading = true;
        let flag = self.cancel.clone();
        let check = flag.clone();
        let work = cx.background_executor().spawn(async move {
            let several = roots.len() > 1;
            let mut groups: Vec<Group> = Vec::new();
            let mut by_path = HashMap::new();
            let mut count = 0;
            let mut truncated = false;
            for root in roots {
                if flag.load(Ordering::Acquire) {
                    break;
                }
                let options = SearchOptions {
                    cwd: root.clone(),
                    ..options.clone()
                };
                let result =
                    match project::search_project(std::path::Path::new(&root), &options, &flag) {
                        Ok(result) => result,
                        // One project failing (moved, unreadable) leaves the others' results.
                        Err(error) if several => {
                            log::debug!("search in {root} failed: {error}");
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                let ranges = project::preview_ranges(&result.matches, &options);
                count += result.matches.len();
                truncated |= result.truncated;
                let project = several.then(|| crate::open_projects::project_name(&root));
                for (source, highlight) in result.matches.into_iter().zip(ranges) {
                    let index = *by_path.entry(source.path.clone()).or_insert_with(|| {
                        let index = groups.len();
                        groups.push(Group {
                            cwd: root.clone(),
                            project: project.clone(),
                            name: source
                                .relative
                                .rsplit('/')
                                .next()
                                .unwrap_or(&source.relative)
                                .to_owned(),
                            relative: source.relative.clone(),
                            matches: Vec::new(),
                        });
                        index
                    });
                    groups[index].matches.push(Match { source, highlight });
                }
            }
            Ok::<_, String>((groups, count, truncated))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            if check.load(Ordering::Acquire) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok((groups, count, truncated)) => {
                        this.groups = groups;
                        this.count = count;
                        this.truncated = truncated;
                    }
                    Err(error) => {
                        this.groups.clear();
                        this.count = 0;
                        this.error = Some(error);
                    }
                }
                this.scroll.set_offset(point(px(0.), px(0.)));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn pick(&self, group: usize, index: usize, pin: bool, cx: &mut Context<Self>) {
        if let Some((group, matched)) = self
            .groups
            .get(group)
            .and_then(|group| Some((group, group.matches.get(index)?)))
        {
            cx.emit(SearchEvent::Open {
                cwd: group.cwd.clone(),
                matched: matched.source.clone(),
                pin,
            });
        }
    }

    fn toggle(
        id: &'static str,
        label: &'static str,
        path: &'static str,
        active: bool,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label)
            .aria_selected(active)
            .size(px(24.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(2.))
            .text_color(content(if active { 1. } else { 0.4 }))
            .when(active, |d| d.bg(content(0.1)))
            .hover(|s| s.bg(content(0.1)).text_color(content(0.7)))
            .child(icon(path, 14.))
    }

    fn filter(input: Entity<Input>) -> AnyElement {
        div()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .rounded(px(6.))
            .border_1()
            .border_color(content(0.1))
            .bg(content(0.05))
            .px_2()
            .py(px(6.))
            .child(div().min_w_0().flex_1().child(input))
            .into_any_element()
    }
}

impl Render for ProjectSearch {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.cwd.is_empty() || self.cwd == "~" {
            return div()
                .px_3()
                .py_2()
                .text_size(px(12.))
                .text_color(content(0.5))
                .child("No project folder")
                .into_any_element();
        }
        let typed = !self.query.read(cx).content.trim().is_empty();
        let status = if self.loading {
            "Searching…".to_owned()
        } else if let Some(error) = &self.error {
            error.clone()
        } else if !typed {
            match self.mode {
                Mode::Files => "Type part of a file name".to_owned(),
                Mode::Content => "Type to search inside the files".to_owned(),
            }
        } else if self.mode == Mode::Files {
            match self.name_hits.len() {
                0 if self.files_key.is_some() && self.files.is_empty() => {
                    "Listing files…".to_owned()
                }
                0 => "No files match".to_owned(),
                n => format!(
                    "{n}{} file{}",
                    if n >= NAME_HITS { "+" } else { "" },
                    if n == 1 { "" } else { "s" }
                ),
            }
        } else if self.count == 0 {
            "No results".to_owned()
        } else {
            let projects = if self.roots.len() > 1 {
                let mut seen: Vec<&str> = Vec::new();
                for group in &self.groups {
                    if !seen.contains(&group.cwd.as_str()) {
                        seen.push(&group.cwd);
                    }
                }
                format!(
                    " across {} project{}",
                    seen.len(),
                    if seen.len() == 1 { "" } else { "s" }
                )
            } else {
                String::new()
            };
            format!(
                "{} result{} in {} file{}{}{}",
                self.count,
                if self.count == 1 { "" } else { "s" },
                self.groups.len(),
                if self.groups.len() == 1 { "" } else { "s" },
                projects,
                if self.truncated { " (limited)" } else { "" }
            )
        };
        let name_rows: Vec<AnyElement> = if self.mode != Mode::Files {
            Vec::new()
        } else {
            self.name_hits
                .iter()
                .enumerate()
                .map(|(index, file)| {
                    let dir = file
                        .relative
                        .rsplit_once('/')
                        .map(|(dir, _)| dir.to_owned())
                        .unwrap_or_default();
                    div()
                        .id(("project-search-file", index))
                        .debug_selector(move || format!("project-search-file-{index}"))
                        .role(Role::Button)
                        .aria_label(file.relative.clone())
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .h(px(26.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(content(0.05)))
                        .child(file_type_icon(&file.name, false, false, 16.))
                        .child(
                            div()
                                .flex_shrink_0()
                                .max_w(px(180.))
                                .truncate()
                                .text_size(px(12.))
                                .child(file.name.clone()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_size(px(10.))
                                .text_color(content(0.4))
                                .child(dir),
                        )
                        .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                            this.pick_file(index, event.click_count() >= 2, cx);
                        }))
                        .into_any_element()
                })
                .collect()
        };
        let groups = self
            .groups
            .iter()
            .enumerate()
            .map(|(group_index, group)| {
                div()
                    .debug_selector(move || format!("project-search-group-{group_index}"))
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(stroke())
                    .child(
                        div()
                            .flex()
                            .min_w_0()
                            .items_center()
                            .gap(px(6.))
                            .px_2()
                            .py(px(6.))
                            .child(file_type_icon(&group.name, false, false, 16.))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_size(px(12.))
                                    .child(group.name.clone()),
                            )
                            .when_some(group.project.clone(), |d, project| {
                                d.child(
                                    div()
                                        .flex_shrink_0()
                                        .max_w(px(120.))
                                        .truncate()
                                        .rounded(px(4.))
                                        .bg(content(0.08))
                                        .px(px(5.))
                                        .py(px(1.))
                                        .text_size(px(10.))
                                        .text_color(content(0.6))
                                        .child(project),
                                )
                            })
                            .child(
                                div()
                                    .rounded_full()
                                    .bg(accent(0.2))
                                    .px(px(6.))
                                    .py(px(2.))
                                    .text_size(px(10.))
                                    .font_features(tabular())
                                    .text_color(accent_color())
                                    .child(group.matches.len().to_string()),
                            ),
                    )
                    .child(
                        div()
                            .truncate()
                            .px_2()
                            .pb_1()
                            .text_size(px(10.))
                            .text_color(content(0.4))
                            .child(group.relative.clone()),
                    )
                    .children(group.matches.iter().enumerate().map(|(index, matched)| {
                        let text = StyledText::new(matched.source.preview.trim_end().to_owned())
                            .with_highlights(matched.highlight.clone().map(|range| {
                                (
                                    range,
                                    HighlightStyle {
                                        background_color: Some(accent(0.35)),
                                        color: Some(content(1.)),
                                        ..Default::default()
                                    },
                                )
                            }));
                        div()
                            .id(("project-search-match", group_index * 501 + index))
                            .debug_selector(move || {
                                format!("project-search-match-{group_index}-{index}")
                            })
                            .role(Role::Button)
                            .aria_label(format!(
                                "{}:{} {}",
                                group.relative, matched.source.line, matched.source.preview
                            ))
                            .w_full()
                            .min_w_0()
                            .flex()
                            .items_start()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .hover(|s| s.bg(content(0.05)))
                            .child(
                                div()
                                    .w(px(28.))
                                    .flex_shrink_0()
                                    .pt(px(1.))
                                    .text_align(gpui::TextAlign::Right)
                                    .font_family("monospace")
                                    .font_features(tabular())
                                    .text_size(px(11.))
                                    .text_color(content(0.35))
                                    .child(matched.source.line.to_string()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .font_family("monospace")
                                    .text_size(px(11.))
                                    .line_height(px(20.))
                                    .text_color(content(0.8))
                                    .child(text),
                            )
                            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                                this.pick(group_index, index, event.click_count() >= 2, cx);
                            }))
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let toggles: Vec<AnyElement> = if self.mode == Mode::Content {
            vec![
                Self::toggle(
                    "project-search-case",
                    "Match case",
                    "case-sensitive",
                    self.case_sensitive,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.case_sensitive = !this.case_sensitive;
                    this.refresh(cx);
                }))
                .into_any_element(),
                Self::toggle(
                    "project-search-word",
                    "Match whole word",
                    "whole-word",
                    self.whole_word,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.whole_word = !this.whole_word;
                    this.refresh(cx);
                }))
                .into_any_element(),
                Self::toggle(
                    "project-search-regex",
                    "Use regular expression",
                    "regex",
                    self.regex,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.regex = !this.regex;
                    this.refresh(cx);
                }))
                .into_any_element(),
            ]
        } else {
            Vec::new()
        };
        let mode = self.mode;
        let tab = |id: &'static str, label: &'static str, target: Mode, cx: &mut Context<Self>| {
            let on = mode == target;
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::Tab)
                .aria_label(label)
                .aria_selected(on)
                .h(px(26.))
                .px(px(12.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(content(if on { 1. } else { 0.5 }))
                .when(on, |d| d.bg(content(0.1)))
                .hover(|s| s.bg(content(0.07)))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.set_mode(target, cx)))
        };
        let tabs = div()
            .flex_shrink_0()
            .flex()
            .gap_1()
            .px_2()
            .pt_2()
            .child(tab("project-search-tab-files", "Files", Mode::Files, cx))
            .child(tab(
                "project-search-tab-content",
                "Content",
                Mode::Content,
                cx,
            ));
        div()
            .id("explorer-project-search")
            .debug_selector(|| "explorer-project-search".into())
            .role(Role::TabPanel)
            .aria_label("Search in files")
            .size_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .text_color(content(1.))
            .capture_action(cx.listener(|this, _: &input::Submit, window, cx| {
                if !this.query.read(cx).is_composing()
                    && this.query.focus_handle(cx).is_focused(window)
                {
                    if this.mode == Mode::Files {
                        this.pick_file(0, false, cx);
                    } else {
                        this.pick(0, 0, false, cx);
                    }
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &crate::shell::DismissMenu, _, cx| {
                if !this.is_composing(cx) {
                    cx.emit(SearchEvent::Close);
                }
                cx.stop_propagation();
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px(px(6.))
                    .py_1()
                    .border_b_1()
                    .border_color(stroke())
                    .child(
                        div()
                            .id("project-search-back")
                            .role(Role::Button)
                            .aria_label("Back to files")
                            .size(px(28.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .text_color(content(0.5))
                            .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
                            .child(icon("chevron-left", 16.))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SearchEvent::Close))),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(content(0.55))
                            .child("Search in files"),
                    ),
            )
            .child(tabs)
            .child(
                div()
                    .flex_shrink_0()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_2()
                    .border_b_1()
                    .border_color(stroke())
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(content(0.1))
                            .bg(content(0.05))
                            .pl_2()
                            .pr_1()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .py(px(6.))
                                    .child(self.query.clone()),
                            )
                            .children(toggles),
                    )
                    .when(self.mode == Mode::Content, |d| {
                        d.child(Self::filter(self.include.clone()))
                            .child(Self::filter(self.exclude.clone()))
                    })
                    .child(
                        crate::open_projects::toggle(
                            "project-search-all-projects",
                            self.all_projects,
                            if self.all_projects {
                                self.roots.len()
                            } else {
                                0
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_all_projects(!this.all_projects, cx)
                        })),
                    ),
            )
            .child(
                div()
                    .min_h(px(32.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py(px(6.))
                    .text_size(px(11.))
                    .text_color(if self.error.is_some() {
                        rgb(RED_400).into()
                    } else {
                        content(0.45)
                    })
                    .when(self.loading, |d| {
                        d.child(spinner("project-search-spinner", "loader", 12.))
                    })
                    .child(div().min_w_0().flex_1().child(status)),
            )
            .child(
                div()
                    .id("project-search-results")
                    .debug_selector(|| "project-search-results".into())
                    .min_h_0()
                    .min_w_0()
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .children(name_rows)
                    .children(if self.mode == Mode::Content {
                        groups
                    } else {
                        Vec::new()
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn results(groups: usize, per_group: usize) -> Vec<Group> {
        (0..groups)
            .map(|g| Group {
                cwd: "/p".into(),
                project: None,
                relative: format!("src/module_{g}/file_{g}.py"),
                name: format!("file_{g}.py"),
                matches: (0..per_group)
                    .map(|m| Match {
                        source: SearchMatch {
                            path: format!("/p/src/module_{g}/file_{g}.py"),
                            relative: format!("src/module_{g}/file_{g}.py"),
                            line: (m + 1) as u32,
                            column: 1,
                            preview: format!(
                                "    hydra_call_{g}_{m}(argument, another_argument)  # note"
                            ),
                        },
                        highlight: Some(4..9),
                    })
                    .collect(),
            })
            .collect()
    }

    /// What the right panel does with the Explorer: a cached view that
    /// flexes inside a column, holding the search pane.
    struct Host(Entity<ProjectSearch>);
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            pane(&self.0)
        }
    }
    struct Column(Entity<Host>);
    impl Render for Column {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().h(px(40.)).flex_shrink_0())
                .child(
                    self.0
                        .clone()
                        .cached(gpui::StyleRefinement::default().flex_1().min_h_0().w_full()),
                )
        }
    }

    #[gpui::test]
    fn the_results_list_has_height_inside_the_panels_cached_hosts(cx: &mut TestAppContext) {
        cx.update(input::bind);
        let search = std::cell::RefCell::new(None);
        let window = cx.update(|cx| {
            let bounds = gpui::Bounds::new(point(px(0.), px(0.)), gpui::size(px(280.), px(900.)));
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| {
                    let s = cx.new(|cx| ProjectSearch::new("/p".into(), true, cx));
                    *search.borrow_mut() = Some(s.clone());
                    let host = cx.new(|_| Host(s));
                    cx.new(|_| Column(host))
                },
            )
            .unwrap()
        });
        let search = search.borrow().clone().unwrap();
        cx.update(|cx| {
            search.update(cx, |search, cx| {
                search.mode = Mode::Content;
                search.groups = results(24, 3);
                search.count = 72;
                cx.notify();
            })
        });
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let list = visual
            .debug_bounds("project-search-results")
            .expect("results list");
        let row = visual
            .debug_bounds("project-search-match-0-0")
            .expect("first match");
        assert!(
            list.size.height > px(300.),
            "the scroller must get the room the pane has: {list:?}"
        );
        assert!(row.size.height >= px(20.), "{row:?}");
    }

    fn project_file(relative: &str) -> ProjectFile {
        ProjectFile {
            name: relative.rsplit('/').next().unwrap().to_owned(),
            path: format!("/p/{relative}"),
            relative: relative.to_owned(),
        }
    }

    #[gpui::test]
    fn file_names_match_the_query_are_listed_first_and_open_the_file(cx: &mut TestAppContext) {
        cx.update(input::bind);
        let search = std::cell::RefCell::new(None);
        let window = cx.update(|cx| {
            let bounds = gpui::Bounds::new(point(px(0.), px(0.)), gpui::size(px(280.), px(900.)));
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| {
                    let s = cx.new(|cx| ProjectSearch::new("/p".into(), true, cx));
                    *search.borrow_mut() = Some(s.clone());
                    let host = cx.new(|_| Host(s));
                    cx.new(|_| Column(host))
                },
            )
            .unwrap()
        });
        let search = search.borrow().clone().unwrap();
        let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::<(String, u32, bool)>::new()));
        cx.update(|cx| {
            let opened = opened.clone();
            cx.subscribe(&search, move |_, event: &SearchEvent, _| {
                if let SearchEvent::Open { matched, pin, .. } = event {
                    opened
                        .borrow_mut()
                        .push((matched.relative.clone(), matched.line, *pin));
                }
            })
            .detach();
            search.update(cx, |search, cx| {
                search.files = Arc::new(vec![
                    project_file("src/inbox.rs"),
                    project_file("docs/notes.md"),
                    project_file("crates/app/src/project_search.rs"),
                    project_file("README.md"),
                ]);
                search
                    .query
                    .update(cx, |q, cx| q.set_text("projsearch".into(), cx));
                search.rank_names(cx);
                cx.notify();
            })
        });
        let names: Vec<String> = cx.update(|cx| {
            search
                .read(cx)
                .name_hits
                .iter()
                .map(|f| f.name.clone())
                .collect()
        });
        assert_eq!(
            names,
            ["project_search.rs"],
            "a fuzzy name match finds the file"
        );
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let row = visual
            .debug_bounds("project-search-file-0")
            .expect("a file row");
        assert!(
            row.size.height >= px(20.) && row.size.width > px(100.),
            "{row:?}"
        );
        cx.update(|cx| search.update(cx, |s, cx| s.pick_file(0, true, cx)));
        assert_eq!(
            opened.borrow().as_slice(),
            [("crates/app/src/project_search.rs".to_owned(), 1, true)]
        );
        // The Content tab shows content matches, not file names.
        assert_eq!(
            cx.update(|cx| search.read(cx).mode),
            Mode::Files,
            "Files is the default tab"
        );
        cx.update(|cx| {
            search.update(cx, |s, cx| {
                s.mode = Mode::Content;
                cx.notify();
            })
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("project-search-file-0").is_none());
        assert!(visual.debug_bounds("project-search-tab-files").is_some());
        assert!(visual.debug_bounds("project-search-tab-content").is_some());
        cx.update(|cx| {
            search.update(cx, |s, cx| {
                s.mode = Mode::Files;
                cx.notify();
            })
        });
        // Nothing typed, nothing listed.
        cx.update(|cx| {
            search.update(cx, |s, cx| {
                s.query.update(cx, |q, cx| q.set_text(String::new(), cx));
                s.rank_names(cx);
                assert!(s.name_hits.is_empty());
            })
        });
    }

    #[gpui::test]
    fn results_fill_the_panel_and_stay_inside_it(cx: &mut TestAppContext) {
        cx.update(input::bind);
        let window = cx.update(|cx| {
            let bounds = gpui::Bounds::new(point(px(0.), px(0.)), gpui::size(px(280.), px(900.)));
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| ProjectSearch::new("/p".into(), true, cx)),
            )
            .unwrap()
        });
        window
            .update(cx, |search, _, cx| {
                search.mode = Mode::Content;
                search.groups = results(24, 3);
                search.count = 72;
                cx.notify();
            })
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let viewport = window.update(visual, |_, w, _| w.viewport_size()).unwrap();
        let list = visual
            .debug_bounds("project-search-results")
            .expect("results list");
        let first = visual
            .debug_bounds("project-search-group-0")
            .expect("first group");
        let row = visual
            .debug_bounds("project-search-match-0-0")
            .expect("first match");
        assert!(list.size.height > px(100.), "the list has height: {list:?}");
        assert!(
            list.bottom() <= viewport.height,
            "{list:?} within {viewport:?}"
        );
        assert!(
            first.size.height > px(30.) && first.size.height < px(400.),
            "{first:?}"
        );
        assert!(
            row.size.height >= px(20.) && row.size.height < px(60.),
            "a match row is one or two lines: {row:?}"
        );
        assert!(
            row.top() >= list.top() && row.bottom() <= list.bottom(),
            "{row:?} inside {list:?}"
        );
        assert!(row.size.width > px(100.), "{row:?}");
    }

    #[gpui::test]
    fn all_open_projects_are_searched_and_open_in_their_own_project(cx: &mut TestAppContext) {
        cx.update(input::bind);
        let one = tempfile::Builder::new().prefix("alpha").tempdir().unwrap();
        let two = tempfile::Builder::new().prefix("beta").tempdir().unwrap();
        let path = |dir: &tempfile::TempDir| {
            dir.path()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        let (a, b) = (path(&one), path(&two));
        std::fs::write(one.path().join("first.rs"), "fn needle_here() {}\n").unwrap();
        std::fs::write(two.path().join("second.rs"), "// needle_here too\n").unwrap();
        cx.update(|cx| crate::open_projects::publish(&[a.clone(), b.clone()], cx));
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| ProjectSearch::new(a.clone(), true, cx))
            })
            .unwrap()
        });
        let search = window.root(cx).unwrap();
        let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        cx.update(|cx| {
            let opened = opened.clone();
            cx.subscribe(&search, move |_, event: &SearchEvent, _| {
                if let SearchEvent::Open { cwd, .. } = event {
                    opened.borrow_mut().push(cwd.clone());
                }
            })
            .detach();
            search.update(cx, |s, cx| {
                s.mode = Mode::Content;
                s.set_all_projects(true, cx);
                s.set_query("needle_here".into(), cx);
            });
        });
        let settle = |cx: &mut TestAppContext, done: &dyn Fn(&ProjectSearch) -> bool| {
            for _ in 0..300 {
                cx.executor().advance_clock(Duration::from_millis(250));
                cx.run_until_parked();
                if cx.update(|cx| done(search.read(cx))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("the search never finished");
        };
        settle(cx, &|s| !s.loading && s.groups.len() == 2);
        cx.update(|cx| {
            let s = search.read(cx);
            assert_eq!(s.roots, [a.clone(), b.clone()], "this project first");
            assert_eq!(s.count, 2);
            assert_eq!(s.groups[1].cwd, b);
            assert!(s.groups[1].project.is_some(), "results name their project");
        });
        cx.update(|cx| search.update(cx, |s, cx| s.pick(1, 0, false, cx)));
        assert_eq!(opened.borrow().last(), Some(&b));

        // File names: the project's name is part of the path matched.
        cx.update(|cx| {
            search.update(cx, |s, cx| {
                s.set_mode(Mode::Files, cx);
                s.set_query("second".into(), cx);
            })
        });
        settle(cx, &|s| !s.name_hits.is_empty());
        cx.update(|cx| search.update(cx, |s, cx| s.pick_file(0, false, cx)));
        assert_eq!(opened.borrow().last(), Some(&b));

        // Off again: only this checkout.
        cx.update(|cx| search.update(cx, |s, cx| s.set_all_projects(false, cx)));
        assert_eq!(cx.update(|cx| search.read(cx).roots.clone()), [a.clone()]);
    }
}
