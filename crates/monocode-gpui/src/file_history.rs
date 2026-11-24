//! One file's history: its versions down the left, and what the chosen
//! version changed beside them, the version before on the left of the diff
//! and the version itself on the right. The same window lists the commits
//! that touched some lines, and compares two texts with no list at all.
use crate::{
    dialog_resize::{self, DialogResize},
    diff_view::{DiffFile, DiffView},
    modal,
    right_panel::stroke,
    theme::*,
};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, KeyDownEvent,
    Role, SharedString, Task, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use monocode_core::{
    diff,
    scm::{self, FileVersion},
};
use std::{path::PathBuf, sync::Arc};

/// More than anyone reads through; the list says when it stopped there.
const LIMIT: usize = 500;
const LIST_WIDTH: f32 = 300.;

pub(crate) enum Event {
    Close,
}

/// What a history window shows instead of all of a file's versions.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Request {
    /// The commits that touched lines `first..=last` (one-based, of `text`
    /// when given, else of the committed file).
    Lines {
        first: usize,
        last: usize,
        text: Option<String>,
    },
    /// The file's versions, with this commit's chosen.
    Commit(String),
    /// `left` against `right`, read-only, under `title`.
    Compare {
        title: String,
        left: String,
        right: String,
    },
}

/// What one window is showing.
enum Mode {
    File,
    Lines {
        first: usize,
        last: usize,
        text: Option<String>,
    },
    Compare {
        left: String,
        right: String,
    },
}

pub(crate) struct FileHistoryDialog {
    cwd: String,
    /// The file as it was asked for, absolute.
    path: String,
    label: String,
    mode: Mode,
    title: SharedString,
    /// The commit to choose once the versions are in.
    choose: Option<String>,
    versions: Vec<FileVersion>,
    loading: bool,
    error: Option<String>,
    selected: usize,
    diff_view: Entity<DiffView>,
    diff_request: u64,
    _history_task: Option<Task<()>>,
    _diff_task: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    size: DialogResize,
    /// The diff last put beside the list (label, added, removed), for tests.
    #[cfg(test)]
    shown: Option<(String, u32, u32)>,
}
impl EventEmitter<Event> for FileHistoryDialog {}
impl dialog_resize::Resizable for FileHistoryDialog {
    fn dialog_resize(&mut self) -> &mut DialogResize {
        &mut self.size
    }
}
impl Focusable for FileHistoryDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The size the user last dragged the window to; the next one opens that big.
static REMEMBERED_SIZE: dialog_resize::Remembered = dialog_resize::remembered();

impl FileHistoryDialog {
    pub(crate) fn new(
        cwd: String,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_request(cwd, path, None, window, cx)
    }
    pub(crate) fn with_request(
        cwd: String,
        path: String,
        request: Option<Request>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let diff_view = cx.new(DiffView::new);
        diff_view.update(cx, |view, cx| {
            view.allow_split = true;
            view.set_split(true, cx);
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let file = monocode_core::files::display_path(&path, Some(&cwd));
        let (mode, title, label, choose) = match request {
            None => (Mode::File, "History", file, None),
            Some(Request::Commit(sha)) => (Mode::File, "History", file, Some(sha)),
            Some(Request::Lines { first, last, text }) => {
                let lines = match first == last {
                    true => format!("{file}: line {first}"),
                    false => format!("{file}: lines {first}–{last}"),
                };
                (Mode::Lines { first, last, text }, "History", lines, None)
            }
            Some(Request::Compare { title, left, right }) => {
                (Mode::Compare { left, right }, "Compare", title, None)
            }
        };
        let mut this = Self {
            cwd,
            path,
            label,
            mode,
            title: title.into(),
            choose,
            versions: Vec::new(),
            loading: true,
            error: None,
            selected: 0,
            diff_view,
            diff_request: 0,
            _history_task: None,
            _diff_task: None,
            scroll: UniformListScrollHandle::new(),
            focus,
            size: DialogResize::new(&REMEMBERED_SIZE),
            #[cfg(test)]
            shown: None,
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let (cwd, path) = (PathBuf::from(&self.cwd), self.path.clone());
        let work = match &self.mode {
            Mode::Compare { left, right } => {
                self.loading = false;
                return self.load_comparison(left.clone(), right.clone(), cx);
            }
            Mode::File => cx
                .background_executor()
                .spawn(async move { scm::file_history(&cwd, &path, LIMIT) }),
            Mode::Lines { first, last, text } => {
                let (first, last, text) = (*first, *last, text.clone());
                cx.background_executor().spawn(async move {
                    let path = monocode_core::files::document::resolve_path(
                        std::path::Path::new(&path),
                        &cwd,
                    );
                    scm::line_history(&path, first, last, text.as_deref(), LIMIT)
                })
            }
        };
        self._history_task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(versions) => this.versions = versions,
                    Err(error) => this.error = Some(error),
                }
                let chosen = this.choose.take().and_then(|sha| {
                    this.versions
                        .iter()
                        .position(|v| !v.sha.is_empty() && v.sha.starts_with(&sha))
                });
                this.selected = chosen.unwrap_or(0);
                if chosen.is_some() {
                    this.scroll
                        .scroll_to_item(this.selected, gpui::ScrollStrategy::Center);
                }
                this.load_diff(cx);
                cx.notify();
            });
        }));
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.versions.len() || ix == self.selected {
            return;
        }
        self.selected = ix;
        self.scroll
            .scroll_to_item(ix, gpui::ScrollStrategy::Nearest);
        self.load_diff(cx);
        cx.notify();
    }

    /// What the chosen version changed, in the two panes beside the list.
    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let Some(version) = self.versions.get(self.selected).cloned() else {
            self.diff_view
                .update(cx, |view, cx| view.set_files(vec![], cx));
            return;
        };
        self.diff_request += 1;
        let request = self.diff_request;
        let cwd = PathBuf::from(&self.cwd);
        let id: SharedString = format!("history-{}", self.selected).into();
        let label = match &version.previous_path {
            Some(before) => format!("{before} → {}", version.path),
            None => version.path.clone(),
        };
        let (path, renamed) = (version.path.clone(), version.previous_path.is_some());
        let file = move |message: Option<String>| DiffFile {
            id: id.clone(),
            path: path.clone(),
            label: label.clone(),
            binary: false,
            too_large: false,
            empty_message: message,
            additions: 0,
            deletions: 0,
            blocks: Arc::new(Vec::new()),
            can_stage: false,
            can_discard: false,
            can_stage_hunk: false,
        };
        self.diff_view.update(cx, |view, cx| {
            view.set_files(vec![file(Some("Loading…".into()))], cx)
        });
        let read = version.clone();
        let work = cx.background_executor().spawn(async move {
            scm::file_version_diff(&cwd, &read).map(|file| {
                let unified = (!file.binary && !file.too_large).then(|| {
                    diff::build_unified_file(&file.original, &file.current, diff::CONTEXT_DEFAULT)
                });
                (file, unified)
            })
        });
        self._diff_task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.diff_request != request {
                    return;
                }
                let shown = match result {
                    Ok((loaded, unified)) => {
                        let (additions, deletions, blocks) = unified
                            .map(|u| (u.additions, u.deletions, u.blocks))
                            .unwrap_or_default();
                        // A rename that changed nothing else has no lines to show.
                        let unchanged = !loaded.binary && !loaded.too_large && blocks.is_empty();
                        DiffFile {
                            binary: loaded.binary,
                            too_large: loaded.too_large,
                            additions,
                            deletions,
                            blocks: Arc::new(blocks),
                            ..file(unchanged.then(|| match renamed {
                                true => "Renamed; the contents are the same.".to_owned(),
                                false => "The contents are the same.".to_owned(),
                            }))
                        }
                    }
                    Err(error) => file(Some(format!("Couldn’t load diff: {error}"))),
                };
                #[cfg(test)]
                {
                    this.shown = Some((shown.label.clone(), shown.additions, shown.deletions));
                }
                let id = shown.id.clone();
                this.diff_view.update(cx, |view, cx| {
                    view.set_files(vec![shown], cx);
                    view.reveal_file(id, cx);
                });
                cx.notify();
            });
        }));
    }

    /// Two texts, the left one before the right: no versions to choose.
    fn load_comparison(&mut self, left: String, right: String, cx: &mut Context<Self>) {
        let (path, label) = (self.path.clone(), self.label.clone());
        let work = cx
            .background_executor()
            .spawn(async move { diff::build_unified_file(&left, &right, diff::CONTEXT_DEFAULT) });
        self._diff_task = Some(cx.spawn(async move |this, cx| {
            let unified = work.await;
            let _ = this.update(cx, |this, cx| {
                let shown = DiffFile {
                    id: "compare".into(),
                    path,
                    label,
                    binary: false,
                    too_large: false,
                    empty_message: unified
                        .blocks
                        .is_empty()
                        .then(|| "The contents are the same.".to_owned()),
                    additions: unified.additions,
                    deletions: unified.deletions,
                    blocks: Arc::new(unified.blocks),
                    can_stage: false,
                    can_discard: false,
                    can_stage_hunk: false,
                };
                #[cfg(test)]
                {
                    this.shown = Some((shown.label.clone(), shown.additions, shown.deletions));
                }
                this.diff_view
                    .update(cx, |view, cx| view.set_files(vec![shown], cx));
                cx.notify();
            });
        }));
    }

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let last = self.versions.len().saturating_sub(1);
        let next = match event.keystroke.key.as_str() {
            "escape" => {
                cx.emit(Event::Close);
                cx.stop_propagation();
                return;
            }
            "down" | "j" => (self.selected + 1).min(last),
            "up" | "k" => self.selected.saturating_sub(1),
            "home" => 0,
            "end" => last,
            _ => return,
        };
        cx.stop_propagation();
        self.select(next, cx);
    }

    fn row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let version = &self.versions[ix];
        let selected = ix == self.selected;
        let uncommitted = version.sha.is_empty();
        let when =
            monocode_core::time::relative(version.time * 1000, monocode_core::time::now_ms());
        let detail = if uncommitted {
            "On disk, not committed".to_owned()
        } else {
            [
                version.short_sha.as_str(),
                version.author.as_str(),
                when.as_str(),
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
        };
        let mark = match version.status.as_str() {
            "added" => Some("Added"),
            "renamed" => Some("Renamed"),
            "deleted" => Some("Deleted"),
            _ => None,
        };
        div()
            .id(("file-history-row", ix))
            .debug_selector(move || format!("file-history-row-{ix}"))
            .role(Role::ListItem)
            .aria_label(SharedString::from(version.subject.clone()))
            .aria_selected(selected)
            .h(px(ROW_HEIGHT))
            .mx_1()
            .px_2()
            .rounded(px(6.))
            .flex()
            .flex_col()
            .justify_center()
            .cursor_pointer()
            .when(selected, |d| d.bg(content(0.1)))
            .when(!selected, |d| d.hover(|s| s.bg(content(0.05))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(12.5))
                            .line_height(px(17.))
                            .when(uncommitted, |d| d.italic())
                            .text_color(content(if selected { 1. } else { 0.85 }))
                            .child(version.subject.clone()),
                    )
                    .when_some(mark, |d, mark| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(10.))
                                .text_color(content(0.5))
                                .child(mark),
                        )
                    }),
            )
            .child(
                div()
                    .truncate()
                    .text_size(px(11.))
                    .line_height(px(15.))
                    .text_color(content(0.45))
                    .child(detail),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.select(ix, cx)))
            .into_any_element()
    }
}

const ROW_HEIGHT: f32 = 44.;

impl Render for FileHistoryDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (width, height) = self.size.size(window.viewport_size());
        let list_width = self.size.side_width(width, LIST_WIDTH);
        let entity = cx.entity();
        let count = self.versions.len();
        let note = |text: String| {
            div()
                .flex_1()
                .p_4()
                .text_size(px(12.))
                .text_color(content(0.45))
                .child(text)
                .into_any_element()
        };
        let list = if self.loading {
            note("Loading…".into())
        } else if let Some(error) = &self.error {
            note(error.clone())
        } else if count == 0 && matches!(self.mode, Mode::Lines { .. }) {
            note("Git has no history for these lines.".into())
        } else if count == 0 {
            note("Git has no history for this file.".into())
        } else {
            uniform_list("file-history-rows", count, move |range, _, cx| {
                entity.update(cx, |this, cx| {
                    range.map(|ix| this.row(ix, cx)).collect::<Vec<_>>()
                })
            })
            .track_scroll(&self.scroll)
            .py_1()
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        let comparing = matches!(self.mode, Mode::Compare { .. });
        let summary = match count {
            _ if self.loading || self.error.is_some() || comparing => String::new(),
            0 => String::new(),
            1 => "1 version".into(),
            n if n >= LIMIT => format!("the newest {n} versions"),
            n => format!("{n} versions"),
        };
        let card = modal::card()
            .debug_selector(|| "file-history-card".into())
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .id("file-history-header")
                    .on_click(cx.listener(dialog_resize::header_click))
                    .relative()
                    .h(px(44.))
                    .flex_shrink_0()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(stroke())
                    .child(modal::title(self.title.clone()))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(content(0.7))
                            .child(self.label.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .text_color(content(0.45))
                            .child(summary),
                    )
                    .child(div().flex_1())
                    .child(dialog_resize::maximize_button(
                        "file-history-maximize",
                        self.size.maximized(),
                        cx,
                    ))
                    .child(
                        icon_button("file-history-close", "Close", "close")
                            .size(px(28.))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(!comparing, |d| {
                        d.child(
                            div()
                                .id("file-history-versions")
                                .debug_selector(|| "file-history-versions".into())
                                .role(Role::List)
                                .aria_label("Versions")
                                .w(px(list_width))
                                .flex_shrink_0()
                                .h_full()
                                .flex()
                                .flex_col()
                                .border_r_1()
                                .border_color(stroke())
                                .child(list),
                        )
                        .child(dialog_resize::side_handle(
                            "file-history-split",
                            list_width,
                            width,
                            cx,
                        ))
                    })
                    .child(
                        div()
                            .debug_selector(|| "file-history-diff".into())
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.diff_view.clone()),
                    ),
            )
            .child(dialog_resize::corner_grip(
                "file-history-resize",
                (width, height),
                cx,
            ));
        div()
            .size_full()
            .child(dialog_resize::drag_listener(cx))
            .track_focus(&self.focus)
            .key_context("FileHistoryDialog")
            .on_key_down(cx.listener(Self::key))
            .child(modal::overlay("file-history-dialog", card, {
                let weak = cx.weak_entity();
                move |_, cx| {
                    let _ = weak.update(cx, |_, cx| cx.emit(Event::Close));
                }
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    fn git(dir: &std::path::Path, args: &[&str]) {
        let done = std::process::Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
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

    #[gpui::test]
    fn a_files_versions_are_listed_and_each_shows_what_it_changed(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        git(dir, &["init"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-m", "first version"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-m", "second version"]);
        git(dir, &["mv", "a.txt", "b.txt"]);
        git(dir, &["commit", "-m", "renamed"]);
        std::fs::write(dir.join("b.txt"), "one\ntwo\n").unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let path = format!("{cwd}/b.txt");

        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| FileHistoryDialog::new(cwd.clone(), path.clone(), window, cx))
            })
            .unwrap()
        });
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let state = |visual: &mut VisualTestContext| {
            window
                .update(visual, |dialog, _, _| {
                    (dialog.selected, dialog.shown.clone())
                })
                .unwrap()
        };
        window
            .update(visual, |dialog, _, _| {
                let subjects: Vec<_> = dialog.versions.iter().map(|v| v.subject.as_str()).collect();
                assert_eq!(
                    subjects,
                    [
                        "Uncommitted changes",
                        "renamed",
                        "second version",
                        "first version"
                    ]
                );
                assert_eq!(dialog.label, "b.txt");
            })
            .unwrap();
        // The newest is shown first: what is on disk against HEAD.
        assert_eq!(state(visual), (0, Some(("b.txt".into(), 0, 1))));
        assert!(visual.debug_bounds("file-history-versions").is_some());
        assert!(visual.debug_bounds("file-history-diff").is_some());

        // A click on a version shows what that commit did to the file.
        let row = visual.debug_bounds("file-history-row-2").unwrap();
        visual.simulate_click(row.center(), gpui::Modifiers::none());
        visual.run_until_parked();
        assert_eq!(state(visual), (2, Some(("a.txt".into(), 2, 0))));
        // The keyboard walks the list; the rename names both paths.
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        assert_eq!(state(visual), (1, Some(("a.txt → b.txt".into(), 0, 0))));
        visual.simulate_keystrokes("end");
        visual.run_until_parked();
        assert_eq!(state(visual), (3, Some(("a.txt".into(), 1, 0))));
        visual.simulate_keystrokes("down");
        visual.run_until_parked();
        assert_eq!(
            state(visual).0,
            3,
            "the last version is the end of the list"
        );

        // The line beside the list drags left and right, within limits.
        let list = |visual: &mut VisualTestContext| {
            let bounds = visual.debug_bounds("file-history-versions").unwrap();
            bounds.size.width.as_f32()
        };
        let drag = |visual: &mut VisualTestContext, by: f32| {
            let from = visual.debug_bounds("file-history-split").unwrap().center();
            let to = gpui::point(from.x + px(by), from.y);
            visual.simulate_mouse_down(from, gpui::MouseButton::Left, gpui::Modifiers::none());
            visual.simulate_mouse_move(to, Some(gpui::MouseButton::Left), gpui::Modifiers::none());
            visual.simulate_mouse_up(to, gpui::MouseButton::Left, gpui::Modifiers::none());
            visual.run_until_parked();
        };
        assert_eq!(list(visual), LIST_WIDTH);
        drag(visual, 90.);
        assert_eq!(list(visual), LIST_WIDTH + 90.);
        drag(visual, -2000.);
        assert_eq!(list(visual), 160., "the list keeps a least width");
        drag(visual, 4000.);
        let card = visual.debug_bounds("file-history-card").unwrap();
        assert_eq!(
            list(visual),
            card.size.width.as_f32() - 240.,
            "and leaves the diff room"
        );
        // Escape asks to close.
        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = closed.clone();
        let _events = window
            .update(visual, |_, _, cx| {
                cx.subscribe(&cx.entity(), move |_, _, _: &Event, _| seen.set(true))
            })
            .unwrap();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(closed.get());
        // The next window opens with the list as wide as it was left.
        let again = visual.update(|window, cx| {
            cx.new(|cx| FileHistoryDialog::new("/tmp".into(), "/tmp/x".into(), window, cx))
        });
        let remembered = visual.update(|_, cx| {
            again.update(cx, |dialog, _| {
                dialog.size.side_width(card.size.width.as_f32(), LIST_WIDTH)
            })
        });
        assert_eq!(remembered, list(visual));
    }

    #[gpui::test]
    fn a_window_lists_one_lines_commits_starts_at_a_commit_or_compares_two_texts(
        cx: &mut TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        git(dir, &["init"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-m", "first"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(dir, &["commit", "-am", "second"]);
        let cwd = dir.to_string_lossy().into_owned();
        let path = format!("{cwd}/a.txt");
        let open = |cx: &mut TestAppContext, asked: Request| {
            let (cwd, path) = (cwd.clone(), path.clone());
            let window = cx.update(|cx| {
                cx.open_window(Default::default(), |window, cx| {
                    cx.new(|cx| FileHistoryDialog::with_request(cwd, path, Some(asked), window, cx))
                })
                .unwrap()
            });
            cx.run_until_parked();
            window
                .update(cx, |dialog, _, _| {
                    let subjects: Vec<String> =
                        dialog.versions.iter().map(|v| v.subject.clone()).collect();
                    (
                        dialog.title.to_string(),
                        dialog.label.clone(),
                        subjects,
                        dialog.selected,
                        dialog.shown.clone(),
                    )
                })
                .unwrap()
        };
        let lines = Request::Lines {
            first: 2,
            last: 2,
            text: None,
        };
        let (title, label, subjects, ..) = open(cx, lines);
        assert_eq!(
            (title.as_str(), label.as_str()),
            ("History", "a.txt: line 2")
        );
        assert_eq!(subjects, ["second"]);
        // A commit's request starts on that commit.
        let first = monocode_core::scm::file_history(dir, &path, 10).unwrap()[1]
            .sha
            .clone();
        let (.., subjects, selected, shown) = open(cx, Request::Commit(first[..7].into()));
        assert_eq!((subjects.len(), selected), (2, 1));
        assert_eq!(shown, Some(("a.txt".into(), 1, 0)));
        // Two texts compare with no list.
        let compare = Request::Compare {
            title: "Clipboard ↔ a.txt".into(),
            left: "one\n".into(),
            right: "one\ntwo\n".into(),
        };
        let (title, label, subjects, _, shown) = open(cx, compare);
        assert_eq!(
            (title.as_str(), label.as_str()),
            ("Compare", "Clipboard ↔ a.txt")
        );
        assert!(subjects.is_empty());
        assert_eq!(shown, Some(("Clipboard ↔ a.txt".into(), 1, 0)));
    }
}
