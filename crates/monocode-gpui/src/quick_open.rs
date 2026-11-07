//! Cmd+P's project-file search and `>` command palette.
use crate::{input, theme::*};
use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Focusable, Role, Subscription, Window, div,
    prelude::*, px,
};
use monocode_core::{
    composer,
    files::ProjectFile,
    search::project::{self, SearchOptions},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// What typing searches: file names, or what is inside the files.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Mode {
    #[default]
    Files,
    Content,
}

impl Mode {
    fn other(self) -> Self {
        match self {
            Self::Files => Self::Content,
            Self::Content => Self::Files,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Command {
    New,
    Project,
    Model,
    Settings,
    Sidebar,
    Panel,
    Terminal,
    Layout,
    WorkspaceStyle,
    Plan,
    Operator,
    Orchestrator,
}
const COMMANDS: &[(Command, &str, &str)] = &[
    (Command::New, "New conversation", "⌘N"),
    (Command::Project, "Open project", "⌘O"),
    (Command::Model, "Switch model", "⌘."),
    (Command::Settings, "Open settings", "⌘,"),
    (Command::Sidebar, "Toggle sidebar", "⌘B"),
    (Command::Panel, "Toggle files and changes", "⌘⇧B"),
    (Command::Terminal, "Toggle terminal", "⌘J"),
    (Command::Layout, "Switch workspace layout", "⌘⇧L"),
    (
        Command::WorkspaceStyle,
        "Switch rounded or boxy workspace",
        "⌘⌥L",
    ),
    (Command::Plan, "Toggle Plan mode", ""),
    (Command::Operator, "Toggle Operator", ""),
    (Command::Orchestrator, "Toggle Orchestrator", ""),
];
/// Most content matches listed.
const CONTENT_ROWS: usize = 80;

pub(crate) enum Picked {
    File(String),
    /// A match inside a file: its path, line and column.
    Match(String, u32, u32),
    Command(Command),
    Dismiss,
}
#[derive(Clone)]
struct Row {
    label: String,
    detail: String,
    path: Option<String>,
    /// Where a content match is (line, column); `None` opens the file at its top.
    at: Option<(u32, u32)>,
    /// The file whose icon the row shows.
    file_name: String,
    command: Option<Command>,
}
pub(crate) struct QuickOpen {
    pub(crate) search: Entity<input::Input>,
    files: Vec<ProjectFile>,
    rows: Vec<Row>,
    active: usize,
    scroll: gpui::ScrollHandle,
    loading: bool,
    error: Option<String>,
    cancel: Arc<AtomicBool>,
    mode: Mode,
    /// Rows of the last content search, shown on the Content tab.
    content_rows: Vec<Row>,
    cwd: String,
    searching: bool,
    content_task: Option<gpui::Task<()>>,
    content_cancel: Arc<AtomicBool>,
    _changed: Subscription,
}
impl EventEmitter<Picked> for QuickOpen {}
impl Focusable for QuickOpen {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}
impl Drop for QuickOpen {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.content_cancel.store(true, Ordering::Relaxed);
    }
}
impl QuickOpen {
    pub(crate) fn new(cwd: String, palette: bool, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            input::Input::field(
                "Go to file or run a command",
                "Search files by name…  (> for commands)",
                content(0.35),
                cx,
            )
        });
        if palette {
            search.update(cx, |input, cx| input.set_text(">".into(), cx));
        }
        let changed = cx.subscribe(&search, |this, _, _: &input::Changed, cx| {
            this.active = 0;
            this.rank(cx);
            this.search_content(cx);
            cx.notify();
        });
        let cwd_for_search = cwd.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let include_ignored = crate::settings::show_excluded_files(cx);
        let work = cx.background_executor().spawn(async move {
            monocode_core::files::list_project_files_with(&cwd, Some(&flag), include_ignored)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(files) => this.files = files,
                    Err(e) => this.error = Some(e),
                };
                this.rank(cx);
                cx.notify();
            });
        })
        .detach();
        let mut this = Self {
            search,
            files: Vec::new(),
            rows: Vec::new(),
            active: 0,
            scroll: gpui::ScrollHandle::new(),
            loading: true,
            error: None,
            cancel,
            mode: Mode::Files,
            content_rows: Vec::new(),
            cwd: cwd_for_search,
            searching: false,
            content_task: None,
            content_cancel: Arc::new(AtomicBool::new(false)),
            _changed: changed,
        };
        this.rank(cx);
        this
    }
    fn rank(&mut self, cx: &gpui::App) {
        let query = self.search.read(cx).content.trim();
        self.rows = if let Some(query) = query.strip_prefix('>') {
            let mut commands: Vec<_> = COMMANDS
                .iter()
                .filter_map(|(command, label, hint)| {
                    composer::score(query.trim(), label).map(|score| {
                        (
                            score,
                            Row {
                                label: (*label).into(),
                                detail: if cfg!(target_os = "macos") {
                                    (*hint).into()
                                } else {
                                    hint.replace('⌘', "Ctrl+").replace('⇧', "Shift+")
                                },
                                path: None,
                                at: None,
                                file_name: String::new(),
                                command: Some(*command),
                            },
                        )
                    })
                })
                .collect();
            commands.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
            commands.into_iter().map(|(_, row)| row).collect()
        } else if self.mode == Mode::Content {
            self.content_rows.clone()
        } else {
            composer::rank_files(&self.files, query, 80)
                .into_iter()
                .map(|file| Row {
                    label: file.name.clone(),
                    detail: file.relative.clone(),
                    path: Some(file.path.clone()),
                    at: None,
                    file_name: file.name.clone(),
                    command: None,
                })
                .collect()
        };
        self.active = self.active.min(self.rows.len().saturating_sub(1));
        self.scroll.scroll_to_item(self.active);
    }
    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.active = 0;
            self.content_rows.clear();
            self.rank(cx);
            self.search_content(cx);
            cx.notify();
        }
    }

    /// On the Content tab, searches inside the files after a short pause.
    fn search_content(&mut self, cx: &mut Context<Self>) {
        self.content_cancel.store(true, Ordering::Relaxed);
        self.content_task = None;
        self.searching = false;
        let query = self.search.read(cx).content.trim().to_owned();
        if self.mode != Mode::Content || query.is_empty() || query.starts_with('>') {
            if !self.content_rows.is_empty() {
                self.content_rows.clear();
                self.rank(cx);
            }
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.content_cancel = cancel.clone();
        self.searching = true;
        let options = SearchOptions {
            cwd: self.cwd.clone(),
            query,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            include: None,
            exclude: None,
            search_id: String::new(),
            include_ignored: crate::settings::show_excluded_files(cx),
        };
        let root = self.cwd.clone();
        self.content_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let flag = cancel.clone();
            let found = cx
                .background_executor()
                .spawn(async move {
                    project::search_project(std::path::Path::new(&root), &options, &flag)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                this.searching = false;
                match found {
                    Ok(result) => {
                        this.error = None;
                        this.content_rows = result
                            .matches
                            .into_iter()
                            .take(CONTENT_ROWS)
                            .map(|m| Row {
                                label: m.preview.trim().to_owned(),
                                detail: format!("{}:{}", m.relative, m.line),
                                file_name: m
                                    .relative
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or(&m.relative)
                                    .to_owned(),
                                path: Some(m.path),
                                at: Some((m.line, m.column)),
                                command: None,
                            })
                            .collect();
                    }
                    Err(error) => {
                        this.content_rows.clear();
                        this.error = Some(error);
                    }
                }
                this.rank(cx);
                cx.notify();
            });
        }));
    }
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if !self.rows.is_empty() {
            self.active =
                (self.active as isize + delta).rem_euclid(self.rows.len() as isize) as usize;
            self.scroll.scroll_to_item(self.active);
            cx.notify();
        }
    }
    fn pick(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.search.read(cx).is_composing() {
            return;
        }
        if let Some(row) = self.rows.get(ix) {
            if let Some(path) = &row.path {
                match row.at {
                    Some((line, column)) => cx.emit(Picked::Match(path.clone(), line, column)),
                    None => cx.emit(Picked::File(path.clone())),
                }
            } else if let Some(command) = row.command {
                cx.emit(Picked::Command(command));
            }
        }
    }
}
impl Render for QuickOpen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .map(|(ix, row)| {
                div()
                    .id(("quick-open-row", ix))
                    .role(Role::ListItem)
                    .aria_label(row.detail.clone())
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .flex()
                    .gap_2p5()
                    .items_center()
                    .bg(if self.active == ix {
                        content(0.1)
                    } else {
                        content(0.)
                    })
                    .hover(|s| s.bg(content(0.07)))
                    .child(if row.path.is_some() {
                        crate::file_icons::file_type_icon(&row.file_name, false, false, 18.)
                    } else {
                        icon("sparkles", 16.).into_any_element()
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.))
                            .child(div().truncate().child(row.label.clone()))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(content(0.4))
                                    .truncate()
                                    .child(row.detail.clone()),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(ix, cx)))
                    .into_any_element()
            })
            .collect();
        div()
            .id("quick-open")
            .role(Role::Dialog)
            .aria_label("Go to file or run a command")
            .w_full()
            .flex()
            .flex_col()
            .capture_action(cx.listener(|this, _: &input::Up, _, cx| {
                this.step(-1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &input::Down, _, cx| {
                this.step(1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &input::Submit, _, cx| {
                this.pick(this.active, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|_, _: &crate::shell::DismissMenu, _, cx| {
                cx.emit(Picked::Dismiss);
                cx.stop_propagation();
            }))
            // Tab is the shell's focus-next action; here it flips the tab.
            .on_action(cx.listener(|this, _: &crate::shell::FocusNext, _, cx| {
                this.set_mode(this.mode.other(), cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &crate::shell::FocusPrevious, _, cx| {
                this.set_mode(this.mode.other(), cx);
                cx.stop_propagation();
            }))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(content(0.08))
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .child(icon("search", 16.).text_color(content(0.5)))
                    .child(div().flex_1().min_w_0().child(self.search.clone())),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(content(0.06))
                    .children(
                        [
                            ("quick-open-tab-files", "Files", Mode::Files),
                            ("quick-open-tab-content", "Content", Mode::Content),
                        ]
                        .into_iter()
                        .map(|(id, label, target)| {
                            let on = self.mode == target;
                            div()
                                .id(id)
                                .debug_selector(move || id.to_owned())
                                .role(Role::Tab)
                                .aria_label(label)
                                .aria_selected(on)
                                .h(px(26.))
                                .px(px(12.))
                                .rounded_md()
                                .flex()
                                .items_center()
                                .cursor_pointer()
                                .text_size(px(12.))
                                .text_color(content(if on { 1. } else { 0.5 }))
                                .when(on, |d| d.bg(content(0.1)))
                                .hover(|s| s.bg(content(0.07)))
                                .child(label)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.set_mode(target, cx)),
                                )
                        }),
                    ),
            )
            .child(
                div()
                    .id("quick-open-results")
                    .role(Role::ListBox)
                    .aria_label("Results")
                    .p_1p5()
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows)
                    .when(self.rows.is_empty(), |d| {
                        d.child(
                            div()
                                .px_3()
                                .py_6()
                                .text_size(px(13.))
                                .text_color(content(0.45))
                                .child(if let Some(error) = self.error.clone() {
                                    error
                                } else if self.mode == Mode::Content {
                                    if self.searching {
                                        "Searching…".to_owned()
                                    } else if self.search.read(cx).content.trim().is_empty() {
                                        "Type to search inside the files".to_owned()
                                    } else {
                                        "No results".to_owned()
                                    }
                                } else if self.loading {
                                    "Loading project files…".to_owned()
                                } else {
                                    "No results".to_owned()
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(content(0.07))
                    .text_size(px(10.))
                    .text_color(content(0.35))
                    .child(
                        "↑ ↓ navigate    ↵ open    ⇥ files / content    Esc close    > commands",
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir.path())
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "--quiet"]);
        std::fs::write(
            dir.path().join("alpha.txt"),
            "first line\nthe needle is here\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("needle_notes.md"), "nothing\n").unwrap();
        git(&["add", "."]);
        dir
    }

    #[gpui::test]
    fn files_is_the_first_tab_and_content_is_the_second(cx: &mut TestAppContext) {
        cx.update(input::bind);
        let dir = repo();
        let cwd = dir.path().to_string_lossy().to_string();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| QuickOpen::new(cwd, false, cx))
            })
            .unwrap()
        });
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
        window
            .update(cx, |open, _, cx| {
                assert_eq!(open.mode, Mode::Files, "file names are searched first");
                open.search
                    .update(cx, |input, cx| input.set_text("needle".into(), cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |open, _, _| {
                let names: Vec<&str> = open.rows.iter().map(|r| r.label.as_str()).collect();
                assert_eq!(
                    names,
                    ["needle_notes.md"],
                    "the Files tab matches names, not contents"
                );
                assert!(open.rows[0].at.is_none());
            })
            .unwrap();
        window
            .update(cx, |open, _, cx| open.set_mode(Mode::Content, cx))
            .unwrap();
        cx.executor().advance_clock(Duration::from_millis(400));
        cx.run_until_parked();
        window
            .update(cx, |open, _, cx| {
                assert_eq!(open.mode, Mode::Content);
                let found: Vec<(&str, Option<(u32, u32)>)> = open
                    .rows
                    .iter()
                    .map(|r| (r.detail.as_str(), r.at))
                    .collect();
                assert_eq!(found, [("alpha.txt:2", Some((2, 5)))], "{found:?}");
                assert_eq!(open.rows[0].label, "the needle is here");
                // Back to Files: names again, no stale content rows.
                open.set_mode(Mode::Files, cx);
                assert_eq!(open.rows[0].label, "needle_notes.md");
            })
            .unwrap();
    }
}
