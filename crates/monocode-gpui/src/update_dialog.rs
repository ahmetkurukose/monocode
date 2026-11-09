//! Update Project: fetch, merge (or rebase onto) the upstream, then show what
//! came in — the commits, the files they changed as a tree, and the selected
//! file's diff. A conflict stops the update and offers to abort it.
use crate::{
    change_tree::{Node, TreeRow, basename},
    diff_view::{DiffFile, DiffView, diff_counts},
    file_icons::file_type_icon,
    git_status::ProjectGit,
    modal,
    right_panel::{DIFF_ADD, DIFF_DEL, spinner, stroke},
    theme::*,
};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, KeyDownEvent,
    SharedString, Task, Window, canvas, div, prelude::*, px, rgb, uniform_list,
};
use monocode_core::{
    diff,
    scm::{self, FileDiff, UpdateMethod, UpdateReport},
};
use std::{collections::HashSet, path::PathBuf, sync::Arc};

const ROW: f32 = 26.;
const MIN_SIZE: (f32, f32) = (480., 360.);
/// Room kept around a dialog that fills the window.
const MARGIN: f32 = 12.;
/// The size the user last dragged a dialog to; the next one opens that big.
static REMEMBERED_SIZE: std::sync::Mutex<Option<(f32, f32)>> = std::sync::Mutex::new(None);

/// A corner drag in progress: where it started and the size it started from.
struct Resize {
    start: gpui::Point<gpui::Pixels>,
    from: (f32, f32),
}

pub(crate) enum UpdateDialogEvent {
    Close,
    /// The push went through.
    Pushed,
}

/// What the dialog runs.
#[derive(Clone)]
pub(crate) enum Source {
    /// Fetch, then bring the upstream in.
    Update(UpdateMethod),
    /// Merge another branch into the current one.
    Merge(String),
    /// Rebase the current branch onto another one.
    RebaseOnto(String),
    /// Show what a push would send, then push on request. `warn` is shown
    /// beside the Push button (pushing the default branch, say).
    Push { warn: Option<String> },
}

enum Phase {
    Running,
    Done(Box<UpdateReport>),
    Failed(String),
}

pub(crate) struct UpdateDialog {
    git: Entity<ProjectGit>,
    cwd: String,
    source: Source,
    phase: Phase,
    rows: Vec<TreeRow>,
    collapsed: HashSet<String>,
    selected: Option<String>,
    diff_view: Entity<DiffView>,
    diff_request: u64,
    _tasks: Vec<Task<()>>,
    aborting: bool,
    pushing: bool,
    error: Option<String>,
    focus: FocusHandle,
    /// Fills the window; toggled from the header or by double-clicking it.
    maximized: bool,
    /// A size chosen by dragging the corner.
    custom: Option<(f32, f32)>,
    resizing: Option<Resize>,
}
impl EventEmitter<UpdateDialogEvent> for UpdateDialog {}
impl Focusable for UpdateDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

pub(crate) fn age(timestamp: i64) -> String {
    let now = monocode_core::time::now_ms() as i64 / 1000;
    let seconds = (now - timestamp).max(0);
    match seconds {
        0..60 => "now".into(),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        86400..2_592_000 => format!("{}d", seconds / 86400),
        _ => chrono::DateTime::from_timestamp(timestamp, 0)
            .map(|date| date.format("%b %-d").to_string())
            .unwrap_or_default(),
    }
}

impl UpdateDialog {
    pub(crate) fn new(
        git: Entity<ProjectGit>,
        cwd: String,
        source: Source,
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
        let mut this = Self {
            git,
            cwd,
            source,
            phase: Phase::Running,
            rows: vec![],
            collapsed: HashSet::new(),
            selected: None,
            diff_view,
            diff_request: 0,
            _tasks: vec![],
            aborting: false,
            pushing: false,
            error: None,
            focus,
            maximized: false,
            custom: *REMEMBERED_SIZE.lock().unwrap(),
            resizing: None,
        };
        this.run(cx);
        this
    }

    fn toggle_maximized(&mut self, cx: &mut Context<Self>) {
        self.maximized = !self.maximized;
        cx.notify();
    }

    fn drag_resize(&mut self, event: &gpui::MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(resize) = &self.resizing else {
            return;
        };
        if event.pressed_button != Some(gpui::MouseButton::Left) {
            self.finish_resize(cx);
            return;
        }
        let moved = event.position - resize.start;
        self.custom = Some((
            resize.from.0 + 2. * moved.x.as_f32(),
            resize.from.1 + 2. * moved.y.as_f32(),
        ));
        cx.notify();
    }

    fn finish_resize(&mut self, cx: &mut Context<Self>) {
        if self.resizing.take().is_some() {
            *REMEMBERED_SIZE.lock().unwrap() = self.custom;
            cx.notify();
        }
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let cwd = PathBuf::from(&self.cwd);
        let source = self.source.clone();
        let work = cx.background_executor().spawn(async move {
            match source {
                Source::Update(method) => scm::update_project(&cwd, method),
                Source::Merge(branch) => scm::merge_branch(&cwd, &branch),
                Source::RebaseOnto(branch) => scm::rebase_branch(&cwd, &branch),
                Source::Push { .. } => scm::outgoing(&cwd),
            }
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.git.update(cx, |git, cx| git.changed(cx));
                match result {
                    Ok(report) => {
                        this.selected = report.files.first().map(|f| f.relative.clone());
                        this.phase = Phase::Done(Box::new(report));
                        this.rebuild_rows();
                        this.load_diff(cx);
                    }
                    Err(error) => this.phase = Phase::Failed(error),
                }
                cx.notify();
            });
        }));
    }

    fn report(&self) -> Option<&UpdateReport> {
        match &self.phase {
            Phase::Done(report) => Some(report),
            _ => None,
        }
    }
    fn rebuild_rows(&mut self) {
        let Some(report) = self.report() else {
            return;
        };
        let mut root = Node::default();
        for (ix, file) in report.files.iter().enumerate() {
            root.insert(&file.relative.split('/').collect::<Vec<_>>(), ix);
        }
        let mut rows = Vec::new();
        root.flatten(&report.files, "update", "", 0, &self.collapsed, &mut rows);
        self.rows = rows;
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(path) = self
            .report()
            .and_then(|r| r.files.get(ix))
            .map(|f| f.relative.clone())
        else {
            return;
        };
        if self.selected.as_ref() == Some(&path) {
            return;
        }
        self.selected = Some(path);
        self.load_diff(cx);
        cx.notify();
    }

    /// The selected file as it was before the update and as it is now.
    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let (Some(relative), Some(report)) = (self.selected.clone(), self.report()) else {
            self.diff_view
                .update(cx, |view, cx| view.set_files(vec![], cx));
            return;
        };
        let (old, new) = (report.old.clone(), report.new.clone());
        self.diff_request += 1;
        let request = self.diff_request;
        let cwd = PathBuf::from(&self.cwd);
        let id: SharedString = format!("update-{relative}").into();
        self.diff_view.update(cx, |view, cx| {
            view.set_files(vec![model(&id, &relative, None)], cx)
        });
        let path = relative.clone();
        let work = cx.background_executor().spawn(async move {
            scm::range_file_diff(&cwd, &old, &new, &path).map(|file| {
                let unified = (!file.binary && !file.too_large).then(|| {
                    diff::build_unified_file(&file.original, &file.current, diff::CONTEXT_DEFAULT)
                });
                (file, unified)
            })
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.diff_request != request {
                    return;
                }
                let file = match result {
                    Ok(loaded) => model(&id, &relative, Some(loaded)),
                    Err(error) => DiffFile {
                        empty_message: Some(format!("Couldn’t load diff: {error}")),
                        ..model(&id, &relative, None)
                    },
                };
                this.diff_view.update(cx, |view, cx| {
                    view.set_files(vec![file], cx);
                    view.reveal_file(id.clone(), cx);
                });
                cx.notify();
            });
        }));
    }

    /// Sends the commits listed, then closes.
    fn push_now(&mut self, cx: &mut Context<Self>) {
        if self.pushing || !matches!(self.phase, Phase::Done(_)) {
            return;
        }
        self.pushing = true;
        self.error = None;
        cx.notify();
        let cwd = PathBuf::from(&self.cwd);
        let work = cx
            .background_executor()
            .spawn(async move { scm::push(&cwd) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.pushing = false;
                this.git.update(cx, |git, cx| git.changed(cx));
                match result {
                    Ok(()) => cx.emit(UpdateDialogEvent::Pushed),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        }));
    }

    fn abort(&mut self, cx: &mut Context<Self>) {
        if self.aborting {
            return;
        }
        self.aborting = true;
        self.error = None;
        let cwd = PathBuf::from(&self.cwd);
        let work = cx
            .background_executor()
            .spawn(async move { scm::abort_update(&cwd) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.aborting = false;
                this.git.update(cx, |git, cx| git.changed(cx));
                match result {
                    Ok(()) => cx.emit(UpdateDialogEvent::Close),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && !matches!(self.phase, Phase::Running) && !self.pushing
        {
            cx.emit(UpdateDialogEvent::Close);
            cx.stop_propagation();
        }
    }

    fn file_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().h(px(ROW)).into_any_element();
        };
        let base = div()
            .id(("update-row", ix))
            .h(px(ROW))
            .mx(px(6.))
            .px(px(6.))
            .rounded(px(5.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .cursor_pointer();
        match row {
            TreeRow::Dir {
                key,
                name,
                depth,
                open,
                ..
            } => base
                .pl(px(6. + depth as f32 * 14.))
                .text_color(content(0.8))
                .hover(|s| s.bg(content(0.05)))
                .child(
                    icon(
                        if open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        },
                        11.,
                    )
                    .text_color(content(0.45)),
                )
                .child(file_type_icon(&name, true, open, 14.))
                .child(div().min_w_0().truncate().child(name))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.collapsed.remove(&key) {
                        this.collapsed.insert(key.clone());
                    }
                    this.rebuild_rows();
                    cx.notify();
                }))
                .into_any_element(),
            TreeRow::File { ix: file_ix, depth } => {
                let Some(file) = self.report().and_then(|r| r.files.get(file_ix)).cloned() else {
                    return base.into_any_element();
                };
                let selected = self.selected.as_deref() == Some(file.relative.as_str());
                let name = basename(&file.relative).to_owned();
                let tint = match file.status.as_str() {
                    "deleted" => Some(rgb(DIFF_DEL)),
                    "added" => Some(rgb(DIFF_ADD)),
                    _ => None,
                };
                base.pl(px(6. + 14. + depth as f32 * 14.))
                    .text_color(content(if selected { 1. } else { 0.8 }))
                    .when(selected, |d| d.bg(content(0.1)))
                    .hover(|s| s.bg(content(0.06)))
                    .child(file_type_icon(&name, false, false, 14.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when_some(tint, |d, tint| d.text_color(tint))
                            .child(name),
                    )
                    .child(diff_counts(
                        file.additions.max(0) as u32,
                        file.deletions.max(0) as u32,
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(file_ix, cx)))
                    .into_any_element()
            }
        }
    }

    fn summary(&self, report: &UpdateReport) -> String {
        if matches!(self.source, Source::Push { .. }) {
            let n = report.commits.len();
            return if n == 0 {
                format!("Nothing to push to {}", report.upstream)
            } else {
                format!(
                    "{n} commit{} to push to {} · {} file{} changed",
                    if n == 1 { "" } else { "s" },
                    report.upstream,
                    report.files.len(),
                    if report.files.len() == 1 { "" } else { "s" },
                )
            };
        }
        let verb = if report.rebase {
            "Rebased onto"
        } else {
            "Merged"
        };
        if !report.conflicts.is_empty() {
            format!("Conflicts while bringing in {}", report.upstream)
        } else if report.commits.is_empty() {
            format!("Already up to date with {}", report.upstream)
        } else {
            let n = report.commits.len();
            format!(
                "{verb} {} · {n} commit{} · {} file{} changed",
                report.upstream,
                if n == 1 { "" } else { "s" },
                report.files.len(),
                if report.files.len() == 1 { "" } else { "s" },
            )
        }
    }

    fn commit_list(&self, report: &UpdateReport) -> AnyElement {
        div()
            .id("update-commits")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .py_1()
            .children(report.commits.iter().enumerate().map(|(ix, commit)| {
                div()
                    .id(("update-commit", ix))
                    .h(px(ROW))
                    .mx(px(6.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_family(crate::fonts::mono())
                            .text_size(px(11.))
                            .text_color(content(0.4))
                            .child(commit.short_sha.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(content(0.85))
                            .child(commit.subject.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .max_w(px(110.))
                            .truncate()
                            .text_color(content(0.4))
                            .child(commit.author.clone()),
                    )
                    .child(
                        div()
                            .w(px(34.))
                            .flex_shrink_0()
                            .text_right()
                            .text_color(content(0.35))
                            .child(age(commit.timestamp)),
                    )
            }))
            .into_any_element()
    }
}

/// The diff pane's model for one file.
pub(crate) fn model(
    id: &SharedString,
    relative: &str,
    loaded: Option<(FileDiff, Option<diff::UnifiedFile>)>,
) -> DiffFile {
    let (binary, too_large, additions, deletions, blocks, empty_message) = match loaded {
        None => (
            false,
            false,
            0,
            0,
            Arc::new(Vec::new()),
            Some("Loading…".to_string()),
        ),
        Some((file, unified)) => {
            let (additions, deletions, blocks) = unified
                .map(|u| (u.additions, u.deletions, u.blocks))
                .unwrap_or_default();
            (
                file.binary,
                file.too_large,
                additions,
                deletions,
                Arc::new(blocks),
                None,
            )
        }
    };
    DiffFile {
        id: id.clone(),
        path: relative.to_owned(),
        label: relative.to_owned(),
        binary,
        too_large,
        empty_message,
        additions,
        deletions,
        blocks,
        can_stage: false,
        can_discard: false,
        can_stage_hunk: false,
    }
}

impl Render for UpdateDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let (room_w, room_h) = (
            (viewport.width.as_f32() - 2. * MARGIN).max(MIN_SIZE.0.min(viewport.width.as_f32())),
            (viewport.height.as_f32() - 2. * MARGIN).max(MIN_SIZE.1.min(viewport.height.as_f32())),
        );
        let (width, height) = if self.maximized {
            (room_w, room_h)
        } else if let Some((w, h)) = self.custom {
            (
                w.clamp(MIN_SIZE.0.min(room_w), room_w),
                h.clamp(MIN_SIZE.1.min(room_h), room_h),
            )
        } else {
            (
                (viewport.width.as_f32() - 80.)
                    .clamp(560., 1040.)
                    .min(room_w),
                (viewport.height.as_f32() - 80.)
                    .clamp(420., 720.)
                    .min(room_h),
            )
        };
        let running = matches!(self.phase, Phase::Running);
        let entity = cx.entity();
        let count = self.rows.len();
        let title = match &self.source {
            Source::Update(UpdateMethod::Rebase) => "Update Project (Rebase)".to_string(),
            Source::Update(UpdateMethod::Merge) => "Update Project".to_string(),
            Source::Merge(branch) => format!("Merge {branch}"),
            Source::RebaseOnto(branch) => format!("Rebase onto {branch}"),
            Source::Push { .. } => "Push".to_string(),
        };
        let (subtitle, conflicts) = match &self.phase {
            Phase::Running if matches!(self.source, Source::Push { .. }) => {
                ("Reading what will be pushed…".to_string(), vec![])
            }
            Phase::Running => ("Bringing in new commits…".to_string(), vec![]),
            Phase::Failed(_) => ("Update failed".to_string(), vec![]),
            Phase::Done(report) => (self.summary(report), report.conflicts.clone()),
        };
        let body: AnyElement = match &self.phase {
            Phase::Running => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .text_size(px(13.))
                .text_color(content(0.55))
                .child(spinner("update-spin", "loader", 16.))
                .child("Updating…")
                .into_any_element(),
            Phase::Failed(error) => div()
                .id("update-error")
                .flex_1()
                .overflow_y_scroll()
                .p_4()
                .text_size(px(13.))
                .text_color(rgb(0xf87171))
                .child(error.clone())
                .into_any_element(),
            Phase::Done(report) if !report.conflicts.is_empty() => div()
                .id("update-conflicts")
                .flex_1()
                .overflow_y_scroll()
                .p_4()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .mb_2()
                        .text_size(px(13.))
                        .text_color(content(0.8))
                        .child(
                            "These files conflict. Resolve them in your editor and finish the \
                             update from a terminal, or abort to restore the branch.",
                        ),
                )
                .children(report.conflicts.iter().map(|path| {
                    div()
                        .font_family(crate::fonts::mono())
                        .text_size(px(12.))
                        .text_color(rgb(0xf87171))
                        .child(path.clone())
                }))
                .into_any_element(),
            Phase::Done(report) if report.commits.is_empty() => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.))
                .text_color(content(0.5))
                .child(if matches!(self.source, Source::Push { .. }) {
                    "Nothing to push."
                } else {
                    "Everything is up to date."
                })
                .into_any_element(),
            Phase::Done(report) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(height * 0.34))
                        .flex_shrink_0()
                        .flex()
                        .child(
                            div()
                                .w(px(width * 0.5))
                                .flex_shrink_0()
                                .flex()
                                .flex_col()
                                .child(self.commit_list(report)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .border_l_1()
                                .border_color(stroke())
                                .flex()
                                .flex_col()
                                .child(if count == 0 {
                                    div()
                                        .p_4()
                                        .text_size(px(12.))
                                        .text_color(content(0.45))
                                        .child("No file changes")
                                        .into_any_element()
                                } else {
                                    uniform_list("update-files", count, move |range, _, cx| {
                                        entity.update(cx, |this, cx| {
                                            range
                                                .map(|ix| this.file_row(ix, cx))
                                                .collect::<Vec<_>>()
                                        })
                                    })
                                    .py_1()
                                    .flex_1()
                                    .min_h_0()
                                    .into_any_element()
                                }),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .border_t_1()
                        .border_color(stroke())
                        .child(self.diff_view.clone()),
                )
                .into_any_element(),
        };
        let (can_push, push_notes) = match (&self.source, &self.phase) {
            (Source::Push { warn }, Phase::Done(report)) => {
                let mut notes: Vec<String> = warn.iter().cloned().collect();
                if report.behind > 0 {
                    notes.push(format!(
                        "{} is {} commit{} ahead of this branch; the push will be rejected until you update.",
                        report.upstream,
                        report.behind,
                        if report.behind == 1 { "" } else { "s" }
                    ));
                }
                (!report.commits.is_empty(), notes)
            }
            _ => (false, Vec::new()),
        };
        let card = modal::card()
            .debug_selector(|| "update-card".into())
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .h(px(44.))
                    .flex_shrink_0()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(stroke())
                    .id("update-header")
                    .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                        if event.click_count() == 2 {
                            this.toggle_maximized(cx);
                        }
                    }))
                    .child(modal::title(title))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(content(0.5))
                            .child(subtitle),
                    )
                    .child(
                        icon_button(
                            "update-maximize",
                            if self.maximized {
                                "Restore size"
                            } else {
                                "Fill the window"
                            },
                            if self.maximized {
                                "minimize-2"
                            } else {
                                "maximize-2"
                            },
                        )
                        .debug_selector(|| "update-maximize".into())
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_maximized(cx)
                        })),
                    ),
            )
            .child(body)
            .child(
                div()
                    .flex_shrink_0()
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(stroke())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .text_size(px(12.))
                            .children(
                                push_notes
                                    .into_iter()
                                    .map(|note| div().text_color(rgb(0xfbbf24)).child(note)),
                            )
                            .child(div().text_color(rgb(0xf87171)).children(self.error.clone())),
                    )
                    .when(!conflicts.is_empty(), |d| {
                        d.child(
                            modal::disabled(
                                modal::danger_button(
                                    "update-abort",
                                    if self.aborting {
                                        "Aborting…"
                                    } else {
                                        "Abort update"
                                    },
                                ),
                                self.aborting,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.abort(cx))),
                        )
                    })
                    .when(can_push, |d| {
                        d.child(
                            modal::disabled(
                                modal::primary_button(
                                    "update-push",
                                    if self.pushing { "Pushing…" } else { "Push" },
                                ),
                                self.pushing,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.push_now(cx))),
                        )
                    })
                    .child(
                        modal::disabled(
                            if conflicts.is_empty() && !can_push {
                                modal::primary_button("update-close", "Close")
                            } else {
                                modal::ghost_button(
                                    "update-close",
                                    if can_push { "Cancel" } else { "Close" },
                                )
                            },
                            running || self.pushing,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !matches!(this.phase, Phase::Running) && !this.pushing {
                                cx.emit(UpdateDialogEvent::Close)
                            }
                        })),
                    ),
            )
            .child(
                // The corner grip. The card stays centred, so it grows by twice
                // the pointer's travel to keep the corner under the pointer.
                div()
                    .id("update-resize")
                    .debug_selector(|| "update-resize".into())
                    .role(gpui::Role::Splitter)
                    .aria_label("Resize dialog")
                    .absolute()
                    .right_0()
                    .bottom_0()
                    .size(px(18.))
                    .cursor(gpui::CursorStyle::ResizeUpLeftDownRight)
                    .child(
                        div()
                            .absolute()
                            .right(px(4.))
                            .bottom(px(4.))
                            .size(px(8.))
                            .border_r_2()
                            .border_b_2()
                            .border_color(content(0.3)),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            this.resizing = Some(Resize {
                                start: event.position,
                                from: (width, height),
                            });
                            this.maximized = false;
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ),
            );
        div()
            .size_full()
            .child(
                // The overlay hides the dialog from hover, so a corner drag
                // listens at the window instead.
                canvas(|_, _, _| (), {
                    let weak = cx.weak_entity();
                    move |_, _, window, _| {
                        let moves = weak.clone();
                        window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                            if phase == gpui::DispatchPhase::Capture {
                                let _ = moves.update(cx, |this, cx| this.drag_resize(event, cx));
                            }
                        });
                        let ups = weak.clone();
                        window.on_mouse_event(move |_: &gpui::MouseUpEvent, phase, _, cx| {
                            if phase == gpui::DispatchPhase::Capture {
                                let _ = ups.update(cx, |this, cx| this.finish_resize(cx));
                            }
                        });
                    }
                })
                .absolute()
                .size_0(),
            )
            .track_focus(&self.focus)
            .key_context("UpdateDialog")
            .on_key_down(cx.listener(Self::key))
            .child(modal::overlay("update-dialog", card, {
                let weak = cx.weak_entity();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if !matches!(this.phase, Phase::Running) && !this.pushing {
                            cx.emit(UpdateDialogEvent::Close);
                        }
                    });
                }
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_reads_in_the_largest_whole_unit() {
        let now = monocode_core::time::now_ms() as i64 / 1000;
        assert_eq!(age(now), "now");
        assert_eq!(age(now - 5 * 60), "5m");
        assert_eq!(age(now - 3 * 3600), "3h");
        assert_eq!(age(now - 2 * 86400), "2d");
    }

    fn open(cx: &mut gpui::TestAppContext) -> gpui::WindowHandle<UpdateDialog> {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        std::mem::forget(dir);
        cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let git = cx.new(|cx| ProjectGit::new(cwd.clone(), cx));
                cx.new(|cx| UpdateDialog::new(git, cwd, Source::Merge("topic".into()), window, cx))
            })
            .unwrap()
        })
    }

    #[gpui::test]
    fn the_dialog_fills_the_window_on_request_and_grows_by_its_corner(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{MouseButton, VisualTestContext, point};
        *REMEMBERED_SIZE.lock().unwrap() = None;
        let window = open(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let card = visual.debug_bounds("update-card").expect("the dialog");
        let viewport = visual.update(|window, _| window.viewport_size());
        assert!(card.size.width < viewport.width - px(2. * MARGIN) + px(0.5));
        // Fill the window and come back.
        let toggle = visual.debug_bounds("update-maximize").unwrap();
        visual.simulate_click(toggle.center(), Default::default());
        visual.run_until_parked();
        let full = visual.debug_bounds("update-card").unwrap();
        assert_eq!(full.size.width, viewport.width - px(2. * MARGIN));
        assert_eq!(full.size.height, viewport.height - px(2. * MARGIN));
        let toggle = visual.debug_bounds("update-maximize").unwrap();
        visual.simulate_click(toggle.center(), Default::default());
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("update-card").unwrap().size, card.size);
        // Dragging the corner resizes about the centre.
        let grip = visual.debug_bounds("update-resize").unwrap().center();
        visual.simulate_mouse_down(grip, MouseButton::Left, Default::default());
        let to = grip - point(px(40.), px(30.));
        visual.simulate_mouse_move(to, MouseButton::Left, Default::default());
        visual.simulate_mouse_up(to, MouseButton::Left, Default::default());
        visual.run_until_parked();
        let smaller = visual.debug_bounds("update-card").unwrap();
        assert_eq!(smaller.size.width, card.size.width - px(80.));
        assert_eq!(smaller.size.height, card.size.height - px(60.));
        // The next dialog opens at that size.
        let again = open(cx);
        let visual = &mut VisualTestContext::from_window(again.into(), cx);
        visual.run_until_parked();
        assert_eq!(
            visual.debug_bounds("update-card").unwrap().size,
            smaller.size
        );
        *REMEMBERED_SIZE.lock().unwrap() = None;
    }
}
